//! Whose sender checks to believe.
//!
//! RFC 8601 §5 has a mail client read only the `Authentication-Results` of a
//! server it trusts: the provider's own border strips any copy claiming its
//! name, but a provider that stamps nothing leaves the sender's header on
//! top, `dmarc=pass` and all. Which server is the provider is not in the
//! message, so it is learned from the account's mail: the authority on top of
//! the mail of at least half the sites the account hears from. A forger would
//! have to send more of the account's mail than everyone else together.

use std::collections::{HashMap, HashSet};

use crate::i18n::{i18n, i18n_f};
use crate::models::{SenderCheck, SenderTrust};

use super::registrable;

/// Sites an authority has to have checked mail from before it is believed:
/// with fewer, half the account's correspondents is a handful of messages.
const MIN_SITES: usize = 3;

/// Per account, the sites whose mail each authority reported on.
#[derive(Debug, Default)]
pub(crate) struct Vouchers {
    accounts: HashMap<u32, Tally>,
}

#[derive(Debug, Default)]
struct Tally {
    /// Authority (its registrable domain, empty for an unnamed one) to the
    /// sender sites it reported on.
    by_authority: HashMap<String, HashSet<String>>,
    /// Every sender site seen, reported on or not.
    sites: HashSet<String>,
}

impl Tally {
    fn trusts(&self, authority: &str) -> bool {
        self.by_authority
            .get(&registrable(authority))
            .is_some_and(|s| s.len() >= MIN_SITES && s.len() * 2 >= self.sites.len())
    }

    fn trusted(&self) -> HashSet<&str> {
        self.by_authority
            .keys()
            .filter(|a| self.trusts(a))
            .map(String::as_str)
            .collect()
    }
}

impl Vouchers {
    /// Count a check the account received. True when that changed which
    /// authorities the account believes, so the verdicts on screen are due
    /// to be shown again.
    pub(crate) fn note(&mut self, account_id: u32, check: &SenderCheck) -> bool {
        if check.from_domain.is_empty() {
            return false;
        }
        let site = registrable(&check.from_domain);
        let tally = self.accounts.entry(account_id).or_default();
        let before: HashSet<String> = tally.trusted().into_iter().map(str::to_string).collect();
        tally.sites.insert(site.clone());
        if let Some(who) = &check.authority {
            tally
                .by_authority
                .entry(registrable(who))
                .or_default()
                .insert(site);
        }
        let after = tally.trusted();
        before.len() != after.len() || after.iter().any(|a| !before.contains(*a))
    }

    /// The check as the reader should show it: a verdict whose authority the
    /// account does not believe (or, with no account, a message opened from a
    /// file) proves nothing, so it reads as unverified.
    pub(crate) fn gate(&self, account_id: Option<u32>, check: &mut SenderCheck) {
        let Some(who) = &check.authority else {
            return;
        };
        if check.trust == SenderTrust::Unverified {
            return;
        }
        let believed = account_id
            .and_then(|a| self.accounts.get(&a))
            .is_some_and(|t| t.trusts(who));
        if believed {
            return;
        }
        let from = if check.from_domain.is_empty() {
            "(unknown)"
        } else {
            &check.from_domain
        };
        check.trust = SenderTrust::Unverified;
        check.summary = i18n_f(
            "Nothing here proves this came from {from} — or that it didn't.",
            &[("from", from)],
        );
        check
            .findings
            .push(i18n("These checks aren't known to come from your mail provider"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(from: &str, authority: Option<&str>, trust: SenderTrust) -> SenderCheck {
        SenderCheck {
            trust,
            authority: authority.map(str::to_string),
            from_domain: from.into(),
            ..Default::default()
        }
    }

    fn pass(from: &str, authority: &str) -> SenderCheck {
        check(from, Some(authority), SenderTrust::Pass)
    }

    fn shown(v: &Vouchers, account: Option<u32>, c: &SenderCheck) -> SenderTrust {
        let mut c = c.clone();
        v.gate(account, &mut c);
        c.trust
    }

    #[test]
    fn the_provider_is_believed_once_it_checked_mail_from_three_sites() {
        let mut v = Vouchers::default();
        let c = pass("gnome.org", "mx.google.com");
        assert_eq!(shown(&v, Some(1), &c), SenderTrust::Unverified);
        assert!(!v.note(1, &pass("a.example", "mx.google.com")));
        assert!(!v.note(1, &pass("b.example", "mx.google.com")));
        assert!(v.note(1, &pass("mail.c.example", "mx2.google.com")));
        assert_eq!(shown(&v, Some(1), &c), SenderTrust::Pass);
        // Per account, and never for a file.
        assert_eq!(shown(&v, Some(2), &c), SenderTrust::Unverified);
        assert_eq!(shown(&v, None, &c), SenderTrust::Unverified);
    }

    /// A server that stamps nothing: the only headers are the senders' own,
    /// and a forger's few cannot outweigh the rest of the account's mail.
    #[test]
    fn a_forged_authority_stays_unbelieved() {
        let mut v = Vouchers::default();
        for site in ["a.example", "b.example", "c.example", "d.example"] {
            v.note(1, &check(site, None, SenderTrust::Unverified));
        }
        for site in ["x.example", "y.example", "z.example"] {
            v.note(1, &pass(site, "mx.google.com"));
        }
        let forged = pass("bank.example", "mx.google.com");
        assert_eq!(shown(&v, Some(1), &forged), SenderTrust::Unverified);
        let mut c = forged.clone();
        v.gate(Some(1), &mut c);
        assert!(c.summary.contains("bank.example"));
        assert_eq!(c.findings.len(), 1);
    }

    #[test]
    fn unnamed_authorities_and_unchecked_mail_pass_through() {
        let mut v = Vouchers::default();
        for site in ["a.example", "b.example", "c.example"] {
            v.note(1, &pass(site, ""));
        }
        assert_eq!(shown(&v, Some(1), &pass("d.example", "")), SenderTrust::Pass);
        let none = check("d.example", None, SenderTrust::Suspicious);
        assert_eq!(shown(&Vouchers::default(), None, &none), SenderTrust::Suspicious);
        let failed = check("d.example", Some("evil.example"), SenderTrust::Fail);
        assert_eq!(shown(&v, Some(1), &failed), SenderTrust::Unverified);
    }
}
