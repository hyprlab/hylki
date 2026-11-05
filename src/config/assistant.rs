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
    /// Search on the mail server, past what is downloaded.
    SearchServer,
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
    /// See the conversation the user has open now.
    OpenConversation,
    /// Read and suggest changes to the open drafts an assistant prepared.
    OpenDrafts,
    /// The same for the drafts the user is writing, unsaved text included.
    OwnDrafts,
}

impl AssistantCap {
    /// Every capability, in the order Settings shows them.
    pub(crate) const ALL: [AssistantCap; 13] = [
        AssistantCap::List,
        AssistantCap::Read,
        AssistantCap::SearchServer,
        AssistantCap::Attachments,
        AssistantCap::Draft,
        AssistantCap::MarkRead,
        AssistantCap::Star,
        AssistantCap::Tag,
        AssistantCap::Move,
        AssistantCap::Trash,
        AssistantCap::OpenConversation,
        AssistantCap::OpenDrafts,
        AssistantCap::OwnDrafts,
    ];

    /// Whether full access covers it. These are about what the user is doing
    /// right now rather than the mail in the account, so they stay asked for
    /// one by one.
    pub(crate) fn in_full_access(self) -> bool {
        !matches!(
            self,
            AssistantCap::OpenConversation | AssistantCap::OpenDrafts | AssistantCap::OwnDrafts
        )
    }

    /// The name an assistant is told it by.
    pub(crate) fn name(self) -> &'static str {
        match self {
            AssistantCap::List => "see message lists",
            AssistantCap::Read => "read message text",
            AssistantCap::SearchServer => "search on the mail server",
            AssistantCap::Attachments => "read attachments",
            AssistantCap::Draft => "prepare messages",
            AssistantCap::MarkRead => "mark mail read or unread",
            AssistantCap::Star => "add and remove stars",
            AssistantCap::Tag => "add and remove tags",
            AssistantCap::Move => "move and archive mail",
            AssistantCap::Trash => "move mail to Trash",
            AssistantCap::OpenConversation => "see the conversation open in Hylki",
            AssistantCap::OpenDrafts => "read and edit the open drafts agents prepared",
            AssistantCap::OwnDrafts => "read and edit the drafts the user is writing",
        }
    }
}

/// What assistants may do in one account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AccountGrant {
    /// The account is shared. Off keeps the choices below for when it is on.
    pub(crate) on: bool,
    /// Everything is allowed, in every folder but Junk and Trash, whatever
    /// the choices below say. They are kept for when it goes off. Junk and
    /// Trash keep their own switches in `folders`.
    pub(crate) full: bool,
    pub(crate) list: bool,
    pub(crate) read: bool,
    pub(crate) search_server: bool,
    pub(crate) attachments: bool,
    pub(crate) draft: bool,
    pub(crate) mark_read: bool,
    pub(crate) star: bool,
    pub(crate) tag: bool,
    pub(crate) move_mail: bool,
    pub(crate) trash: bool,
    pub(crate) open_conversation: bool,
    pub(crate) open_drafts: bool,
    pub(crate) own_drafts: bool,
    /// Folders set apart from the default of [`Self::folder_allowed`], by path.
    pub(crate) folders: BTreeMap<String, bool>,
}

impl Default for AccountGrant {
    /// What an account starts with once the user shares it: looking and
    /// drafting. Anything that changes mail is asked for separately.
    fn default() -> Self {
        Self {
            on: false,
            full: false,
            list: true,
            read: true,
            search_server: true,
            attachments: false,
            draft: true,
            mark_read: false,
            star: false,
            tag: false,
            move_mail: false,
            trash: false,
            open_conversation: false,
            open_drafts: false,
            own_drafts: false,
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
            AssistantCap::SearchServer => self.search_server,
            AssistantCap::Attachments => self.attachments,
            AssistantCap::Draft => self.draft,
            AssistantCap::MarkRead => self.mark_read,
            AssistantCap::Star => self.star,
            AssistantCap::Tag => self.tag,
            AssistantCap::Move => self.move_mail,
            AssistantCap::Trash => self.trash,
            AssistantCap::OpenConversation => self.open_conversation,
            AssistantCap::OpenDrafts => self.open_drafts,
            AssistantCap::OwnDrafts => self.own_drafts,
        }
    }

    pub(crate) fn set_switch(&mut self, cap: AssistantCap, value: bool) {
        match cap {
            AssistantCap::List => self.list = value,
            AssistantCap::Read => self.read = value,
            AssistantCap::SearchServer => self.search_server = value,
            AssistantCap::Attachments => self.attachments = value,
            AssistantCap::Draft => self.draft = value,
            AssistantCap::MarkRead => self.mark_read = value,
            AssistantCap::Star => self.star = value,
            AssistantCap::Tag => self.tag = value,
            AssistantCap::Move => self.move_mail = value,
            AssistantCap::Trash => self.trash = value,
            AssistantCap::OpenConversation => self.open_conversation = value,
            AssistantCap::OpenDrafts => self.open_drafts = value,
            AssistantCap::OwnDrafts => self.own_drafts = value,
        }
    }

    /// Back to what an account starts with. Whether it is shared stays as
    /// it is: that is the account's own switch, not one of its permissions.
    pub(crate) fn reset(&mut self) {
        *self = Self {
            on: self.on,
            ..Self::default()
        };
    }

    /// Whether `cap` is allowed. A message is only ever named by what a
    /// list handed out, so everything done to one needs the list too: an
    /// id written by hand must not get further than a listed one. A new
    /// message, which names none, is checked by [`Self::may_compose`]. A
    /// server searches the text of messages, so what it finds says what
    /// they hold, and asking it needs leave to read them. An open draft is
    /// no message from a list, so it needs none.
    pub(crate) fn allows(&self, cap: AssistantCap) -> bool {
        if !self.on {
            return false;
        }
        match cap {
            AssistantCap::OpenConversation => {
                self.open_conversation && (self.full || (self.list && self.read))
            }
            AssistantCap::OpenDrafts => self.open_drafts,
            AssistantCap::OwnDrafts => self.open_drafts && self.own_drafts,
            _ => {
                self.full
                    || (self.list
                        && self.switch(cap)
                        && (cap != AssistantCap::SearchServer || self.read))
            }
        }
    }

    /// Whether a new message may be started from the account: unlike a
    /// reply, it needs no message to have been listed.
    pub(crate) fn may_compose(&self) -> bool {
        self.on && (self.full || self.draft)
    }

    /// Whether assistants may see into a folder. Junk and Trash stay shut
    /// until asked for one by one, even under full access: mail in them is
    /// the likeliest to carry instructions aimed at an assistant.
    pub(crate) fn folder_allowed(&self, path: &str, kind: FolderKind) -> bool {
        let open_by_default = Self::full_covers(kind);
        (self.full && open_by_default) || self.folders.get(path).copied().unwrap_or(open_by_default)
    }

    /// Whether full access decides a folder of this kind, leaving its own
    /// switch with nothing to say.
    pub(crate) fn full_covers(kind: FolderKind) -> bool {
        !matches!(kind, FolderKind::Junk | FolderKind::Trash)
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
        for cap in [
            AssistantCap::List,
            AssistantCap::Read,
            AssistantCap::SearchServer,
            AssistantCap::Draft,
        ] {
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
            // Searching on the server, and seeing what is open, need leave
            // to read as well. Their own drafts come on top of the others.
            let read_too = matches!(
                cap,
                AssistantCap::SearchServer | AssistantCap::OpenConversation
            );
            grant.read = grant.read || read_too;
            let drafts_too = cap == AssistantCap::OwnDrafts;
            grant.open_drafts = grant.open_drafts || drafts_too;
            for other in AssistantCap::ALL {
                assert_eq!(
                    grant.allows(other),
                    other == cap
                        || other == AssistantCap::List
                        || (read_too && other == AssistantCap::Read)
                        || (drafts_too && other == AssistantCap::OpenDrafts),
                    "{cap:?} / {other:?}"
                );
            }
        }
    }

    #[test]
    fn a_server_is_not_asked_what_the_text_holds_without_leave_to_read_it() {
        let mut grant = AccountGrant {
            on: true,
            ..Default::default()
        };
        assert!(grant.allows(AssistantCap::SearchServer));
        grant.read = false;
        assert!(!grant.allows(AssistantCap::SearchServer));
    }

    #[test]
    fn full_access_allows_everything_but_junk_and_trash_and_keeps_the_choices() {
        let mut grant = AccountGrant {
            on: true,
            full: true,
            ..Default::default()
        };
        for cap in AssistantCap::ALL {
            grant.set_switch(cap, false);
        }
        grant.folders.insert("Private".into(), false);
        for cap in AssistantCap::ALL {
            assert_eq!(grant.allows(cap), cap.in_full_access(), "{cap:?}");
        }
        assert!(grant.may_compose());
        assert!(grant.folder_allowed("INBOX", FolderKind::Inbox));
        assert!(grant.folder_allowed("Private", FolderKind::Custom));
        assert!(!grant.folder_allowed("Spam", FolderKind::Junk));
        assert!(!grant.folder_allowed("Bin", FolderKind::Trash));

        // Their own switches still work, and only for themselves.
        grant.folders.insert("Spam".into(), true);
        assert!(grant.folder_allowed("Spam", FolderKind::Junk));
        assert!(!grant.folder_allowed("Bin", FolderKind::Trash));
        grant.folders.remove("Spam");

        grant.full = false;
        assert!(AssistantCap::ALL.iter().all(|c| !grant.allows(*c)));
        assert!(!grant.folder_allowed("Spam", FolderKind::Junk));
        assert!(!grant.folder_allowed("Private", FolderKind::Custom));

        // An account not shared stays that way, whatever it would allow.
        grant.full = true;
        grant.on = false;
        assert!(AssistantCap::ALL.iter().all(|c| !grant.allows(*c)));
        assert!(!grant.may_compose());
    }

    #[test]
    fn what_the_user_is_doing_now_stays_its_own_choice_under_full_access() {
        let mut grant = AccountGrant {
            on: true,
            full: true,
            list: false,
            read: false,
            ..Default::default()
        };
        grant.open_conversation = true;
        assert!(grant.allows(AssistantCap::OpenConversation));
        assert!(!grant.allows(AssistantCap::OpenDrafts));
        // Their own drafts need the others' switch too.
        grant.own_drafts = true;
        assert!(!grant.allows(AssistantCap::OwnDrafts));
        grant.open_drafts = true;
        assert!(grant.allows(AssistantCap::OwnDrafts));
        // Without full access, seeing what is open needs the list and the text.
        grant.full = false;
        assert!(!grant.allows(AssistantCap::OpenConversation));
        grant.list = true;
        grant.read = true;
        assert!(grant.allows(AssistantCap::OpenConversation));
    }

    #[test]
    fn a_reset_goes_back_to_the_start_and_leaves_the_account_shared_or_not() {
        for on in [true, false] {
            let mut grant = AccountGrant {
                on,
                full: true,
                list: false,
                trash: true,
                open_conversation: true,
                open_drafts: true,
                own_drafts: true,
                ..Default::default()
            };
            grant.folders.insert("Spam".into(), true);
            grant.folders.insert("Private".into(), false);
            grant.reset();
            assert_eq!(
                grant,
                AccountGrant {
                    on,
                    ..Default::default()
                }
            );
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
        let old = old.grant("ada@example.org").unwrap();
        assert!(!old.full && !old.allows(AssistantCap::Trash));
        assert!(!old.allows(AssistantCap::OpenConversation));
        assert!(!old.allows(AssistantCap::OpenDrafts));
    }
}
