//! How an account's folders nest and how a typed name becomes a path, as
//! the account's server or service reports it (#554). Every path the app
//! makes (a new folder, a rename, a move, a standard folder made on first
//! use) is built from this, never from a look at the paths themselves.

use crate::i18n::{i18n, i18n_f};

use super::{Folder, FolderKind};

/// The character that stands for a '/' inside a folder's own name in the
/// path of a JMAP or Microsoft 365 account, whose names may hold one: the
/// division slash, which looks the same.
pub(crate) const SLASH_STAND_IN: char = '∕';

/// How a name typed for a folder becomes the last level of its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NameStyle {
    /// IMAP: modified UTF-7 (RFC 3501 §5.1.3), and the server's delimiter
    /// can't be part of a name.
    Mutf7,
    /// UTF-8, the delimiter refused (folders on this computer).
    Utf8,
    /// UTF-8, a delimiter in the name written as this character in the
    /// path (JMAP and Microsoft 365, where a name is free to hold one and
    /// the path is only the app's).
    Utf8StandIn(char),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FolderHierarchy {
    /// The character between the levels of a path, or `None` where folders
    /// don't nest at all (an IMAP server whose LIST answers NIL, POP3).
    pub(crate) delimiter: Option<char>,
    /// What a top-level folder's path starts with: the personal namespace,
    /// "INBOX." on Courier and Cyrus, "" on most servers.
    pub(crate) prefix: String,
    pub(crate) names: NameStyle,
    /// Whether a folder can be made at the top level (no on POP3, nor on a
    /// JMAP account that says so).
    pub(crate) top_level: bool,
    /// Whether the server said what `prefix` is. An IMAP server without
    /// NAMESPACE can't (RFC 3501 has no way to ask): its first refusal of a
    /// folder at the top tells, and the worker learns from it.
    pub(crate) prefix_said: bool,
}

impl FolderHierarchy {
    /// An IMAP account's, from its NAMESPACE or LIST answers. `prefix` is
    /// `None` when the server has no personal namespace, so no folder of
    /// the user's own can be made at the top.
    pub(crate) fn imap(delimiter: Option<char>, prefix: Option<String>) -> Self {
        Self {
            delimiter,
            top_level: prefix.is_some(),
            prefix: prefix.unwrap_or_default(),
            names: NameStyle::Mutf7,
            prefix_said: true,
        }
    }

    /// The same, its prefix not said by the server.
    pub(crate) fn unsaid(self) -> Self {
        Self {
            prefix_said: false,
            ..self
        }
    }

    /// A JMAP or Microsoft 365 account's: paths are the app's own, '/'
    /// between the names.
    pub(crate) fn slashed(top_level: bool) -> Self {
        Self {
            delimiter: Some('/'),
            prefix: String::new(),
            names: NameStyle::Utf8StandIn(SLASH_STAND_IN),
            top_level,
            prefix_said: true,
        }
    }

    /// On This Computer's (#552) and the demo's: '/' between levels, which
    /// no name can hold.
    pub(crate) fn maildir() -> Self {
        Self {
            delimiter: Some('/'),
            prefix: String::new(),
            names: NameStyle::Utf8,
            top_level: true,
            prefix_said: true,
        }
    }

    /// POP3's: the Inbox and nothing else.
    pub(crate) fn inbox_only() -> Self {
        Self {
            delimiter: None,
            prefix: String::new(),
            names: NameStyle::Utf8,
            top_level: false,
            prefix_said: true,
        }
    }

    /// The tree as the paths suggest it, for drawing an account whose
    /// server hasn't said yet: an IMAP account cached before its hierarchy
    /// was, until it connects. Never used to make a path.
    pub(crate) fn inferred(folders: &[Folder]) -> Self {
        let delimiter = ['/', '.', '\\']
            .into_iter()
            .find(|d| folders.iter().any(|f| f.path.contains(*d)))
            .unwrap_or('/');
        let root = format!("INBOX{delimiter}");
        let customs: Vec<&Folder> = folders
            .iter()
            .filter(|f| f.kind == FolderKind::Custom)
            .collect();
        let under_inbox = |f: &&Folder| {
            f.path
                .get(..root.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(&root))
        };
        let prefix = if !customs.is_empty() && customs.iter().all(under_inbox) {
            root
        } else {
            String::new()
        };
        Self::imap(Some(delimiter), Some(prefix)).unsaid()
    }

    /// Whether a folder can be made inside another at all.
    pub(crate) fn nests(&self) -> bool {
        self.delimiter.is_some()
    }

    /// Whether `child` is somewhere below `parent`.
    pub(crate) fn is_under(&self, child: &str, parent: &str) -> bool {
        let Some(d) = self.delimiter else {
            return false;
        };
        child
            .strip_prefix(parent)
            .and_then(|rest| rest.strip_prefix(d))
            .is_some_and(|rest| !rest.is_empty())
    }

    /// A folder's parent on the server, "" for the top level (inside the
    /// namespace, so "INBOX.Clients" is at the top when the prefix is
    /// "INBOX.").
    pub(crate) fn parent(&self, path: &str) -> String {
        let Some(d) = self.delimiter else {
            return String::new();
        };
        let Some((head, _)) = path.rsplit_once(d) else {
            return String::new();
        };
        if self.prefix.strip_suffix(d) == Some(head) {
            String::new()
        } else {
            head.to_string()
        }
    }

    /// The last level of a path, as the server spells it.
    pub(crate) fn leaf<'a>(&self, path: &'a str) -> &'a str {
        match self.delimiter.and_then(|d| path.rsplit_once(d)) {
            Some((_, leaf)) => leaf,
            None => path.strip_prefix(&self.prefix).unwrap_or(path),
        }
    }

    /// The path of a folder named `leaf` (as [`Self::leaf_for`] makes it)
    /// inside `parent`, "" being the top level. `None` where that can't be:
    /// inside a folder when folders don't nest, or at the top where the
    /// account allows none.
    pub(crate) fn child(&self, parent: &str, leaf: &str) -> Option<String> {
        if parent.is_empty() {
            return self.top_level.then(|| format!("{}{leaf}", self.prefix));
        }
        self.delimiter.map(|d| format!("{parent}{d}{leaf}"))
    }

    /// A name typed for a folder as the last level of its path, or why it
    /// can't be one.
    pub(crate) fn leaf_for(&self, typed: &str) -> Result<String, String> {
        let name = typed.trim();
        if name.is_empty() {
            return Err(i18n("A folder needs a name."));
        }
        match (self.names, self.delimiter) {
            (NameStyle::Utf8StandIn(stand_in), Some(d)) => {
                return Ok(name.replace(d, &stand_in.to_string()))
            }
            (_, Some(d)) if name.contains(d) => {
                return Err(i18n_f(
                    "A folder name can't contain “{c}” on this account.",
                    &[("c", &d.to_string())],
                ))
            }
            _ => {}
        }
        Ok(match self.names {
            NameStyle::Mutf7 => crate::mail::mutf7::encode(name),
            NameStyle::Utf8 | NameStyle::Utf8StandIn(_) => name.to_string(),
        })
    }

    /// The name a path's last level is shown by.
    pub(crate) fn name_of(&self, leaf: &str) -> String {
        match self.names {
            NameStyle::Mutf7 => crate::mail::mutf7::decode(leaf),
            NameStyle::Utf8 | NameStyle::Utf8StandIn(_) => leaf.to_string(),
        }
    }
}

/// A name out of a JMAP or Microsoft 365 path level, as the service
/// spells it: the stand-in back to the slash it stands for.
pub(crate) fn slashed_name(leaf: &str) -> String {
    leaf.replace(SLASH_STAND_IN, "/")
}

/// The other way: a name from the service as one level of a path.
pub(crate) fn slashed_leaf(name: &str) -> String {
    name.replace('/', &SLASH_STAND_IN.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imap(d: char, prefix: &str) -> FolderHierarchy {
        FolderHierarchy::imap(Some(d), Some(prefix.to_string()))
    }

    #[test]
    fn paths_are_built_with_the_servers_own_delimiter_and_prefix() {
        let courier = imap('.', "INBOX.");
        assert_eq!(courier.child("", "Work").as_deref(), Some("INBOX.Work"));
        assert_eq!(
            courier.child("INBOX.Work", "Q1").as_deref(),
            Some("INBOX.Work.Q1")
        );
        let dovecot = imap('.', "");
        assert_eq!(dovecot.child("Work", "Q1").as_deref(), Some("Work.Q1"));
        let exchange = imap('/', "");
        assert_eq!(exchange.child("Work", "Q1").as_deref(), Some("Work/Q1"));
        let backslash = imap('\\', "");
        assert_eq!(backslash.child("Work", "Q1").as_deref(), Some("Work\\Q1"));
    }

    #[test]
    fn a_flat_server_makes_no_folder_inside_another() {
        let flat = FolderHierarchy::imap(None, Some(String::new()));
        assert!(!flat.nests());
        assert_eq!(flat.child("Work", "Q1"), None);
        assert_eq!(flat.child("", "Q1").as_deref(), Some("Q1"));
        assert!(!flat.is_under("Work.Q1", "Work"));
        assert_eq!(flat.parent("a/b.c"), "");
        assert_eq!(flat.leaf("a/b.c"), "a/b.c");
    }

    #[test]
    fn no_personal_namespace_or_pop3_makes_nothing_at_the_top() {
        let shared_only = FolderHierarchy::imap(Some('/'), None);
        assert_eq!(shared_only.child("", "Work"), None);
        assert_eq!(shared_only.child("Work", "Q1").as_deref(), Some("Work/Q1"));
        let pop3 = FolderHierarchy::inbox_only();
        assert_eq!(pop3.child("", "Work"), None);
        assert_eq!(pop3.child("INBOX", "Work"), None);
    }

    #[test]
    fn levels_split_only_on_the_real_delimiter() {
        let slash = imap('/', "");
        assert!(slash.is_under("v1/2", "v1"));
        assert!(!slash.is_under("v1.2", "v1"));
        assert!(!slash.is_under("v12", "v1"));
        assert!(!slash.is_under("v1/", "v1"));
        assert_eq!(slash.leaf("Work/v1.2"), "v1.2");
        assert_eq!(slash.parent("Work/v1.2"), "Work");
        let dot = imap('.', "INBOX.");
        assert!(dot.is_under("INBOX.a.b", "INBOX.a"));
        assert!(!dot.is_under("INBOX.a/b", "INBOX.a"));
        assert_eq!(dot.parent("INBOX.a"), "");
        assert_eq!(dot.parent("INBOX.a.b"), "INBOX.a");
        assert_eq!(dot.leaf("INBOX.a"), "a");
    }

    #[test]
    fn names_are_encoded_the_way_each_kind_of_account_needs() {
        let slash = imap('/', "");
        assert_eq!(slash.leaf_for("  Café ").unwrap(), "Caf&AOk-");
        assert_eq!(slash.name_of("Caf&AOk-"), "Café");
        assert!(slash.leaf_for("a/b").is_err());
        assert_eq!(imap('.', "").leaf_for("v1/2").unwrap(), "v1/2");
        assert!(slash.leaf_for("   ").is_err());

        let jmap = FolderHierarchy::slashed(true);
        assert_eq!(jmap.leaf_for("Café").unwrap(), "Café");
        assert_eq!(jmap.leaf_for("a/b").unwrap(), "a∕b");
        assert_eq!(jmap.name_of("a&b"), "a&b");
        assert_eq!(slashed_name("a∕b"), "a/b");
        assert_eq!(slashed_leaf("a/b"), "a∕b");

        let local = FolderHierarchy::maildir();
        assert_eq!(local.leaf_for("Café").unwrap(), "Café");
        assert!(local.leaf_for("a/b").is_err());
    }

    #[test]
    fn the_drawing_fallback_reads_the_paths() {
        let folder = |path: &str, kind| Folder {
            id: 0,
            account_id: 1,
            name: path.into(),
            path: path.into(),
            kind,
            unread: 0,
            childless: false,
        };
        let h = FolderHierarchy::inferred(&[
            folder("INBOX", FolderKind::Inbox),
            folder("INBOX.a", FolderKind::Custom),
        ]);
        assert_eq!((h.delimiter, h.prefix.as_str()), (Some('.'), "INBOX."));
        let h = FolderHierarchy::inferred(&[
            folder("v1.2", FolderKind::Custom),
            folder("a/b", FolderKind::Custom),
        ]);
        assert_eq!((h.delimiter, h.prefix.as_str()), (Some('/'), ""));
    }
}
