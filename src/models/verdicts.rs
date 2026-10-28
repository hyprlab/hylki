//! What a fetch learns about a message besides its text: the sender check,
//! the OpenPGP verdict, how to unsubscribe, and a meeting invitation. The
//! cache stores these as JSON (see `cache::bodies`), so their serde shape
//! is an on-disk format.

use crate::i18n::{i18n, i18n_f};

/// How much a message's claimed sender can be trusted, worst finding wins.
///
/// This answers "did this really come from the domain in the From: line?" — not
/// "is this message safe". A phisher who registers their own domain and
/// authenticates it properly earns [`SenderTrust::Pass`]; the check proves the
/// From: address wasn't forged, nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SenderTrust {
    /// The receiving server authenticated the From: domain (DMARC, or aligned
    /// SPF/DKIM). Forging this address would have been rejected.
    Pass,
    /// No usable authentication result — an old server, a POP3 account, or mail
    /// that predates the check. Says nothing either way.
    Unverified,
    /// Authenticated, but something about the addressing is off: a reply-to on
    /// another domain, or a display name impersonating one.
    Suspicious,
    /// Authentication failed. The From: address is very likely forged.
    Fail,
}

impl SenderTrust {
    /// Round-trip tag for the cache.
    pub(crate) fn as_tag(self) -> &'static str {
        match self {
            SenderTrust::Pass => "pass",
            SenderTrust::Unverified => "unverified",
            SenderTrust::Suspicious => "suspicious",
            SenderTrust::Fail => "fail",
        }
    }

    pub(crate) fn from_tag(tag: &str) -> SenderTrust {
        match tag {
            "pass" => SenderTrust::Pass,
            "suspicious" => SenderTrust::Suspicious,
            "fail" => SenderTrust::Fail,
            _ => SenderTrust::Unverified,
        }
    }

    /// Heading for the details popover, and the badge's tooltip. Not drawn on
    /// screen: the toolbar badge is the icon alone, colored by verdict.
    pub(crate) fn label(self) -> String {
        match self {
            SenderTrust::Pass => i18n("Verified sender"),
            SenderTrust::Unverified => i18n("Sender not verified"),
            SenderTrust::Suspicious => i18n("Check this sender"),
            SenderTrust::Fail => i18n("Possible forgery"),
        }
    }

    /// CSS class for the badge's color.
    pub(crate) fn css_class(self) -> &'static str {
        match self {
            SenderTrust::Pass => "trust-pass",
            SenderTrust::Unverified => "trust-unverified",
            SenderTrust::Suspicious => "trust-suspicious",
            SenderTrust::Fail => "trust-fail",
        }
    }
}

/// How far the user's keyring trusts an OpenPGP signing key (#133).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum PgpTrust {
    /// Fully or ultimately trusted: a key the user vouched for.
    Full,
    Marginal,
    /// In the keyring, but never assessed.
    Unknown,
    /// Explicitly distrusted.
    Never,
}

/// The verdict on an OpenPGP signature (#133).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum PgpSignature {
    /// Not signed (or nothing gpg could say about it).
    #[default]
    None,
    Good {
        signer: String,
        key_id: String,
        trust: PgpTrust,
    },
    /// The signature does not match the content.
    Bad {
        signer: String,
    },
    /// Signed with a key the keyring does not hold.
    NoKey {
        key_id: String,
    },
    ExpiredKey {
        signer: String,
    },
    RevokedKey {
        signer: String,
    },
    ExpiredSignature {
        signer: String,
    },
}

/// What OpenPGP made of a message (#133): encrypted or not, decrypted or
/// not, and the signature's standing. Carried on the sender check so it
/// reaches the reader and the cache by the same road.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PgpStatus {
    pub(crate) encrypted: bool,
    pub(crate) decrypted: bool,
    pub(crate) signature: PgpSignature,
    /// Supporting lines for the details popover.
    pub(crate) notes: Vec<String>,
    /// The From address, for fetching the sender's key.
    #[serde(default)]
    pub(crate) sender_addr: String,
    /// The sender's key from the message's Autocrypt header, base64, when
    /// it carried one: the first place a missing key is looked for.
    #[serde(default)]
    pub(crate) autocrypt: Option<String>,
    /// The real subject an encrypted message carries inside (protected
    /// headers, #493), where the outer one is a placeholder such as
    /// Thunderbird's "...". Plaintext, so never written to the cache.
    #[serde(skip)]
    pub(crate) subject: Option<String>,
}

impl PgpStatus {
    /// Whether a signature was present at all.
    pub(crate) fn signed(&self) -> bool {
        !matches!(self.signature, PgpSignature::None)
    }

    /// The key id the signature named, whether or not the keyring has it.
    pub(crate) fn signing_key_id(&self) -> Option<&str> {
        match &self.signature {
            PgpSignature::Good { key_id, .. } | PgpSignature::NoKey { key_id } => Some(key_id),
            _ => None,
        }
    }

    /// Whether the sender's key is missing from the keyring.
    pub(crate) fn key_missing(&self) -> bool {
        matches!(self.signature, PgpSignature::NoKey { .. })
    }

    /// Whether the signature is good but the key is not vouched for yet.
    pub(crate) fn key_untrusted(&self) -> bool {
        matches!(self.signature, PgpSignature::Good { trust, .. } if trust != PgpTrust::Full)
    }

    /// One line for the chip's tooltip and the popover heading.
    pub(crate) fn summary(&self) -> String {
        use PgpSignature as S;
        let key = |k: &str| crate::mail::pgp::key_display(k);
        if self.encrypted && !self.decrypted {
            return i18n("Encrypted with OpenPGP; could not be decrypted");
        }
        if self.encrypted {
            return match &self.signature {
                S::None => i18n("Encrypted with OpenPGP"),
                S::Good { signer, .. } => {
                    i18n_f("Encrypted, and signed by {signer}", &[("signer", signer)])
                }
                S::Bad { .. } => i18n(
                    "Encrypted; the signature does not match, the message may have been altered",
                ),
                S::NoKey { key_id } => i18n_f(
                    "Encrypted; signed with a key you don't have ({id})",
                    &[("id", &key(key_id))],
                ),
                S::ExpiredKey { signer } => i18n_f(
                    "Encrypted, and signed by {signer} with an expired key",
                    &[("signer", signer)],
                ),
                S::RevokedKey { signer } => i18n_f(
                    "Encrypted, and signed by {signer} with a revoked key",
                    &[("signer", signer)],
                ),
                S::ExpiredSignature { signer } => i18n_f(
                    "Encrypted, and signed by {signer}; the signature has expired",
                    &[("signer", signer)],
                ),
            };
        }
        match &self.signature {
            S::None => i18n("OpenPGP-signed; the signature could not be checked"),
            S::Good { signer, .. } => i18n_f("Signed by {signer}", &[("signer", signer)]),
            S::Bad { .. } => {
                i18n("The OpenPGP signature does not match; the message may have been altered")
            }
            S::NoKey { key_id } => i18n_f(
                "Signed with a key you don't have ({id})",
                &[("id", &key(key_id))],
            ),
            S::ExpiredKey { signer } => i18n_f(
                "Signed by {signer} with an expired key",
                &[("signer", signer)],
            ),
            S::RevokedKey { signer } => i18n_f(
                "Signed by {signer} with a revoked key",
                &[("signer", signer)],
            ),
            S::ExpiredSignature { signer } => i18n_f(
                "Signed by {signer}; the signature has expired",
                &[("signer", signer)],
            ),
        }
    }

    /// The chip's own words (#300): what the message is, or what is wrong
    /// with it, short enough to sit beside the sender's name. The tooltip
    /// and the details popover carry the full sentence.
    pub(crate) fn short_label(&self) -> String {
        use PgpSignature as S;
        if self.encrypted && !self.decrypted {
            return i18n("Not decrypted");
        }
        match &self.signature {
            S::None if self.encrypted => i18n("Encrypted"),
            S::None => i18n("Signature not checked"),
            S::Good {
                trust: PgpTrust::Full,
                ..
            } if self.encrypted => i18n("Encrypted and signed"),
            S::Good {
                trust: PgpTrust::Full,
                ..
            } => i18n("Signed"),
            S::Good { .. } => i18n("Signed, key not trusted"),
            S::Bad { .. } => i18n("Bad signature"),
            S::NoKey { .. } => i18n("Signed, unknown key"),
            S::ExpiredKey { .. } => i18n("Signed, key expired"),
            S::RevokedKey { .. } => i18n("Signed, key revoked"),
            S::ExpiredSignature { .. } => i18n("Signature expired"),
        }
    }

    /// CSS class for the chip's color: green when everything checks out,
    /// amber for a doubt, red for a failure.
    pub(crate) fn css_class(&self) -> &'static str {
        use PgpSignature as S;
        if self.encrypted && !self.decrypted {
            return "pgp-bad";
        }
        match &self.signature {
            S::Bad { .. } | S::RevokedKey { .. } => "pgp-bad",
            S::Good {
                trust: PgpTrust::Full,
                ..
            } => "pgp-good",
            S::Good { .. }
            | S::NoKey { .. }
            | S::ExpiredKey { .. }
            | S::ExpiredSignature { .. } => "pgp-warn",
            S::None if self.encrypted => "pgp-good",
            S::None => "pgp-warn",
        }
    }
}

/// How a mailing list lets its readers leave (RFC 2369, RFC 8058): read out
/// of the message's headers with the sender check, kept in the cache beside
/// it, and drawn as the card's Unsubscribe banner. See [`crate::mail::unsubscribe`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Unsubscribe {
    /// The https handle that takes a one-click POST (RFC 8058), when the
    /// message promised one. The route the button prefers: no browser, no
    /// page, no mail.
    #[serde(default)]
    pub(crate) one_click: Option<String>,
    /// A `mailto:` handle: a short message to the list from the account the
    /// message arrived in.
    #[serde(default)]
    pub(crate) mailto: Option<String>,
    /// A "reply with UNSUBSCRIBE" instruction read out of the body: where
    /// to answer and the word to put in the subject.
    #[serde(default)]
    pub(crate) reply: Option<ReplyRoute>,
    /// A web page to visit — the browser, only when nothing better is on
    /// offer or the better routes failed.
    #[serde(default)]
    pub(crate) web: Option<String>,
    /// The list's own identifier (`List-Id`, lowercased, without the
    /// brackets); empty when the message named none.
    #[serde(default)]
    pub(crate) list_id: String,
    /// Whether the message's own headers said any of this. False means it
    /// was read out of the body, which is a guess — a good one, but the
    /// banner says "looks like" rather than stating it.
    #[serde(default)]
    pub(crate) in_headers: bool,
}

/// "Reply to this email with UNSUBSCRIBE in the subject": the oldest
/// mechanism of all, and still in use by hand-run lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ReplyRoute {
    /// The address to answer (Reply-To, else From).
    pub(crate) to: String,
    /// The word the list asked for, as the subject (UNSUBSCRIBE, STOP, …).
    pub(crate) subject: String,
}

impl Unsubscribe {
    /// Whether the button can unsubscribe without a browser.
    pub(crate) fn direct(&self) -> bool {
        self.one_click.is_some() || self.mailto.is_some() || self.reply.is_some()
    }

    /// What a list is remembered by once left: its List-Id, or failing
    /// that the address its mail comes from.
    pub(crate) fn key(&self, from_addr: &str) -> String {
        if self.list_id.is_empty() {
            format!("from:{}", from_addr.trim().to_ascii_lowercase())
        } else {
            format!("list:{}", self.list_id)
        }
    }
}

/// A meeting invitation read out of a message's `text/calendar` part
/// (#223), kept beside the sender check and drawn as the card's invitation
/// banner. See [`crate::mail::invite`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Invite {
    /// The iTIP method, upper-cased: REQUEST (an invitation), REPLY
    /// (someone answering one), CANCEL, PUBLISH, COUNTER.
    #[serde(default)]
    pub(crate) method: String,
    /// The event's identity, the same in every message about it. What a
    /// remembered answer is filed under.
    #[serde(default)]
    pub(crate) uid: String,
    /// Which revision of the event this is: an organizer who moves a
    /// meeting sends it again with a higher number.
    #[serde(default)]
    pub(crate) sequence: i64,
    #[serde(default)]
    pub(crate) summary: String,
    #[serde(default)]
    pub(crate) location: String,
    /// Start and end in unix seconds; 0 when the event named no readable
    /// time (the banner then leaves the line out rather than inventing one).
    #[serde(default)]
    pub(crate) start: i64,
    #[serde(default)]
    pub(crate) end: i64,
    /// A whole-day event, which has a date but no time of day.
    #[serde(default)]
    pub(crate) all_day: bool,
    /// `RECURRENCE-ID` as written, when the message is about one occurrence
    /// of a series rather than the series itself. Carried into the reply.
    #[serde(default)]
    pub(crate) recurrence_id: String,
    /// The `RRULE` as written, when the event repeats; empty for a one-off.
    #[serde(default)]
    pub(crate) repeats: String,
    #[serde(default)]
    pub(crate) organizer: Option<InvitePerson>,
    #[serde(default)]
    pub(crate) attendees: Vec<InvitePerson>,
    /// The event has been called off (`STATUS:CANCELLED`).
    #[serde(default)]
    pub(crate) cancelled: bool,
    /// The calendar part as it arrived, so the event can be handed to a
    /// calendar application without going back to the server.
    #[serde(default)]
    pub(crate) ics: String,
}

/// Someone named on an invitation: the organizer, or one of the invited.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct InvitePerson {
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) email: String,
    /// `PARTSTAT`: NEEDS-ACTION, ACCEPTED, TENTATIVE, DECLINED.
    #[serde(default)]
    pub(crate) status: String,
    /// The organizer asked this person for an answer.
    #[serde(default)]
    pub(crate) rsvp: bool,
    /// A room or a piece of equipment rather than a person.
    #[serde(default)]
    pub(crate) resource: bool,
}

impl InvitePerson {
    /// What to call them: their name if the invitation gave one, else the
    /// address.
    pub(crate) fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.email
        } else {
            &self.name
        }
    }
}

/// An answer to an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Rsvp {
    Accepted,
    Tentative,
    Declined,
}

impl Rsvp {
    /// The `PARTSTAT` value that says this.
    pub(crate) fn partstat(self) -> &'static str {
        match self {
            Rsvp::Accepted => "ACCEPTED",
            Rsvp::Tentative => "TENTATIVE",
            Rsvp::Declined => "DECLINED",
        }
    }

    /// The word a `PARTSTAT` stands for, or `None` for one that is not an
    /// answer (NEEDS-ACTION, DELEGATED).
    pub(crate) fn from_partstat(value: &str) -> Option<Rsvp> {
        match value.trim().to_ascii_uppercase().as_str() {
            "ACCEPTED" => Some(Rsvp::Accepted),
            "TENTATIVE" => Some(Rsvp::Tentative),
            "DECLINED" => Some(Rsvp::Declined),
            _ => None,
        }
    }
}

impl Invite {
    /// What an answer to this event is remembered by: the event's UID, plus
    /// the occurrence when the message is about one of a series.
    pub(crate) fn key(&self) -> String {
        if self.recurrence_id.is_empty() {
            self.uid.clone()
        } else {
            format!("{}#{}", self.uid, self.recurrence_id)
        }
    }

    /// Whether this message is asking the reader to answer: an invitation
    /// (or a re-invitation) that has not been called off, from an organizer
    /// there is somewhere to answer to.
    pub(crate) fn asks_for_an_answer(&self) -> bool {
        !self.cancelled
            && matches!(self.method.as_str(), "REQUEST" | "COUNTER")
            && !self.uid.is_empty()
            && self
                .organizer
                .as_ref()
                .is_some_and(|o| o.email.contains('@'))
    }

    /// The attendee entry for whoever is reading, matched against the
    /// addresses this account answers to.
    pub(crate) fn me<'a>(&'a self, addresses: &[String]) -> Option<&'a InvitePerson> {
        self.attendees.iter().find(|a| {
            addresses
                .iter()
                .any(|mine| mine.eq_ignore_ascii_case(&a.email))
        })
    }

    /// The people invited: not the rooms and the equipment, and not the
    /// organizer, whom several calendars put on the attendee list as well
    /// and who is named in their own right.
    pub(crate) fn guests(&self) -> impl Iterator<Item = &InvitePerson> {
        let chair = self
            .organizer
            .as_ref()
            .map(|o| o.email.to_ascii_lowercase());
        self.attendees.iter().filter(move |a| {
            !a.resource && chair.as_deref() != Some(a.email.to_ascii_lowercase().as_str())
        })
    }
}

/// The result of checking whether a message's From: address was forged.
#[derive(Debug, Clone)]
pub(crate) struct SenderCheck {
    pub(crate) trust: SenderTrust,
    /// One-line plain-English verdict, shown in the banner and popover heading.
    pub(crate) summary: String,
    /// Supporting detail, one line each, for the "Details" popover.
    pub(crate) findings: Vec<String>,
    /// The OpenPGP verdict (#133), when the message carried any.
    pub(crate) pgp: Option<PgpStatus>,
    /// How to leave the list this came from, when it is from one. Read with
    /// the verdict: both come out of the raw headers at the one fetch.
    pub(crate) unsubscribe: Option<Unsubscribe>,
    /// The meeting this message invites the reader to (#223), when it
    /// carries a `text/calendar` part. Read in the same pass, for the same
    /// reason: the raw message is in hand exactly once.
    pub(crate) invite: Option<Box<Invite>>,
    /// The sensitivity label the sender's organization set (#411), read in
    /// the same pass.
    pub(crate) label: Option<crate::mail::sensitivity::SensitivityLabel>,
    /// The read receipt the sender asked for (#520), read in the same pass.
    pub(crate) receipt: Option<crate::mail::receipt::ReceiptRequest>,
    /// The read receipt this message is (#520), if it is one: which
    /// message it says was read, and by whom. Not cached: it is acted on
    /// as it arrives.
    pub(crate) read: Option<crate::mail::receipt::ReadReport>,
}

impl Default for SenderCheck {
    fn default() -> Self {
        SenderCheck {
            trust: SenderTrust::Unverified,
            summary: i18n("This message hasn't been checked."),
            findings: Vec::new(),
            pgp: None,
            unsubscribe: None,
            invite: None,
            label: None,
            receipt: None,
            read: None,
        }
    }
}
