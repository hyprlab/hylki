//! Demo data: the sample mailbox the app shows with no account configured
//! (an empty `XDG_CONFIG_HOME`) and in the showcase.
//!
//! [`MockBackend`] holds the sample accounts, folders and messages; the
//! worker's offline path answers the UI's requests from it, with the same
//! protocol as a real account, so the app is fully navigable offline.

mod demo_cache;
mod demo_mail;

pub(crate) use demo_cache::seed_demo_cache;
pub(crate) use demo_mail::demo_arrival;
pub(crate) use demo_mail::demo_raw;
use demo_mail::sample_messages;

use crate::models::{Account, Folder, FolderKind, Message, ThreadLatest, ThreadSummary};

/// Read access to mail data.
pub(crate) trait MailBackend {
    fn accounts(&self) -> Vec<Account>;
    fn folders(&self, account_id: u32) -> Vec<Folder>;
    fn messages(&self, folder_id: u32) -> Vec<Message>;
}

/// In-memory sample data provider for the offline demo.
pub(crate) struct MockBackend {
    accounts: Vec<Account>,
    folders: Vec<Folder>,
    messages: Vec<Message>,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MockBackend {
    pub(crate) fn new() -> Self {
        let accounts = vec![
            Account {
                id: 1,
                name: "Jason M.".into(),
                email: "jason@hylki.hyprlab.co".into(),
                label: "jason@hylki.hyprlab.co".into(),
                accent: "#3584e4".into(),
            },
            Account {
                id: 2,
                name: "Hyprlab".into(),
                email: "hello@hyprlab.dev".into(),
                label: "hello@hyprlab.dev".into(),
                accent: "#f6d32d".into(),
            },
            Account {
                id: 3,
                name: "Jason (Personal)".into(),
                email: "jason.m@fastmail.com".into(),
                label: "jason.m@fastmail.com".into(),
                accent: "#f66151".into(),
            },
        ];

        // Folder ids: 1–8 for account 1, 11–18 for account 2, 21–28 for
        // account 3. Inbox ids 1, 11 and 21. The eighth of each is a custom
        // folder fed by a demo filter rule (see app.rs's demo_filters), so
        // the sidebar's Filtered Folders section has rows to show.
        let folders = vec![
            folder(1, 1, "Inbox", FolderKind::Inbox, 6),
            folder(2, 1, "Starred", FolderKind::Starred, 0),
            folder(3, 1, "Sent", FolderKind::Sent, 0),
            folder(4, 1, "Drafts", FolderKind::Drafts, 1),
            folder(5, 1, "Archive", FolderKind::Archive, 0),
            folder(6, 1, "Junk", FolderKind::Junk, 2),
            folder(7, 1, "Trash", FolderKind::Trash, 0),
            folder(8, 1, "Newsletters", FolderKind::Custom, 2),
            folder(11, 2, "Inbox", FolderKind::Inbox, 12),
            folder(12, 2, "Starred", FolderKind::Starred, 0),
            folder(13, 2, "Sent", FolderKind::Sent, 0),
            folder(14, 2, "Drafts", FolderKind::Drafts, 0),
            folder(15, 2, "Archive", FolderKind::Archive, 0),
            folder(16, 2, "Junk", FolderKind::Junk, 0),
            folder(17, 2, "Trash", FolderKind::Trash, 0),
            folder(18, 2, "Invoices", FolderKind::Custom, 1),
            folder(21, 3, "Inbox", FolderKind::Inbox, 3),
            folder(22, 3, "Starred", FolderKind::Starred, 0),
            folder(23, 3, "Sent", FolderKind::Sent, 0),
            folder(24, 3, "Drafts", FolderKind::Drafts, 0),
            folder(25, 3, "Archive", FolderKind::Archive, 0),
            folder(26, 3, "Junk", FolderKind::Junk, 1),
            folder(27, 3, "Trash", FolderKind::Trash, 0),
            folder(28, 3, "Orders", FolderKind::Custom, 1),
        ];

        let mut messages = sample_messages();
        // HYLKI_DEMO_SENT_THREAD=1: file the demo's own replies where a real
        // client keeps them — in Sent — instead of beside the mail they answer.
        // The sample thread is written entirely into the Inbox, which is tidy
        // but is exactly the case #222 is about: the list can only count its
        // own folder, so a conversation the user has answered wears a badge
        // short by every reply they sent. With this on, the "Reader redesign"
        // thread is 3 rows in the Inbox and 5 messages, which is what the chip
        // should say.
        if std::env::var("HYLKI_DEMO_SENT_THREAD").is_ok_and(|v| v == "1") {
            const SENT: u32 = 3;
            const ME: &str = "jason@hylki.hyprlab.co";
            // Real mail carries the whole ancestry in References; the sample
            // specs carry only the immediate parent (which is why the cache has
            // a `refs_repair` pass at all). Thicken them first, or moving a
            // reply out of the Inbox would cut the chain at that message and
            // split one conversation into two rows — an artefact of the demo
            // data, not of where the message is filed.
            let parent: std::collections::HashMap<String, String> = messages
                .iter()
                .filter(|m| !m.message_id.is_empty() && !m.references.is_empty())
                .map(|m| (m.message_id.clone(), m.references.clone()))
                .collect();
            for m in messages.iter_mut() {
                let mut chain: Vec<String> = Vec::new();
                let mut next = m.references.clone();
                while !next.is_empty() && !chain.contains(&next) {
                    chain.push(next.clone());
                    next = parent.get(&next).cloned().unwrap_or_default();
                }
                chain.reverse();
                m.references = chain.join(" ");
            }
            for m in messages.iter_mut() {
                if m.account_id == 1 && m.folder_id == 1 && m.from_addr == ME {
                    m.folder_id = SENT;
                }
            }
        }

        Self {
            accounts,
            folders,
            messages,
        }
    }

    /// Look up a message by id across all folders (for body/source requests).
    pub(crate) fn message(&self, id: u32) -> Option<Message> {
        self.messages.iter().find(|m| m.id == id).cloned()
    }

    /// The demo's answer to the cache's `thread_summaries` (#222, #236): how
    /// big each conversation is across the account's folders and which message
    /// moved it last, so a thread row in the demo reads as it would against a
    /// real mailbox.
    pub(crate) fn thread_summaries(
        &self,
        account_id: u32,
        groups: &[(String, Vec<String>)],
    ) -> Vec<(String, ThreadSummary)> {
        use std::collections::HashSet;
        let hidden: HashSet<u32> = self
            .folders
            .iter()
            .filter(|f| f.account_id == account_id)
            .filter(|f| matches!(f.kind, FolderKind::Trash | FolderKind::Junk))
            .map(|f| f.id)
            .collect();
        let unsent: HashSet<u32> = self
            .folders
            .iter()
            .filter(|f| f.account_id == account_id)
            .filter(|f| matches!(f.kind, FolderKind::Drafts))
            .map(|f| f.id)
            .collect();
        groups
            .iter()
            .filter_map(|(tag, ids)| {
                let want: HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
                let mut seen: HashSet<&str> = HashSet::new();
                let mut newest: Option<&Message> = None;
                let mut members: Vec<Message> = Vec::new();
                for m in &self.messages {
                    if m.account_id != account_id || hidden.contains(&m.folder_id) {
                        continue;
                    }
                    let hit = want.contains(m.message_id.as_str())
                        || m.references.split_whitespace().any(|r| want.contains(r));
                    if !hit {
                        continue;
                    }
                    if !m.message_id.is_empty() {
                        seen.insert(m.message_id.as_str());
                    }
                    if unsent.contains(&m.folder_id) {
                        continue;
                    }
                    if newest.is_none_or(|n| m.timestamp > n.timestamp) {
                        newest = Some(m);
                    }
                    members.push(Message {
                        body: String::new(),
                        ..m.clone()
                    });
                }
                (!seen.is_empty()).then(|| {
                    (
                        tag.clone(),
                        ThreadSummary {
                            count: seen.len(),
                            latest: newest.map(|m| ThreadLatest {
                                from_name: m.from_name.clone(),
                                from_addr: m.from_addr.clone(),
                                preview: m.preview.clone(),
                                timestamp: m.timestamp,
                                date: m.date.clone(),
                            }),
                            members: if seen.len() > 1 { members } else { Vec::new() },
                        },
                    )
                })
            })
            .collect()
    }
}

impl MailBackend for MockBackend {
    fn accounts(&self) -> Vec<Account> {
        self.accounts.clone()
    }

    fn folders(&self, account_id: u32) -> Vec<Folder> {
        self.folders
            .iter()
            .filter(|f| f.account_id == account_id)
            .cloned()
            .collect()
    }

    fn messages(&self, folder_id: u32) -> Vec<Message> {
        self.messages
            .iter()
            .filter(|m| m.folder_id == folder_id)
            .cloned()
            .collect()
    }
}

fn folder(id: u32, account_id: u32, name: &str, kind: FolderKind, unread: u32) -> Folder {
    Folder {
        id,
        account_id,
        name: name.into(),
        path: name.into(),
        kind,
        unread,
        childless: false,
    }
}
