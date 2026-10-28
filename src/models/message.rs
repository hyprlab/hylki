//! A message summary as the list and the reader use it, with the helpers
//! that describe it: importance, previews, pills, dates and conversations.

use crate::i18n::{i18n, i18n_f};

/// A single message (summary + body). In a real backend the body is loaded
/// lazily; here it is always present.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Message {
    pub(crate) id: u32,
    /// Owning account; needed to route actions and to merge the unified inbox.
    pub(crate) account_id: u32,
    pub(crate) folder_id: u32,
    /// IMAP UID, used to fetch the full body lazily. Mock data reuses `id`.
    pub(crate) uid: u32,
    pub(crate) from_name: String,
    pub(crate) from_addr: String,
    /// Where the sender asked replies to go (comma-separated emails), from the
    /// Reply-To header. Empty when the header is absent — reply to `from_addr`.
    pub(crate) reply_to: String,
    /// Original recipients (comma-separated emails), used for Reply All.
    pub(crate) to: String,
    pub(crate) cc: String,
    /// Bcc recipients, comma-separated. Only a sender's own copy names them
    /// (#476); a received message has none.
    pub(crate) bcc: String,
    pub(crate) subject: String,
    pub(crate) preview: String,
    pub(crate) body: String,
    /// Human-readable timestamp for display.
    pub(crate) date: String,
    /// Unix timestamp (seconds) for sorting, e.g. the unified inbox. 0 if unknown.
    pub(crate) timestamp: i64,
    pub(crate) unread: bool,
    pub(crate) starred: bool,
    /// Replied to (IMAP `\Answered`, JMAP `$answered`, Microsoft 365's last
    /// verb) and forwarded (`$Forwarded`, `$forwarded`), for the list's
    /// arrows (#454). Set by any client, this one included when it sends.
    pub(crate) answered: bool,
    pub(crate) forwarded: bool,
    /// The message's IMAP keywords (user flags) as the server reports them —
    /// `$label1`, `Work`, `$Junk`… — verbatim and in server order. Tags (#71)
    /// are the ones a [`crate::config::Tag`] maps to; the rest are ignored.
    /// Microsoft 365 categories and POP3's local-only tags land here too.
    pub(crate) keywords: Vec<String>,
    pub(crate) has_attachment: bool,
    /// This message's own Message-ID (normalized, no angle brackets). Empty if
    /// unknown. Used to thread replies accurately (instead of by subject alone).
    pub(crate) message_id: String,
    /// Referenced Message-IDs (In-Reply-To + References), space-separated and
    /// normalized. Links a reply to the messages it descends from.
    pub(crate) references: String,
    /// How important the sender marked it (#334).
    pub(crate) importance: Importance,
    /// When a Microsoft 365 follow-up flag falls due (Unix seconds), or 0.
    /// Only Graph has such a date; IMAP and JMAP have no standard for one.
    pub(crate) due: i64,
    /// The Gmail labels the message wears besides the folder it is listed
    /// in (#374), as the server names them (modified UTF-7, `Parent/Child`
    /// for a nested one). System labels (`\Inbox`, `\Important`…) are left
    /// out. Empty on every other server.
    pub(crate) labels: Vec<String>,
}

/// How important a message's sender marked it: the `X-Priority`, `Importance`
/// or `Priority` header, or Microsoft 365's `importance` (#334).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Importance {
    #[default]
    Normal,
    High,
    Low,
}

impl Importance {
    /// The value a header gives: `X-Priority: 1 (Highest)`, `Importance:
    /// high`, `Priority: urgent` and so on. Anything else is normal.
    pub(super) fn from_header(name: &str, value: &str) -> Importance {
        let value = value.trim().to_ascii_lowercase();
        if name.eq_ignore_ascii_case("x-priority") {
            return match value.chars().next() {
                Some('1' | '2') => Importance::High,
                Some('4' | '5') => Importance::Low,
                _ => Importance::Normal,
            };
        }
        match value
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
            .next()
            .unwrap_or("")
        {
            "high" | "urgent" => Importance::High,
            "low" | "non-urgent" => Importance::Low,
            _ => Importance::Normal,
        }
    }

    /// The first of `X-Priority`, `Importance` and `Priority` that says
    /// anything, from a lookup by header name.
    pub(crate) fn from_headers<'a>(header: impl Fn(&'static str) -> Option<&'a str>) -> Importance {
        ["X-Priority", "Importance", "Priority"]
            .iter()
            .filter_map(|name| header(name).map(|v| Importance::from_header(name, v)))
            .find(|i| *i != Importance::Normal)
            .unwrap_or_default()
    }

    /// The number the cache stores.
    pub(crate) fn to_i64(self) -> i64 {
        match self {
            Importance::Normal => 0,
            Importance::High => 1,
            Importance::Low => -1,
        }
    }

    pub(crate) fn from_i64(n: i64) -> Importance {
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
pub(crate) struct DraftOrigin {
    pub(crate) account_id: u32,
    pub(crate) folder_id: u32,
    pub(crate) path: String,
    pub(crate) uid: u32,
}

/// The list preview stored for an OpenPGP-encrypted message (#133): a
/// marker rather than words, so the row can draw a lock icon and say
/// "Encrypted message" in whatever language is current, and nothing of the
/// message itself is written down. A lock glyph, so it still reads if it
/// ever shows raw.
pub(crate) const ENCRYPTED_PREVIEW: &str = "\u{1F512}";

/// Whether a stored preview is the encrypted-message marker.
pub(crate) fn preview_is_encrypted(preview: &str) -> bool {
    preview.trim() == ENCRYPTED_PREVIEW
}

/// Whether a subject wears one of the prefixes mail programs put on an
/// answer or a forward, in any of the languages they write them in.
pub(crate) fn is_reply_or_forward_subject(subject: &str) -> bool {
    let subject = subject.trim_start().to_ascii_lowercase();
    [
        "re:", "fw:", "fwd:", "aw:", "sv:", "vs:", "tr:", "rv:", "wg:", "antw:", "r:", "i:",
    ]
    .iter()
    .any(|p| subject.starts_with(p))
}

/// The words the list shows for a stored preview.
pub(crate) fn preview_display(preview: &str) -> String {
    if preview_is_encrypted(preview) {
        i18n("Encrypted message")
    } else {
        preview.to_string()
    }
}

impl Message {
    /// The sender's name as shown: the address book's for a saved contact
    /// when Settings asks for it (#541), else the one the mail gives.
    pub(crate) fn shown_from_name(&self) -> std::borrow::Cow<'_, str> {
        crate::contacts::shown_name(&self.from_name, &self.from_addr)
    }

    /// Whether the message carries `keyword`. IMAP keywords are atoms
    /// compared case-insensitively (RFC 3501), so `work` and `Work` are one.
    pub(crate) fn has_keyword(&self, keyword: &str) -> bool {
        self.keywords
            .iter()
            .any(|k| k.eq_ignore_ascii_case(keyword))
    }

    /// Add or drop `keyword`, keeping the list free of duplicates.
    pub(crate) fn set_keyword(&mut self, keyword: &str, add: bool) {
        self.keywords.retain(|k| !k.eq_ignore_ascii_case(keyword));
        if add {
            self.keywords.push(keyword.to_string());
        }
    }

    /// Whether the message carries `tag`: its keyword, or on Gmail a label
    /// named like it (#374), which is how a tag set in Gmail itself arrives.
    pub(crate) fn has_tag(&self, tag: &crate::config::Tag) -> bool {
        self.has_keyword(&tag.keyword) || self.labels.iter().any(|l| label_names_tag(l, tag))
    }

    /// The message's keywords, with the keyword of every tag a Gmail label
    /// stands for: what the tag checks go by.
    pub(crate) fn tag_keywords(&self, tags: &[crate::config::Tag]) -> Vec<String> {
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
    pub(crate) fn drop_tag(&mut self, tag: &crate::config::Tag) {
        self.set_keyword(&tag.keyword, false);
        self.labels.retain(|l| !label_names_tag(l, tag));
    }

    /// The pills the message wears in the list and the reader; see [`pills`].
    pub(crate) fn pills<'a>(&self, tags: &'a [crate::config::Tag]) -> Vec<Pill<'a>> {
        pills(&self.keywords, &self.labels, tags)
    }

    /// Strip interior NUL bytes from every text field. GTK's C strings end at
    /// the first NUL and glib panics rather than truncate when handed one
    /// mid-string — a single message with a stray 0x00 in its envelope (they
    /// exist in the wild) took the whole message list down with it. Called at
    /// the ingestion choke points (envelope parse, cache load), so nothing
    /// NUL-bearing ever reaches a label, tooltip, or document.
    pub(crate) fn scrub_nuls(&mut self) {
        for s in [
            &mut self.from_name,
            &mut self.from_addr,
            &mut self.reply_to,
            &mut self.to,
            &mut self.cc,
            &mut self.bcc,
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
    pub(crate) fn datetime_full(&self) -> String {
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
    pub(crate) fn datetime_list(&self) -> String {
        datetime_list_at(self.timestamp, &self.date)
    }
}

/// One pill on a message: a tag (#71), or a Gmail label no tag stands for
/// (#374), by its display name.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Pill<'a> {
    Tag(&'a crate::config::Tag),
    Label(String),
}

/// The tags a message carries, in the order Settings lists them, then the
/// Gmail labels none of them stands for, in the server's order. A label
/// named like a tag (by keyword or by name) shows as that tag, once: on
/// Gmail, which keeps no keywords, a label is how such a tag arrives.
pub(crate) fn pills<'a>(
    keywords: &[String],
    labels: &[String],
    tags: &'a [crate::config::Tag],
) -> Vec<Pill<'a>> {
    // A decoded name can hold a NUL (`&AAA-`), which a GTK label aborts on.
    let shown: Vec<String> = labels
        .iter()
        .map(|l| crate::mail::mutf7::decode(l).replace('\0', " "))
        .collect();
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

/// Which of the tag palette's colors a Gmail label no tag stands for wears
/// (#374): worked out from its name, so a label has the same color on every
/// row, in the reader and from one run to the next. A tag named like the
/// label is how a user picks the color instead.
pub(crate) fn label_hue(name: &str) -> usize {
    // FNV-1a: std's hasher makes no promise to stay the same across releases.
    let hash = name.to_lowercase().bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ b as u32).wrapping_mul(0x0100_0193)
    });
    hash as usize % crate::config::TAG_COLORS.len()
}

/// Whether a Gmail label (as the server writes it, modified UTF-7) stands for
/// a tag: named like its keyword, or like its name.
fn label_names_tag(label: &str, tag: &crate::config::Tag) -> bool {
    tag.keyword.eq_ignore_ascii_case(label)
        || tag
            .name
            .eq_ignore_ascii_case(&crate::mail::mutf7::decode(label))
}

/// A short date for a single line: the time for today, the day and month
/// this year, the year too before that. Used where the full list date would
/// crowd out the text: a folded card (#326), a one-line list row (#334).
pub(crate) fn date_short(timestamp: i64, date: &str) -> String {
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
pub(crate) fn datetime_list_at(timestamp: i64, date: &str) -> String {
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
pub(crate) struct ThreadLatest {
    pub(crate) from_name: String,
    pub(crate) from_addr: String,
    pub(crate) preview: String,
    /// Unix seconds, for deciding whether this really is newer than what the
    /// folder itself holds.
    pub(crate) timestamp: i64,
    /// The Date header as it was displayed, for a message with no usable
    /// timestamp.
    pub(crate) date: String,
}

/// What the list cannot work out about a conversation on its own: how big it
/// really is (#222) and which message moved it last (#236). Both are answered
/// in one pass of the cache, for a whole page of rows at a time.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ThreadSummary {
    pub(crate) count: usize,
    pub(crate) latest: Option<ThreadLatest>,
    /// The conversation's messages across the account, drafts, Trash and
    /// Junk left out, for a row that opens out into the parts filed in other
    /// folders (#309). Empty for a conversation of one.
    pub(crate) members: Vec<Message>,
}

/// Every Message-ID that identifies a conversation: the messages' own ids plus
/// the ones they reference. This is what the cache is searched by to find the
/// parts of the thread filed in other folders — both to assemble a conversation
/// the reader has opened and to count one the list is only showing a slice of.
pub(crate) fn thread_ids<'a>(msgs: impl IntoIterator<Item = &'a Message>) -> Vec<String> {
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
