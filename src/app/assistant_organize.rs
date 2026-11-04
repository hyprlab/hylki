//! Changing mail for an AI assistant (`crate::mcp`), what Settings allows
//! being checked already: through the same steps as the app's own menus, so
//! it shows at once, reaches the server and lands in the Undo history.
//!
//! The menus only ever act on what is on screen, so they can find a message
//! by its UID alone. An assistant names mail in any folder, where the same
//! UID is another message, so nothing here touches the reader, a pop-out or a
//! list row without knowing it is the same message.

use std::collections::{HashMap, HashSet};

use relm4::prelude::*;

use crate::mcp::{Change, Organized, Target};
use crate::models::{FolderKind, Message};
use crate::ui::message_list::MessageListInput;
use crate::worker::MailRequest;

use super::flags::{read_label, star_label};
use super::model::AppModel;
use super::undo::FlagKind;

/// A message to move, under the id the assistant named it by.
struct Moving {
    id: String,
    message: Message,
    dest: String,
}

impl AppModel {
    pub(super) fn organize_for_assistant(
        &mut self,
        change: Change,
        targets: Vec<Target>,
    ) -> Organized {
        let mut done = Organized::default();
        let mut ready: Vec<(String, Message)> = Vec::with_capacity(targets.len());
        for target in targets {
            let account = target.message.account_id;
            let Some(folder_id) = self.folder_by_path(account, &target.path).map(|f| f.id) else {
                done.skipped
                    .push((target.id, "its folder is not known right now".to_string()));
                continue;
            };
            if !self.workers.contains_key(&account) {
                done.skipped
                    .push((target.id, "its account is off or offline".to_string()));
                continue;
            }
            let mut message = target.message;
            message.folder_id = folder_id;
            ready.push((target.id, message));
        }
        match change {
            Change::Read(read) => {
                self.flag_for_assistant(ready, read_label(read), FlagKind::Read, read, &mut done);
            }
            Change::Star(star) => {
                self.flag_for_assistant(ready, star_label(star), FlagKind::Star, star, &mut done);
            }
            Change::Tag { keyword, add } => {
                let what = self.tag_label(&keyword, add);
                self.flag_for_assistant(ready, what, FlagKind::Tag(keyword), add, &mut done);
            }
            Change::MoveTo(folders) => {
                let moving = ready
                    .into_iter()
                    .filter_map(|(id, message)| match folders.get(&message.account_id) {
                        Some(dest) => Some(Moving {
                            id,
                            dest: dest.clone(),
                            message,
                        }),
                        None => {
                            done.skipped
                                .push((id, "its account has no such folder".to_string()));
                            None
                        }
                    })
                    .collect();
                self.move_for_assistant(moving, &mut done);
            }
            Change::Archive => {
                let moving = ready
                    .into_iter()
                    .filter_map(|(id, message)| self.archive_step(id, message, &mut done))
                    .collect();
                self.move_for_assistant(moving, &mut done);
            }
            Change::Trash => {
                let moving = ready
                    .into_iter()
                    .filter_map(|(id, message)| self.trash_step(id, message, &mut done))
                    .collect();
                self.move_for_assistant(moving, &mut done);
            }
        }
        done
    }

    fn flag_for_assistant(
        &mut self,
        ready: Vec<(String, Message)>,
        what: String,
        kind: FlagKind,
        value: bool,
        done: &mut Organized,
    ) {
        let (ids, messages): (Vec<String>, Vec<Message>) = ready.into_iter().unzip();
        self.user_set_flags(what, &messages, kind, value);
        done.changed.extend(ids);
    }

    /// Where archiving takes a message: as the Archive button does, on Gmail
    /// that is its Inbox copy out of the Inbox (#392).
    fn archive_step(&self, id: String, message: Message, done: &mut Organized) -> Option<Moving> {
        let message = if self.archives_in_place(&message) {
            let Some(copy) = self.inbox_copy(&message) else {
                done.skipped
                    .push((id, "it is not in the Inbox".to_string()));
                return None;
            };
            copy
        } else {
            message
        };
        let Some(dest) = self.folder_path_for(message.account_id, FolderKind::Archive) else {
            done.skipped
                .push((id, "its account has no Archive folder".to_string()));
            return None;
        };
        Some(Moving { id, message, dest })
    }

    /// Where Trash is for a message. One already there stays: going further
    /// erases it for good, and only the user does that.
    fn trash_step(&self, id: String, message: Message, done: &mut Organized) -> Option<Moving> {
        if self.in_trash(&message) {
            done.skipped.push((
                id,
                "it is already in Trash; only the user can erase it for good".to_string(),
            ));
            return None;
        }
        let Some(dest) = self.folder_path_for(message.account_id, FolderKind::Trash) else {
            done.skipped
                .push((id, "its account has no Trash folder".to_string()));
            return None;
        };
        Some(Moving { id, message, dest })
    }

    /// Move each message, one server request per source folder, as a bulk
    /// move does, each group one step of the Undo history.
    fn move_for_assistant(&mut self, moving: Vec<Moving>, done: &mut Organized) {
        // (account, source folder) → (destination, UIDs, the rows for Undo).
        type Group = (String, Vec<u32>, Vec<Message>);
        let mut groups: HashMap<(u32, String), Group> = HashMap::new();
        let mut gone: Vec<(u32, u32, u32)> = Vec::new();
        let all: HashSet<(u32, u32)> = moving
            .iter()
            .map(|m| (m.message.account_id, m.message.id))
            .collect();
        for Moving { id, message, dest } in moving {
            let Some(src) = self.resolve_folder_path(&message) else {
                done.skipped
                    .push((id, "its folder is not known right now".to_string()));
                continue;
            };
            if src == dest {
                done.skipped.push((id, "it is already there".to_string()));
                continue;
            }
            let slot = (message.account_id, message.folder_id, message.id);
            let group = groups
                .entry((message.account_id, src))
                .or_insert_with(|| (dest, Vec::new(), Vec::new()));
            group.1.push(message.uid);
            if self.discard_for_assistant(&message, &all) {
                gone.push(slot);
            }
            group.2.push(message);
            done.changed.push(id);
        }
        if groups.is_empty() {
            return;
        }
        self.bulk_pending += groups.len();
        for ((account_id, src), (dest, uids, rows)) in groups {
            self.push_undo_move(account_id, &dest, &src, rows);
            self.send_to(
                account_id,
                MailRequest::MoveMessages {
                    path: src,
                    uids,
                    dest,
                },
            );
        }
        self.update_busy_indicator();
        self.message_list.emit(MessageListInput::RemoveExact(gone));
        self.push_unread_counts();
    }

    /// Take a message that is leaving out of what the app holds. Returns
    /// whether its list row goes too.
    fn discard_for_assistant(&mut self, m: &Message, moving: &HashSet<(u32, u32)>) -> bool {
        if self.another_message_open_with_same_uid(m) {
            self.discard_from_caches(m);
            return true;
        }
        self.discard_moved(m, moving)
    }

    /// Whether the reader or a pop-out shows another folder's message that
    /// has the same UID as `m`: closing it would close the wrong mail.
    fn another_message_open_with_same_uid(&self, m: &Message) -> bool {
        let other = |c: &Message| {
            c.account_id == m.account_id && c.id == m.id && c.folder_id != m.folder_id
        };
        self.current.as_ref().is_some_and(other)
            || self
                .popouts
                .get(&(m.account_id, m.id))
                .is_some_and(|p| p.message.1 != m.folder_id)
    }
}
