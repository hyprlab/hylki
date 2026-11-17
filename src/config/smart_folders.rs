//! Smart folders (#417): saved searches over the cached mail, and
//! smart_folders.toml.

use std::path::PathBuf;

use super::filters::{FilterCondition, FilterField, FilterInput, FilterMatch};
use super::{config_file, is_false, save_private_toml};

/// A smart folder (#417): a saved search over the mail already on this
/// computer, listed in the sidebar's unified section. It looks in one
/// account or every account in the unified section, in one kind of folder
/// or every folder but Trash and Junk, and keeps the messages its
/// conditions match, all of them or any one.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SmartFolder {
    /// A name of its own for the sidebar to find it by, kept when renamed.
    pub(crate) id: String,
    pub(crate) name: String,
    /// The account it looks in; empty for every account in the unified
    /// section.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) account_email: String,
    /// The kind of folder it looks in; none for every folder but Trash and
    /// Junk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) folder: Option<SmartFolderKind>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) any: bool,
    #[serde(default)]
    pub(crate) conditions: Vec<SmartCondition>,
}

/// The kinds of folder a smart folder can be held to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SmartFolderKind {
    Inbox,
    Sent,
    Drafts,
    Archive,
    Junk,
    Trash,
}

impl SmartFolderKind {
    /// Every kind, in the order the editor lists them.
    pub(crate) const ALL: [SmartFolderKind; 6] = [
        SmartFolderKind::Inbox,
        SmartFolderKind::Sent,
        SmartFolderKind::Drafts,
        SmartFolderKind::Archive,
        SmartFolderKind::Junk,
        SmartFolderKind::Trash,
    ];

    pub(crate) fn kind(self) -> crate::models::FolderKind {
        use crate::models::FolderKind;
        match self {
            SmartFolderKind::Inbox => FolderKind::Inbox,
            SmartFolderKind::Sent => FolderKind::Sent,
            SmartFolderKind::Drafts => FolderKind::Drafts,
            SmartFolderKind::Archive => FolderKind::Archive,
            SmartFolderKind::Junk => FolderKind::Junk,
            SmartFolderKind::Trash => FolderKind::Trash,
        }
    }
}

/// One condition of a [`SmartFolder`]: the filter conditions, which look at
/// text, and the message's own state, which a filter has no use for.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SmartCondition {
    pub(crate) field: SmartField,
    #[serde(default = "contains_match")]
    pub(crate) matcher: FilterMatch,
    /// The text to match, commas separating alternatives, or for
    /// [`SmartField::Tagged`] the tag's keyword (empty for any tag). For
    /// [`SmartField::Label`], empty means any label.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) value: String,
    /// The condition holds when the rest of it does not (#639): "does not
    /// contain", "not tagged", "no attachments". The filter matchers stay
    /// as they are: a filter has no use for a negation, and its server-side
    /// body search has none to offer.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) not: bool,
}

fn contains_match() -> FilterMatch {
    FilterMatch::Contains
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SmartField {
    #[default]
    FromAddress,
    FromName,
    Subject,
    Recipients,
    ReplyTo,
    /// The list preview: a smart folder reads only what is on this computer.
    Body,
    FromInContacts,
    FromNotInContacts,
    Starred,
    NotStarred,
    Unread,
    Read,
    Attachment,
    Tagged,
    /// A Gmail label (#639), by its name; any label when the text is empty.
    Label,
}

impl SmartField {
    /// What the editor's Where row offers, in its order: each field, and
    /// the negation of the ones with no opposite of their own. The text
    /// fields are negated by their matcher instead.
    pub(crate) const CHOICES: [(SmartField, bool); 17] = [
        (SmartField::Starred, false),
        (SmartField::NotStarred, false),
        (SmartField::Unread, false),
        (SmartField::Read, false),
        (SmartField::Attachment, false),
        (SmartField::Attachment, true),
        (SmartField::Tagged, false),
        (SmartField::Tagged, true),
        (SmartField::Label, false),
        (SmartField::FromAddress, false),
        (SmartField::FromName, false),
        (SmartField::Subject, false),
        (SmartField::Recipients, false),
        (SmartField::ReplyTo, false),
        (SmartField::Body, false),
        (SmartField::FromInContacts, false),
        (SmartField::FromNotInContacts, false),
    ];

    /// The filter field the condition matches through, for the ones that
    /// look at text or the address books.
    pub(crate) fn filter_field(self) -> Option<FilterField> {
        Some(match self {
            SmartField::FromAddress => FilterField::FromAddress,
            SmartField::FromName => FilterField::FromName,
            SmartField::Subject => FilterField::Subject,
            SmartField::Recipients => FilterField::Recipients,
            SmartField::ReplyTo => FilterField::ReplyTo,
            SmartField::Body => FilterField::Body,
            SmartField::FromInContacts => FilterField::FromInContacts,
            SmartField::FromNotInContacts => FilterField::FromNotInContacts,
            _ => return None,
        })
    }

    /// Whether the field takes text to match.
    pub(crate) fn takes_text(self) -> bool {
        self == SmartField::Label || self.filter_field().is_some_and(|f| !f.is_contact_check())
    }

    /// Whether the editor's Where row says the condition's negation itself,
    /// as "Not tagged" does, rather than its matcher.
    pub(crate) fn negated_by_choice(self) -> bool {
        matches!(self, SmartField::Attachment | SmartField::Tagged)
    }
}

/// What a smart folder's conditions look at in one message.
pub(crate) struct SmartInput<'a> {
    pub(crate) text: FilterInput<'a>,
    pub(crate) starred: bool,
    pub(crate) unread: bool,
    pub(crate) attachment: bool,
    pub(crate) keywords: &'a [String],
    /// The keywords of the tags there are, lower case: what "any tag" means,
    /// since a server sets keywords of its own ($Forwarded, $Junk) too.
    pub(crate) tags: &'a [String],
    /// The Gmail labels on the message as the server names them, the one
    /// of the folder this copy sits in included; empty elsewhere.
    pub(crate) labels: &'a [String],
}

impl SmartCondition {
    /// Whether the message meets the condition, its negation applied. One
    /// that cannot be judged, text with nothing to match or the address
    /// books unread, matches nothing either way: "not in Contacts" must
    /// not take in every sender because the books failed to load.
    pub(crate) fn matches(&self, input: &SmartInput) -> bool {
        self.holds(input).is_some_and(|hit| hit != self.not)
    }

    fn holds(&self, input: &SmartInput) -> Option<bool> {
        Some(match self.field {
            SmartField::Starred => input.starred,
            SmartField::NotStarred => !input.starred,
            SmartField::Unread => input.unread,
            SmartField::Read => !input.unread,
            SmartField::Attachment => input.attachment,
            SmartField::Tagged if self.value.is_empty() => input
                .keywords
                .iter()
                .any(|k| input.tags.contains(&k.to_lowercase())),
            SmartField::Tagged => input
                .keywords
                .iter()
                .any(|k| k.eq_ignore_ascii_case(&self.value)),
            SmartField::Label => {
                let names: Vec<String> = input
                    .labels
                    .iter()
                    .map(|l| crate::mail::mutf7::decode(l).to_lowercase())
                    .collect();
                let alts = FilterCondition::alternatives(&self.value);
                if alts.is_empty() {
                    !names.is_empty()
                } else {
                    alts.iter()
                        .any(|a| names.iter().any(|n| self.matcher.test(n, a)))
                }
            }
            field => {
                let field = field.filter_field()?;
                if field.is_contact_check() {
                    input.text.contacts?;
                } else if FilterCondition::alternatives(&self.value).is_empty() {
                    return None;
                }
                FilterCondition {
                    field,
                    matcher: self.matcher,
                    value: self.value.clone(),
                }
                .matches(&input.text)
            }
        })
    }
}

/// What the cache can rule out before a message is read: the state the
/// conditions require when every one of them must hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SmartPrefilter {
    pub(crate) starred: Option<bool>,
    pub(crate) unread: Option<bool>,
    pub(crate) attachment: Option<bool>,
}

impl SmartFolder {
    /// Every condition holds, or with `any` one does. A folder with no
    /// conditions keeps everything it looks at.
    pub(crate) fn matches(&self, input: &SmartInput) -> bool {
        if self.conditions.is_empty() {
            return true;
        }
        if self.any {
            self.conditions.iter().any(|c| c.matches(input))
        } else {
            self.conditions.iter().all(|c| c.matches(input))
        }
    }

    pub(crate) fn prefilter(&self) -> SmartPrefilter {
        let mut pre = SmartPrefilter::default();
        if self.any {
            return pre;
        }
        for c in &self.conditions {
            let yes = !c.not;
            match c.field {
                SmartField::Starred => pre.starred = Some(yes),
                SmartField::NotStarred => pre.starred = Some(!yes),
                SmartField::Unread => pre.unread = Some(yes),
                SmartField::Read => pre.unread = Some(!yes),
                SmartField::Attachment => pre.attachment = Some(yes),
                _ => {}
            }
        }
        pre
    }

    /// Whether a condition asks about the address books, which are read
    /// only when one does.
    pub(crate) fn needs_contacts(&self) -> bool {
        self.conditions.iter().any(|c| {
            matches!(
                c.field,
                SmartField::FromInContacts | SmartField::FromNotInContacts
            )
        })
    }
}

/// A new smart folder's id: random, so a folder renamed or recreated under
/// an old name is never taken for another.
pub(crate) fn new_smart_folder_id() -> String {
    crate::rng::token(12).unwrap_or_else(|_| {
        format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        )
    })
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) struct SmartFoldersFile {
    #[serde(default)]
    pub(super) folders: Vec<SmartFolder>,
}

fn smart_folders_path() -> Option<PathBuf> {
    config_file("smart_folders.toml")
}

pub(crate) fn load_smart_folders() -> Vec<SmartFolder> {
    let Some(path) = smart_folders_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    toml::from_str::<SmartFoldersFile>(&text)
        .map(|f| f.folders)
        .unwrap_or_else(|e| {
            tracing::warn!("could not read smart folders: {e}");
            Vec::new()
        })
}

pub(crate) fn save_smart_folders(folders: &[SmartFolder]) {
    let file = SmartFoldersFile {
        folders: folders.to_vec(),
    };
    save_private_toml(smart_folders_path(), &file, "smart folders", false);
}
