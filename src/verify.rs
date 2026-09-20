//! Did this message really come from the address in the From: line?
//!
//! Email was designed with no way to prove who sent it — anyone can put anything
//! in `From:`. SPF, DKIM and DMARC close that hole, but the checks are performed
//! by the *receiving* server, which records the outcome in an
//! `Authentication-Results` header. This module reads that verdict back out and
//! turns it into something a person can act on.
//!
//! **Which headers to trust.** A sender can put whatever they like in the message
//! they hand us, `Authentication-Results` included. What they cannot do is forge
//! the headers our own provider prepends on delivery. Trace headers are
//! prepended, so the ones nearest the top are the last hop's — our provider's.
//! Providers don't agree on layout: Gmail packs every method into one header,
//! iCloud emits one per method (`dmarc.icloud.com`, `dkim-verifier.icloud.com`,
//! …) and leads with BIMI. So [`check_sender`] reads them all in order and keeps
//! the *first* verdict for each method, which is the provider's; a forged copy
//! further down loses. The residual gap: if a provider reports nothing at all for
//! a method, a forged verdict below could be the only one found. The authority
//! that supplied each verdict is therefore reported in the findings, where an
//! unfamiliar name is itself the tell.
//!
//! **What a pass means.** It proves the From: domain wasn't forged. It does not
//! prove the message is safe: a phisher who registers `amaz0n-security.com` and
//! sets up DKIM properly gets a clean pass. That's why domain and display-name
//! mismatches are reported alongside the authentication verdict.

use crate::models::{SenderCheck, SenderTrust};
use crate::i18n::{i18n, i18n_f};

/// One authentication method's outcome, as reported by the receiving server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthResult {
    Pass,
    Fail,
    /// Present but inconclusive (`none`, `neutral`, `temperror`, `policy`, …).
    Other,
    /// The method wasn't reported at all.
    Missing,
}

impl AuthResult {
    fn parse(value: &str) -> AuthResult {
        match value {
            "pass" => AuthResult::Pass,
            // `softfail` is SPF's "probably not authorised"; DMARC treats it as a
            // failure and so do we.
            "fail" | "softfail" | "permerror" => AuthResult::Fail,
            _ => AuthResult::Other,
        }
    }
}

/// Everything [`check_sender`] extracts before forming a verdict. Split out so
/// the parsing and the judgement can be tested apart from each other.
#[derive(Debug, Default)]
struct Evidence {
    spf: Option<AuthResult>,
    dkim: Option<AuthResult>,
    dmarc: Option<AuthResult>,
    /// Domain that signed the message (`header.d=`), if DKIM reported one.
    dkim_domain: Option<String>,
    from_domain: String,
    from_name: String,
    reply_to_domain: Option<String>,
    return_path_domain: Option<String>,
    /// Whether the receiving server reported any authentication at all.
    checked: bool,
    /// Which authserv-id supplied each verdict, so the details can name who
    /// vouched — a check attributed to an unfamiliar authority is itself a clue.
    authorities: Vec<(&'static str, String)>,
    /// How to leave the list this came from (RFC 2369/8058), read in the
    /// same pass over the headers so it rides with the verdict.
    unsubscribe: Option<crate::models::Unsubscribe>,
    /// The meeting it invites the reader to (#223), read in the same pass
    /// over the same parse, for the same reason.
    invite: Option<Box<crate::models::Invite>>,
}

impl Evidence {
    fn get(&self, which: Method) -> AuthResult {
        let slot = match which {
            Method::Spf => self.spf,
            Method::Dkim => self.dkim,
            Method::Dmarc => self.dmarc,
        };
        slot.unwrap_or(AuthResult::Missing)
    }
}

#[derive(Clone, Copy)]
enum Method {
    Spf,
    Dkim,
    Dmarc,
}

/// Check whether a raw RFC 822 message's From: address was forged.
pub fn check_sender(raw: &[u8]) -> SenderCheck {
    let evidence = gather(raw);
    judge(&evidence)
}

/// Pull the authentication verdict and addressing out of the raw message.
fn gather(raw: &[u8]) -> Evidence {
    use mail_parser::MessageParser;

    let mut ev = Evidence::default();
    let Some(parsed) = MessageParser::default().parse(raw) else {
        return ev;
    };
    ev.unsubscribe = crate::unsubscribe::detect(&parsed);
    ev.invite = crate::invite::detect(&parsed).map(Box::new);


    let first_addr = |a: Option<&mail_parser::Address>| -> Option<String> {
        a.and_then(|a| a.first())
            .and_then(|x| x.address())
            .map(|s| s.to_ascii_lowercase())
    };

    if let Some(addr) = first_addr(parsed.from()) {
        ev.from_domain = domain_of(&addr).unwrap_or_default();
    }
    ev.from_name = parsed
        .from()
        .and_then(|a| a.first())
        .and_then(|x| x.name())
        .unwrap_or_default()
        .to_string();
    ev.reply_to_domain = first_addr(parsed.reply_to()).and_then(|a| domain_of(&a));
    // Return-Path is the bounce address (SMTP MAIL FROM) — the envelope sender,
    // which is what SPF actually authorises. It routinely differs from From: for
    // legitimate bulk mail, so it is reported, never judged on its own.
    ev.return_path_domain = parsed
        .header("Return-Path")
        .and_then(header_text)
        .and_then(|v| domain_of(v.trim().trim_start_matches('<').trim_end_matches('>')));


    // Providers split their verdicts differently: Gmail packs SPF, DKIM and DMARC
    // into one header, while iCloud emits one header per method, each with its
    // own authserv-id (`dmarc.icloud.com`, `dkim-verifier.icloud.com`, …) — and
    // puts BIMI first. Reading only the topmost header therefore saw nothing but
    // BIMI and reported every iCloud message as unverified. Scan them all in
    // order instead, keeping the first verdict found for each method: trace
    // headers are prepended, so the first is the most recent, and the provider's
    // own verdict still wins over anything the sender shipped further down.
    for header in parsed
        .headers()
        .iter()
        .filter(|h| h.name().eq_ignore_ascii_case("Authentication-Results"))
    {
        if let Some(line) = header_text(&header.value) {
            ev.checked = true;
            parse_auth_results(&line, &mut ev);
        }
    }
    // Some servers only emit Received-SPF. Use it when Authentication-Results
    // didn't report SPF itself.
    if ev.spf.is_none() {
        if let Some(line) = parsed.header("Received-SPF").and_then(header_text) {
            let word = line.split_whitespace().next().unwrap_or("").to_string();
            ev.spf = Some(AuthResult::parse(&word.to_ascii_lowercase()));
            ev.checked = true;
        }
    }
    ev
}

/// A header's text, whatever shape `mail_parser` stored it in.
fn header_text(value: &mail_parser::HeaderValue) -> Option<String> {
    match value {
        mail_parser::HeaderValue::Text(t) => Some(t.to_string()),
        mail_parser::HeaderValue::TextList(list) => list.first().map(|t| t.to_string()),
        _ => None,
    }
}

/// Read `spf=`, `dkim=` and `dmarc=` (plus DKIM's `header.d=`) out of one
/// `Authentication-Results` header.
fn parse_auth_results(line: &str, ev: &mut Evidence) {
    let lower = line.to_ascii_lowercase();
    // Everything before the first `;` is the authserv-id: who is vouching.
    let authority = lower.split(';').next().unwrap_or("").trim().to_string();

    let mut this_spf = None;
    let mut this_dkim = None;
    let mut this_dmarc = None;
    let mut this_domain = None;
    // Tokens are separated by `;` and whitespace; values may trail a comment,
    // e.g. `dmarc=pass (policy=none)`.
    for token in lower.split([';', ' ', '\t', '\r', '\n']) {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        let value =
            value.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '-');
        match key {
            "spf" => this_spf = Some(AuthResult::parse(value)),
            "dkim" => this_dkim = Some(AuthResult::parse(value)),
            "dmarc" => this_dmarc = Some(AuthResult::parse(value)),
            "header.d" if !value.is_empty() => this_domain = Some(value.to_string()),
            _ => {}
        }
    }

    // First header to report a method wins it. `header.d` counts only when this
    // same header reported DKIM — iCloud's BIMI header carries a `header.d` too,
    // and crediting it as the signing domain would invent a signature that was
    // never checked.
    if ev.spf.is_none() {
        if let Some(r) = this_spf {
            ev.spf = Some(r);
            ev.authorities.push(("SPF", authority.clone()));
        }
    }
    if ev.dkim.is_none() {
        if let Some(r) = this_dkim {
            ev.dkim = Some(r);
            ev.dkim_domain = this_domain;
            ev.authorities.push(("DKIM", authority.clone()));
        }
    }
    if ev.dmarc.is_none() {
        if let Some(r) = this_dmarc {
            ev.dmarc = Some(r);
            ev.authorities.push(("DMARC", authority));
        }
    }
}

/// Whether at least one authentication method failed and none of them passed —
/// the only no-DMARC shape worth remarking on.
fn failed_and_nothing_passed(dkim: AuthResult, spf: AuthResult) -> bool {
    let any_failed = dkim == AuthResult::Fail || spf == AuthResult::Fail;
    let any_passed = dkim == AuthResult::Pass || spf == AuthResult::Pass;
    any_failed && !any_passed
}

/// Turn the evidence into a verdict a person can act on.
fn judge(ev: &Evidence) -> SenderCheck {
    let mut findings = Vec::new();
    let from = if ev.from_domain.is_empty() {
        "(unknown)"
    } else {
        &ev.from_domain
    };

    // --- Authentication ----------------------------------------------------
    let dmarc = ev.get(Method::Dmarc);
    let dkim = ev.get(Method::Dkim);
    let spf = ev.get(Method::Spf);
    // DKIM only vouches for the From: domain when the signing domain lines up
    // with it — that alignment is exactly what DMARC checks, so we only fall
    // back to it when DMARC wasn't reported.
    let dkim_aligned = dkim == AuthResult::Pass
        && ev
            .dkim_domain
            .as_deref()
            .is_some_and(|d| same_site(d, &ev.from_domain));

    let (mut trust, summary) = match (dmarc, dkim, spf) {
        (AuthResult::Pass, _, _) => (
            SenderTrust::Pass,
            i18n_f("Your mail provider confirmed this really came from {from}.", &[("from", &from.to_string())]),
        ),
        (AuthResult::Fail, _, _) => (
            SenderTrust::Fail,
            i18n_f("This message failed the checks that prove it came from {from}. Treat it as a forgery.", &[("from", &from.to_string())]),
        ),
        _ if dkim_aligned => (
            SenderTrust::Pass,
            i18n_f("This message carries a valid signature from {from}.", &[("from", &from.to_string())]),
        ),
        // Without a DMARC verdict, one method failing while another passes is
        // routine: mail relayed through a bulk sender or a mailing list breaks
        // DKIM or SPF as a matter of course. Only when *nothing* passed is there
        // something to say — and even then it's "couldn't confirm", not
        // "forgery", which is a claim only DMARC's alignment check can support.
        // Crying forgery over a broken relay signature would teach you to ignore
        // the badge, which is worse than not having one.
        _ if failed_and_nothing_passed(dkim, spf) => (
            SenderTrust::Suspicious,
            i18n_f("This message couldn't be confirmed as coming from {from}, and the checks that ran on it failed.", &[("from", &from.to_string())]),
        ),
        _ if !ev.checked => (
            SenderTrust::Unverified,
            i18n("Your mail provider didn't report any sender checks for this message."),
        ),
        _ => (
            SenderTrust::Unverified,
            i18n_f("Nothing here proves this came from {from} — or that it didn't.", &[("from", &from.to_string())]),
        ),
    };

    let describe = |name: &str, r: AuthResult| match r {
        AuthResult::Pass => Some(i18n_f("{name}: passed", &[("name", name)])),
        AuthResult::Fail => Some(i18n_f("{name}: FAILED", &[("name", name)])),
        AuthResult::Other => Some(i18n_f("{name}: inconclusive", &[("name", name)])),
        AuthResult::Missing => None,
    };
    findings.extend(describe(&i18n("DMARC (is the From: address genuine)"), dmarc));
    findings.extend(describe(&i18n("DKIM (signature)"), dkim));
    findings.extend(describe(&i18n("SPF (sending server)"), spf));
    if let Some(d) = &ev.dkim_domain {
        findings.push(i18n_f("Signed by: {d}", &[("d", d)]));
    }
    if let Some((_, who)) = ev.authorities.first() {
        findings.push(i18n_f("Checked by: {who}", &[("who", who)]));
    }
    findings.push(i18n_f("From: {from}", &[("from", &from.to_string())]));

    // --- Addressing --------------------------------------------------------
    // These never rescue a failure, but they can pull a technically-authenticated
    // message down: the classic phish is a real domain nobody recognises wearing
    // a name everybody does.
    let mut downgraded_by_addressing = false;
    if let Some(reply) = &ev.reply_to_domain {
        if !same_site(reply, &ev.from_domain) {
            findings.push(i18n_f("Replies would go to {reply}, not {from}", &[("reply", reply), ("from", &from.to_string())]));
            if trust == SenderTrust::Pass {
                trust = SenderTrust::Suspicious;
                downgraded_by_addressing = true;
            }
        }
    }
    if let Some(rp) = &ev.return_path_domain {
        if !same_site(rp, &ev.from_domain) {
            findings.push(i18n_f("Bounces would go to {rp}", &[("rp", rp)]));
        }
    }
    if let Some(claimed) = domain_in_display_name(&ev.from_name) {
        if !same_site(&claimed, &ev.from_domain) {
            findings.push(i18n_f(
                "The sender's name says \"{claimed}\" but the address is {from}",
                &[("claimed", &claimed), ("from", &from.to_string())],
            ));
            if trust == SenderTrust::Pass {
                trust = SenderTrust::Suspicious;
                downgraded_by_addressing = true;
            }
        }
    }

    // Only relabel when the *addressing* is what pulled a clean pass down; a
    // "couldn't confirm" verdict already carries the right sentence.
    let summary = if trust == SenderTrust::Suspicious && downgraded_by_addressing {
        format!("{from} checks out, but the addressing doesn't match the name on the message.")
    } else {
        summary
    };
    SenderCheck {
        trust,
        summary,
        findings,
        pgp: None,
        unsubscribe: ev.unsubscribe.clone(),
        invite: ev.invite.clone(),
    }
}

/// The domain part of an email address, lowercased.
fn domain_of(addr: &str) -> Option<String> {
    let (_, domain) = addr.rsplit_once('@')?;
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    (!domain.is_empty()).then_some(domain)
}

/// Whether two domains belong to the same site — `mail.amazon.in` and
/// `amazon.in` do, `amazon.in` and `amazon-security.com` don't.
///
/// This compares the registrable domain, approximated without the Public Suffix
/// List: the last two labels, or three when the second-to-last is one of the
/// common two-part suffixes. An approximation can only mis-group siblings under
/// an unusual suffix, and every use here is advisory — it adds a note, it never
/// turns a pass into a failure.
fn same_site(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && registrable(a) == registrable(b)
}

fn registrable(domain: &str) -> String {
    const TWO_PART: &[&str] = &[
        "co", "com", "net", "org", "gov", "edu", "ac", "or", "ne", "gouv",
    ];
    let labels: Vec<&str> = domain.split('.').filter(|l| !l.is_empty()).collect();
    let n = labels.len();
    if n < 3 {
        return labels.join(".");
    }
    // `foo.co.uk` keeps three labels; `mail.amazon.in` keeps two.
    let take = if labels[n - 1].len() <= 3 && TWO_PART.contains(&labels[n - 2]) {
        3
    } else {
        2
    };
    labels[n - take..].join(".")
}

/// A domain a display name claims to be, if it states one — either an embedded
/// address (`"billing@apple.com" <x@evil.com>`) or a bare hostname
/// (`"Amazon.co.uk Security"`). Returns `None` for ordinary human names.
fn domain_in_display_name(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    if let Some(at) = lower.find('@') {
        let rest = &lower[at + 1..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
            .unwrap_or(rest.len());
        let domain = rest[..end].trim_end_matches('.');
        if domain.contains('.') {
            return Some(domain.to_string());
        }
    }
    // A bare hostname: a word with a dot and a plausible TLD.
    lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
        .find(|word| {
            let word = word.trim_end_matches('.');
            match word.rsplit_once('.') {
                Some((host, tld)) => {
                    !host.is_empty()
                        && (2..=24).contains(&tld.len())
                        && tld.chars().all(|c| c.is_ascii_alphabetic())
                }
                None => false,
            }
        })
        .map(|w| w.trim_end_matches('.').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(headers: &str) -> Vec<u8> {
        format!("{headers}\r\n\r\nbody\r\n").into_bytes()
    }

    #[test]
    fn dmarc_pass_is_a_verified_sender() {
        let check = check_sender(&msg(concat!(
            "Authentication-Results: mx.icloud.com; spf=pass smtp.mailfrom=amazon.in; ",
            "dkim=pass header.d=amazon.in; dmarc=pass (policy=reject)\r\n",
            "From: Amazon <account-update@amazon.in>\r\n",
            "Subject: Your order",
        )));
        assert_eq!(check.trust, SenderTrust::Pass);
        assert!(check.findings.iter().any(|f| f.contains("DMARC")), "{check:?}");
    }

    /// iCloud splits its verdicts across several headers, one per method, and
    /// puts BIMI first — which carries a `header.d` but no authentication result.
    /// Reading only the topmost header reported every iCloud message as
    /// unverified, and credited BIMI's domain as the DKIM signer.
    #[test]
    fn icloud_one_header_per_method_is_read_in_full() {
        let check = check_sender(&msg(concat!(
            "Authentication-Results: bimi.icloud.com; bimi=pass header.d=amazon.com header.selector=default\r\n",
            "Authentication-Results: arc.icloud.com; arc=none\r\n",
            "Authentication-Results: dmarc.icloud.com; dmarc=pass header.from=amazon.com\r\n",
            "Authentication-Results: dkim-verifier.icloud.com; dkim=pass header.d=amazon.com header.i=@amazon.com\r\n",
            "Authentication-Results: spf.icloud.com; spf=pass smtp.mailfrom=bounces.amazon.com\r\n",
            "From: Amazon.com <account-update@amazon.com>\r\n",
        )));
        assert_eq!(check.trust, SenderTrust::Pass, "{check:?}");
        assert!(check.findings.iter().any(|f| f.contains("Checked by: dmarc.icloud.com")), "{check:?}");
    }

    /// A `header.d` on a header that reported no DKIM result (iCloud's BIMI one)
    /// must not be credited as the signing domain.
    #[test]
    fn a_domain_from_a_non_dkim_header_is_not_a_signature() {
        let check = check_sender(&msg(concat!(
            "Authentication-Results: bimi.icloud.com; bimi=pass header.d=amazon.com\r\n",
            "From: Amazon <account-update@amazon.com>\r\n",
        )));
        assert_eq!(check.trust, SenderTrust::Unverified, "{check:?}");
        assert!(!check.findings.iter().any(|f| f.starts_with("Signed by")), "{check:?}");
    }

    #[test]
    fn dmarc_fail_is_a_forgery() {
        let check = check_sender(&msg(concat!(
            "Authentication-Results: mx.icloud.com; spf=fail; dkim=none; dmarc=fail\r\n",
            "From: Amazon <account-update@amazon.in>\r\n",
        )));
        assert_eq!(check.trust, SenderTrust::Fail);
        assert!(check.summary.contains("forgery"), "{check:?}");
    }

    #[test]
    fn a_forged_authentication_results_in_the_body_cannot_launder_a_failure() {
        // The attacker's own header sits *below* the receiving server's, because
        // trace headers are prepended on delivery. Only the topmost counts.
        let check = check_sender(&msg(concat!(
            "Authentication-Results: mx.icloud.com; dmarc=fail\r\n",
            "Authentication-Results: totally.legit; spf=pass; dkim=pass; dmarc=pass\r\n",
            "From: Amazon <account-update@amazon.in>\r\n",
        )));
        assert_eq!(check.trust, SenderTrust::Fail);
    }

    /// Real mail from the author's mailbox: a billing notice relayed through a
    /// bulk sender. DKIM breaks in the relay, SPF passes on the relay's own
    /// domain, and no DMARC verdict is reported. Calling this a forgery — as an
    /// earlier rule did — cried wolf on legitimate mail from Toyota and T-Mobile.
    #[test]
    fn a_relay_that_breaks_dkim_while_spf_passes_is_not_a_forgery() {
        let check = check_sender(&msg(concat!(
            "Authentication-Results: mx.icloud.com; dkim=fail header.d=tracking.epriority.com; ",
            "spf=pass smtp.mailfrom=tracking.epriority.com\r\n",
            "From: Toyota Financial <toyotafinancial@toyota.com>\r\n",
            "Return-Path: <bounce@tracking.epriority.com>\r\n",
        )));
        assert_ne!(check.trust, SenderTrust::Fail, "{check:?}");
        // The detail is still there for anyone who looks.
        assert!(
            check.findings.iter().any(|f| f.contains("DKIM") && f.contains("FAILED")),
            "{check:?}"
        );
    }

    /// Also real: a DMARC failure, which *is* authoritative — it checks alignment
    /// with the From: domain, so nothing legitimate explains it away.
    #[test]
    fn a_reported_dmarc_failure_is_still_a_forgery() {
        let check = check_sender(&msg(concat!(
            "Authentication-Results: mx.icloud.com; dmarc=fail; spf=none\r\n",
            "From: AT&T <attorderstatus@oceff.att-mail.com>\r\n",
        )));
        assert_eq!(check.trust, SenderTrust::Fail);
    }

    #[test]
    fn everything_reported_failing_is_flagged_without_claiming_forgery() {
        let check = check_sender(&msg(concat!(
            "Authentication-Results: mx.icloud.com; spf=fail; dkim=fail\r\n",
            "From: Bank <alerts@bank.example>\r\n",
        )));
        assert_eq!(check.trust, SenderTrust::Suspicious);
    }

    #[test]
    fn no_authentication_results_is_unverified_not_a_failure() {
        let check = check_sender(&msg("From: A Friend <friend@example.com>\r\n"));
        assert_eq!(check.trust, SenderTrust::Unverified);
    }

    #[test]
    fn aligned_dkim_alone_verifies_when_dmarc_is_silent() {
        let check = check_sender(&msg(concat!(
            "Authentication-Results: mx.icloud.com; dkim=pass header.d=mail.amazon.in\r\n",
            "From: Amazon <account-update@amazon.in>\r\n",
        )));
        assert_eq!(check.trust, SenderTrust::Pass);
    }

    #[test]
    fn dkim_signed_by_an_unrelated_domain_does_not_verify() {
        let check = check_sender(&msg(concat!(
            "Authentication-Results: mx.icloud.com; dkim=pass header.d=bulk-sender.net\r\n",
            "From: Amazon <account-update@amazon.in>\r\n",
        )));
        assert_ne!(check.trust, SenderTrust::Pass);
    }

    #[test]
    fn a_display_name_wearing_another_domain_is_suspicious() {
        let check = check_sender(&msg(concat!(
            "Authentication-Results: mx.icloud.com; dmarc=pass\r\n",
            "From: \"Amazon.in Security\" <no-reply@shop-alerts.example>\r\n",
        )));
        assert_eq!(check.trust, SenderTrust::Suspicious);
        assert!(
            check.findings.iter().any(|f| f.contains("amazon.in")),
            "{check:?}"
        );
    }

    #[test]
    fn a_reply_to_on_another_domain_is_suspicious() {
        let check = check_sender(&msg(concat!(
            "Authentication-Results: mx.icloud.com; dmarc=pass\r\n",
            "From: Support <support@shop-alerts.example>\r\n",
            "Reply-To: collector@gmail.com\r\n",
        )));
        assert_eq!(check.trust, SenderTrust::Suspicious);
    }

    #[test]
    fn an_authenticated_failure_is_never_softened_by_addressing() {
        let check = check_sender(&msg(concat!(
            "Authentication-Results: mx.icloud.com; dmarc=fail\r\n",
            "From: \"Amazon.in\" <account-update@amazon.in>\r\n",
            "Reply-To: collector@gmail.com\r\n",
        )));
        assert_eq!(check.trust, SenderTrust::Fail);
    }

    #[test]
    fn subdomains_count_as_the_same_site() {
        assert!(same_site("mail.amazon.in", "amazon.in"));
        assert!(same_site("amazon.co.uk", "help.amazon.co.uk"));
        assert!(!same_site("amazon.in", "amazon-security.in"));
        assert!(!same_site("amazon.co.uk", "amazon.co"));
    }

    #[test]
    fn display_names_that_state_no_domain_are_left_alone() {
        assert_eq!(domain_in_display_name("Charles Robinson"), None);
        assert_eq!(domain_in_display_name("Accounts Payable"), None);
        assert_eq!(
            domain_in_display_name("billing@apple.com"),
            Some("apple.com".into())
        );
        assert_eq!(
            domain_in_display_name("Amazon.in Security"),
            Some("amazon.in".into())
        );
    }

    #[test]
    fn received_spf_is_used_when_authentication_results_omits_spf() {
        let check = check_sender(&msg(concat!(
            "Received-SPF: fail (domain does not designate sender)\r\n",
            "From: Amazon <account-update@amazon.in>\r\n",
        )));
        assert_eq!(check.trust, SenderTrust::Suspicious);
    }
}
