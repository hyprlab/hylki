//! Moving and copying mail between accounts.

use std::collections::{HashMap, HashSet};

use relm4::prelude::*;

use crate::config;
use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::models::{FolderKind, Message};
use crate::ui::message_list::MessageListInput;
use crate::ui::notifications::NotifyInput;
use crate::worker::MailRequest;

use super::demo::demo_mode;
use super::model::AppModel;
use super::msg::AppMsg;
use super::undo::{UndoEntry, UndoStep};

/// One message being moved to another account (#265): copied out of its
/// folder, into the destination, and only then taken out of the source.
pub(super) struct Transfer {
    src_account: u32,
    src_folder: u32,
    src_path: String,
    uid: u32,
    dest_account: u32,
    dest_path: String,
    seen: bool,
    flagged: bool,
    /// The message as the list had it, for the undo step.
    row: Option<Message>,
    /// A copy (Copy To): the original stays where it is.
    copy: bool,
}

/// A message that has landed in another account, and where its original
/// went: what undoing the move needs (#265).
struct Landed {
    src_account: u32,
    src_path: String,
    /// The source account's Trash, where the original now is; `None` when
    /// it was erased, which cannot be undone.
    src_trash: Option<String>,
    dest_account: u32,
    dest_path: String,
    row: Message,
}

/// The moves to other accounts under way, all batches together.
#[derive(Default)]
pub(super) struct TransferTally {
    total: usize,
    done: usize,
    failed: usize,
    /// The first failure's reason, for the report.
    error: Option<String>,
    /// The receiving accounts, named in the report.
    dest_accounts: std::collections::BTreeSet<u32>,
    /// The folders mail went into, synced once everything has landed.
    dest_folders: std::collections::BTreeSet<(u32, String)>,
    /// What has landed so far, recorded as one undo step at the end.
    landed: Vec<Landed>,
    /// The mail is being copied, not moved: the status and the report say so.
    copying: bool,
}

/// One side of a cross-account move each, with its Trash: (source
/// account, source path, its Trash, receiving account, destination path,
/// its Trash).
type TrashPair = (u32, String, String, u32, String, String);

impl AppModel {
    /// Whether mail from another account can be moved into this one (#265):
    /// IMAP and JMAP accounts take a message whole, and so does On This
    /// Computer (#552), which is what it is for. A Microsoft account files
    /// one posted to a folder as a draft, and POP3 has only an inbox.
    fn can_receive(&self, account_id: u32) -> bool {
        if demo_mode() {
            return true;
        }
        self.saved_account_config(account_id).is_some_and(|c| {
            c.enabled
                && matches!(
                    c.protocol,
                    config::Protocol::Imap | config::Protocol::Jmap | config::Protocol::Local
                )
        })
    }

    /// What the Move To picker lists: `own` account's folders less
    /// `exclude`, then, each under its name and in sidebar order, the
    /// folders of the other accounts that can take the mail (#265).
    pub(super) fn picker_accounts(
        &self,
        own: u32,
        exclude: Option<String>,
    ) -> Vec<crate::ui::folder_picker::PickerAccount> {
        use crate::ui::folder_picker::PickerAccount;
        let mut out = Vec::new();
        if let Some(folders) = self.folders.get(&own).filter(|f| !f.is_empty()) {
            out.push(PickerAccount {
                account_id: own,
                heading: None,
                folders: folders.clone(),
                exclude,
                hierarchy: self.drawn_hierarchy(own),
            });
        }
        for email in self.ordered_emails() {
            let Some(a) = self
                .accounts
                .iter()
                .find(|a| a.email == email && a.id != own)
            else {
                continue;
            };
            if !self.can_receive(a.id) {
                continue;
            }
            let Some(folders) = self.folders.get(&a.id).filter(|f| !f.is_empty()) else {
                continue;
            };
            out.push(PickerAccount {
                account_id: a.id,
                heading: Some(self.account_name(a.id)),
                folders: folders.clone(),
                exclude: None,
                hierarchy: self.drawn_hierarchy(a.id),
            });
        }
        out
    }

    /// Move mail from other accounts into `dest` on `dest_account` (#265):
    /// each message is copied out of its account and into this one, with
    /// its read and starred state, and only once the copy is stored is the
    /// original put in its own account's Trash (erased where there is
    /// none). The rows leave the list at once and come back if it fails.
    fn transfer_messages(
        &mut self,
        items: Vec<(u32, u32, u32, u32)>,
        dest_account: u32,
        dest: String,
    ) {
        let mut removed_ids = Vec::new();
        for (aid, fid, uid, id) in items {
            let cached = self.find_cached_message(aid, id);
            let src = match &cached {
                Some(m) => self.resolve_folder_path(m),
                None => self.folder_path(aid, fid),
            };
            let Some(src) = src else { continue };
            let src_folder = cached.as_ref().map_or(fid, |m| m.folder_id);
            let token = self.next_transfer;
            self.next_transfer += 1;
            self.transfers.insert(
                token,
                Transfer {
                    src_account: aid,
                    src_folder,
                    src_path: src.clone(),
                    uid,
                    dest_account,
                    dest_path: dest.clone(),
                    seen: cached.as_ref().is_none_or(|m| !m.unread),
                    flagged: cached.as_ref().is_some_and(|m| m.starred),
                    row: cached.clone(),
                    copy: false,
                },
            );
            // Off the source's lists until the move has landed or failed.
            self.pending_moves
                .borrow_mut()
                .insert((aid, src.clone(), uid), std::time::Instant::now());
            if let Some(m) = &cached {
                self.discard_message_local(m);
            }
            removed_ids.push(id);
            self.transfer_tally.total += 1;
            self.send_to(
                aid,
                MailRequest::ExportRaw {
                    token,
                    path: src,
                    uid,
                    for_reader: false,
                },
            );
        }
        self.transfer_tally.dest_accounts.insert(dest_account);
        self.transfer_tally
            .dest_folders
            .insert((dest_account, dest));
        if !removed_ids.is_empty() {
            self.message_list
                .emit(MessageListInput::RemoveMany(removed_ids));
            self.push_unread_counts();
        }
        self.show_transfer_status();
    }

    /// The Copy To picker's accounts: `own`'s folders less `exclude` when
    /// it can take a message whole, then those of the other accounts that
    /// can.
    pub(super) fn copy_picker_accounts(
        &self,
        own: u32,
        exclude: Option<String>,
    ) -> Vec<crate::ui::folder_picker::PickerAccount> {
        let mut accounts = self.picker_accounts(own, exclude);
        if !self.can_receive(own) {
            accounts.retain(|a| a.account_id != own);
        }
        accounts
    }

    /// Copy messages into `dest` on `dest_account` (Copy To): each is read
    /// whole out of its folder and stored again there, with its read and
    /// starred state, the way a move to another account copies it. The
    /// originals stay.
    pub(super) fn copy_messages(
        &mut self,
        messages: Vec<Message>,
        dest_account: u32,
        dest: String,
    ) {
        for m in messages {
            let Some(src) = self.resolve_folder_path(&m) else {
                continue;
            };
            if m.account_id == dest_account && src == dest {
                continue;
            }
            let token = self.next_transfer;
            self.next_transfer += 1;
            self.transfers.insert(
                token,
                Transfer {
                    src_account: m.account_id,
                    src_folder: m.folder_id,
                    src_path: src.clone(),
                    uid: m.uid,
                    dest_account,
                    dest_path: dest.clone(),
                    seen: !m.unread,
                    flagged: m.starred,
                    row: None,
                    copy: true,
                },
            );
            self.transfer_tally.total += 1;
            self.transfer_tally.copying = true;
            self.send_to(
                m.account_id,
                MailRequest::ExportRaw {
                    token,
                    path: src,
                    uid: m.uid,
                    for_reader: false,
                },
            );
        }
        self.transfer_tally.dest_accounts.insert(dest_account);
        self.transfer_tally
            .dest_folders
            .insert((dest_account, dest));
        self.transfer_step();
    }

    /// A message is stored in its new account: take the original out,
    /// unless it was a copy.
    fn transfer_landed(&mut self, token: u64) {
        let Some(t) = self.transfers.remove(&token) else {
            return;
        };
        if t.copy {
            self.transfer_tally.done += 1;
            self.transfer_step();
            return;
        }
        let trash = self
            .folder_path_for(t.src_account, FolderKind::Trash)
            .filter(|trash| *trash != t.src_path);
        if let Some(row) = t.row.filter(|m| !m.message_id.is_empty()) {
            self.transfer_tally.landed.push(Landed {
                src_account: t.src_account,
                src_path: t.src_path.clone(),
                src_trash: trash.clone(),
                dest_account: t.dest_account,
                dest_path: t.dest_path.clone(),
                row,
            });
        }
        let req = match trash {
            Some(dest) => MailRequest::MoveMessage {
                path: t.src_path,
                uid: t.uid,
                dest,
            },
            None => MailRequest::PurgeMessages {
                path: t.src_path,
                uids: vec![t.uid],
            },
        };
        self.send_to(t.src_account, req);
        self.transfer_tally.done += 1;
        self.transfer_step();
    }

    /// A message could not be copied across: it stays where it was, and
    /// comes back on its folder's list.
    fn transfer_failed(&mut self, token: u64, error: String) {
        let Some(t) = self.transfers.remove(&token) else {
            return;
        };
        tracing::warn!("move to another account failed: {error}");
        if t.copy {
            self.transfer_tally.failed += 1;
            self.transfer_tally.error.get_or_insert(error);
            self.transfer_step();
            return;
        }
        self.pending_moves
            .borrow_mut()
            .remove(&(t.src_account, t.src_path.clone(), t.uid));
        self.send_to(
            t.src_account,
            MailRequest::SyncFolder {
                folder_id: t.src_folder,
                path: t.src_path,
            },
        );
        self.transfer_tally.failed += 1;
        self.transfer_tally.error.get_or_insert(error);
        self.transfer_step();
    }

    /// After each message: the status line, and once all are through, a
    /// fresh look at the folders the mail went into and, if any failed, the
    /// one error report.
    fn transfer_step(&mut self) {
        let tally = &self.transfer_tally;
        if tally.done + tally.failed < tally.total {
            self.show_transfer_status();
            return;
        }
        let mut tally = std::mem::take(&mut self.transfer_tally);
        self.notifications
            .emit(NotifyInput::SetStatus(self.copy_status()));
        self.record_transfer_undo(std::mem::take(&mut tally.landed));
        for (account_id, path) in &tally.dest_folders {
            if let Some(folder_id) = self
                .folders
                .get(account_id)
                .and_then(|fs| fs.iter().find(|f| f.path == *path))
                .map(|f| f.id)
            {
                self.send_to(
                    *account_id,
                    MailRequest::SyncFolder {
                        folder_id,
                        path: path.clone(),
                    },
                );
            }
        }
        // The bar only speaks up for a failure: a move that worked shows
        // itself, in the folders.
        if tally.failed > 0 {
            let names: Vec<String> = tally
                .dest_accounts
                .iter()
                .map(|a| self.account_label(*a))
                .collect();
            let n = tally.failed.to_string();
            let account = names.join(", ");
            let args = [
                ("n", n.as_str()),
                ("account", account.as_str()),
                ("error", tally.error.as_deref().unwrap_or("")),
            ];
            let text = if tally.copying {
                ni18n_f(
                    "One message could not be copied to {account}: {error}",
                    "{n} messages could not be copied to {account}: {error}",
                    tally.failed as u32,
                    &args,
                )
            } else {
                ni18n_f(
                    "One message could not be moved to {account}: {error}",
                    "{n} messages could not be moved to {account}: {error}",
                    tally.failed as u32,
                    &args,
                )
            };
            self.notice_error(text);
        }
    }

    /// One undo step for everything a batch of moves to other accounts put
    /// down (#265): per source folder, the originals come back out of their
    /// account's Trash, and the copies go into the receiving account's.
    /// Mail that was erased at the source, or that went to an account with
    /// no Trash, is left out: undoing it would leave a copy behind.
    fn record_transfer_undo(&mut self, landed: Vec<Landed>) {
        let mut groups: Vec<(TrashPair, Vec<Message>)> = Vec::new();
        for l in landed {
            let Some(src_trash) = l.src_trash else {
                continue;
            };
            let Some(dest_trash) = self
                .folder_path_for(l.dest_account, FolderKind::Trash)
                .filter(|t| *t != l.dest_path)
            else {
                continue;
            };
            let key = (
                l.src_account,
                l.src_path,
                src_trash,
                l.dest_account,
                l.dest_path,
                dest_trash,
            );
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, rows)) => rows.push(l.row),
                None => groups.push((key, vec![l.row])),
            }
        }
        let Some(((src_account, src_path, src_trash, dest_account, dest_path, dest_trash), rows)) =
            groups.first().cloned()
        else {
            return;
        };
        let ids = |rows: &[Message]| {
            rows.iter()
                .map(|m| m.message_id.clone())
                .collect::<Vec<_>>()
        };
        // The first source folder is the entry's own step, so a quick undo
        // puts its rows straight back; everything else rides along.
        let mut also = vec![(
            dest_account,
            UndoStep::Move {
                from: dest_path.clone(),
                to: dest_trash,
                message_ids: ids(&rows),
            },
        )];
        for ((sa, sp, st, da, dp, dt), rows) in groups.iter().skip(1) {
            also.push((
                *sa,
                UndoStep::Move {
                    from: st.clone(),
                    to: sp.clone(),
                    message_ids: ids(rows),
                },
            ));
            also.push((
                *da,
                UndoStep::Move {
                    from: dp.clone(),
                    to: dt.clone(),
                    message_ids: ids(rows),
                },
            ));
        }
        let what = self.move_label(dest_account, &dest_path);
        let threads = Vec::new();
        self.push_undo_entry(UndoEntry {
            account_id: src_account,
            what,
            step: UndoStep::Move {
                from: src_trash,
                to: src_path,
                message_ids: ids(&rows),
            },
            at: std::time::Instant::now(),
            rows,
            threads,
            rows_return: true,
            also,
        });
    }

    fn show_transfer_status(&self) {
        let tally = &self.transfer_tally;
        let left = tally.total.saturating_sub(tally.done + tally.failed);
        if left == 0 {
            return;
        }
        let names: Vec<String> = tally
            .dest_accounts
            .iter()
            .map(|a| self.account_label(*a))
            .collect();
        let n = left.to_string();
        let account = names.join(", ");
        let args = [("n", n.as_str()), ("account", account.as_str())];
        let text = if tally.copying {
            ni18n_f(
                "Copying one message to {account}…",
                "Copying {n} messages to {account}…",
                left as u32,
                &args,
            )
        } else {
            ni18n_f(
                "Moving one message to {account}…",
                "Moving {n} messages to {account}…",
                left as u32,
                &args,
            )
        };
        self.notifications.emit(NotifyInput::SetStatus(text));
    }

    /// Move a dropped drag selection into `dest` on `dest_account`. Messages are
    /// grouped by source folder and each group moved in a single `MoveMessages`
    /// request, so dragging a multi-selection moves all of it (#23). Anything
    /// from another account (a drag from the unified inbox, a folder of
    /// another account picked in Move To) is copied across (#265), or, where
    /// the destination cannot take it, stays put and is reported.
    pub(super) fn drop_move(
        &mut self,
        dest_account: u32,
        dest: String,
        items: Vec<(u32, u32, u32, u32)>,
    ) {
        let mut groups: HashMap<String, (Vec<u32>, Vec<Message>)> = HashMap::new();
        let mut removed_ids: Vec<u32> = Vec::new();
        let (foreign, items): (Vec<_>, Vec<_>) = items
            .into_iter()
            .partition(|(aid, ..)| *aid != dest_account);
        // Mail from another account is copied across when this one can take
        // it (#265); otherwise it stays put, and says so.
        let foreign = if !foreign.is_empty() && self.can_receive(dest_account) {
            self.transfer_messages(foreign, dest_account, dest.clone());
            0
        } else {
            foreign.len()
        };
        let moving: HashSet<(u32, u32)> = items.iter().map(|&(a, _, _, i)| (a, i)).collect();
        for (aid, fid, uid, id) in items {
            // Prefer the cached message: it knows its own source folder (in the
            // unified inbox that isn't the folder the row was listed under) and
            // lets the caches be cleaned up optimistically.
            let cached = self.find_cached_message(aid, id);
            let src = match &cached {
                Some(m) => self.resolve_folder_path(m),
                None => self.folder_path(aid, fid),
            };
            let Some(src) = src else { continue };
            if src == dest {
                continue; // already in that folder
            }
            // A card dragged out of the open conversation leaves it, and
            // the conversation's row stays for the rest.
            let row_goes = match &cached {
                Some(m) => self.discard_moved(m, &moving),
                None => true,
            };
            if row_goes {
                removed_ids.push(id);
            }
            let slot = groups.entry(src).or_default();
            slot.0.push(uid);
            // A row with no cached message is moved, but not recorded: there
            // is nothing to find it by, and nothing to put back on the list.
            slot.1.extend(cached);
        }
        if foreign > 0 {
            self.notice_error(if foreign == 1 {
                i18n("One message stayed put: this account can't take mail from another")
            } else {
                i18n_f(
                    "{foreign} messages stayed put: this account can't take mail from another",
                    &[("foreign", &foreign.to_string())],
                )
            });
        }
        if groups.is_empty() {
            return;
        }
        self.bulk_pending += groups.len();
        for (src, (uids, rows)) in groups {
            self.push_undo_move(dest_account, &dest, &src, rows);
            self.send_to(
                dest_account,
                MailRequest::MoveMessages {
                    path: src,
                    uids,
                    dest: dest.clone(),
                },
            );
        }
        self.message_list
            .emit(MessageListInput::RemoveMany(removed_ids));
        self.push_unread_counts();
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_transfer(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::RawExported { token, raw } => {
                if let Some(name) = self.pending_saves.remove(&token) {
                    match raw {
                        Ok(raw) => self.save_message_file(name, raw),
                        Err(e) => self
                            .notice_error(i18n_f("Could not save the message: {e}", &[("e", &e)])),
                    }
                    return;
                }
                if self.pending_draft_token == Some(token) {
                    self.pending_draft_token = None;
                    let Some((m, inline, extra)) = self.pending_draft.take() else {
                        return;
                    };
                    match raw {
                        Ok(raw) => self.compose_from_draft_raw(m, &raw, inline, extra, &sender),
                        // Offline, say: the body the reader has is still
                        // better than nothing.
                        Err(e) => {
                            tracing::warn!("draft {} could not be fetched whole: {e}", m.uid);
                            self.open_draft_body(m, inline, extra, &sender);
                        }
                    }
                    return;
                }
                let Some(t) = self.transfers.get(&token) else {
                    return;
                };
                match raw {
                    Ok(raw) => self.send_to(
                        t.dest_account,
                        MailRequest::ImportRaw {
                            token,
                            path: t.dest_path.clone(),
                            raw,
                            seen: t.seen,
                            flagged: t.flagged,
                        },
                    ),
                    Err(e) => self.transfer_failed(token, e),
                }
            }

            AppMsg::RawImported { token, result } => match result {
                Ok(()) => self.transfer_landed(token),
                Err(e) => self.transfer_failed(token, e),
            },
            _ => unreachable!("not routed to on_transfer"),
        }
    }
}
