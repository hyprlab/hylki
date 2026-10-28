//! Snooze (#527): mail put away in the account's Snoozed folder until a set
//! time, then moved back, unread, to the folder it came from. The times are
//! kept in snoozed.toml; the app's clock does the waking, so mail snoozed
//! here comes back while Hylki is running, or as soon as it starts.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use adw::prelude::*;
use relm4::prelude::*;

use super::types::UnifiedView;
use crate::config::{self, Snooze};
use crate::i18n::i18n;
use crate::models::{FolderKind, Message};
use crate::ui::later::SnoozeChoice;
use crate::ui::message_list::{BulkAction, MessageListInput};
use crate::worker::MailRequest;

use super::demo::demo_mode;
use super::model::AppModel;
use super::msg::AppMsg;
use super::removal::MoveGroups;

/// How long a wake is given before it is asked for again: an answer lost
/// with a dropped connection must not leave the mail asleep for good, and a
/// server that refused should not be asked every half minute.
const WAKE_RETRY: Duration = Duration::from_secs(5 * 60);

/// The snoozed mail on record, or none in the demo, which must never
/// read or write the real file.
pub(super) fn load_snoozed() -> Vec<Snooze> {
    if demo_mode() {
        Vec::new()
    } else {
        config::load_snoozed()
    }
}

/// The messages a reply in `messages` brings back early: the snoozed ones it
/// refers to, if it was written after they were snoozed.
fn woken_by_replies(asleep: &HashMap<&str, i64>, messages: &[Message]) -> HashSet<String> {
    let mut woken = HashSet::new();
    for m in messages {
        for id in m.references.split_whitespace() {
            if asleep.get(id).is_some_and(|at| m.timestamp > *at) {
                woken.insert(id.to_string());
            }
        }
    }
    woken
}

impl AppModel {
    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_snooze(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::Snooze(until) => {
                self.snooze_until = Some(until);
                self.snooze_target(BulkAction::Snooze, &sender);
            }
            AppMsg::Unsnooze => self.snooze_target(BulkAction::Unsnooze, &sender),
            AppMsg::PickSnoozeTime => {
                let s = sender.input_sender().clone();
                self.pick_snooze_time(move |t| {
                    let _ = s.send(AppMsg::Snooze(t));
                });
            }
            AppMsg::SnoozeMessages { messages, choice } => {
                self.snooze_choice(messages, choice, &sender)
            }
            AppMsg::SnoozeMenu => self.show_snooze_menu(&sender),
            AppMsg::WakeSnoozed => self.wake_due(),
            AppMsg::Unsnoozed {
                account_id,
                message_ids,
                done,
            } => self.on_unsnoozed(account_id, message_ids, done),
            AppMsg::SnoozedCount {
                account_id,
                path,
                count,
            } => {
                self.snoozed_held.insert((account_id, path), count);
                self.sync_snoozed_row();
            }
            _ => unreachable!("not routed to on_snooze"),
        }
    }

    /// The calendar, for a snooze time of one's own.
    fn pick_snooze_time(&self, pick: impl Fn(i64) + 'static) {
        crate::ui::later::pick_later_time(
            Some(self.window.upcast_ref()),
            None,
            &i18n("Snooze until"),
            &i18n("Snooze"),
            pick,
        );
    }

    /// A Snooze menu's choice for `messages`, which a row, a selection or
    /// a pop-out named.
    pub(super) fn snooze_choice(
        &mut self,
        messages: Vec<Message>,
        choice: SnoozeChoice,
        sender: &ComponentSender<Self>,
    ) {
        match choice {
            SnoozeChoice::Until(t) => self.snooze_messages(messages, t),
            SnoozeChoice::Wake => self.unsnooze_messages(messages),
            SnoozeChoice::Pick => {
                let s = sender.input_sender().clone();
                self.pick_snooze_time(move |t| {
                    let _ = s.send(AppMsg::SnoozeMessages {
                        messages: messages.clone(),
                        choice: SnoozeChoice::Until(t),
                    });
                });
            }
        }
    }

    /// What the list's menus offer in a folder of `kind` (#527): nothing
    /// in Drafts, Templates or a POP3 inbox, Unsnooze in Snoozed.
    pub(super) fn snooze_offer(&self, account_id: u32, kind: Option<FolderKind>) -> Option<bool> {
        let pop3 = self
            .saved_account_config(account_id)
            .is_none_or(|c| c.protocol == config::Protocol::Pop3);
        match kind {
            _ if pop3 => None,
            Some(FolderKind::Drafts | FolderKind::Templates) | None => None,
            Some(k) => Some(k == FolderKind::Snoozed),
        }
    }

    /// The Snooze menu, under the toolbar's button, or the overflow's when
    /// the toolbar has folded into it.
    fn show_snooze_menu(&self, sender: &ComponentSender<Self>) {
        if !self.snooze_offered() {
            return;
        }
        let btn = if self.reader_snooze_btn.is_mapped() {
            self.reader_snooze_btn.clone()
        } else {
            self.reader_overflow_btn.clone()
        };
        let sections = self.snooze_sections(sender);
        crate::ui::context_menu::show_context_menu(
            &btn,
            (btn.width() / 2) as f64,
            btn.height() as f64,
            sections,
        );
    }

    /// The Snooze menu's entries, for what the reader's actions act on.
    pub(super) fn snooze_sections(
        &self,
        sender: &ComponentSender<Self>,
    ) -> Vec<Vec<crate::ui::context_menu::MenuEntry>> {
        let s = sender.input_sender().clone();
        crate::ui::later::snooze_menu(self.target_snoozed(), move |choice| {
            let _ = s.send(match choice {
                SnoozeChoice::Until(t) => AppMsg::Snooze(t),
                SnoozeChoice::Pick => AppMsg::PickSnoozeTime,
                SnoozeChoice::Wake => AppMsg::Unsnooze,
            });
        })
    }

    /// Whether the reader's actions can snooze what they act on.
    pub(super) fn snooze_offered(&self) -> bool {
        !self.showing_outbox
            && match self.reply_target() {
                Some(m) => self.snoozable(&m),
                None => self.many_selected(),
            }
    }

    /// Whether what the reader's actions act on is snoozed already: the
    /// message being read, or a selection made in a Snoozed folder or the
    /// unified Snoozed (#542).
    pub(super) fn target_snoozed(&self) -> bool {
        let snoozed = |account_id, folder_id| {
            self.folder_kind(account_id, folder_id) == Some(FolderKind::Snoozed)
        };
        if self.many_selected() {
            if self.unified {
                return self.unified_view == UnifiedView::Kind(FolderKind::Snoozed);
            }
            return self
                .selected
                .as_ref()
                .is_some_and(|f| snoozed(f.account_id, f.folder_id));
        }
        self.reply_target()
            .is_some_and(|m| snoozed(m.account_id, m.folder_id))
    }

    /// Whether `m` can be snoozed: it has a Message-ID to be found again
    /// by, a folder on a server to wait in, and is not a draft or template.
    pub(super) fn snoozable(&self, m: &Message) -> bool {
        !m.message_id.is_empty()
            && self
                .saved_account_config(m.account_id)
                .is_some_and(|c| c.protocol != config::Protocol::Pop3)
            && !matches!(
                self.folder_kind(m.account_id, m.folder_id),
                Some(FolderKind::Drafts | FolderKind::Templates) | None
            )
    }

    /// Snooze, or bring back, what an action means: the picked cards, the
    /// list's selection (which the list resolves and hands back), the open
    /// conversation's row, or the message being read.
    fn snooze_target(&mut self, action: BulkAction, sender: &ComponentSender<Self>) {
        if self.bulk_on_cards(action, sender) {
            return;
        }
        if self.many_selected() {
            self.message_list.emit(MessageListInput::Bulk(action));
            return;
        }
        let messages = if self.acts_on_conversation(false) {
            self.conversation_to_archive()
        } else {
            Vec::new()
        };
        let messages = if messages.len() > 1 {
            messages
        } else {
            self.reply_target().into_iter().collect()
        };
        self.apply_snooze(action, messages);
    }

    /// The bulk end of [`AppModel::snooze_target`].
    pub(super) fn apply_snooze(&mut self, action: BulkAction, messages: Vec<Message>) {
        match action {
            BulkAction::Snooze => {
                if let Some(until) = self.snooze_until.take() {
                    self.snooze_messages(messages, until);
                }
            }
            BulkAction::Unsnooze => self.unsnooze_messages(messages),
            _ => {}
        }
    }

    /// The account id with this address: account ids are positions, the
    /// address is what snoozed.toml keeps.
    fn account_id_by_email(&self, email: &str) -> Option<u32> {
        self.config
            .iter()
            .position(|c| c.email.eq_ignore_ascii_case(email))
            .map(|i| i as u32 + 1)
    }

    fn account_email(&self, account_id: u32) -> Option<String> {
        self.saved_account_config(account_id)
            .map(|c| c.email.clone())
    }

    /// Keep `s`, in place of any earlier snooze of the same message.
    fn record_snooze(&mut self, s: Snooze) {
        match self
            .snoozed
            .iter_mut()
            .find(|o| o.account == s.account && o.message_id == s.message_id)
        {
            Some(o) => *o = s,
            None => self.snoozed.push(s),
        }
    }

    fn save_snoozed(&self) {
        if !demo_mode() {
            config::save_snoozed(&self.snoozed);
        }
        self.push_snoozed();
        self.sync_snoozed_row();
        // Recount the Snoozed folders once the moves just asked for have
        // landed (a worker takes requests in order), so the row does not
        // wait for the next round to go with the last of their mail.
        let accounts: HashSet<u32> = self
            .folders
            .iter()
            .filter(|(_, fs)| fs.iter().any(|f| f.kind == FolderKind::Snoozed))
            .map(|(a, _)| *a)
            .collect();
        for account_id in accounts {
            self.send_to(account_id, MailRequest::RefreshUnread);
        }
    }

    /// The unified Snoozed row comes with the first snoozed message and
    /// goes with the last (#542).
    pub(super) fn sync_snoozed_row(&self) {
        if self.unified_kinds_listed().snoozed != self.snoozed_row.get() {
            self.rebuild_sidebar();
        }
    }

    /// A Snoozed folder's listing says how much it holds (#542): fresher
    /// than the last recount when the folder is the one being worked, which
    /// the IMAP recount passes over.
    pub(super) fn note_snoozed_listing(&mut self, account_id: u32, folder_id: u32, count: usize) {
        let Some(f) = self
            .folders
            .get(&account_id)
            .and_then(|fs| fs.iter().find(|f| f.id == folder_id))
            .filter(|f| f.kind == FolderKind::Snoozed)
        else {
            return;
        };
        let key = (account_id, f.path.clone());
        self.snoozed_held.insert(key, count as u32);
        self.sync_snoozed_row();
    }

    /// Undo and Redo of a Snooze (#542): mail an undo takes out of its
    /// Snoozed folder takes its record along, kept for a redo to bring
    /// back. A record left behind would wake nothing, but would keep the
    /// unified Snoozed row up with nothing in it.
    pub(super) fn follow_undo_move(
        &mut self,
        account_id: u32,
        from: &str,
        to: &str,
        message_ids: &[String],
    ) {
        let Some(account) = self.account_email(account_id) else {
            return;
        };
        let moved = |s: &Snooze, path: &str| {
            s.account == account && s.path == path && message_ids.contains(&s.message_id)
        };
        let (out, keep): (Vec<Snooze>, Vec<Snooze>) = std::mem::take(&mut self.snoozed)
            .into_iter()
            .partition(|s| moved(s, from));
        self.snoozed = keep;
        let mut changed = !out.is_empty();
        for s in out {
            self.snooze_undone
                .insert((s.account.clone(), s.message_id.clone()), s);
        }
        let back: Vec<(String, String)> = self
            .snooze_undone
            .iter()
            .filter(|(_, s)| moved(s, to))
            .map(|(k, _)| k.clone())
            .collect();
        for key in back {
            if let Some(s) = self.snooze_undone.remove(&key) {
                self.record_snooze(s);
                changed = true;
            }
        }
        if changed {
            self.save_snoozed();
        }
    }

    /// The unified rows the sidebar lists: those switched on, Snoozed only
    /// while mail of an account in the unified section is snoozed (#542),
    /// as Filters and Tags show only with a rule or a tag to list. Snoozed
    /// means a Snoozed folder the server says holds mail, whoever put it
    /// there, or a snooze made here that no recount has seen land yet.
    pub(super) fn unified_kinds_listed(&self) -> config::UnifiedKinds {
        let mut kinds = self.unified_kinds;
        let recorded = self.snoozed.iter().any(|s| {
            self.account_id_by_email(&s.account)
                .is_some_and(|id| self.in_unified(id))
        });
        let held = self
            .accounts
            .iter()
            .filter(|a| self.in_unified(a.id))
            .filter_map(|a| {
                let path = self.folder_path_for(a.id, FolderKind::Snoozed)?;
                self.snoozed_held.get(&(a.id, path)).copied()
            })
            .any(|n| n > 0);
        kinds.snoozed &= recorded || held;
        kinds
    }

    /// Tell the list when each snoozed message comes back, for its rows.
    /// Folder ids shift as folders come and go, so this follows every
    /// listing too.
    pub(super) fn push_snoozed(&self) {
        let times = self
            .snoozed
            .iter()
            .filter_map(|s| {
                let id = self.account_id_by_email(&s.account)?;
                Some(((id, s.message_id.clone()), s.until))
            })
            .collect();
        let folders = self
            .folders
            .iter()
            .flat_map(|(account_id, fs)| {
                fs.iter()
                    .filter(|f| f.kind == FolderKind::Snoozed)
                    .map(move |f| (*account_id, f.id))
            })
            .collect();
        self.message_list.emit(MessageListInput::SetSnoozed(
            crate::ui::message_row::SnoozedRows { times, folders },
        ));
    }

    /// Put `messages` in their accounts' Snoozed folders until `until`.
    /// Mail already snoozed only gets the new time.
    fn snooze_messages(&mut self, messages: Vec<Message>, until: i64) {
        let now = crate::datefmt::now();
        // On Gmail what is snoozed from a label is the Inbox copy, as
        // Archive takes (#392); the label stays on it.
        let mut seen = HashSet::new();
        let messages: Vec<Message> = messages
            .into_iter()
            .filter_map(|m| {
                if self.archives_in_place(&m) {
                    self.inbox_copy(&m).or(Some(m))
                } else {
                    Some(m)
                }
            })
            .filter(|m| seen.insert((m.account_id, m.id)))
            .collect();
        // (account, source) → (Snoozed folder, uids, rows for undo)
        let mut groups: MoveGroups = HashMap::new();
        let mut leaving = Vec::new();
        for m in &messages {
            if !self.snoozable(m) {
                continue;
            }
            let (Some(src), Some(account)) = (
                self.resolve_folder_path(m),
                self.account_email(m.account_id),
            ) else {
                continue;
            };
            if self.folder_kind(m.account_id, m.folder_id) == Some(FolderKind::Snoozed) {
                let back = self
                    .snoozed
                    .iter()
                    .find(|s| s.account == account && s.message_id == m.message_id)
                    .map(|s| s.folder.clone())
                    .or_else(|| self.inbox_of(m.account_id).map(|f| f.path.clone()));
                if let Some(folder) = back {
                    self.record_snooze(Snooze {
                        account,
                        message_id: m.message_id.clone(),
                        until,
                        at: now,
                        folder,
                        path: src,
                        subject: m.subject.clone(),
                    });
                }
                continue;
            }
            let Some(dest) = self.folder_path_for(m.account_id, FolderKind::Snoozed) else {
                continue;
            };
            self.record_snooze(Snooze {
                account,
                message_id: m.message_id.clone(),
                until,
                at: now,
                folder: src.clone(),
                path: dest.clone(),
                subject: m.subject.clone(),
            });
            let slot = groups
                .entry((m.account_id, src))
                .or_insert_with(|| (dest, Vec::new(), Vec::new()));
            slot.1.push(m.uid);
            slot.2.push(m.clone());
            leaving.push(m.clone());
        }
        self.bulk_pending += groups.len();
        for ((account_id, src), (dest, uids, rows)) in groups {
            self.push_undo_move_as(account_id, i18n("Snooze"), &dest, &src, rows);
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
        self.take_off_list(leaving);
        self.save_snoozed();
    }

    /// Take mail that left its folder off the list, as Archive does: one
    /// message as a single archive, so the reader moves on to the next.
    fn take_off_list(&mut self, leaving: Vec<Message>) {
        if let [m] = leaving.as_slice() {
            self.discard_message(m);
            return;
        }
        let moving: HashSet<(u32, u32)> = leaving.iter().map(|m| (m.account_id, m.id)).collect();
        let removed_ids: Vec<u32> = leaving
            .iter()
            .filter(|m| self.discard_moved(m, &moving))
            .map(|m| m.id)
            .collect();
        self.message_list
            .emit(MessageListInput::RemoveMany(removed_ids));
        self.push_unread_counts();
    }

    /// Bring `messages` back now. One snoozed elsewhere, with no time on
    /// record here, goes to its account's Inbox.
    fn unsnooze_messages(&mut self, messages: Vec<Message>) {
        let now = crate::datefmt::now();
        let mut leaving = Vec::new();
        for m in &messages {
            if m.message_id.is_empty() {
                continue;
            }
            let Some(account) = self.account_email(m.account_id) else {
                continue;
            };
            let in_snoozed =
                self.folder_kind(m.account_id, m.folder_id) == Some(FolderKind::Snoozed);
            match self
                .snoozed
                .iter_mut()
                .find(|s| s.account == account && s.message_id == m.message_id)
            {
                Some(s) => s.until = now,
                None if in_snoozed => {
                    let (Some(path), Some(inbox)) = (
                        self.resolve_folder_path(m),
                        self.inbox_of(m.account_id).map(|f| f.path.clone()),
                    ) else {
                        continue;
                    };
                    self.snoozed.push(Snooze {
                        account,
                        message_id: m.message_id.clone(),
                        until: now,
                        at: now,
                        folder: inbox,
                        path,
                        subject: m.subject.clone(),
                    });
                }
                None => continue,
            }
            if in_snoozed {
                leaving.push(m.clone());
            }
        }
        self.take_off_list(leaving);
        self.save_snoozed();
        self.wake_due();
    }

    /// The clock (#527): ask each account's worker to bring back the mail
    /// whose time has come. An account not listed yet waits for the next
    /// tick, as does everything while there is no network.
    pub(super) fn wake_due(&mut self) {
        if self.snoozed.is_empty() || !crate::desktop::power::reaches_internet() {
            return;
        }
        let now = crate::datefmt::now();
        let clock = Instant::now();
        // (account, Snoozed folder, folder back to) → Message-IDs
        let mut groups: HashMap<(u32, String, String), Vec<String>> = HashMap::new();
        for s in &self.snoozed {
            if s.until > now
                || self
                    .snooze_waking
                    .get(&(s.account.clone(), s.message_id.clone()))
                    .is_some_and(|retry| *retry > clock)
            {
                continue;
            }
            let Some(account_id) = self.account_id_by_email(&s.account) else {
                continue;
            };
            let Some(folders) = self.folders.get(&account_id) else {
                continue;
            };
            // The folder it came from may have gone meanwhile.
            let dest = if folders.iter().any(|f| f.path == s.folder) {
                s.folder.clone()
            } else if let Some(inbox) = self.inbox_of(account_id) {
                inbox.path.clone()
            } else {
                continue;
            };
            groups
                .entry((account_id, s.path.clone(), dest))
                .or_default()
                .push(s.message_id.clone());
        }
        for ((account_id, path, dest), message_ids) in groups {
            let (Some(dest_folder_id), Some(account)) = (
                self.folder_by_path(account_id, &dest).map(|f| f.id),
                self.account_email(account_id),
            ) else {
                continue;
            };
            for id in &message_ids {
                self.snooze_waking
                    .insert((account.clone(), id.clone()), clock + WAKE_RETRY);
            }
            tracing::info!(
                "snooze: waking {} in {path:?} for {dest:?}",
                message_ids.len()
            );
            self.send_to(
                account_id,
                MailRequest::Unsnooze {
                    path,
                    dest,
                    dest_folder_id,
                    message_ids,
                },
            );
        }
    }

    /// A worker's answer to [`MailRequest::Unsnooze`]. Mail that went back,
    /// or was no longer there to go, is off the record; mail the server
    /// could not be asked about waits for [`WAKE_RETRY`].
    fn on_unsnoozed(&mut self, account_id: u32, message_ids: Vec<String>, done: bool) {
        let Some(account) = self.account_email(account_id) else {
            return;
        };
        if !done {
            return;
        }
        for id in &message_ids {
            self.snooze_waking.remove(&(account.clone(), id.clone()));
        }
        let now = crate::datefmt::now();
        // A message snoozed again meanwhile keeps its new time.
        self.snoozed.retain(|s| {
            !(s.account == account && s.until <= now && message_ids.contains(&s.message_id))
        });
        // The rows the Snoozed folder still lists.
        let snoozed_folders: Vec<u32> = self
            .folders
            .get(&account_id)
            .map(|fs| {
                fs.iter()
                    .filter(|f| f.kind == FolderKind::Snoozed)
                    .map(|f| f.id)
                    .collect()
            })
            .unwrap_or_default();
        let gone: Vec<Message> = snoozed_folders
            .iter()
            .filter_map(|fid| self.message_cache.get(&(account_id, *fid)))
            .flat_map(|ms| ms.iter())
            .filter(|m| message_ids.contains(&m.message_id))
            .cloned()
            .collect();
        for m in &gone {
            self.discard_message(m);
        }
        self.save_snoozed();
    }

    /// Mail arriving in a conversation brings its snoozed messages back
    /// early, as a reply does in Gmail. Only mail written after the snooze
    /// counts, so the reload of a folder holding older replies wakes nothing.
    pub(super) fn wake_on_reply(&mut self, account_id: u32, folder_id: u32, messages: &[Message]) {
        if self.snoozed.is_empty() {
            return;
        }
        if !matches!(
            self.folder_kind(account_id, folder_id),
            Some(
                FolderKind::Inbox | FolderKind::Archive | FolderKind::Starred | FolderKind::Custom
            )
        ) {
            return;
        }
        let Some(account) = self.account_email(account_id) else {
            return;
        };
        let now = crate::datefmt::now();
        let asleep: HashMap<&str, i64> = self
            .snoozed
            .iter()
            .filter(|s| s.account == account && s.until > now)
            .map(|s| (s.message_id.as_str(), s.at))
            .collect();
        if asleep.is_empty() {
            return;
        }
        let woken = woken_by_replies(&asleep, messages);
        if woken.is_empty() {
            return;
        }
        tracing::info!("snooze: a reply wakes {} early", woken.len());
        for s in &mut self.snoozed {
            if s.account == account && woken.contains(&s.message_id) {
                s.until = now;
            }
        }
        self.save_snoozed();
        self.wake_due();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(timestamp: i64, references: &str) -> Message {
        Message {
            timestamp,
            references: references.into(),
            ..crate::models::test_util::msg(1, 1, 1)
        }
    }

    #[test]
    fn only_a_reply_written_after_the_snooze_wakes_it() {
        let asleep: HashMap<&str, i64> = [("a@x", 100), ("b@x", 100)].into_iter().collect();
        let mail = [
            message(150, "a@x z@x"),
            // Older than the snooze: a reply that was already there.
            message(50, "b@x"),
            message(200, ""),
        ];
        let woken = woken_by_replies(&asleep, &mail);
        assert_eq!(woken, HashSet::from(["a@x".to_string()]));
    }
}
