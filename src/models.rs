//! Core domain types shared across the UI and backend layers.
use crate::i18n::{i18n, i18n_f};

/// A configured mail account (one IMAP/SMTP identity).
#[derive(Debug, Clone)]
pub struct Account {
    pub id: u32,
    pub name: String,
    pub email: String,
    /// How the account is labelled in the UI (All Inboxes, reader chip). Defaults
    /// to the email address.
    pub label: String,
    /// Accent color used for the account dot, as a CSS color string.
    pub accent: String,
}

/// The well-known role of a folder, used to pick an icon and ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FolderKind {
    Inbox,
    Starred,
    Sent,
    Drafts,
    /// Messages kept to start new ones from (#360): no server marks this
    /// role, so it is found by name or assigned under Special Folders.
    Templates,
    Archive,
    Junk,
    Trash,
    Custom,
}

impl FolderKind {
    pub fn icon(self) -> &'static str {
        match self {
            FolderKind::Inbox => "mail-inbox-symbolic",
            FolderKind::Starred => "starred-symbolic",
            FolderKind::Sent => "mail-send-symbolic",
            FolderKind::Drafts => "document-edit-symbolic",
            FolderKind::Templates => "folder-templates-symbolic",
            FolderKind::Archive => "mail-archive-symbolic",
            FolderKind::Junk => "mail-mark-junk-symbolic",
            FolderKind::Trash => "user-trash-symbolic",
            FolderKind::Custom => "folder-symbolic",
        }
    }
}

/// A keyword found in use on a mail server: what the tag finder (Settings →
/// Tags → Find Tags…) reports per account, before the user decides which
/// become tags.
#[derive(Debug, Clone)]
pub struct KeywordFinding {
    /// The keyword as the server spells it (a Microsoft 365 category's name).
    pub keyword: String,
    /// A display name the server itself carries (Microsoft 365 categories).
    pub name: Option<String>,
    /// A color the server itself carries, `#rrggbb`.
    pub color: Option<String>,
    /// Messages carrying it, where the server could say (0 = not counted).
    pub count: usize,
    /// The folders it was seen in (display names).
    pub folders: Vec<String>,
}

/// How much of an account's mail storage the server says is in use (#298),
/// in bytes. Only servers that report a limit give one: IMAP's QUOTA
/// extension and JMAP's quotas; Microsoft 365 and POP3 do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxQuota {
    pub used: u64,
    pub limit: u64,
}

impl MailboxQuota {
    /// The share in use, 0 to 1.
    pub fn fraction(&self) -> f64 {
        if self.limit == 0 {
            return 0.0;
        }
        (self.used as f64 / self.limit as f64).clamp(0.0, 1.0)
    }

    /// "2.1 GB of 15 GB used".
    pub fn used_line(&self) -> String {
        i18n_f(
            "{used} of {limit} used",
            &[("used", &human_size(self.used)), ("limit", &human_size(self.limit))],
        )
    }

    /// "12.9 GB free (86%)".
    pub fn free_line(&self) -> String {
        let free = self.limit.saturating_sub(self.used);
        let pct = ((1.0 - self.fraction()) * 100.0).round() as u32;
        i18n_f("{free} free ({pct}%)", &[("free", &human_size(free)), ("pct", &pct.to_string())])
    }
}

/// A mail folder within an account.
#[derive(Debug, Clone)]
pub struct Folder {
    pub id: u32,
    pub account_id: u32,
    pub name: String,
    /// IMAP mailbox path (e.g. "INBOX", "INBOX.Sent"). For the mock backend
    /// this mirrors `name`.
    pub path: String,
    pub kind: FolderKind,
    /// The sidebar chip's number: unread mail, except Drafts, where it is
    /// how many drafts there are (see `worker::chip_counts_all`).
    pub unread: u32,
}

/// Show the special folders under the app's own names for them in a language
/// other than English (#251), where the server calls them by one of the
/// usual English names. The folder a server or the user named otherwise
/// keeps that name, and so does every folder in English: "Deleted Items"
/// stays "Deleted Items" rather than becoming "Trash". Only the label
/// changes; the path the server knows the folder by is left alone.
pub fn localize_special_folder_names(folders: &mut [Folder]) {
    for f in folders {
        let (label, english): (&str, &[&str]) = match f.kind {
            FolderKind::Inbox => ("Inbox", &["inbox"]),
            FolderKind::Sent => ("Sent", &["sent", "sent items", "sent mail", "sent messages"]),
            FolderKind::Drafts => ("Drafts", &["drafts", "draft"]),
            FolderKind::Templates => ("Templates", &["templates", "template"]),
            FolderKind::Trash => ("Trash", &["trash", "deleted items", "deleted messages", "bin"]),
            FolderKind::Junk => (
                "Junk",
                &["junk", "junk mail", "junk e-mail", "junk email", "spam", "bulk mail"],
            ),
            FolderKind::Archive => ("Archive", &["archive", "archives"]),
            FolderKind::Starred => ("Starred", &["starred"]),
            FolderKind::Custom => continue,
        };
        if !english.iter().any(|e| f.name.eq_ignore_ascii_case(e)) {
            continue;
        }
        let translated = i18n(label);
        if translated != label {
            f.name = translated;
        }
    }
}

/// Whether `path` is one of an account's hidden folders (#239), or lies
/// under one: hiding a folder hides its sub-folders with it. `delimiter`
/// is the server's, when known; otherwise any of the usual three counts.
pub fn folder_is_hidden(path: &str, delimiter: Option<&str>, hidden: &[String]) -> bool {
    hidden.iter().any(|h| {
        if h == path {
            return true;
        }
        let Some(rest) = path.strip_prefix(h.as_str()) else { return false };
        match delimiter {
            Some(d) if !d.is_empty() => rest.starts_with(d),
            _ => rest.starts_with(['/', '.', '\\']),
        }
    })
}

/// The folders an Exchange server lists over IMAP that hold no mail
/// (#239): the calendar, the address book, the task list and the rest of
/// what its own webmail keeps out of sight. Only claimed when the listing
/// looks like Exchange, which is when at least two of Calendar, Contacts,
/// Tasks and Journal sit at the top level; a lone "Notes" folder on any
/// other server is somebody's mail. Top-level paths only: a sub-folder
/// follows its parent through [`folder_is_hidden`].
pub fn exchange_non_mail_folders(folders: &[Folder]) -> Vec<String> {
    const MARKERS: [&str; 4] = ["calendar", "contacts", "tasks", "journal"];
    const HIDDEN: [&str; 10] = [
        "calendar",
        "contacts",
        "tasks",
        "journal",
        "notes",
        "outbox",
        "sync issues",
        "conversation history",
        "scheduled",
        "snoozed",
    ];
    let top: Vec<(&Folder, String)> = folders
        .iter()
        .filter(|f| !f.path.contains(['/', '\\']))
        .map(|f| (f, f.name.trim().to_lowercase()))
        .collect();
    let markers = top.iter().filter(|(_, n)| MARKERS.contains(&n.as_str())).count();
    if markers < 2 {
        return Vec::new();
    }
    top.into_iter()
        .filter(|(f, n)| f.kind == FolderKind::Custom && HIDDEN.contains(&n.as_str()))
        .map(|(f, _)| f.path.clone())
        .collect()
}

/// Whether a folder's own name makes it the Templates folder (#360). No
/// server marks one, so the name is all there is to go by; any other is
/// assigned under Special Folders.
pub fn is_templates_name(name: &str) -> bool {
    matches!(name.trim().to_lowercase().as_str(), "templates" | "template")
}

/// The [`FolderKind`] behind a Special Folders role key (#82).
pub fn role_kind(role: &str) -> Option<FolderKind> {
    match role {
        "sent" => Some(FolderKind::Sent),
        "drafts" => Some(FolderKind::Drafts),
        "templates" => Some(FolderKind::Templates),
        "trash" => Some(FolderKind::Trash),
        "junk" => Some(FolderKind::Junk),
        "archive" => Some(FolderKind::Archive),
        _ => None,
    }
}

/// Re-kind an account's folders by its manual special-folder assignments
/// (#82): the chosen folder takes the role and whatever held it demotes to
/// Custom. Order and ids are untouched (the worker's cache keys ids by
/// position; the app re-sorts its own copy). The worker applies this to
/// every listing so its counts see the assignment too — a folder assigned
/// as Drafts counts every draft, like a detected one, rather than the
/// unseen mail (none) its detected kind would count.
pub fn assign_folder_roles(
    roles: &std::collections::BTreeMap<String, String>,
    folders: &mut [Folder],
) {
    for (role, path) in roles {
        let Some(kind) = role_kind(role) else { continue };
        if !folders.iter().any(|f| &f.path == path) {
            // The assigned folder vanished server-side: leave detection alone.
            continue;
        }
        // The Inbox is never re-roled (#136): an account whose "Sent" was
        // pointed at INBOX lost its inbox — and with it its place under All
        // Inboxes, its new-mail notifications and its filters. The
        // assignment is ignored; the editor no longer offers the Inbox.
        if folders.iter().any(|f| &f.path == path && f.kind == FolderKind::Inbox) {
            continue;
        }
        for f in folders.iter_mut() {
            if f.kind == kind {
                f.kind = FolderKind::Custom;
            }
        }
        if let Some(f) = folders.iter_mut().find(|f| &f.path == path) {
            f.kind = kind;
        }
    }
}

/// A single message (summary + body). In a real backend the body is loaded
/// lazily; here it is always present.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub id: u32,
    /// Owning account; needed to route actions and to merge the unified inbox.
    pub account_id: u32,
    pub folder_id: u32,
    /// IMAP UID, used to fetch the full body lazily. Mock data reuses `id`.
    pub uid: u32,
    pub from_name: String,
    pub from_addr: String,
    /// Where the sender asked replies to go (comma-separated emails), from the
    /// Reply-To header. Empty when the header is absent — reply to `from_addr`.
    pub reply_to: String,
    /// Original recipients (comma-separated emails), used for Reply All.
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub preview: String,
    pub body: String,
    /// Human-readable timestamp for display.
    pub date: String,
    /// Unix timestamp (seconds) for sorting, e.g. the unified inbox. 0 if unknown.
    pub timestamp: i64,
    pub unread: bool,
    pub starred: bool,
    /// The message's IMAP keywords (user flags) as the server reports them —
    /// `$label1`, `Work`, `$Junk`… — verbatim and in server order. Tags (#71)
    /// are the ones a [`crate::config::Tag`] maps to; the rest are ignored.
    /// Microsoft 365 categories and POP3's local-only tags land here too.
    pub keywords: Vec<String>,
    pub has_attachment: bool,
    /// This message's own Message-ID (normalized, no angle brackets). Empty if
    /// unknown. Used to thread replies accurately (instead of by subject alone).
    pub message_id: String,
    /// Referenced Message-IDs (In-Reply-To + References), space-separated and
    /// normalized. Links a reply to the messages it descends from.
    pub references: String,
    /// How important the sender marked it (#334).
    pub importance: Importance,
    /// When a Microsoft 365 follow-up flag falls due (Unix seconds), or 0.
    /// Only Graph has such a date; IMAP and JMAP have no standard for one.
    pub due: i64,
    /// The Gmail labels the message wears besides the folder it is listed
    /// in (#374), as the server names them (modified UTF-7, `Parent/Child`
    /// for a nested one). System labels (`\Inbox`, `\Important`…) are left
    /// out. Empty on every other server.
    pub labels: Vec<String>,
}

/// How important a message's sender marked it: the `X-Priority`, `Importance`
/// or `Priority` header, or Microsoft 365's `importance` (#334).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Importance {
    #[default]
    Normal,
    High,
    Low,
}

impl Importance {
    /// The value a header gives: `X-Priority: 1 (Highest)`, `Importance:
    /// high`, `Priority: urgent` and so on. Anything else is normal.
    pub fn from_header(name: &str, value: &str) -> Importance {
        let value = value.trim().to_ascii_lowercase();
        if name.eq_ignore_ascii_case("x-priority") {
            return match value.chars().next() {
                Some('1' | '2') => Importance::High,
                Some('4' | '5') => Importance::Low,
                _ => Importance::Normal,
            };
        }
        match value.split(|c: char| !c.is_ascii_alphanumeric() && c != '-').next().unwrap_or("") {
            "high" | "urgent" => Importance::High,
            "low" | "non-urgent" => Importance::Low,
            _ => Importance::Normal,
        }
    }

    /// The first of `X-Priority`, `Importance` and `Priority` that says
    /// anything, from a lookup by header name.
    pub fn from_headers<'a>(header: impl Fn(&'static str) -> Option<&'a str>) -> Importance {
        ["X-Priority", "Importance", "Priority"]
            .iter()
            .filter_map(|name| header(name).map(|v| Importance::from_header(name, v)))
            .find(|i| *i != Importance::Normal)
            .unwrap_or_default()
    }

    /// The number the cache stores.
    pub fn to_i64(self) -> i64 {
        match self {
            Importance::Normal => 0,
            Importance::High => 1,
            Importance::Low => -1,
        }
    }

    pub fn from_i64(n: i64) -> Importance {
        match n {
            1 => Importance::High,
            -1 => Importance::Low,
            _ => Importance::Normal,
        }
    }
}

/// Identifies an existing draft being edited, so saving/sending replaces it
/// (removing the previous version from the Drafts folder).
#[derive(Debug, Clone)]
pub struct DraftOrigin {
    pub account_id: u32,
    pub folder_id: u32,
    pub path: String,
    pub uid: u32,
}

/// How much a message's claimed sender can be trusted, worst finding wins.
///
/// This answers "did this really come from the domain in the From: line?" — not
/// "is this message safe". A phisher who registers their own domain and
/// authenticates it properly earns [`SenderTrust::Pass`]; the check proves the
/// From: address wasn't forged, nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenderTrust {
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
    pub fn as_tag(self) -> &'static str {
        match self {
            SenderTrust::Pass => "pass",
            SenderTrust::Unverified => "unverified",
            SenderTrust::Suspicious => "suspicious",
            SenderTrust::Fail => "fail",
        }
    }

    pub fn from_tag(tag: &str) -> SenderTrust {
        match tag {
            "pass" => SenderTrust::Pass,
            "suspicious" => SenderTrust::Suspicious,
            "fail" => SenderTrust::Fail,
            _ => SenderTrust::Unverified,
        }
    }

    /// Heading for the details popover, and the badge's tooltip. Not drawn on
    /// screen: the toolbar badge is the icon alone, colored by verdict.
    pub fn label(self) -> String {
        match self {
            SenderTrust::Pass => i18n("Verified sender"),
            SenderTrust::Unverified => i18n("Sender not verified"),
            SenderTrust::Suspicious => i18n("Check this sender"),
            SenderTrust::Fail => i18n("Possible forgery"),
        }
    }

    /// CSS class for the badge's color.
    pub fn css_class(self) -> &'static str {
        match self {
            SenderTrust::Pass => "trust-pass",
            SenderTrust::Unverified => "trust-unverified",
            SenderTrust::Suspicious => "trust-suspicious",
            SenderTrust::Fail => "trust-fail",
        }
    }
}

/// The list preview stored for an OpenPGP-encrypted message (#133): a
/// marker rather than words, so the row can draw a lock icon and say
/// "Encrypted message" in whatever language is current, and nothing of the
/// message itself is written down. A lock glyph, so it still reads if it
/// ever shows raw.
pub const ENCRYPTED_PREVIEW: &str = "\u{1F512}";

/// Whether a stored preview is the encrypted-message marker.
pub fn preview_is_encrypted(preview: &str) -> bool {
    preview.trim() == ENCRYPTED_PREVIEW
}

/// Whether a subject wears one of the prefixes mail programs put on an
/// answer or a forward, in any of the languages they write them in.
pub fn is_reply_or_forward_subject(subject: &str) -> bool {
    let subject = subject.trim_start().to_ascii_lowercase();
    ["re:", "fw:", "fwd:", "aw:", "sv:", "vs:", "tr:", "rv:", "wg:", "antw:", "r:", "i:"]
        .iter()
        .any(|p| subject.starts_with(p))
}

/// The words the list shows for a stored preview.
pub fn preview_display(preview: &str) -> String {
    if preview_is_encrypted(preview) {
        i18n("Encrypted message")
    } else {
        preview.to_string()
    }
}

/// How far the user's keyring trusts an OpenPGP signing key (#133).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PgpTrust {
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
pub enum PgpSignature {
    /// Not signed (or nothing gpg could say about it).
    #[default]
    None,
    Good { signer: String, key_id: String, trust: PgpTrust },
    /// The signature does not match the content.
    Bad { signer: String },
    /// Signed with a key the keyring does not hold.
    NoKey { key_id: String },
    ExpiredKey { signer: String },
    RevokedKey { signer: String },
    ExpiredSignature { signer: String },
}

/// What OpenPGP made of a message (#133): encrypted or not, decrypted or
/// not, and the signature's standing. Carried on the sender check so it
/// reaches the reader and the cache by the same road.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PgpStatus {
    pub encrypted: bool,
    pub decrypted: bool,
    pub signature: PgpSignature,
    /// Supporting lines for the details popover.
    pub notes: Vec<String>,
    /// The From address, for fetching the sender's key.
    #[serde(default)]
    pub sender_addr: String,
    /// The sender's key from the message's Autocrypt header, base64, when
    /// it carried one: the first place a missing key is looked for.
    #[serde(default)]
    pub autocrypt: Option<String>,
}

impl PgpStatus {
    /// Whether a signature was present at all.
    pub fn signed(&self) -> bool {
        !matches!(self.signature, PgpSignature::None)
    }

    /// The key id the signature named, whether or not the keyring has it.
    pub fn signing_key_id(&self) -> Option<&str> {
        match &self.signature {
            PgpSignature::Good { key_id, .. } | PgpSignature::NoKey { key_id } => Some(key_id),
            _ => None,
        }
    }

    /// Whether the sender's key is missing from the keyring.
    pub fn key_missing(&self) -> bool {
        matches!(self.signature, PgpSignature::NoKey { .. })
    }

    /// Whether the signature is good but the key is not vouched for yet.
    pub fn key_untrusted(&self) -> bool {
        matches!(self.signature, PgpSignature::Good { trust, .. } if trust != PgpTrust::Full)
    }

    /// One line for the chip's tooltip and the popover heading.
    pub fn summary(&self) -> String {
        use PgpSignature as S;
        let key = |k: &str| crate::pgp::key_display(k);
        if self.encrypted && !self.decrypted {
            return i18n("Encrypted with OpenPGP; could not be decrypted");
        }
        if self.encrypted {
            return match &self.signature {
                S::None => i18n("Encrypted with OpenPGP"),
                S::Good { signer, .. } => i18n_f("Encrypted, and signed by {signer}", &[("signer", signer)]),
                S::Bad { .. } => i18n("Encrypted; the signature does not match, the message may have been altered"),
                S::NoKey { key_id } => i18n_f("Encrypted; signed with a key you don't have ({id})", &[("id", &key(key_id))]),
                S::ExpiredKey { signer } => i18n_f("Encrypted, and signed by {signer} with an expired key", &[("signer", signer)]),
                S::RevokedKey { signer } => i18n_f("Encrypted, and signed by {signer} with a revoked key", &[("signer", signer)]),
                S::ExpiredSignature { signer } => i18n_f("Encrypted, and signed by {signer}; the signature has expired", &[("signer", signer)]),
            };
        }
        match &self.signature {
            S::None => i18n("OpenPGP-signed; the signature could not be checked"),
            S::Good { signer, .. } => i18n_f("Signed by {signer}", &[("signer", signer)]),
            S::Bad { .. } => i18n("The OpenPGP signature does not match; the message may have been altered"),
            S::NoKey { key_id } => i18n_f("Signed with a key you don't have ({id})", &[("id", &key(key_id))]),
            S::ExpiredKey { signer } => i18n_f("Signed by {signer} with an expired key", &[("signer", signer)]),
            S::RevokedKey { signer } => i18n_f("Signed by {signer} with a revoked key", &[("signer", signer)]),
            S::ExpiredSignature { signer } => i18n_f("Signed by {signer}; the signature has expired", &[("signer", signer)]),
        }
    }

    /// The chip's own words (#300): what the message is, or what is wrong
    /// with it, short enough to sit beside the sender's name. The tooltip
    /// and the details popover carry the full sentence.
    pub fn short_label(&self) -> String {
        use PgpSignature as S;
        if self.encrypted && !self.decrypted {
            return i18n("Not decrypted");
        }
        match &self.signature {
            S::None if self.encrypted => i18n("Encrypted"),
            S::None => i18n("Signature not checked"),
            S::Good { trust: PgpTrust::Full, .. } if self.encrypted => i18n("Encrypted and signed"),
            S::Good { trust: PgpTrust::Full, .. } => i18n("Signed"),
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
    pub fn css_class(&self) -> &'static str {
        use PgpSignature as S;
        if self.encrypted && !self.decrypted {
            return "pgp-bad";
        }
        match &self.signature {
            S::Bad { .. } | S::RevokedKey { .. } => "pgp-bad",
            S::Good { trust: PgpTrust::Full, .. } => "pgp-good",
            S::Good { .. } | S::NoKey { .. } | S::ExpiredKey { .. } | S::ExpiredSignature { .. } => "pgp-warn",
            S::None if self.encrypted => "pgp-good",
            S::None => "pgp-warn",
        }
    }
}

/// How a mailing list lets its readers leave (RFC 2369, RFC 8058): read out
/// of the message's headers with the sender check, kept in the cache beside
/// it, and drawn as the card's Unsubscribe banner. See [`crate::unsubscribe`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Unsubscribe {
    /// The https handle that takes a one-click POST (RFC 8058), when the
    /// message promised one. The route the button prefers: no browser, no
    /// page, no mail.
    #[serde(default)]
    pub one_click: Option<String>,
    /// A `mailto:` handle: a short message to the list from the account the
    /// message arrived in.
    #[serde(default)]
    pub mailto: Option<String>,
    /// A "reply with UNSUBSCRIBE" instruction read out of the body: where
    /// to answer and the word to put in the subject.
    #[serde(default)]
    pub reply: Option<ReplyRoute>,
    /// A web page to visit — the browser, only when nothing better is on
    /// offer or the better routes failed.
    #[serde(default)]
    pub web: Option<String>,
    /// The list's own identifier (`List-Id`, lowercased, without the
    /// brackets); empty when the message named none.
    #[serde(default)]
    pub list_id: String,
    /// Whether the message's own headers said any of this. False means it
    /// was read out of the body, which is a guess — a good one, but the
    /// banner says "looks like" rather than stating it.
    #[serde(default)]
    pub in_headers: bool,
}

/// "Reply to this email with UNSUBSCRIBE in the subject": the oldest
/// mechanism of all, and still in use by hand-run lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReplyRoute {
    /// The address to answer (Reply-To, else From).
    pub to: String,
    /// The word the list asked for, as the subject (UNSUBSCRIBE, STOP, …).
    pub subject: String,
}

impl Unsubscribe {
    /// Whether the button can unsubscribe without a browser.
    pub fn direct(&self) -> bool {
        self.one_click.is_some() || self.mailto.is_some() || self.reply.is_some()
    }

    /// What a list is remembered by once left: its List-Id, or failing
    /// that the address its mail comes from.
    pub fn key(&self, from_addr: &str) -> String {
        if self.list_id.is_empty() {
            format!("from:{}", from_addr.trim().to_ascii_lowercase())
        } else {
            format!("list:{}", self.list_id)
        }
    }
}

/// A meeting invitation read out of a message's `text/calendar` part
/// (#223), kept beside the sender check and drawn as the card's invitation
/// banner. See [`crate::invite`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Invite {
    /// The iTIP method, upper-cased: REQUEST (an invitation), REPLY
    /// (someone answering one), CANCEL, PUBLISH, COUNTER.
    #[serde(default)]
    pub method: String,
    /// The event's identity, the same in every message about it. What a
    /// remembered answer is filed under.
    #[serde(default)]
    pub uid: String,
    /// Which revision of the event this is: an organizer who moves a
    /// meeting sends it again with a higher number.
    #[serde(default)]
    pub sequence: i64,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub location: String,
    /// Start and end in unix seconds; 0 when the event named no readable
    /// time (the banner then leaves the line out rather than inventing one).
    #[serde(default)]
    pub start: i64,
    #[serde(default)]
    pub end: i64,
    /// A whole-day event, which has a date but no time of day.
    #[serde(default)]
    pub all_day: bool,
    /// `RECURRENCE-ID` as written, when the message is about one occurrence
    /// of a series rather than the series itself. Carried into the reply.
    #[serde(default)]
    pub recurrence_id: String,
    /// The `RRULE` as written, when the event repeats; empty for a one-off.
    #[serde(default)]
    pub repeats: String,
    #[serde(default)]
    pub organizer: Option<InvitePerson>,
    #[serde(default)]
    pub attendees: Vec<InvitePerson>,
    /// The event has been called off (`STATUS:CANCELLED`).
    #[serde(default)]
    pub cancelled: bool,
    /// The calendar part as it arrived, so the event can be handed to a
    /// calendar application without going back to the server.
    #[serde(default)]
    pub ics: String,
}

/// Someone named on an invitation: the organizer, or one of the invited.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InvitePerson {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
    /// `PARTSTAT`: NEEDS-ACTION, ACCEPTED, TENTATIVE, DECLINED.
    #[serde(default)]
    pub status: String,
    /// The organizer asked this person for an answer.
    #[serde(default)]
    pub rsvp: bool,
    /// A room or a piece of equipment rather than a person.
    #[serde(default)]
    pub resource: bool,
}

impl InvitePerson {
    /// What to call them: their name if the invitation gave one, else the
    /// address.
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.email
        } else {
            &self.name
        }
    }
}

/// An answer to an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Rsvp {
    Accepted,
    Tentative,
    Declined,
}

impl Rsvp {
    /// The `PARTSTAT` value that says this.
    pub fn partstat(self) -> &'static str {
        match self {
            Rsvp::Accepted => "ACCEPTED",
            Rsvp::Tentative => "TENTATIVE",
            Rsvp::Declined => "DECLINED",
        }
    }

    /// The word a `PARTSTAT` stands for, or `None` for one that is not an
    /// answer (NEEDS-ACTION, DELEGATED).
    pub fn from_partstat(value: &str) -> Option<Rsvp> {
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
    pub fn key(&self) -> String {
        if self.recurrence_id.is_empty() {
            self.uid.clone()
        } else {
            format!("{}#{}", self.uid, self.recurrence_id)
        }
    }

    /// Whether this message is asking the reader to answer: an invitation
    /// (or a re-invitation) that has not been called off, from an organizer
    /// there is somewhere to answer to.
    pub fn asks_for_an_answer(&self) -> bool {
        !self.cancelled
            && matches!(self.method.as_str(), "REQUEST" | "COUNTER")
            && !self.uid.is_empty()
            && self.organizer.as_ref().is_some_and(|o| o.email.contains('@'))
    }

    /// The attendee entry for whoever is reading, matched against the
    /// addresses this account answers to.
    pub fn me<'a>(&'a self, addresses: &[String]) -> Option<&'a InvitePerson> {
        self.attendees.iter().find(|a| {
            addresses.iter().any(|mine| mine.eq_ignore_ascii_case(&a.email))
        })
    }

    /// The people invited: not the rooms and the equipment, and not the
    /// organizer, whom several calendars put on the attendee list as well
    /// and who is named in their own right.
    pub fn guests(&self) -> impl Iterator<Item = &InvitePerson> {
        let chair = self.organizer.as_ref().map(|o| o.email.to_ascii_lowercase());
        self.attendees.iter().filter(move |a| {
            !a.resource && chair.as_deref() != Some(a.email.to_ascii_lowercase().as_str())
        })
    }
}

/// The result of checking whether a message's From: address was forged.
#[derive(Debug, Clone)]
pub struct SenderCheck {
    pub trust: SenderTrust,
    /// One-line plain-English verdict, shown in the banner and popover heading.
    pub summary: String,
    /// Supporting detail, one line each, for the "Details" popover.
    pub findings: Vec<String>,
    /// The OpenPGP verdict (#133), when the message carried any.
    pub pgp: Option<PgpStatus>,
    /// How to leave the list this came from, when it is from one. Read with
    /// the verdict: both come out of the raw headers at the one fetch.
    pub unsubscribe: Option<Unsubscribe>,
    /// The meeting this message invites the reader to (#223), when it
    /// carries a `text/calendar` part. Read in the same pass, for the same
    /// reason: the raw message is in hand exactly once.
    pub invite: Option<Box<Invite>>,
    /// The sensitivity label the sender's organization set (#411), read in
    /// the same pass.
    pub label: Option<crate::sensitivity::SensitivityLabel>,
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
        }
    }
}

/// A message that could not be sent and is waiting in the Outbox.
///
/// The built MIME bytes are stored rather than the composed fields: a retry has
/// to send exactly what was composed, and the attachments' files may be gone by
/// then — under Flatpak the portal paths handed to the file chooser expire.
/// `from_addr` and `rcpts` are the SMTP envelope, which is not always what the
/// headers say (Bcc is in the envelope only).
#[derive(Debug, Clone)]
pub struct OutboxItem {
    pub id: u32,
    pub account_id: u32,
    /// Envelope sender.
    pub from_addr: String,
    /// Envelope recipients (To + Cc + Bcc), one per line as stored.
    pub rcpts: Vec<String>,
    /// Header recipients, for display ("Ada Lovelace, bob@example.com").
    pub recipients: String,
    pub subject: String,
    pub preview: String,
    pub raw: Vec<u8>,
    /// The account's Sent folder at queue time, appended to once it goes out.
    pub sent_path: Option<String>,
    /// Unix seconds when it was first queued.
    pub queued_at: i64,
    pub attempts: u32,
    /// Why the last attempt failed, shown in the list.
    pub last_error: String,
    /// Send Later (#145): unix seconds it is due; `None` goes as soon as it can.
    pub send_at: Option<i64>,
}

impl OutboxItem {
    /// "Waiting since 10 minutes ago" — what the reader shows where a received
    /// message shows its date, since an unsent message has no send time yet.
    pub fn waiting_label(queued_at: i64) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(queued_at);
        let secs = (now - queued_at).max(0);
        let ago = match secs {
            s if s < 90 => i18n("just now"),
            s if s < 3600 => i18n_f("{n} minutes ago", &[("n", &(s / 60).to_string())]),
            s if s < 7200 => i18n("an hour ago"),
            s if s < 86400 => i18n_f("{n} hours ago", &[("n", &(s / 3600).to_string())]),
            s if s < 172_800 => i18n("yesterday"),
            s => i18n_f("{n} days ago", &[("n", &(s / 86400).to_string())]),
        };
        i18n_f("Waiting since {ago}", &[("ago", &ago)])
    }

    /// The queued message as a list row. Everything the list needs is stored with
    /// the message, so the Outbox reads as an ordinary folder: same list, same
    /// reader, same sorting. `folder_id` is `OUTBOX_FOLDER_ID`, which no real
    /// folder uses, so a row can always be traced back here.
    pub fn as_message(&self) -> Message {
        Message {
            id: self.id,
            account_id: self.account_id,
            folder_id: OUTBOX_FOLDER_ID,
            uid: self.id,
            // The list's headline column is the sender everywhere else; for
            // unsent mail the useful name is who it is going to.
            from_name: if self.recipients.trim().is_empty() {
                "(no recipients)".to_string()
            } else {
                self.recipients.clone()
            },
            from_addr: self.from_addr.clone(),
            reply_to: String::new(),
            to: self.recipients.clone(),
            cc: String::new(),
            subject: if self.subject.trim().is_empty() {
                "(no subject)".to_string()
            } else {
                self.subject.clone()
            },
            // The row's one line of context: how long it has been stuck and what
            // is stopping it, falling back to the body when nothing has failed
            // yet (a message queued while offline never got an error).
            preview: {
                // A scheduled message says when it goes, until it is due.
                let waiting = match self.send_at {
                    Some(at) if at > crate::datefmt::now() => {
                        i18n_f("Scheduled for {when}", &[("when", &crate::datefmt::date_time(at))])
                    }
                    _ => Self::waiting_label(self.queued_at),
                };
                let attempts = match self.attempts {
                    0 | 1 => String::new(),
                    n => format!(" · {n} attempts"),
                };
                let reason = if self.last_error.trim().is_empty() {
                    self.preview.trim().to_string()
                } else {
                    self.last_error.trim().to_string()
                };
                if reason.is_empty() {
                    format!("{waiting}{attempts}")
                } else {
                    format!("{waiting}{attempts} · {reason}")
                }
            },
            body: String::new(),
            date: String::new(),
            timestamp: self.send_at.unwrap_or(self.queued_at),
            // Never dimmed as read: it is still waiting to go out.
            unread: true,
            starred: false,
            keywords: Vec::new(),
            has_attachment: false,
            message_id: String::new(),
            references: String::new(),
            importance: Default::default(),
            due: 0,
            labels: Vec::new(),
        }
    }
}

/// Folder id for the Outbox's synthetic rows. Real folders are numbered from 1
/// by the worker, so this can't collide.
pub const OUTBOX_FOLDER_ID: u32 = u32::MAX;

/// Folder id for the Snoozed view's rows, which stand for messages waiting
/// in a server folder the app does not list them from. One below the
/// Outbox's, for the same reason.
pub const SNOOZED_FOLDER_ID: u32 = u32::MAX - 1;

/// The name of the folder snoozed mail waits in on the server. English on
/// every system, like the folder Gmail and Outlook use: phones and webmail
/// see the same folder whatever language Hylki speaks.
pub const SNOOZED_FOLDER_NAME: &str = "Snoozed";

/// A message put away until later (Snooze). The message itself waits on the
/// server, in the account's Snoozed folder, so it is out of the Inbox on
/// every device; this is what Hylki keeps of it locally: when it comes back,
/// where to, and enough of it to list and read it in the Snoozed view
/// without asking the server.
#[derive(Debug, Clone, PartialEq)]
pub struct SnoozedItem {
    pub id: u32,
    pub account_id: u32,
    /// The Message-ID it is found by in the Snoozed folder: the move gave it
    /// a new UID there, and will again on the way back.
    pub message_id: String,
    /// The folder it was snoozed from, and goes back to.
    pub folder_path: String,
    /// Where it waits on the server.
    pub snoozed_path: String,
    /// Unix seconds it comes back.
    pub wake_at: i64,
    /// Messages snoozed together, a conversation's, share this, and the
    /// Snoozed view shows them as one row that acts on all of them.
    pub batch: String,
    pub from_name: String,
    pub from_addr: String,
    pub subject: String,
    pub preview: String,
    /// The body as the reader last rendered it, when it had been opened;
    /// empty otherwise, and the reader then shows the preview.
    pub body: String,
    /// The message's own date.
    pub timestamp: i64,
    pub has_attachment: bool,
}

impl SnoozedItem {
    /// "Until …", with the date and time a snoozed message comes back.
    pub fn until_label(&self) -> String {
        i18n_f("Until {when}", &[("when", &crate::datefmt::date_time(self.wake_at))])
    }

    /// The snoozed message as a row of the Snoozed view, with the time it
    /// comes back where a row shows its date (the list sorts by it, too) and
    /// in front of its preview. `folder_id` is [`SNOOZED_FOLDER_ID`], so no
    /// server action is ever aimed at it, and it has no Message-ID, so the
    /// list never threads it.
    pub fn as_message(&self) -> Message {
        let preview = self.preview.trim();
        Message {
            id: self.id,
            account_id: self.account_id,
            folder_id: SNOOZED_FOLDER_ID,
            uid: self.id,
            from_name: self.from_name.clone(),
            from_addr: self.from_addr.clone(),
            reply_to: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: if self.subject.trim().is_empty() {
                i18n("(no subject)")
            } else {
                self.subject.clone()
            },
            preview: if preview.is_empty() {
                self.until_label()
            } else {
                format!("{} · {preview}", self.until_label())
            },
            body: String::new(),
            date: String::new(),
            timestamp: self.wake_at,
            unread: false,
            starred: false,
            keywords: Vec::new(),
            has_attachment: self.has_attachment,
            message_id: String::new(),
            references: String::new(),
            importance: Default::default(),
            due: 0,
            labels: Vec::new(),
        }
    }
}

/// The Snoozed view's rows: one per batch (a conversation snoozed together
/// is one row), standing for its newest message, soonest back first.
pub fn snoozed_rows(items: &[SnoozedItem]) -> Vec<&SnoozedItem> {
    let mut out: Vec<&SnoozedItem> = Vec::new();
    for item in items {
        match out.iter_mut().find(|o| o.account_id == item.account_id && o.batch == item.batch) {
            Some(o) if item.timestamp > o.timestamp => *o = item,
            Some(_) => {}
            None => out.push(item),
        }
    }
    out.sort_by_key(|i| (i.wake_at, i.id));
    out
}

/// The snoozed messages whose time has come by `now`.
pub fn due_snoozes(items: &[SnoozedItem], now: i64) -> Vec<&SnoozedItem> {
    items.iter().filter(|i| i.wake_at <= now).collect()
}

/// Where an account's snoozed mail waits: a Snoozed folder it already has,
/// at the top level or under the server's personal namespace (`INBOX.` on
/// some servers), whatever its case; Outlook keeps one of its own, and
/// another client may have made one. Otherwise the path the worker creates
/// it at on first use.
pub fn snoozed_folder_path(folders: &[Folder], namespace: &str) -> String {
    let wanted = format!("{namespace}{SNOOZED_FOLDER_NAME}");
    folders
        .iter()
        .find(|f| f.path.eq_ignore_ascii_case(&wanted) || f.path.eq_ignore_ascii_case(SNOOZED_FOLDER_NAME))
        .map(|f| f.path.clone())
        .unwrap_or(wanted)
}

/// A decoded message attachment (fetched on demand for the reader).
#[derive(Debug, Clone)]
pub struct Attachment {
    pub name: String,
    pub data: Vec<u8>,
}

impl Attachment {
    /// Human-readable size, e.g. "12.3 KB".
    pub fn human_size(&self) -> String {
        human_size(self.data.len() as u64)
    }
}

/// Human-readable byte size, e.g. "12.3 KB".
pub fn human_size(bytes: u64) -> String {
    gtk::glib::format_size(bytes).into()
}

/// Whether a filename looks like a raster image we can thumbnail/preview inline.
pub fn is_image_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".ico", ".heic", ".heif", ".avif"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// The lowercase extension of a filename (empty when there is none).
/// The file's extension, lower-cased, or empty when the name has none: a
/// generated "attachment-1", or a dotless name from a sender's client.
/// Whatever follows the last dot only counts as an extension when it looks
/// like one (short, alphanumeric), so "Report v1.2 draft" has none either.
pub fn ext_of(name: &str) -> String {
    let Some((_, ext)) = name.rsplit_once('.') else { return String::new() };
    if ext.is_empty() || ext.len() > 8 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return String::new();
    }
    ext.to_ascii_lowercase()
}

/// Searchable category words for a file, keyed off its extension, so a query
/// like "image" or "spreadsheet" matches even when the word isn't in the name.
pub fn type_keywords(name: &str) -> &'static str {
    match ext_of(name).as_str() {
        "pdf" => "pdf document",
        "doc" | "docx" | "odt" | "rtf" => "word document",
        "xls" | "xlsx" | "ods" | "csv" => "excel spreadsheet",
        "ppt" | "pptx" | "odp" => "powerpoint presentation slides",
        "zip" | "gz" | "tar" | "7z" | "rar" | "xz" | "bz2" => "archive compressed",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => "audio music sound",
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => "video movie",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "heif" | "avif" | "ico" => {
            "image photo picture"
        }
        "ics" => "calendar event",
        "txt" | "md" => "text document",
        _ => "file",
    }
}

/// Which row of the gallery's type dropdown a file belongs to: 1 images,
/// 2 PDFs, 3 documents, 4 archives, 5 audio/video, 6 anything else. Stored
/// alongside each attachment so the filter and the "Type" sort are one SQL
/// query rather than a pass over every row.
pub fn type_bucket(name: &str) -> u32 {
    match ext_of(name).as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "heif" | "avif" | "ico" => 1,
        "pdf" => 2,
        "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "xls" | "xlsx" | "ods" | "csv" | "ppt"
        | "pptx" | "odp" | "ics" => 3,
        "zip" | "gz" | "tar" | "7z" | "rar" | "xz" | "bz2" => 4,
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "mp4" | "mov" | "mkv" | "webm" | "avi"
        | "m4v" => 5,
        _ => 6,
    }
}

/// The file extension matching an image's magic bytes ("jpg" when unsure —
/// for content that is known to be an image but arrived without a name).
pub fn image_ext(data: &[u8]) -> &'static str {
    match data {
        [0x89, b'P', b'N', b'G', ..] => "png",
        [b'G', b'I', b'F', b'8', ..] => "gif",
        [b'B', b'M', ..] => "bmp",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "webp",
        _ => "jpg",
    }
}

/// How the attachments gallery orders its items. The dropdown row indices are
/// part of the stored settings (`gallery_sort`), so the mapping either way is
/// pinned here rather than being re-derived at each call site; the cache turns
/// the same value into an ORDER BY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GallerySort {
    #[default]
    Newest,
    Oldest,
    Name,
    NameDesc,
    Sender,
    SenderDesc,
    Largest,
    Smallest,
    Type,
    TypeDesc,
}

impl GallerySort {
    /// Map the sort dropdown's selected row to a criterion. The order must match
    /// the `StringList` built in the view.
    pub fn from_index(i: u32) -> GallerySort {
        match i {
            1 => GallerySort::Oldest,
            2 => GallerySort::Name,
            3 => GallerySort::NameDesc,
            4 => GallerySort::Sender,
            5 => GallerySort::SenderDesc,
            6 => GallerySort::Largest,
            7 => GallerySort::Smallest,
            8 => GallerySort::Type,
            9 => GallerySort::TypeDesc,
            _ => GallerySort::Newest,
        }
    }

    /// The dropdown row for a criterion — [`GallerySort::from_index`]'s inverse, so
    /// a table-header click can move the dropdown's selection with it.
    pub fn index(self) -> u32 {
        match self {
            GallerySort::Newest => 0,
            GallerySort::Oldest => 1,
            GallerySort::Name => 2,
            GallerySort::NameDesc => 3,
            GallerySort::Sender => 4,
            GallerySort::SenderDesc => 5,
            GallerySort::Largest => 6,
            GallerySort::Smallest => 7,
            GallerySort::Type => 8,
            GallerySort::TypeDesc => 9,
        }
    }
}

/// What the server says about one attachment, without its bytes: what the
/// gallery's scan records so a file can be listed, searched and sorted long
/// before anyone asks to open it.
#[derive(Debug, Clone)]
pub struct AttachmentMeta {
    /// Which attachment of the message this is, in the order the scan found them.
    pub idx: u32,
    pub name: String,
    /// MIME type as declared, e.g. "application/pdf"; empty when unknown.
    pub mime: String,
    /// Decoded size in bytes (BODYSTRUCTURE reports the encoded size, which
    /// base64 inflates by 4/3).
    pub size: u64,
    /// IMAP part section, e.g. "2" or "1.3" — what to FETCH to get just this
    /// file. Empty when the scan could not work the structure out, in which
    /// case opening it falls back to fetching the whole message.
    pub section: String,
}

/// One attachment for the gallery: metadata plus the source message context.
/// `data` is loaded eagerly for small files whose bytes are cached (so the
/// preview/open is instant); it is `None` both for a large cached file and for
/// one that has never been downloaded — `downloaded` tells those apart, and
/// either way opening it fetches on demand.
#[derive(Debug, Clone)]
pub struct GalleryItem {
    pub account_id: u32,
    pub folder_path: String,
    pub uid: u32,
    pub name: String,
    /// Size in bytes as the server declared it, decoded.
    pub size: u64,
    /// Whether the bytes are in the cache (regardless of `data`, which is only
    /// filled for files under the eager-load cap).
    pub downloaded: bool,
    /// Sender display name of the source message.
    pub from_name: String,
    pub subject: String,
    /// Source message timestamp (for sorting, newest first).
    pub timestamp: i64,
    pub data: Option<Vec<u8>>,
}

impl GalleryItem {
    pub fn is_image(&self) -> bool {
        is_image_name(&self.name)
    }
    pub fn human_size(&self) -> String {
        human_size(self.size)
    }

    /// Compact date of the source message for the gallery meta, e.g. "Jul 12"
    /// (or "Jul 12, 2025" if not this year). Empty when the date is unknown.
    pub fn date_label(&self) -> String {
        if self.timestamp <= 0 {
            return String::new();
        }
        if crate::datefmt::year(self.timestamp) == crate::datefmt::year(crate::datefmt::now()) {
            crate::datefmt::day_month(self.timestamp)
        } else {
            crate::datefmt::day_month_year(self.timestamp)
        }
    }
}

impl Message {
    /// Whether the message carries `keyword`. IMAP keywords are atoms
    /// compared case-insensitively (RFC 3501), so `work` and `Work` are one.
    pub fn has_keyword(&self, keyword: &str) -> bool {
        self.keywords.iter().any(|k| k.eq_ignore_ascii_case(keyword))
    }

    /// Add or drop `keyword`, keeping the list free of duplicates.
    pub fn set_keyword(&mut self, keyword: &str, add: bool) {
        self.keywords.retain(|k| !k.eq_ignore_ascii_case(keyword));
        if add {
            self.keywords.push(keyword.to_string());
        }
    }

    /// Whether the message carries `tag`: its keyword, or on Gmail a label
    /// named like it (#374), which is how a tag set in Gmail itself arrives.
    pub fn has_tag(&self, tag: &crate::config::Tag) -> bool {
        self.has_keyword(&tag.keyword) || self.labels.iter().any(|l| label_names_tag(l, tag))
    }

    /// The message's keywords, with the keyword of every tag a Gmail label
    /// stands for: what the tag checks go by.
    pub fn tag_keywords(&self, tags: &[crate::config::Tag]) -> Vec<String> {
        let mut out = self.keywords.clone();
        for t in tags {
            if !self.has_keyword(&t.keyword) && self.labels.iter().any(|l| label_names_tag(l, t)) {
                out.push(t.keyword.clone());
            }
        }
        out
    }

    /// Take a tag off this copy of the message: its keyword and the labels
    /// that stand for it.
    pub fn drop_tag(&mut self, tag: &crate::config::Tag) {
        self.set_keyword(&tag.keyword, false);
        self.labels.retain(|l| !label_names_tag(l, tag));
    }

    /// The pills the message wears in the list and the reader; see [`pills`].
    pub fn pills<'a>(&self, tags: &'a [crate::config::Tag]) -> Vec<Pill<'a>> {
        pills(&self.keywords, &self.labels, tags)
    }

    /// Strip interior NUL bytes from every text field. GTK's C strings end at
    /// the first NUL and glib panics rather than truncate when handed one
    /// mid-string — a single message with a stray 0x00 in its envelope (they
    /// exist in the wild) took the whole message list down with it. Called at
    /// the ingestion choke points (envelope parse, cache load), so nothing
    /// NUL-bearing ever reaches a label, tooltip, or document.
    pub fn scrub_nuls(&mut self) {
        for s in [
            &mut self.from_name,
            &mut self.from_addr,
            &mut self.reply_to,
            &mut self.to,
            &mut self.cc,
            &mut self.subject,
            &mut self.preview,
            &mut self.body,
            &mut self.date,
        ] {
            if s.contains('\0') {
                *s = s.replace('\0', " ");
            }
        }
    }

    /// Full receipt date and time for the reader header, e.g.
    /// "Jun 27, 2026 at 3:15 PM". Falls back to the short label if unknown.
    pub fn datetime_full(&self) -> String {
        if self.timestamp <= 0 {
            return self.date.clone();
        }
        let full = crate::datefmt::date_time(self.timestamp);
        if full.trim().is_empty() {
            self.date.clone()
        } else {
            full
        }
    }

    /// Compact date + time for list rows: always shows the time, with the day
    /// (and year, if not this year).
    pub fn datetime_list(&self) -> String {
        datetime_list_at(self.timestamp, &self.date)
    }

}

/// One pill on a message: a tag (#71), or a Gmail label no tag stands for
/// (#374), by its display name.
#[derive(Debug, Clone, PartialEq)]
pub enum Pill<'a> {
    Tag(&'a crate::config::Tag),
    Label(String),
}

/// The tags a message carries, in the order Settings lists them, then the
/// Gmail labels none of them stands for, in the server's order. A label
/// named like a tag (by keyword or by name) shows as that tag, once: on
/// Gmail, which keeps no keywords, a label is how such a tag arrives.
pub fn pills<'a>(
    keywords: &[String],
    labels: &[String],
    tags: &'a [crate::config::Tag],
) -> Vec<Pill<'a>> {
    // A decoded name can hold a NUL (`&AAA-`), which a GTK label aborts on.
    let shown: Vec<String> =
        labels.iter().map(|l| crate::mutf7::decode(l).replace('\0', " ")).collect();
    let names_label = |t: &crate::config::Tag, i: usize| {
        t.keyword.eq_ignore_ascii_case(&labels[i]) || t.name.eq_ignore_ascii_case(&shown[i])
    };
    let mut out: Vec<Pill<'a>> = tags
        .iter()
        .filter(|t| {
            keywords.iter().any(|k| k.eq_ignore_ascii_case(&t.keyword))
                || (0..labels.len()).any(|i| names_label(t, i))
        })
        .map(Pill::Tag)
        .collect();
    for (i, name) in shown.iter().enumerate() {
        if !tags.iter().any(|t| names_label(t, i)) {
            out.push(Pill::Label(name.clone()));
        }
    }
    out
}

/// Whether a Gmail label (as the server writes it, modified UTF-7) stands for
/// a tag: named like its keyword, or like its name.
pub fn label_names_tag(label: &str, tag: &crate::config::Tag) -> bool {
    tag.keyword.eq_ignore_ascii_case(label) || tag.name.eq_ignore_ascii_case(&crate::mutf7::decode(label))
}

/// A short date for a single line: the time for today, the day and month
/// this year, the year too before that. Used where the full list date would
/// crowd out the text: a folded card (#326), a one-line list row (#334).
pub fn date_short(timestamp: i64, date: &str) -> String {
    use crate::datefmt as d;
    if timestamp <= 0 {
        return date.to_string();
    }
    let now = d::now();
    if d::day_key(timestamp) == d::day_key(now) {
        d::time(timestamp)
    } else if d::year(timestamp) == d::year(now) {
        d::day_month(timestamp)
    } else {
        d::day_month_year(timestamp)
    }
}

/// [`Message::datetime_list`] for a time the app holds without a message:
/// the newest member of a conversation that lives in another folder (#236),
/// which the cache answers with as a timestamp and the header it was sent
/// with. `date` is the fallback for a mail whose date could not be parsed.
pub fn datetime_list_at(timestamp: i64, date: &str) -> String {
    if timestamp <= 0 {
        return date.to_string();
    }
    let now = crate::datefmt::now();
    let time = crate::datefmt::time(timestamp);
    if time.is_empty() {
        return date.to_string();
    }
    if crate::datefmt::day_key(timestamp) == crate::datefmt::day_key(now) {
        i18n_f("Today, {time}", &[("time", &time)])
    } else if crate::datefmt::year(timestamp) == crate::datefmt::year(now) {
        format!("{}, {time}", crate::datefmt::day_month(timestamp))
    } else {
        format!("{}, {time}", crate::datefmt::day_month_year(timestamp))
    }
}

/// The newest message of a conversation, as the cache sees it across the
/// whole account (#236).
///
/// A list shows one folder, so the newest thing it knows about a conversation
/// is the last mail that *arrived*. The reply you sent went to Sent, and the
/// row went on quoting the other side — nothing on screen said you had
/// answered. This is that missing message, summarised.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadLatest {
    pub from_name: String,
    pub from_addr: String,
    pub preview: String,
    /// Unix seconds, for deciding whether this really is newer than what the
    /// folder itself holds.
    pub timestamp: i64,
    /// The Date header as it was displayed, for a message with no usable
    /// timestamp.
    pub date: String,
}

/// What the list cannot work out about a conversation on its own: how big it
/// really is (#222) and which message moved it last (#236). Both are answered
/// in one pass of the cache, for a whole page of rows at a time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThreadSummary {
    pub count: usize,
    pub latest: Option<ThreadLatest>,
    /// The conversation's messages across the account, drafts, Trash and
    /// Junk left out, for a row that opens out into the parts filed in other
    /// folders (#309). Empty for a conversation of one.
    pub members: Vec<Message>,
}

/// Every Message-ID that identifies a conversation: the messages' own ids plus
/// the ones they reference. This is what the cache is searched by to find the
/// parts of the thread filed in other folders — both to assemble a conversation
/// the reader has opened and to count one the list is only showing a slice of.
pub fn thread_ids<'a>(msgs: impl IntoIterator<Item = &'a Message>) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut push = |id: &str| {
        if !id.is_empty() && !ids.iter().any(|x| x == id) {
            ids.push(id.to_string());
        }
    };
    for m in msgs {
        push(&m.message_id);
        for r in m.references.split_whitespace() {
            push(r);
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #374: tags first in Settings order, then the labels no tag stands
    /// for, decoded; a label named like a tag shows as that tag, once.
    #[test]
    fn pills_put_tags_first_and_show_a_label_once() {
        let tag = |name: &str, keyword: &str| crate::config::Tag {
            name: name.into(),
            keyword: keyword.into(),
            color: "#1c71d8".into(),
        };
        let tags = [tag("Urgent", "$label1"), tag("Work", "Work"), tag("Receipts", "receipts")];
        let keywords = vec!["$Label1".to_string()];
        let labels: Vec<String> =
            ["Clients/Acme", "work", "&AOk-t&AOk-", "My Label", "Receipts"].iter().map(|l| l.to_string()).collect();
        let names: Vec<String> = pills(&keywords, &labels, &tags)
            .into_iter()
            .map(|p| match p {
                Pill::Tag(t) => format!("tag:{}", t.name),
                Pill::Label(l) => format!("label:{l}"),
            })
            .collect();
        assert_eq!(
            names,
            ["tag:Urgent", "tag:Work", "tag:Receipts", "label:Clients/Acme", "label:été", "label:My Label"]
        );
        assert!(pills(&[], &[], &tags).is_empty());
        assert_eq!(pills(&[], &["&AAA-".to_string()], &[]), [Pill::Label(" ".into())]);
    }

    /// #374: a Gmail label named like a tag is the tag, for every check that
    /// asks; taking the tag off takes those labels too.
    #[test]
    fn a_label_named_like_a_tag_is_the_tag() {
        let tag = |name: &str, keyword: &str| crate::config::Tag {
            name: name.into(),
            keyword: keyword.into(),
            color: "#1c71d8".into(),
        };
        let work = tag("Work", "$label2");
        let ete = tag("Été", "ete");
        let mut m = crate::backend::MockBackend::new().message(1).expect("a demo message");
        m.keywords.clear();
        m.labels = vec!["work".into(), "&AMk-t&AOk-".into(), "Other".into()];
        assert!(m.has_tag(&work) && m.has_tag(&ete));
        assert!(!m.has_tag(&tag("Home", "home")));
        assert_eq!(m.tag_keywords(&[work.clone(), ete.clone()]), ["$label2", "ete"]);
        m.keywords = vec!["$Label2".into()];
        assert_eq!(m.tag_keywords(&[work.clone()]), ["$Label2"], "no second copy of a keyword it has");
        m.drop_tag(&work);
        assert!(!m.has_tag(&work) && m.keywords.is_empty());
        assert_eq!(m.labels, ["&AMk-t&AOk-", "Other"]);
    }

    /// #334: each priority header's spellings, and normal for anything else.
    #[test]
    fn importance_reads_every_priority_header() {
        assert_eq!(Importance::from_header("X-Priority", "1 (Highest)"), Importance::High);
        assert_eq!(Importance::from_header("x-priority", " 2"), Importance::High);
        assert_eq!(Importance::from_header("X-Priority", "3 (Normal)"), Importance::Normal);
        assert_eq!(Importance::from_header("X-Priority", "5 (Lowest)"), Importance::Low);
        assert_eq!(Importance::from_header("Importance", "High"), Importance::High);
        assert_eq!(Importance::from_header("Importance", "low"), Importance::Low);
        assert_eq!(Importance::from_header("Priority", "urgent"), Importance::High);
        assert_eq!(Importance::from_header("Priority", "non-urgent"), Importance::Low);
        assert_eq!(Importance::from_header("Importance", "whatever"), Importance::Normal);
        // The first header that says anything wins; a normal one says nothing.
        let headers = [("X-Priority", "3"), ("Importance", "high")];
        let lookup = |name: &str| headers.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
        assert_eq!(Importance::from_headers(lookup), Importance::High);
        assert_eq!(Importance::from_headers(|_| None), Importance::Normal);
        for i in [Importance::Normal, Importance::High, Importance::Low] {
            assert_eq!(Importance::from_i64(i.to_i64()), i);
        }
    }

    #[test]
    fn a_quota_says_what_is_used_and_free() {
        let q = MailboxQuota { used: 250, limit: 1000 };
        assert_eq!(q.fraction(), 0.25);
        assert!(q.free_line().ends_with("(75%)"));
        let over = MailboxQuota { used: 1200, limit: 1000 };
        assert_eq!(over.fraction(), 1.0);
        assert!(over.free_line().ends_with("(0%)"));
    }

    #[test]
    fn the_pgp_chip_names_its_verdict() {
        let good = |trust| PgpSignature::Good { signer: "A".into(), key_id: "1".into(), trust };
        let status = |encrypted, signature| PgpStatus { encrypted, decrypted: encrypted, signature, ..Default::default() };
        assert_eq!(status(false, good(PgpTrust::Full)).short_label(), "Signed");
        assert_eq!(status(true, good(PgpTrust::Full)).short_label(), "Encrypted and signed");
        assert_eq!(status(false, good(PgpTrust::Unknown)).short_label(), "Signed, key not trusted");
        assert_eq!(status(false, PgpSignature::Bad { signer: "A".into() }).short_label(), "Bad signature");
        assert_eq!(status(true, PgpSignature::None).short_label(), "Encrypted");
        let locked = PgpStatus { encrypted: true, decrypted: false, ..Default::default() };
        assert_eq!(locked.short_label(), "Not decrypted");
    }

    #[test]
    fn english_folder_names_stay_as_the_server_has_them() {
        let folder = |name: &str, kind| Folder {
            id: 1,
            account_id: 1,
            name: name.into(),
            path: name.into(),
            kind,
            unread: 0,
        };
        let mut folders = vec![
            folder("Deleted Items", FolderKind::Trash),
            folder("Junk Mail", FolderKind::Junk),
            folder("Old Stuff", FolderKind::Archive),
            folder("Sent", FolderKind::Custom),
        ];
        let before: Vec<String> = folders.iter().map(|f| f.name.clone()).collect();
        // The tests run without a catalogue, as English does.
        localize_special_folder_names(&mut folders);
        assert_eq!(folders.iter().map(|f| f.name.clone()).collect::<Vec<_>>(), before);
    }

    fn item() -> OutboxItem {
        OutboxItem {
            id: 7,
            account_id: 1,
            from_addr: "me@example.com".into(),
            rcpts: vec!["ada@example.com".into()],
            recipients: "Ada Lovelace <ada@example.com>".into(),
            subject: "Quarterly numbers".into(),
            preview: "Here are the figures".into(),
            raw: Vec::new(),
            sent_path: None,
            queued_at: 0,
            attempts: 1,
            send_at: None,
            last_error: String::new(),
        }
    }

    #[test]
    fn a_queued_message_reads_as_an_ordinary_row() {
        let row = item().as_message();
        // The list's headline column is who it is going to, not who sent it: in
        // an Outbox every message is from the same person.
        assert_eq!(row.from_name, "Ada Lovelace <ada@example.com>");
        assert_eq!(row.subject, "Quarterly numbers");
        assert_eq!(row.folder_id, OUTBOX_FOLDER_ID);
        assert_eq!(row.id, 7);
        // Still waiting, so never shown as read.
        assert!(row.unread);
    }

    #[test]
    fn a_queued_row_says_why_it_is_stuck() {
        let mut i = item();
        i.attempts = 4;
        i.last_error = "Connection refused (os error 111)".into();
        let row = i.as_message();
        assert!(row.preview.contains("4 attempts"), "{}", row.preview);
        assert!(row.preview.contains("Connection refused"), "{}", row.preview);
        // With nothing failed yet, the body stands in for the reason.
        let row = item().as_message();
        assert!(row.preview.contains("Here are the figures"), "{}", row.preview);
        assert!(!row.preview.contains("attempts"), "{}", row.preview);
    }

    #[test]
    fn an_empty_subject_or_recipient_still_reads_sensibly() {
        let mut i = item();
        i.subject = "  ".into();
        i.recipients = String::new();
        let row = i.as_message();
        assert_eq!(row.subject, "(no subject)");
        assert_eq!(row.from_name, "(no recipients)");
    }

    #[test]
    fn type_keywords_cover_common_kinds() {
        assert!(type_keywords("a.pdf").contains("document"));
        assert!(type_keywords("a.png").contains("image"));
        assert!(type_keywords("a.mp3").contains("audio"));
        assert!(type_keywords("a.ics").contains("calendar"));
    }

    #[test]
    fn type_buckets_match_the_footer_dropdown_rows() {
        assert_eq!(type_bucket("photo.JPG"), 1);
        assert_eq!(type_bucket("report.pdf"), 2);
        assert_eq!(type_bucket("notes.docx"), 3);
        assert_eq!(type_bucket("backup.tar"), 4);
        assert_eq!(type_bucket("song.flac"), 5);
        assert_eq!(type_bucket("unknown.xyz"), 6);
    }
    #[test]
    fn assigned_drafts_folder_takes_the_kind_that_counts_every_draft() {
        let f = |id: u32, path: &str, kind: FolderKind| Folder {
            id,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
        };
        // A laposte.net-style listing: nothing detected as Drafts, and the
        // user assigned "Brouillons" under Special Folders.
        let mut folders = vec![
            f(1, "INBOX", FolderKind::Inbox),
            f(2, "Brouillons", FolderKind::Custom),
            f(3, "Sent", FolderKind::Sent),
        ];
        let roles: std::collections::BTreeMap<String, String> =
            [("drafts".to_string(), "Brouillons".to_string())].into();
        assign_folder_roles(&roles, &mut folders);
        let drafts = folders.iter().find(|f| f.path == "Brouillons").unwrap();
        assert_eq!(drafts.kind, FolderKind::Drafts);
        assert!(crate::worker::chip_counts_all(drafts.kind), "chip counts every draft");
        // Positions and ids are untouched: the worker's cache keys ids by order.
        assert_eq!(folders.iter().map(|f| f.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(folders[1].path, "Brouillons");
    }

    #[test]
    fn templates_can_be_assigned_and_are_named_in_the_language() {
        // #360: "Vorlagen" is pointed at under Special Folders, and a
        // detected "Templates" gives way to it.
        let f = |id: u32, path: &str, kind: FolderKind| Folder {
            id,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
        };
        let mut folders = vec![
            f(1, "INBOX", FolderKind::Inbox),
            f(2, "Templates", FolderKind::Templates),
            f(3, "Vorlagen", FolderKind::Custom),
        ];
        let roles: std::collections::BTreeMap<String, String> =
            [("templates".to_string(), "Vorlagen".to_string())].into();
        assign_folder_roles(&roles, &mut folders);
        assert_eq!(folders[1].kind, FolderKind::Custom);
        assert_eq!(folders[2].kind, FolderKind::Templates);
        assert!(is_templates_name(" Templates "));
        assert!(!is_templates_name("My Templates"));
    }

    fn plain_folder(path: &str, kind: FolderKind) -> Folder {
        Folder {
            id: 0,
            account_id: 1,
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
        }
    }

    #[test]
    fn a_hidden_folder_takes_its_sub_folders_with_it() {
        let hidden = vec!["Sync Issues".to_string()];
        assert!(folder_is_hidden("Sync Issues", Some("/"), &hidden));
        assert!(folder_is_hidden("Sync Issues/Conflicts", Some("/"), &hidden));
        assert!(!folder_is_hidden("Sync Issues Archive", Some("/"), &hidden));
        assert!(!folder_is_hidden("INBOX/Sync Issues", Some("/"), &hidden));
        // Without a known delimiter, the usual three all count.
        assert!(folder_is_hidden("Sync Issues.Conflicts", None, &hidden));
        assert!(!folder_is_hidden("Sync Issues Archive", None, &hidden));
    }

    #[test]
    fn exchange_non_mail_folders_need_an_exchange_looking_listing() {
        let exchange: Vec<Folder> = [
            ("INBOX", FolderKind::Inbox),
            ("Calendar", FolderKind::Custom),
            ("Calendar/Birthdays", FolderKind::Custom),
            ("Contacts", FolderKind::Custom),
            ("Tasks", FolderKind::Custom),
            ("Notes", FolderKind::Custom),
            ("Sync Issues", FolderKind::Custom),
            ("Sync Issues/Conflicts", FolderKind::Custom),
            ("Conversation History", FolderKind::Custom),
            ("Projects", FolderKind::Custom),
            ("Sent Items", FolderKind::Sent),
        ]
        .iter()
        .map(|(p, k)| plain_folder(p, *k))
        .collect();
        let mut hidden = exchange_non_mail_folders(&exchange);
        hidden.sort();
        assert_eq!(
            hidden,
            vec!["Calendar", "Contacts", "Conversation History", "Notes", "Sync Issues", "Tasks"]
        );

        // Apple Notes keeps a "Notes" folder on ordinary IMAP servers; that
        // alone is no reason to hide anything.
        let plain: Vec<Folder> = [("INBOX", FolderKind::Inbox), ("Notes", FolderKind::Custom)]
            .iter()
            .map(|(p, k)| plain_folder(p, *k))
            .collect();
        assert!(exchange_non_mail_folders(&plain).is_empty());
    }

    fn snoozed(id: u32, batch: &str, timestamp: i64, wake_at: i64) -> SnoozedItem {
        SnoozedItem {
            id,
            account_id: 1,
            message_id: format!("m{id}@example.com"),
            folder_path: "INBOX".into(),
            snoozed_path: "Snoozed".into(),
            wake_at,
            batch: batch.into(),
            from_name: "Ada".into(),
            from_addr: "ada@example.com".into(),
            subject: "Lunch".into(),
            preview: "Shall we?".into(),
            body: String::new(),
            timestamp,
            has_attachment: false,
        }
    }

    /// A conversation snoozed together is one row, its newest message's,
    /// and the rows come soonest back first.
    #[test]
    fn snoozed_rows_are_one_per_batch_soonest_first() {
        let items = vec![
            snoozed(1, "a", 100, 5000),
            snoozed(2, "a", 300, 5000),
            snoozed(3, "b", 200, 4000),
            snoozed(4, "a", 200, 5000),
        ];
        let rows: Vec<u32> = snoozed_rows(&items).iter().map(|i| i.id).collect();
        assert_eq!(rows, [3, 2]);
        // The same batch name on another account is another batch.
        let mut other = snoozed(5, "a", 50, 6000);
        other.account_id = 2;
        let items = vec![snoozed(1, "a", 100, 5000), other];
        assert_eq!(snoozed_rows(&items).len(), 2);
    }

    #[test]
    fn due_snoozes_are_the_ones_whose_time_has_come() {
        let items = vec![snoozed(1, "a", 0, 100), snoozed(2, "b", 0, 200), snoozed(3, "c", 0, 201)];
        let due: Vec<u32> = due_snoozes(&items, 200).iter().map(|i| i.id).collect();
        assert_eq!(due, [1, 2]);
        assert!(due_snoozes(&items, 99).is_empty());
    }

    /// A snoozed row is no server message: the Snoozed view's folder, no
    /// Message-ID to thread on, sorted by when it comes back.
    #[test]
    fn a_snoozed_row_reads_as_a_row_of_its_own_view() {
        let row = snoozed(7, "a", 100, 5000).as_message();
        assert_eq!(row.folder_id, SNOOZED_FOLDER_ID);
        assert_eq!(row.id, 7);
        assert_eq!(row.timestamp, 5000);
        assert!(row.message_id.is_empty() && row.references.is_empty());
        assert!(row.preview.ends_with(" · Shall we?"), "{}", row.preview);
    }

    /// An existing Snoozed folder is used whatever its case, at the top or
    /// under the namespace; otherwise it is made under the namespace.
    #[test]
    fn the_snoozed_folder_is_found_before_it_is_made() {
        let folders = vec![plain_folder("INBOX", FolderKind::Inbox), plain_folder("Archive", FolderKind::Archive)];
        assert_eq!(snoozed_folder_path(&folders, ""), "Snoozed");
        assert_eq!(snoozed_folder_path(&folders, "INBOX."), "INBOX.Snoozed");
        let mut outlook = folders.clone();
        outlook.push(plain_folder("snoozed", FolderKind::Custom));
        assert_eq!(snoozed_folder_path(&outlook, ""), "snoozed");
        let mut dovecot = folders.clone();
        dovecot.push(plain_folder("INBOX.Snoozed", FolderKind::Custom));
        assert_eq!(snoozed_folder_path(&dovecot, "INBOX."), "INBOX.Snoozed");
        // A Snoozed folder nested somewhere else is somebody's own.
        let mut nested = folders;
        nested.push(plain_folder("Projects/Snoozed", FolderKind::Custom));
        assert_eq!(snoozed_folder_path(&nested, ""), "Snoozed");
    }
}
