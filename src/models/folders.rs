//! Folders: their kinds, names in the user's language, the special-folder
//! roles, and the folders kept out of the sidebar.

use super::attachments::human_size;
use crate::i18n::{i18n, i18n_f};

/// The well-known role of a folder, used to pick an icon and ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FolderKind {
    Inbox,
    Starred,
    Sent,
    Drafts,
    /// Messages kept to start new ones from (#360): no server marks this
    /// role, so it is found by name or assigned under Special Folders.
    Templates,
    /// Mail put away until a set time (#527). No server marks this role
    /// either; the app keeps the times and moves the mail back.
    Snoozed,
    Archive,
    Junk,
    Trash,
    Custom,
}

impl FolderKind {
    pub(crate) fn icon(self) -> &'static str {
        match self {
            FolderKind::Inbox => "mail-inbox-symbolic",
            FolderKind::Starred => "starred-symbolic",
            FolderKind::Sent => "mail-send-symbolic",
            FolderKind::Drafts => "document-edit-symbolic",
            FolderKind::Templates => "folder-templates-symbolic",
            FolderKind::Snoozed => "alarm-symbolic",
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
pub(crate) struct KeywordFinding {
    /// The keyword as the server spells it (a Microsoft 365 category's name).
    pub(crate) keyword: String,
    /// A display name the server itself carries (Microsoft 365 categories).
    pub(crate) name: Option<String>,
    /// A color the server itself carries, `#rrggbb`.
    pub(crate) color: Option<String>,
    /// Messages carrying it, where the server could say (0 = not counted).
    pub(crate) count: usize,
    /// The folders it was seen in (display names).
    pub(crate) folders: Vec<String>,
}

/// How much of an account's mail storage the server says is in use (#298),
/// in bytes. Only servers that report a limit give one: IMAP's QUOTA
/// extension and JMAP's quotas; Microsoft 365 and POP3 do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MailboxQuota {
    pub(crate) used: u64,
    pub(crate) limit: u64,
}

impl MailboxQuota {
    /// The share in use, 0 to 1.
    pub(crate) fn fraction(&self) -> f64 {
        if self.limit == 0 {
            return 0.0;
        }
        (self.used as f64 / self.limit as f64).clamp(0.0, 1.0)
    }

    /// "2.1 GB of 15 GB used".
    pub(crate) fn used_line(&self) -> String {
        i18n_f(
            "{used} of {limit} used",
            &[
                ("used", &human_size(self.used)),
                ("limit", &human_size(self.limit)),
            ],
        )
    }

    /// "12.9 GB free (86%)".
    pub(crate) fn free_line(&self) -> String {
        let free = self.limit.saturating_sub(self.used);
        let pct = ((1.0 - self.fraction()) * 100.0).round() as u32;
        i18n_f(
            "{free} free ({pct}%)",
            &[("free", &human_size(free)), ("pct", &pct.to_string())],
        )
    }
}

/// A mail folder within an account.
#[derive(Debug, Clone)]
pub(crate) struct Folder {
    pub(crate) id: u32,
    pub(crate) account_id: u32,
    pub(crate) name: String,
    /// IMAP mailbox path (e.g. "INBOX", "INBOX.Sent"). For the mock backend
    /// this mirrors `name`.
    pub(crate) path: String,
    pub(crate) kind: FolderKind,
    /// The sidebar chip's number: unread mail, except Drafts, where it is
    /// how many drafts there are (see `worker::chip_counts_all`).
    pub(crate) unread: u32,
    /// The server makes no folder inside this one (IMAP's `\Noinferiors`,
    /// a JMAP mailbox without `mayCreateChild` or at the depth limit).
    pub(crate) childless: bool,
}

/// Show the special folders under the app's own names for them in a language
/// other than English (#251), where the server calls them by one of the
/// usual English names. The folder a server or the user named otherwise
/// keeps that name, and so does every folder in English: "Deleted Items"
/// stays "Deleted Items" rather than becoming "Trash". Only the label
/// changes; the path the server knows the folder by is left alone.
pub(crate) fn localize_special_folder_names(folders: &mut [Folder]) {
    for f in folders {
        let (label, english): (&str, &[&str]) = match f.kind {
            FolderKind::Inbox => ("Inbox", &["inbox"]),
            FolderKind::Sent => (
                "Sent",
                &["sent", "sent items", "sent mail", "sent messages"],
            ),
            FolderKind::Drafts => ("Drafts", &["drafts", "draft"]),
            FolderKind::Templates => ("Templates", &["templates", "template"]),
            FolderKind::Snoozed => ("Snoozed", &["snoozed"]),
            FolderKind::Trash => (
                "Trash",
                &["trash", "deleted items", "deleted messages", "bin"],
            ),
            FolderKind::Junk => (
                "Junk",
                &[
                    "junk",
                    "junk mail",
                    "junk e-mail",
                    "junk email",
                    "spam",
                    "bulk mail",
                ],
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
/// is the server's; `None` for a server whose folders don't nest.
pub(crate) fn folder_is_hidden(path: &str, delimiter: Option<&str>, hidden: &[String]) -> bool {
    hidden.iter().any(|h| {
        if h == path {
            return true;
        }
        let Some(rest) = path.strip_prefix(h.as_str()) else {
            return false;
        };
        delimiter.is_some_and(|d| !d.is_empty() && rest.starts_with(d))
    })
}

/// The folders an Exchange server lists over IMAP that hold no mail
/// (#239): the calendar, the address book, the task list and the rest of
/// what its own webmail keeps out of sight. Only claimed when the listing
/// looks like Exchange, which is when at least two of Calendar, Contacts,
/// Tasks and Journal sit at the top level; a lone "Notes" folder on any
/// other server is somebody's mail. Top-level paths only: a sub-folder
/// follows its parent through [`folder_is_hidden`].
pub(crate) fn exchange_non_mail_folders(folders: &[Folder]) -> Vec<String> {
    const MARKERS: [&str; 4] = ["calendar", "contacts", "tasks", "journal"];
    const HIDDEN: [&str; 9] = [
        "calendar",
        "contacts",
        "tasks",
        "journal",
        "notes",
        "outbox",
        "sync issues",
        "conversation history",
        "scheduled",
    ];
    let top: Vec<(&Folder, String)> = folders
        .iter()
        .filter(|f| !f.path.contains(['/', '\\']))
        .map(|f| (f, f.name.trim().to_lowercase()))
        .collect();
    let markers = top
        .iter()
        .filter(|(_, n)| MARKERS.contains(&n.as_str()))
        .count();
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
pub(crate) fn is_templates_name(name: &str) -> bool {
    matches!(
        name.trim().to_lowercase().as_str(),
        "templates" | "template"
    )
}

/// Whether a folder's own name makes it the Snoozed folder (#527), found
/// the way the Templates folder is.
pub(crate) fn is_snoozed_name(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("snoozed")
}

/// The [`FolderKind`] behind a Special Folders role key (#82).
fn role_kind(role: &str) -> Option<FolderKind> {
    match role {
        "sent" => Some(FolderKind::Sent),
        "drafts" => Some(FolderKind::Drafts),
        "templates" => Some(FolderKind::Templates),
        "snoozed" => Some(FolderKind::Snoozed),
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
pub(crate) fn assign_folder_roles(
    roles: &std::collections::BTreeMap<String, String>,
    folders: &mut [Folder],
) {
    for (role, path) in roles {
        let Some(kind) = role_kind(role) else {
            continue;
        };
        if !folders.iter().any(|f| &f.path == path) {
            // The assigned folder vanished server-side: leave detection alone.
            continue;
        }
        // The Inbox is never re-roled (#136): an account whose "Sent" was
        // pointed at INBOX lost its inbox — and with it its place under All
        // Inboxes, its new-mail notifications and its filters. The
        // assignment is ignored; the editor no longer offers the Inbox.
        if folders
            .iter()
            .any(|f| &f.path == path && f.kind == FolderKind::Inbox)
        {
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
