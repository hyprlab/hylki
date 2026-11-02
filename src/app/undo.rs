//! Undo and redo (#200): the steps recorded for each action and applying
//! them, instantly on screen when the action was recent.

use std::collections::HashMap;

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::{i18n, i18n_f};
use crate::models::{FolderKind, Message};
use crate::ui::compose::ComposeInput;
use crate::ui::message_list::MessageListInput;
use crate::worker::MailRequest;

use super::compose_host::ReaderCompose;
use super::conversation::{body_key, CARRIED_BODY_LIMIT};
use super::model::AppModel;
use super::msg::AppMsg;

/// Which flag a recorded change touched (#200).
#[derive(Clone, Debug, PartialEq)]
pub(super) enum FlagKind {
    Read,
    Star,
    Tag(String),
}

/// One message a flag change covers. Folder ids are re-checked when the
/// change is applied — they are index-based and shift as folders come and go.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FlagRef {
    pub(super) folder_id: u32,
    pub(super) uid: u32,
    /// The list row's id, which is how the setters find every copy of it.
    pub(super) id: u32,
}

/// Put `kind` to `value` on each of `items`. Only messages the original
/// action actually changed are listed, so one value covers them all.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FlagChange {
    pub(super) kind: FlagKind,
    pub(super) value: bool,
    pub(super) items: Vec<FlagRef>,
}

/// One step of the undo/redo history (#200). A step is a thing to *do*: the
/// undo stack holds the step that reverses each action, and applying a step
/// pushes its [`UndoStep::inverse`] onto the other stack. Operations with no
/// inverse — a permanent delete, an emptied folder, a sent message — are
/// never recorded.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum UndoStep {
    /// Take `message_ids` out of `from` and put them in `to`. Symmetric, so
    /// redoing a move is the same request with the two folders swapped.
    Move {
        from: String,
        to: String,
        message_ids: Vec<String>,
    },
    /// Set flags back to what they were.
    Flags(Vec<FlagChange>),
    CreateFolder {
        path: String,
    },
    /// Only ever the inverse of a [`UndoStep::CreateFolder`]: deleting a
    /// folder the user made is undoing that, not a delete of its own. A
    /// folder the user deletes is not undoable at all and never recorded.
    DeleteFolder {
        path: String,
    },
    RenameFolder {
        from: String,
        to: String,
    },
}

impl UndoStep {
    /// The step that puts this one back.
    fn inverse(&self) -> UndoStep {
        match self {
            UndoStep::Move {
                from,
                to,
                message_ids,
            } => UndoStep::Move {
                from: to.clone(),
                to: from.clone(),
                message_ids: message_ids.clone(),
            },
            UndoStep::Flags(changes) => UndoStep::Flags(
                changes
                    .iter()
                    .map(|c| FlagChange {
                        value: !c.value,
                        ..c.clone()
                    })
                    .collect(),
            ),
            UndoStep::CreateFolder { path } => UndoStep::DeleteFolder { path: path.clone() },
            UndoStep::DeleteFolder { path } => UndoStep::CreateFolder { path: path.clone() },
            UndoStep::RenameFolder { from, to } => UndoStep::RenameFolder {
                from: to.clone(),
                to: from.clone(),
            },
        }
    }
}

/// One entry on the undo (or redo) stack: the step to apply, and what the
/// user called the action it belongs to, so the menu can say "Undo Archive"
/// rather than just "Undo".
pub(super) struct UndoEntry {
    pub(super) account_id: u32,
    pub(super) what: String,
    pub(super) step: UndoStep,
    /// When the action happened, for the [`INSTANT_UNDO`] window.
    pub(super) at: std::time::Instant,
    /// The rows a move took off screen, kept whole (bodies included) so a
    /// quick undo can put them straight back without a round trip.
    pub(super) rows: Vec<Message>,
    /// The conversations those rows were part of, by Message-ID. Taken as
    /// the move is recorded: moving a message forgets its account's
    /// conversations, so by the time the undo runs they are gone.
    pub(super) threads: Vec<(String, Vec<Message>)>,
    /// Whether applying this entry puts `rows` back on screen (undoing a
    /// move) or takes them away again (redoing it).
    pub(super) rows_return: bool,
    /// Steps on other accounts that belong to the same action, applied with
    /// `step`: a move to another account (#265) is undone by bringing the
    /// original back on one account and taking the copy away on the other.
    pub(super) also: Vec<(u32, UndoStep)>,
}

/// How long after an action its undo still repaints the list itself rather
/// than waiting for the server (#200). Long enough to cover "that was a
/// mistake", short enough that the rows it puts back are still what is
/// there — past this, another client or a sync may have moved on.
const INSTANT_UNDO: std::time::Duration = std::time::Duration::from_secs(10);

impl AppModel {
    /// Sets or clears one kind of flag on a message, through the setter
    /// for that kind.
    fn set_flag(&mut self, m: &Message, kind: &FlagKind, value: bool) {
        match kind {
            FlagKind::Read => self.set_read(m, value),
            FlagKind::Star => self.set_star(m, value),
            FlagKind::Tag(keyword) => self.set_tag(m, keyword, value),
        }
    }

    /// Record a just-issued move so Ctrl+Z can bring it back. Skips silently
    /// when nothing identifies the messages (no Message-ID) — an unrecordable
    /// move simply isn't undoable.
    pub(super) fn push_undo_move(
        &mut self,
        account_id: u32,
        moved_to: &str,
        restore_to: &str,
        rows: Vec<Message>,
    ) {
        let what = self.move_label(account_id, moved_to);
        self.push_undo_move_as(account_id, what, moved_to, restore_to, rows);
    }

    /// [`push_undo_move`], for the moves whose destination doesn't name them:
    /// Not Spam puts mail back in the Inbox, which is not "Move to Inbox".
    pub(super) fn push_undo_move_as(
        &mut self,
        account_id: u32,
        what: String,
        moved_to: &str,
        restore_to: &str,
        rows: Vec<Message>,
    ) {
        // A message with no Message-ID can't be found again on the server, so
        // it can't be part of the step — and a step of nothing isn't undoable.
        let rows: Vec<Message> = rows
            .into_iter()
            .filter(|m| !m.message_id.is_empty())
            .collect();
        if rows.is_empty() {
            return;
        }
        let ids: Vec<String> = rows.iter().map(|m| m.message_id.clone()).collect();
        // Keep each row's body with it. The reader renders from the message's
        // own body before it looks anywhere else, so a restored row opens
        // instantly instead of asking the server for a UID the move has
        // already retired.
        let rows: Vec<Message> = rows
            .into_iter()
            .map(|mut m| {
                if m.body.is_empty() {
                    if let Some(body) = self.body_cache.get(&body_key(&m)) {
                        m.body = body.clone();
                    }
                }
                m
            })
            .collect();
        let threads: Vec<(String, Vec<Message>)> = rows
            .iter()
            .filter_map(|m| {
                self.thread_cache
                    .get(&body_key(m))
                    .map(|t| (m.message_id.clone(), t.clone()))
            })
            .collect();
        self.push_undo_entry(UndoEntry {
            account_id,
            what,
            step: UndoStep::Move {
                from: moved_to.to_string(),
                to: restore_to.to_string(),
                message_ids: ids,
            },
            at: std::time::Instant::now(),
            rows,
            threads,
            rows_return: true,
            also: Vec::new(),
        });
    }

    /// What to call a move in the Undo menu, taken from where it landed:
    /// the named actions where the destination has a role, "Move to X"
    /// otherwise.
    pub(super) fn move_label(&self, account_id: u32, dest: &str) -> String {
        let folder = self.folder_by_path(account_id, dest);
        match folder.map(|f| f.kind) {
            Some(FolderKind::Trash) => i18n("Delete"),
            Some(FolderKind::Archive) => i18n("Archive"),
            Some(FolderKind::Junk) => i18n("Mark as Spam"),
            _ => {
                let name = folder
                    .map(|f| f.name.clone())
                    .unwrap_or_else(|| dest.to_string());
                i18n_f("Move to {name}", &[("name", &name)])
            }
        }
    }

    /// The same for a tag, named as the user knows it.
    pub(super) fn tag_label(&self, keyword: &str, add: bool) -> String {
        let name = self
            .tags
            .iter()
            .find(|t| t.keyword.eq_ignore_ascii_case(keyword))
            .map(|t| t.name.clone())
            .unwrap_or_else(|| keyword.to_string());
        if add {
            i18n_f("Add Tag {name}", &[("name", &name)])
        } else {
            i18n_f("Remove Tag {name}", &[("name", &name)])
        }
    }

    /// Record one reversible action (#200). The step is what *undoes* it.
    /// A new action ends whatever redo branch was open, as undo histories do.
    pub(super) fn push_undo(&mut self, account_id: u32, what: String, step: UndoStep) {
        self.push_undo_entry(UndoEntry {
            account_id,
            what,
            step,
            at: std::time::Instant::now(),
            rows: Vec::new(),
            threads: Vec::new(),
            rows_return: true,
            also: Vec::new(),
        });
    }

    pub(super) fn push_undo_entry(&mut self, entry: UndoEntry) {
        self.undo_stack.push(entry);
        self.redo_stack.clear();
        self.refresh_undo_menu();
    }

    /// Who the window's Undo and Redo entries belong to. Not the same
    /// question as [`AppModel::focused_compose`], and deliberately not
    /// decided by focus: opening the menu is itself a focus change, and
    /// whether it lands on the button, inside the popover, or somewhere in
    /// between is GTK's business and varies with how the menu was opened.
    /// An entry that reads correctly only until you reach for it is no use.
    ///
    /// So: an open inline composer with something in its history owns them.
    /// Anything it can take back is more recent than anything in the mail
    /// history — it is being written now — and while it has nothing the
    /// entries go back to the mail, which is what someone with an untouched
    /// reply on screen would expect. The keys keep following focus, because
    /// Ctrl+Z in the body must undo the body.
    fn menu_compose(&self) -> Option<&ReaderCompose> {
        self.reader_compose.as_ref().filter(|r| {
            r.window.is_none()
                && (self.compose_history.0.is_some() || self.compose_history.1.is_some())
        })
    }

    /// Ctrl+Z / Ctrl+Shift+Z: take the top of one stack, apply it, and put
    /// its inverse on the other so the move can be made again.
    ///
    /// Typing into the inline composer hands both over to it: its history
    /// covers the body and its attachments, and nobody writing a reply means
    /// "archive that message again" by Ctrl+Z. The menu entries follow the
    /// same rule, so what they say is always what the key would do.
    fn undo_redo(&mut self, redo: bool) {
        if let Some(compose) = self.menu_compose() {
            compose.controller.emit(ComposeInput::History { redo });
            return;
        }
        let entry = if redo {
            self.redo_stack.pop()
        } else {
            self.undo_stack.pop()
        };
        // Nothing to say when there is nothing to do: the menu entry is
        // already greyed out, and the key should just be inert (#200).
        let Some(entry) = entry else { return };
        let back = UndoEntry {
            account_id: entry.account_id,
            what: entry.what.clone(),
            step: entry.step.inverse(),
            // The window restarts: whatever the state is now, it is current.
            at: std::time::Instant::now(),
            rows: entry.rows.clone(),
            threads: entry.threads.clone(),
            rows_return: !entry.rows_return,
            also: entry
                .also
                .iter()
                .map(|(a, step)| (*a, step.inverse()))
                .collect(),
        };
        // Hold on to the bodies whatever happens: the move is about to change
        // these messages' UIDs, and the reload that follows would otherwise
        // find no body for them and go back to the server for one.
        if entry.rows_return {
            if self.carried_bodies.len() > CARRIED_BODY_LIMIT {
                self.carried_bodies.clear();
            }
            for row in entry.rows.iter().filter(|m| !m.body.is_empty()) {
                self.carried_bodies
                    .insert((entry.account_id, row.message_id.clone()), row.body.clone());
            }
            for (message_id, thread) in &entry.threads {
                self.carried_threads
                    .insert((entry.account_id, message_id.clone()), thread.clone());
            }
            tracing::debug!(
                target: "hylki::undo",
                "carrying {} bodies and {} conversations over the move",
                entry.rows.iter().filter(|m| !m.body.is_empty()).count(),
                entry.threads.len(),
            );
        }
        // Repaint the list now rather than a few round trips from now, while
        // the rows we took off it are still known to be what is there (#200).
        // The server request still goes out; its reload lands behind this and
        // replaces these rows with the real ones, UIDs and all.
        if entry.at.elapsed() <= INSTANT_UNDO && !entry.rows.is_empty() {
            self.instant_move(&entry);
        }
        // A step that could not be carried out is dropped rather than put
        // back: leaving it on top would jam the key on something that will
        // fail again every time. The entries under it are usually about other
        // messages entirely, so the rest of the history stands.
        if self.apply_undo_step(entry.account_id, &entry.step) {
            for (account_id, step) in &entry.also {
                if self.apply_undo_step(*account_id, step) {
                    // The step reloads the folder the mail goes into; the one
                    // it leaves is looked at again here, so its rows go too.
                    if let UndoStep::Move { from, .. } = step {
                        if let Some(folder_id) = self
                            .folders
                            .get(account_id)
                            .and_then(|fs| fs.iter().find(|f| &f.path == from))
                            .map(|f| f.id)
                        {
                            self.send_to(
                                *account_id,
                                MailRequest::SyncFolder {
                                    folder_id,
                                    path: from.clone(),
                                },
                            );
                        }
                    }
                }
            }
            if redo {
                self.undo_stack.push(back);
            } else {
                self.redo_stack.push(back);
            }
        }
        self.refresh_undo_menu();
    }

    /// Put a move's rows back on screen (or take them away again), without
    /// waiting for the server (#200). Only ever called inside the
    /// [`INSTANT_UNDO`] window, where the rows are still the truth.
    fn instant_move(&mut self, entry: &UndoEntry) {
        let UndoStep::Move { from, to, .. } = &entry.step else {
            return;
        };
        let account_id = entry.account_id;
        // The folder the rows are in right now: where the step will put them
        // when it is returning them, and where it will take them from when it
        // is not.
        let here = if entry.rows_return { to } else { from };
        let Some(folder_id) = self.folder_by_path(account_id, here).map(|f| f.id) else {
            return;
        };
        if !entry.rows_return {
            // Redoing the move: take the rows off the list again, using the
            // copies the caches hold now — a reload may have landed since,
            // and those carry the UIDs the next action will need.
            for row in &entry.rows {
                let current = self
                    .message_cache
                    .get(&(account_id, folder_id))
                    .and_then(|ms| ms.iter().find(|c| c.message_id == row.message_id).cloned());
                if let Some(m) = current {
                    self.discard_message(&m);
                }
            }
            return;
        }
        let known: std::collections::HashSet<String> = self
            .message_cache
            .get(&(account_id, folder_id))
            .map(|ms| ms.iter().map(|m| m.message_id.clone()).collect())
            .unwrap_or_default();
        let mut added = false;
        for row in &entry.rows {
            // The reload may have beaten us to it, or the user may have
            // pressed twice; either way the row is already there.
            if known.contains(&row.message_id) {
                continue;
            }
            let mut m = row.clone();
            // Folder ids are positional and shift as folders come and go.
            m.folder_id = folder_id;
            if m.unread {
                *self
                    .folder_unread
                    .entry((account_id, folder_id))
                    .or_default() += 1;
            }
            if self.is_unified_target(account_id, folder_id) {
                self.unified_slices
                    .entry((account_id, folder_id))
                    .or_default()
                    .push(m.clone());
            }
            self.message_cache
                .entry((account_id, folder_id))
                .or_default()
                .push(m);
            added = true;
        }
        tracing::debug!(target: "hylki::undo", "instant restore: added={added} to folder {folder_id}");
        if !added {
            return;
        }
        let key = (account_id, folder_id);
        if let Some(ms) = self.message_cache.get_mut(&key) {
            ms.sort_by_key(|m| std::cmp::Reverse(m.timestamp));
        }
        if let Some(ms) = self.unified_slices.get_mut(&key) {
            ms.sort_by_key(|m| std::cmp::Reverse(m.timestamp));
        }
        self.forget_threads(account_id);
        self.refresh_list_display();
        self.push_unread_counts();
        // Undo means "put that back", and what you want back in front of you
        // is the message itself, not whatever the list moved on to when it
        // went away. Select it now, with the row, rather than when the server
        // gets round to confirming — which is the whole point of doing this
        // here (#200). The list processes SetMessages first, so the row is
        // there to select. Restoring several at once picks the newest, the
        // one nearest the top of the list.
        let newest = entry
            .rows
            .iter()
            .max_by_key(|m| m.timestamp)
            .map(|m| (account_id, m.id));
        tracing::debug!(target: "hylki::undo", "instant restore: selecting {newest:?}");
        if let Some(key) = newest {
            self.message_list.emit(MessageListInput::SelectAndLoad(key));
        }
    }

    /// Carry out one step. Returns false when it could not be done at all
    /// (the folder is gone, the messages are no longer where they were), in
    /// which case the step is dropped rather than left to fail again.
    fn apply_undo_step(&mut self, account_id: u32, step: &UndoStep) -> bool {
        match step {
            UndoStep::Move {
                from,
                to,
                message_ids,
            } => {
                let Some(dest_folder_id) = self.folder_by_path(account_id, to).map(|f| f.id) else {
                    tracing::info!("undo: {to:?} is no longer there, dropping the step");
                    return false;
                };
                self.send_to(
                    account_id,
                    MailRequest::UndoMove {
                        path: from.clone(),
                        dest: to.clone(),
                        dest_folder_id,
                        message_ids: message_ids.clone(),
                    },
                );
                self.follow_undo_move(account_id, from, to, message_ids);
                // Finding, moving and reloading the messages takes a few round
                // trips — spin the refresh indicator until the worker's
                // BulkComplete says it has landed.
                self.bulk_pending += 1;
                self.update_busy_indicator();
                true
            }
            UndoStep::Flags(changes) => {
                let mut touched = 0usize;
                for change in changes {
                    for item in &change.items {
                        let Some(m) = self
                            .message_cache
                            .get(&(account_id, item.folder_id))
                            .and_then(|msgs| {
                                msgs.iter()
                                    .find(|m| m.uid == item.uid && m.id == item.id)
                                    .cloned()
                            })
                        else {
                            continue;
                        };
                        touched += 1;
                        self.set_flag(&m, &change.kind, change.value);
                    }
                }
                if touched == 0 {
                    tracing::info!("undo: those messages are no longer here, dropping the step");
                }
                touched > 0
            }
            UndoStep::CreateFolder { path } => {
                self.create_folder(account_id, path.clone());
                true
            }
            UndoStep::DeleteFolder { path } => {
                self.delete_folder(account_id, path.clone());
                true
            }
            UndoStep::RenameFolder { from, to } => {
                if !self
                    .folders
                    .get(&account_id)
                    .is_some_and(|fs| fs.iter().any(|f| &f.path == from))
                {
                    tracing::info!("undo: {from:?} is no longer there, dropping the step");
                    return false;
                }
                self.apply_folder_rename(account_id, from.clone(), to.clone(), None);
                true
            }
        }
    }

    /// (Re)build the burger menu's Undo/Redo section, naming the action each
    /// one would reverse, and grey the entries out when a stack is empty.
    pub(super) fn refresh_undo_menu(&self) {
        // The composer's history is the one on offer whenever it has one.
        let composing = self.menu_compose().is_some();
        let (undo_what, redo_what) = if composing {
            (
                self.compose_history.0.clone(),
                self.compose_history.1.clone(),
            )
        } else {
            (
                self.undo_stack.last().map(|e| e.what.clone()),
                self.redo_stack.last().map(|e| e.what.clone()),
            )
        };
        let label = |verb: &str, what: Option<&String>| match what {
            Some(w) => format!("{verb} {w}"),
            None => verb.to_string(),
        };
        // The shortcut is shown with the "accel" attribute rather than a real
        // accelerator: registering one would have GTK match Ctrl+Z before the
        // keystroke reached whatever has focus, taking text undo away from
        // entries and the composer. The key handler keeps that distinction,
        // and this only draws the reminder next to the entry.
        let item = |text: String, action: &str, accel: &str| {
            let item = gtk::gio::MenuItem::new(Some(&text), Some(action));
            item.set_attribute_value("accel", Some(&accel.to_variant()));
            item
        };
        self.undo_menu.remove_all();
        self.undo_menu.append_item(&item(
            label(&i18n("Undo"), undo_what.as_ref()),
            "win.undo",
            "<Control>z",
        ));
        self.undo_menu.append_item(&item(
            label(&i18n("Redo"), redo_what.as_ref()),
            "win.redo",
            "<Control><Shift>z",
        ));
        if std::env::var_os("HYLKI_SHOWCASE_COMPOSE_UNDO").is_some() {
            let mut chain = Vec::new();
            let mut node = gtk::prelude::GtkWindowExt::focus(&self.window);
            while let Some(w) = node {
                chain.push(format!("{}", w.type_()));
                node = w.parent();
            }
            tracing::info!(
                target: "hylki::compose::undo",
                "menu: composing={composing} undo={undo_what:?} redo={redo_what:?} focus=[{}]",
                chain.join(" < ")
            );
        }
        if let Some(a) = &self.undo_action {
            a.set_enabled(undo_what.is_some());
        }
        if let Some(a) = &self.redo_action {
            a.set_enabled(redo_what.is_some());
        }
    }

    /// A read/star/tag change the user asked for: applied, and recorded so
    /// Ctrl+Z puts it back (#200). Only messages the change actually moves
    /// are recorded, so one value covers every one of them. Automatic marks
    /// — reading a message marks it read — use the plain setters and are
    /// deliberately not recorded: they are not actions anyone took.
    pub(super) fn user_set_flags(
        &mut self,
        what: String,
        msgs: &[Message],
        kind: FlagKind,
        value: bool,
    ) {
        let mut by_account: HashMap<u32, Vec<FlagRef>> = HashMap::new();
        for m in msgs {
            let changes = match &kind {
                // A draft is neither read nor unread, and a queued message
                // has no server flags at all: the setters decline both, so
                // recording them would leave dead entries in the history.
                FlagKind::Read => {
                    m.unread == value && !self.is_drafts_folder(m.account_id, m.folder_id)
                }
                FlagKind::Star => m.starred != value,
                FlagKind::Tag(keyword) => {
                    self.carries_tag(m, keyword) != value
                        && self.outbox_item(m.account_id, m.id).is_none()
                }
            };
            if !changes {
                continue;
            }
            by_account.entry(m.account_id).or_default().push(FlagRef {
                folder_id: m.folder_id,
                uid: m.uid,
                id: m.id,
            });
            self.set_flag(m, &kind, value);
        }
        // One entry per account: undo sends its requests to one worker.
        for (account_id, items) in by_account {
            self.push_undo(
                account_id,
                what.clone(),
                UndoStep::Flags(vec![FlagChange {
                    kind: kind.clone(),
                    value: !value,
                    items,
                }]),
            );
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_undo(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::Undo => self.undo_redo(false),

            AppMsg::Redo => self.undo_redo(true),

            AppMsg::UndoRestored {
                account_id,
                folder_id,
                message_ids,
            } => {
                // The undone move landed and the folder's reload has already
                // been processed (the worker sends Messages first): put the
                // user on the restored message — selected, loaded in the
                // reader, and scrolled into view — instead of wherever the
                // reload left the list. If the restored message isn't in the
                // current view (folder switched meanwhile), SelectAndLoad
                // finds no row and nothing moves.
                // Unless the reader is already on it, which it will be when
                // the undo put the row back itself (#200): selecting it again
                // would tear the message down and render it a second time for
                // no change the reader can see.
                let already_open = self.current.as_ref().is_some_and(|c| {
                    !c.message_id.is_empty() && message_ids.contains(&c.message_id)
                });
                if already_open {
                    return;
                }
                let restored = self
                    .message_cache
                    .get(&(account_id, folder_id))
                    .and_then(|msgs| msgs.iter().find(|m| message_ids.contains(&m.message_id)))
                    .map(|m| (m.account_id, m.id));
                if let Some(key) = restored {
                    self.message_list.emit(MessageListInput::SelectAndLoad(key));
                }
            }
            _ => unreachable!("not routed to on_undo"),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_undo_step_is_its_own_inverse_twice_over() {
        use super::{FlagChange, FlagKind, FlagRef, UndoStep};
        let steps = vec![
            UndoStep::Move {
                from: "Archive".into(),
                to: "INBOX".into(),
                message_ids: vec!["a@b.c".into()],
            },
            UndoStep::Flags(vec![
                FlagChange {
                    kind: FlagKind::Read,
                    value: false,
                    items: vec![FlagRef {
                        folder_id: 1,
                        uid: 7,
                        id: 7,
                    }],
                },
                FlagChange {
                    kind: FlagKind::Tag("Work".into()),
                    value: true,
                    items: vec![FlagRef {
                        folder_id: 1,
                        uid: 8,
                        id: 8,
                    }],
                },
            ]),
            UndoStep::CreateFolder {
                path: "Projects".into(),
            },
            UndoStep::DeleteFolder {
                path: "Projects".into(),
            },
            UndoStep::RenameFolder {
                from: "Old".into(),
                to: "New".into(),
            },
        ];
        for step in &steps {
            assert_eq!(&step.inverse().inverse(), step, "{step:?} round-trips");
            assert_ne!(&step.inverse(), step, "{step:?} actually reverses");
        }
        // A move undoes by swapping the two folders, which is what makes
        // redo the very same request back the other way.
        assert_eq!(
            steps[0].inverse(),
            UndoStep::Move {
                from: "INBOX".into(),
                to: "Archive".into(),
                message_ids: vec!["a@b.c".into()],
            }
        );
        // Undoing a folder someone made deletes it again; there is no step
        // that undoes a folder the user deleted.
        assert_eq!(
            UndoStep::CreateFolder {
                path: "Projects".into()
            }
            .inverse(),
            UndoStep::DeleteFolder {
                path: "Projects".into()
            }
        );
    }
}
