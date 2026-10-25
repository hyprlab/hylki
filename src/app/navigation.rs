//! Opening a folder or a unified view in the list.

use relm4::prelude::*;

use crate::config;
use crate::models::{FolderKind, Message};
use crate::ui::message_list::MessageListInput;
use crate::ui::message_view::MessageViewInput;
use crate::ui::sidebar::SidebarInput;
use crate::worker::MailRequest;

use super::demo::demo_mode;
use super::model::AppModel;
use super::msg::AppMsg;
use super::search::default_scope;
use super::types::{SelectedFolder, UnifiedView};
use super::unified::LIST_NEWEST;

/// Reads folders not seen since launch whole from the on-disk index, off
/// the main thread: an Archive is tens of thousands of rows.
fn read_unseen_folders(unseen: Vec<(u32, u32, String)>, sender: &ComponentSender<AppModel>) {
    let s = sender.input_sender().clone();
    std::thread::spawn(move || {
        let t = std::time::Instant::now();
        let lists: Vec<((u32, u32), Vec<Message>)> = match crate::cache::Cache::open() {
            Ok(cache) => unseen
                .into_iter()
                .map(|(a, f, path)| ((a, f), cache.load_messages(a, &path, f)))
                .collect(),
            Err(_) => Vec::new(),
        };
        tracing::debug!(
            "unified: {} folders read from the index in {:?}",
            lists.len(),
            t.elapsed()
        );
        let _ = s.send(AppMsg::UnifiedFoldersLoaded { lists });
    });
}

impl AppModel {
    /// More than one switched-on account takes part in the unified section:
    /// with one, or none (every other account left out of it, #267), its
    /// rows would only repeat an account's own. Counted from config, not
    /// from the workers that have reported in: at startup the accounts
    /// stream in one by one, and counting only the connected ones made the
    /// first sidebar build look single-account — its default selection then
    /// landed on that account's inbox (possibly inside a collapsed section,
    /// so nothing visibly highlighted) instead of the "All Inboxes" the app
    /// should open with.
    pub(super) fn multi_account(&self) -> bool {
        self.config.iter().filter(|c| c.enabled && c.in_unified).count() > 1
            // The demo has no config-file accounts, but its mock accounts
            // deserve the same All Inboxes opening as a real multi-account setup.
            || (demo_mode() && self.accounts.iter().filter(|a| self.in_unified(a.id)).count() > 1)
    }

    /// The sidebar has an Inboxes row: the preference is on and there is
    /// more than one account to merge.
    pub(super) fn unified_inboxes_shown(&self) -> bool {
        self.show_unified_pref && self.multi_account()
    }

    /// An account joined or left the unified section (#267): the unified
    /// Tags lists are read again, and the unified view that was open
    /// (`open`) is loaded afresh over the accounts now in it. When its row
    /// went with the change (one account or none left in the section), the
    /// sidebar has already moved to the first inbox instead.
    pub(super) fn reload_unified_views(
        &mut self,
        open: Option<UnifiedView>,
        sender: &ComponentSender<Self>,
    ) {
        self.tag_view_cache.retain(|(scope, _), _| scope.is_some());
        if self
            .tag_view
            .as_ref()
            .is_some_and(|(scope, _)| scope.is_none())
        {
            self.refresh_tag_view(sender);
        }
        let Some(view) = open else { return };
        let listed = match view {
            UnifiedView::Kind(FolderKind::Inbox) => self.unified_inboxes_shown(),
            UnifiedView::Kind(kind) => self.multi_account() && self.unified_kinds.has(kind),
            UnifiedView::Filtered => self.filtered_shown(),
            UnifiedView::Favorites => {
                self.favorites_placement.shown() && !self.favorites.is_empty()
            }
        };
        if listed {
            // A reconnect has emptied the account list; each account's
            // folders asks for its slice again as they arrive.
            self.unified_boot_requested.clear();
            let account = self.unified_account.filter(|a| self.in_unified(*a));
            self.open_unified_in(view, account, sender);
        }
    }

    /// Open a unified view (Inboxes, Starred, Sent, …, Filtered): the merged
    /// list of every account's folder of that kind. What clicking the
    /// sidebar row does; a notification click lands here too.
    pub(super) fn open_unified(&mut self, view: UnifiedView, sender: &ComponentSender<Self>) {
        self.open_unified_in(view, None, sender);
    }

    /// Open a unified view, narrowed to one account when `account` names one.
    fn open_unified_in(
        &mut self,
        view: UnifiedView,
        account: Option<u32>,
        sender: &ComponentSender<Self>,
    ) {
        let t_open = std::time::Instant::now();
        if view == UnifiedView::Kind(FolderKind::Inbox) && !demo_mode() {
            config::save_last_view(config::LastView::default());
        }
        self.close_sidebar_peek();
        self.mirror_selection(match view {
            UnifiedView::Kind(kind) if let Some(acc) = account => {
                crate::ui::sidebar::Sel::UnifiedKindRow(kind, acc)
            }
            UnifiedView::Kind(FolderKind::Inbox) => crate::ui::sidebar::Sel::Unified,
            UnifiedView::Kind(kind) => crate::ui::sidebar::Sel::UnifiedKind(kind),
            UnifiedView::Filtered => crate::ui::sidebar::Sel::UnifiedFiltered,
            UnifiedView::Favorites => crate::ui::sidebar::Sel::UnifiedFavorites,
        });
        self.leave_gallery();
        self.showing_contacts = false;
        self.showing_outbox = false;
        // Another view's slices are another view's: start afresh
        // (the loads below refill them).
        if self.unified_view != view || self.unified_account != account {
            // Freed after the switch is drawn: an Archive's slices are tens
            // of thousands of messages.
            let mut old = Some(std::mem::take(&mut self.unified_slices));
            gtk::glib::idle_add_local_once(move || drop(old.take()));
            self.unified_boot_requested.clear();
        }
        self.unified_unfilled.clear();
        self.unified_view = view;
        self.unified_account = account;
        self.unified = true;
        self.tag_view = None;
        self.smart_view = None;
        self.selected = None;
        if self.seen_counts {
            self.push_unread_counts_later(sender);
        }
        self.blank_reader();
        self.reset_list_for_unified(view, account);
        let reqs = self.unified_targets();
        let unseen = self.seed_unified_slices(&reqs);
        self.fill_unified_starred();
        self.show_unified_view(sender);
        // Request every account's inbox; each result replaces that
        // account's slice as it arrives.
        for (account_id, folder_id, path) in reqs {
            self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
        }
        if view == UnifiedView::Kind(FolderKind::Starred) {
            self.refresh_unified_starred(sender);
        }
        if !unseen.is_empty() && self.cache.is_some() {
            read_unseen_folders(unseen, sender);
        }
        self.push_index_complete();
        self.shed_large_folders();
        tracing::info!(
            "unified {view:?}: opened in {:?} with {} cached messages",
            t_open.elapsed(),
            self.unified_slices.values().map(Vec::len).sum::<usize>()
        );
    }

    /// The list and the reader set up for a merged view: nothing selected,
    /// the scope, colors, recipient names and what rows can do as that
    /// kind of folder has them.
    fn reset_list_for_unified(&self, view: UnifiedView, account: Option<u32>) {
        self.message_list.emit(MessageListInput::SetSelected(None));
        self.message_list.emit(MessageListInput::ResetPaging);
        self.message_list
            .emit(MessageListInput::SetScope(default_scope(
                view == UnifiedView::Kind(FolderKind::Inbox),
            )));
        self.message_list
            .emit(MessageListInput::SetColorize(account.is_none()));
        // A Sent view's rows all come from you — name the recipients.
        self.message_list.emit(MessageListInput::SetShowRecipient(
            view == UnifiedView::Kind(FolderKind::Sent),
        ));
        // Unified Trash and Junk offer the way back as each account's own
        // folder does (#369); the move and Not Spam go per message.
        let restorable = matches!(
            view,
            UnifiedView::Kind(FolderKind::Trash | FolderKind::Junk)
        );
        self.message_list
            .emit(MessageListInput::SetRestorable(restorable));
        self.message_list.emit(MessageListInput::SetInJunk(
            view == UnifiedView::Kind(FolderKind::Junk),
        ));
        self.message_list.emit(MessageListInput::SetInDrafts(
            view == UnifiedView::Kind(FolderKind::Drafts),
        ));
        // Each message is checked when it is snoozed (#527).
        self.message_list.emit(MessageListInput::SetSnoozeOffer(
            (view != UnifiedView::Kind(FolderKind::Drafts)).then_some(false),
        ));
        self.message_list
            .emit(MessageListInput::SetInTemplates(false));
        self.message_view.emit(MessageViewInput::SetDraftsView(
            view == UnifiedView::Kind(FolderKind::Drafts),
        ));
    }

    /// Keep every account's last known slice and top it up from the
    /// folder caches, the way opening a single folder does. This used
    /// to clear the lot and wait: an account whose worker was slow to
    /// answer — busy backfilling a large mailbox, reconnecting, or
    /// offline — was simply absent from "All Inboxes", while its own
    /// Inbox, served from cache, still listed its mail. Each account's
    /// slice is replaced when its load lands.
    ///
    /// Returns the folders not seen since launch, read whole off the main
    /// thread afterwards.
    fn seed_unified_slices(&mut self, reqs: &[(u32, u32, String)]) -> Vec<(u32, u32, String)> {
        let mut unseen: Vec<(u32, u32, String)> = Vec::new();
        for (account_id, folder_id, path) in reqs {
            // A folder not seen since launch (only inboxes are primed
            // at startup): the on-disk index has it as last synced,
            // so the view opens at once — and after a restart — rather
            // than waiting on the worker.
            let seen = self
                .message_cache
                .get(&(*account_id, *folder_id))
                .is_some_and(|c| !c.is_empty());
            // Its newest messages now, from the index, and the rest off the
            // main thread: an Archive is tens of thousands of rows, and the
            // click would wait on all of them. The newest are this view's
            // alone; the folder's own list is only ever the whole folder.
            if !seen {
                let newest = self
                    .cache
                    .as_ref()
                    .map(|c| c.load_newest(*account_id, path, *folder_id, LIST_NEWEST))
                    .unwrap_or_default();
                // Nothing newest is nothing at all.
                if !newest.is_empty() {
                    let newest = self.overlay_local_state(*account_id, *folder_id, newest);
                    self.unified_slices
                        .insert((*account_id, *folder_id), newest);
                    unseen.push((*account_id, *folder_id, path.clone()));
                }
            }
            // Copied from the folder's cache below, or after the frame when
            // the list has the view kept.
            self.unified_unfilled.insert((*account_id, *folder_id));
        }
        // Forget folders that no longer contribute (an account
        // removed or disabled, a rule gone, since the last visit).
        let live: std::collections::HashSet<(u32, u32)> =
            reqs.iter().map(|(a, f, _)| (*a, *f)).collect();
        self.unified_slices.retain(|k, _| live.contains(k));
        self.unified_unfilled
            .retain(|k| self.message_cache.get(k).is_some_and(|c| !c.is_empty()));
        unseen
    }

    /// The list keeps the views it showed as built: one whose content is
    /// the same as then comes back at once, without a merge or a regroup.
    fn show_unified_view(&mut self, sender: &ComponentSender<Self>) {
        let key = self.unified_view_key();
        self.list_fp.set(None);
        if self.unified_slices.is_empty() && self.unified_unfilled.is_empty() {
            self.message_list.emit(MessageListInput::SetView {
                key: Some(key),
                fp: 0,
                restore: false,
            });
            self.message_list.emit(MessageListInput::SetLoading);
        } else {
            let fp = self.unified_fp();
            let kept = self.message_list.model().has_view(key, fp);
            if !kept {
                tracing::debug!(
                    "unified: not kept by the list ({})",
                    self.message_list.model().view_miss(key, fp)
                );
            }
            self.message_list.emit(MessageListInput::SetView {
                key: Some(key),
                fp,
                restore: kept,
            });
            if kept {
                self.list_fp.set(Some(fp));
                let s = sender.input_sender().clone();
                gtk::glib::idle_add_local_once(move || {
                    let _ = s.send(AppMsg::FillUnifiedSlices);
                });
            } else {
                self.fill_unified_slices();
                self.emit_unified();
            }
        }
    }

    /// Switch the message list to a folder: reset the view, show its cached
    /// messages instantly (if any), and kick off a background sync. Shared by the
    /// sidebar selection and the "open message from notification" flow.
    pub(super) fn select_folder(
        &mut self,
        account_id: u32,
        folder_id: u32,
        _name: String,
        path: String,
    ) {
        // For "Open at startup" (#256). The demo's accounts are not real.
        if let Some(email) = self.email_of(account_id).filter(|_| !demo_mode()) {
            config::save_last_view(config::LastView {
                email,
                path: path.clone(),
            });
        }
        self.leave_gallery();
        self.showing_contacts = false;
        self.showing_outbox = false;
        // Mirror the selection in the sidebar. Navigation that starts in the
        // sidebar hits its already-selected guard; navigation from anywhere
        // else ("Go to Message", a notification) moves the highlight — which
        // also lets the Attachments row be clicked again to return.
        self.sidebars_emit(SidebarInput::SelectFolderRow {
            account_id,
            path: path.clone(),
        });
        self.unified = false;
        self.tag_view = None;
        self.smart_view = None;
        self.attachments.clear();
        self.sync_attachment_drawer();
        self.attachments_loading = false;
        self.message_list.emit(MessageListInput::SetSelected(None));
        self.message_list.emit(MessageListInput::ResetPaging);
        self.message_list
            .emit(MessageListInput::SetScope(default_scope(
                self.folder_kind(account_id, folder_id) == Some(FolderKind::Inbox),
            )));
        self.message_list.emit(MessageListInput::SetColorize(false));
        // A Sent folder's rows all come from you — name the recipients (#27).
        let is_sent = self.folder_kind(account_id, folder_id) == Some(FolderKind::Sent);
        self.message_list
            .emit(MessageListInput::SetShowRecipient(is_sent));
        // Trash and Junk offer the way back to the Inbox (#138); Junk's is
        // "Not Spam" (#168).
        let kind = self.folder_kind(account_id, folder_id);
        let restorable = kind.is_some_and(|k| matches!(k, FolderKind::Trash | FolderKind::Junk));
        self.message_list
            .emit(MessageListInput::SetRestorable(restorable));
        self.message_list
            .emit(MessageListInput::SetInJunk(kind == Some(FolderKind::Junk)));
        // A template is neither read nor unread either.
        self.message_list
            .emit(MessageListInput::SetInDrafts(matches!(
                kind,
                Some(FolderKind::Drafts | FolderKind::Templates)
            )));
        self.message_list.emit(MessageListInput::SetInTemplates(
            kind == Some(FolderKind::Templates),
        ));
        self.message_list.emit(MessageListInput::SetSnoozeOffer(
            self.snooze_offer(account_id, kind),
        ));
        self.message_view.emit(MessageViewInput::SetDraftsView(
            kind == Some(FolderKind::Drafts),
        ));
        self.selected = Some(SelectedFolder {
            account_id,
            folder_id,
            path: path.clone(),
        });
        // Opening a folder is looking at it (#343).
        if self.seen_counts {
            self.push_unread_counts();
        }
        self.current = None;
        self.current_thread.clear();
        self.show_message(None, false);
        match self.message_cache.get(&(account_id, folder_id)) {
            Some(cached) => self.message_list.emit(MessageListInput::SetMessages {
                messages: cached.to_vec(),
            }),
            None => {
                // Not in memory: the folder's newest from the index at once,
                // the whole of it when the account answers.
                let newest = self
                    .cache
                    .as_ref()
                    .map(|c| c.load_newest(account_id, &path, folder_id, LIST_NEWEST))
                    .unwrap_or_default();
                if newest.is_empty() {
                    self.message_list.emit(MessageListInput::SetLoading);
                } else {
                    let newest = self.overlay_local_state(account_id, folder_id, newest);
                    self.message_list
                        .emit(MessageListInput::SetMessages { messages: newest });
                }
            }
        }
        self.push_index_complete();
        self.shed_large_folders();
        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_navigation(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::UnifiedSelected(view) => {
                self.open_unified(view, &sender);
            }

            AppMsg::AccountStarredSelected(account_id) => {
                self.open_unified_in(
                    UnifiedView::Kind(FolderKind::Starred),
                    Some(account_id),
                    &sender,
                );
            }

            AppMsg::FolderSelected {
                account_id,
                folder_id,
                name,
                path,
            } => {
                self.close_sidebar_peek();
                self.select_folder(account_id, folder_id, name, path);
            }
            _ => unreachable!("not routed to on_navigation"),
        }
    }
}
