//! What AI assistants may reach (`crate::mcp`). Nothing is allowed until the
//! user allows it: an account not listed here is not shared at all.
//!
//! Sending, erasing mail for good and encrypted mail are not here because
//! no setting can allow them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::models::FolderKind;

/// One thing an assistant may do in an account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AssistantCap {
    /// Senders, subjects, dates, previews and folders.
    List,
    /// The text of a message.
    Read,
    /// The files on a message.
    Attachments,
    /// Open a composer on a message for the user to check and send.
    Draft,
    MarkRead,
    Star,
    Tag,
    /// Move to a folder, or archive.
    Move,
    /// Move to Trash, which Undo brings back.
    Trash,
}

impl AssistantCap {
    /// Every capability, in the order Settings shows them.
    pub(crate) const ALL: [AssistantCap; 9] = [
        AssistantCap::List,
        AssistantCap::Read,
        AssistantCap::Attachments,
        AssistantCap::Draft,
        AssistantCap::MarkRead,
        AssistantCap::Star,
        AssistantCap::Tag,
        AssistantCap::Move,
        AssistantCap::Trash,
    ];

    /// The name an assistant is told it by.
    pub(crate) fn name(self) -> &'static str {
        match self {
            AssistantCap::List => "see message lists",
            AssistantCap::Read => "read message text",
            AssistantCap::Attachments => "read attachments",
            AssistantCap::Draft => "prepare messages",
            AssistantCap::MarkRead => "mark mail read or unread",
            AssistantCap::Star => "add and remove stars",
            AssistantCap::Tag => "add and remove tags",
            AssistantCap::Move => "move and archive mail",
            AssistantCap::Trash => "move mail to Trash",
        }
    }
}

/// What assistants may do in one account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AccountGrant {
    /// The account is shared. Off keeps the choices below for when it is on.
    pub(crate) on: bool,
    pub(crate) list: bool,
    pub(crate) read: bool,
    pub(crate) attachments: bool,
    pub(crate) draft: bool,
    pub(crate) mark_read: bool,
    pub(crate) star: bool,
    pub(crate) tag: bool,
    pub(crate) move_mail: bool,
    pub(crate) trash: bool,
    /// Folders set apart from the default of [`Self::folder_allowed`], by path.
    pub(crate) folders: BTreeMap<String, bool>,
}

impl Default for AccountGrant {
    /// What an account starts with once the user shares it: looking and
    /// drafting. Anything that changes mail is asked for separately.
    fn default() -> Self {
        Self {
            on: false,
            list: true,
            read: true,
            attachments: false,
            draft: true,
            mark_read: false,
            star: false,
            tag: false,
            move_mail: false,
            trash: false,
            folders: BTreeMap::new(),
        }
    }
}

impl AccountGrant {
    /// The switch itself, whatever others depend on it.
    pub(crate) fn switch(&self, cap: AssistantCap) -> bool {
        match cap {
            AssistantCap::List => self.list,
            AssistantCap::Read => self.read,
            AssistantCap::Attachments => self.attachments,
            AssistantCap::Draft => self.draft,
            AssistantCap::MarkRead => self.mark_read,
            AssistantCap::Star => self.star,
            AssistantCap::Tag => self.tag,
            AssistantCap::Move => self.move_mail,
            AssistantCap::Trash => self.trash,
        }
    }

    pub(crate) fn set_switch(&mut self, cap: AssistantCap, value: bool) {
        match cap {
            AssistantCap::List => self.list = value,
            AssistantCap::Read => self.read = value,
            AssistantCap::Attachments => self.attachments = value,
            AssistantCap::Draft => self.draft = value,
            AssistantCap::MarkRead => self.mark_read = value,
            AssistantCap::Star => self.star = value,
            AssistantCap::Tag => self.tag = value,
            AssistantCap::Move => self.move_mail = value,
            AssistantCap::Trash => self.trash = value,
        }
    }

    /// Whether `cap` is allowed. A message is only ever named by what a
    /// list handed out, so everything done to one needs the list too: an
    /// id written by hand must not get further than a listed one. A new
    /// message, which names none, is checked by [`Self::may_compose`].
    pub(crate) fn allows(&self, cap: AssistantCap) -> bool {
        self.on && self.list && self.switch(cap)
    }

    /// Whether a new message may be started from the account: unlike a
    /// reply, it needs no message to have been listed.
    pub(crate) fn may_compose(&self) -> bool {
        self.on && self.draft
    }

    /// Whether assistants may see into a folder. Junk and Trash stay shut
    /// until asked for: mail in them is the likeliest to carry instructions
    /// aimed at an assistant.
    pub(crate) fn folder_allowed(&self, path: &str, kind: FolderKind) -> bool {
        self.folders
            .get(path)
            .copied()
            .unwrap_or(!matches!(kind, FolderKind::Junk | FolderKind::Trash))
    }
}

/// Everything Settings → MCP decides.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AssistantPolicy {
    /// Assistants can connect at all.
    pub(crate) enabled: bool,
    /// Address books and directories may be searched.
    pub(crate) address_books: bool,
    /// People found in the mail may be searched: they come from every
    /// account, shared or not, so this is its own choice.
    pub(crate) mail_people: bool,
    /// By address, lower case.
    pub(crate) accounts: BTreeMap<String, AccountGrant>,
}

impl AssistantPolicy {
    /// The account's grant while it is shared; `None` otherwise.
    pub(crate) fn grant(&self, address: &str) -> Option<&AccountGrant> {
        self.accounts.get(&address.to_lowercase()).filter(|g| g.on)
    }

    /// The account's grant to change, made when it has none.
    pub(crate) fn grant_mut(&mut self, address: &str) -> &mut AccountGrant {
        self.accounts.entry(address.to_lowercase()).or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_shared_until_it_is_allowed() {
        let policy = AssistantPolicy::default();
        assert!(!policy.enabled);
        assert!(policy.grant("ada@example.org").is_none());
        assert!(!policy.address_books && !policy.mail_people);
    }

    #[test]
    fn an_account_starts_with_looking_and_drafting_only() {
        let mut policy = AssistantPolicy::default();
        policy.grant_mut("Ada@Example.org").on = true;
        let grant = policy.grant("ada@example.ORG").unwrap();
        for cap in [AssistantCap::List, AssistantCap::Read, AssistantCap::Draft] {
            assert!(grant.allows(cap), "{cap:?}");
        }
        for cap in [
            AssistantCap::Attachments,
            AssistantCap::MarkRead,
            AssistantCap::Star,
            AssistantCap::Tag,
            AssistantCap::Move,
            AssistantCap::Trash,
        ] {
            assert!(!grant.allows(cap), "{cap:?}");
        }
    }

    #[test]
    fn turning_an_account_off_refuses_everything_and_keeps_its_choices() {
        let mut policy = AssistantPolicy::default();
        let grant = policy.grant_mut("ada@example.org");
        grant.on = true;
        grant.trash = true;
        grant.on = false;
        assert!(policy.grant("ada@example.org").is_none());
        let grant = policy.grant_mut("ada@example.org");
        assert!(AssistantCap::ALL.iter().all(|c| !grant.allows(*c)));
        grant.on = true;
        assert!(grant.allows(AssistantCap::Trash));
    }

    #[test]
    fn what_is_done_to_a_message_needs_the_list_that_names_it() {
        let mut grant = AccountGrant {
            on: true,
            trash: true,
            ..Default::default()
        };
        grant.list = false;
        assert!(AssistantCap::ALL.iter().all(|c| !grant.allows(*c)));
        // A new message names none.
        assert!(grant.may_compose());
    }

    #[test]
    fn every_switch_sets_its_own_capability() {
        for cap in AssistantCap::ALL {
            let mut grant = AccountGrant {
                on: true,
                ..Default::default()
            };
            for other in AssistantCap::ALL {
                grant.set_switch(other, false);
            }
            grant.set_switch(AssistantCap::List, true);
            grant.set_switch(cap, true);
            for other in AssistantCap::ALL {
                assert_eq!(
                    grant.allows(other),
                    other == cap || other == AssistantCap::List,
                    "{cap:?} / {other:?}"
                );
            }
        }
    }

    #[test]
    fn junk_and_trash_stay_shut_until_asked_for() {
        let mut grant = AccountGrant::default();
        assert!(grant.folder_allowed("INBOX", FolderKind::Inbox));
        assert!(grant.folder_allowed("Receipts", FolderKind::Custom));
        assert!(!grant.folder_allowed("Spam", FolderKind::Junk));
        assert!(!grant.folder_allowed("Bin", FolderKind::Trash));
        grant.folders.insert("Spam".into(), true);
        grant.folders.insert("Receipts".into(), false);
        assert!(grant.folder_allowed("Spam", FolderKind::Junk));
        assert!(!grant.folder_allowed("Receipts", FolderKind::Custom));
    }

    #[test]
    fn a_policy_survives_the_settings_file() {
        let mut policy = AssistantPolicy {
            enabled: true,
            mail_people: true,
            ..Default::default()
        };
        let grant = policy.grant_mut("ada@example.org");
        grant.on = true;
        grant.star = true;
        grant.folders.insert("Work/Secret".into(), false);
        let text = toml::to_string_pretty(&policy).unwrap();
        assert_eq!(toml::from_str::<AssistantPolicy>(&text).unwrap(), policy);
        // A file from before a capability existed reads as not allowing it.
        let old: AssistantPolicy =
            toml::from_str("enabled = true\n[accounts.\"ada@example.org\"]\non = true\n").unwrap();
        assert!(!old
            .grant("ada@example.org")
            .unwrap()
            .allows(AssistantCap::Trash));
    }
}
