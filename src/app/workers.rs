//! The mail workers: one per account, and their events turned into AppMsg.

use adw::prelude::*;
use relm4::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::AccountConfig;
use crate::models::FolderKind;
use crate::ui::message_list::MessageListInput;
use crate::ui::message_view::MessageViewInput;
use crate::ui::notifications::NotifyInput;
use crate::ui::sidebar::SidebarInput;
use crate::worker::{self, MailRequest, WorkerEvent};

use super::demo::demo_mode;
use super::model::AppModel;
use super::msg::AppMsg;
use super::sidebar::set_sidebar_header_compact;
use super::sidebar_peek::SIDEBAR_RAIL_WIDTH;
use super::AppModelWidgets;

fn map_event(account_id: u32, event: WorkerEvent) -> AppMsg {
    match event {
        WorkerEvent::AttachmentsScanned {
            folder_path,
            added,
            remaining,
        } => AppMsg::AttachmentsScanned {
            account_id,
            folder_path,
            added,
            remaining,
        },
        WorkerEvent::BulkComplete => AppMsg::BulkComplete,
        WorkerEvent::Related {
            message_id,
            messages,
        } => AppMsg::Related {
            account_id,
            message_id,
            messages,
        },
        WorkerEvent::ThreadSummaries { summaries } => AppMsg::ThreadSummaries {
            account_id,
            summaries,
        },
        WorkerEvent::Account(a) => AppMsg::SetAccount(a),
        WorkerEvent::Identities(identities) => AppMsg::ServerIdentities {
            account_id,
            identities,
        },
        WorkerEvent::Folders(folders) => AppMsg::SetFolders {
            account_id,
            folders,
        },
        WorkerEvent::Messages {
            folder_id,
            messages,
        } => AppMsg::Messages {
            account_id,
            folder_id,
            messages,
        },
        WorkerEvent::BodyHits { folder_id, hits } => AppMsg::BodyHits {
            account_id,
            folder_id,
            hits,
        },
        WorkerEvent::MessagesAppend {
            folder_id,
            messages,
        } => AppMsg::MessagesAppend {
            account_id,
            folder_id,
            messages,
        },
        WorkerEvent::Located { message_id, hit } => AppMsg::MidLocated {
            account_id,
            message_id,
            hit,
        },
        WorkerEvent::KeywordsFound(findings) => AppMsg::KeywordsFound {
            account_id,
            findings,
        },
        WorkerEvent::Quota(quota) => AppMsg::QuotaFound { account_id, quota },
        WorkerEvent::KeywordsSynced { paths } => AppMsg::KeywordsSynced { account_id, paths },
        WorkerEvent::FlagsSynced { paths } => AppMsg::FlagsSynced { account_id, paths },
        WorkerEvent::Restored {
            folder_id,
            message_ids,
        } => AppMsg::UndoRestored {
            account_id,
            folder_id,
            message_ids,
        },
        WorkerEvent::Unsnoozed { message_ids, done } => AppMsg::Unsnoozed {
            account_id,
            message_ids,
            done,
        },
        WorkerEvent::BackfillDone { folder_id } => AppMsg::BackfillDone {
            account_id,
            folder_id,
        },
        WorkerEvent::FolderSynced { folder_id } => AppMsg::FolderSynced {
            account_id,
            folder_id,
        },
        WorkerEvent::FolderUnread { folder_id, unread } => AppMsg::FolderUnread {
            account_id,
            folder_id,
            unread,
        },
        WorkerEvent::FolderUnreadByPath { path, unread } => AppMsg::FolderUnreadByPath {
            account_id,
            path,
            unread,
        },
        WorkerEvent::SeenSettled { path, uid } => AppMsg::SeenSettled {
            account_id,
            path,
            uid,
        },
        WorkerEvent::Answered {
            path,
            uid,
            forwarded,
        } => AppMsg::Answered {
            account_id,
            path,
            uid,
            forwarded,
        },
        WorkerEvent::TagSettled { path, uid, keyword } => AppMsg::TagSettled {
            account_id,
            path,
            uid,
            keyword,
        },
        WorkerEvent::MovesSettled { path, uids } => AppMsg::MovesSettled {
            account_id,
            path,
            uids,
        },
        WorkerEvent::RawExported { token, raw } => AppMsg::RawExported { token, raw },
        WorkerEvent::RawImported { token, result } => AppMsg::RawImported { token, result },
        WorkerEvent::AttachmentDeleted {
            message_id,
            path,
            uid,
            new_uid,
            name,
            size,
        } => AppMsg::AttachmentDeleted {
            account_id,
            message_id,
            path,
            uid,
            new_uid,
            name,
            size,
        },
        WorkerEvent::AttachmentNotDeleted {
            message_id,
            name,
            size,
        } => AppMsg::AttachmentNotDeleted {
            account_id,
            message_id,
            name,
            size,
        },
        WorkerEvent::Gone {
            message_id,
            path,
            uid,
        } => AppMsg::MessageGone {
            account_id,
            message_id,
            path,
            uid,
        },
        WorkerEvent::RefsRepaired { folder_id } => AppMsg::RefsRepaired {
            account_id,
            folder_id,
        },
        WorkerEvent::Body {
            message_id,
            path,
            body,
        } => {
            let body = crate::desktop::legacy::current_markup(body);
            AppMsg::Body {
                account_id,
                message_id,
                path,
                body,
            }
        }
        WorkerEvent::SenderChecked {
            path,
            message_id,
            check,
        } => AppMsg::SenderChecked {
            account_id,
            path,
            message_id,
            check: Box::new(check),
        },
        WorkerEvent::Source { text, .. } => AppMsg::Source { text },
        WorkerEvent::Attachments {
            path,
            message_id,
            items,
        } => AppMsg::Attachments {
            account_id,
            path,
            message_id,
            items,
        },
        WorkerEvent::AttachmentsPending { path, message_id } => AppMsg::AttachmentsPending {
            account_id,
            path,
            message_id,
        },
        WorkerEvent::NoAttachments { path, message_id } => AppMsg::NoAttachments {
            account_id,
            path,
            message_id,
        },
        WorkerEvent::HasAttachments { path, message_id } => AppMsg::HasAttachments {
            account_id,
            path,
            message_id,
        },
        WorkerEvent::Sent => AppMsg::Sent { account_id },
        WorkerEvent::SendDone { ticket, sent } => AppMsg::SendDone { ticket, sent },
        WorkerEvent::Unsent(message) => AppMsg::Unsent {
            account_id,
            message,
        },
        WorkerEvent::Outbox { items } => AppMsg::OutboxItems { account_id, items },
        WorkerEvent::Notice(text) => AppMsg::Notice(text),
        WorkerEvent::DraftSaved {
            autosave,
            message_id,
        } => AppMsg::DraftSaved {
            account_id,
            autosave,
            message_id,
        },
        WorkerEvent::Status(text) => AppMsg::Status { account_id, text },
        WorkerEvent::Error { text, connectivity } => AppMsg::Error {
            account_id,
            text,
            connectivity,
        },
    }
}

/// A Refresh button's two faces: the icon, and the spinner shown while
/// any account is busy.
fn fill_refresh_faces(stack: &gtk::Stack, spinner: &gtk::Spinner) {
    let icon = gtk::Image::from_icon_name("view-refresh-symbolic");
    stack.add_named(&icon, Some("icon"));
    stack.add_named(spinner, Some("spinner"));
    stack.set_visible_child_name("icon");
}

impl AppModel {
    /// Tell the message list whether the folder(s) currently shown are fully
    /// indexed, so it knows whether to expect more rows while scrolling.
    pub(super) fn push_index_complete(&self) {
        self.message_list.emit(MessageListInput::SetIndexComplete(
            self.current_index_complete(),
        ));
    }

    fn current_index_complete(&self) -> bool {
        if self.unified {
            self.unified_targets()
                .iter()
                .all(|(a, f, _)| self.indexed_folders.contains(&(*a, *f)))
        } else if let Some(sel) = &self.selected {
            self.indexed_folders
                .contains(&(sel.account_id, sel.folder_id))
        } else {
            true
        }
    }

    /// (Re)arm the repeating auto-fetch timer to the current interval.
    /// Sockets left open across a suspend or a network drop are dead.
    /// Reconnect drops the stale session, logs in fresh and re-arms IMAP IDLE,
    /// and it unsticks any worker parked inside an IDLE wait, since the
    /// request breaks its select loop. Then reload the visible folder so new
    /// mail appears without waiting for the next auto-fetch.
    fn reconnect_workers(&self, sender: &ComponentSender<Self>) {
        for w in self.workers.values() {
            let _ = w.send(MailRequest::Reconnect);
        }
        sender.input(AppMsg::Refresh);
        // A Gravatar lookup that failed while the network was down gets
        // another go now that it is back (#189).
        self.warm_own_gravatars(sender);
    }

    /// The Refresh buttons and the status bar follow the network (#442).
    fn show_network_state(&self) {
        let offline = !crate::desktop::power::online();
        crate::ui::show_refresh_offline(&self.sidebar_refresh, offline);
        if let Some(button) = self
            .peek_refresh_stack
            .parent()
            .and_downcast::<gtk::Button>()
        {
            crate::ui::show_refresh_offline(&button, offline);
        }
        self.sidebars_emit(SidebarInput::SetOffline(offline));
        self.notifications.emit(NotifyInput::SetOffline(offline));
    }

    /// The work held back on a metered connection may go again: wake every
    /// worker, whose idle loop then picks up what it set aside. A worker
    /// waits on its next request, and a recount is one it does anyway
    /// (#442).
    pub(super) fn saving_data_changed(&self, was: bool) {
        if was && !crate::desktop::power::saving_data() {
            tracing::info!("saving data no longer: resuming the work done ahead");
            for w in self.workers.values() {
                let _ = w.send(MailRequest::RefreshUnread);
            }
        }
    }

    pub(super) fn arm_auto_fetch(&mut self, sender: &ComponentSender<Self>) {
        if let Some(id) = self.auto_fetch_source.take() {
            id.remove();
        }
        if self.fetch_interval_secs > 0 {
            let input = sender.input_sender().clone();
            let secs = self.fetch_interval_secs.min(u32::MAX as u64) as u32;
            let id = gtk::glib::timeout_add_seconds_local(secs, move || {
                // Offline the check would only fail, and a Refresh would
                // try anyway (#442).
                if crate::desktop::power::online() {
                    let _ = input.send(AppMsg::Refresh);
                }
                gtk::glib::ControlFlow::Continue
            });
            self.auto_fetch_source = Some(id);
        }
    }

    /// Send a request to a specific account's worker.
    ///
    /// A request that takes mail out of a folder is recorded in
    /// `pending_moves` and followed by a [`MailRequest::Settle`], so the
    /// mail stays off that folder's list until the worker has run it,
    /// whichever of the many move paths sent it.
    pub(super) fn send_to(&self, account_id: u32, req: MailRequest) {
        let Some(worker) = self.workers.get(&account_id) else {
            return;
        };
        let taken = match &req {
            MailRequest::MoveMessage { path, uid, .. }
            | MailRequest::MarkSpam { path, uid, .. }
            | MailRequest::MarkHam { path, uid, .. } => Some((path.clone(), vec![*uid])),
            MailRequest::MoveMessages { path, uids, .. }
            | MailRequest::MarkHamMany { path, uids, .. }
            | MailRequest::PurgeMessages { path, uids } => Some((path.clone(), uids.clone())),
            _ => None,
        };
        let _ = worker.send(req);
        if let Some((path, uids)) = taken {
            let now = std::time::Instant::now();
            let mut pending = self.pending_moves.borrow_mut();
            for uid in &uids {
                pending.insert((account_id, path.clone(), *uid), now);
            }
            let _ = worker.send(MailRequest::Settle { path, uids });
        }
    }

    /// Before anything reads the cache on disk: an account removed since the
    /// last start has moved the ones after it up a place, and their cached
    /// mail has to move with them (see `Cache::claim_accounts`).
    fn claim_cached_accounts(&self) {
        if self.config.is_empty() {
            return;
        }
        let owners: Vec<(u32, String)> = self
            .config
            .iter()
            .enumerate()
            .map(|(i, a)| (i as u32 + 1, a.email.clone()))
            .collect();
        if let Err(e) = crate::cache::Cache::open().and_then(|cache| cache.claim_accounts(&owners))
        {
            tracing::warn!("cache: could not match the cached mail to the accounts: {e}");
        }
    }

    /// Paint the mail panes straight from the disk cache, before any worker
    /// has spoken: every enabled account's folder list and inbox slice is
    /// loaded synchronously at startup, so "All Inboxes" (the launch view) is
    /// full the instant the window appears. The workers' own cache-first
    /// loads and syncs then replace each slice with whatever changed since
    /// the app last ran.
    fn prime_from_cache(&mut self) {
        self.claim_cached_accounts();
        let Ok(cache) = crate::cache::Cache::open() else {
            return;
        };
        for (i, c) in self.config.iter().enumerate() {
            if !c.enabled {
                continue;
            }
            let account_id = (i + 1) as u32;
            let mut folders = cache.load_folders(account_id);
            if folders.is_empty() {
                continue;
            }
            crate::models::localize_special_folder_names(&mut folders);
            if let Some(inbox) = folders.iter().find(|f| f.kind == FolderKind::Inbox) {
                let messages = cache.load_messages(account_id, &inbox.path, inbox.id);
                if !messages.is_empty() {
                    self.message_cache
                        .insert((account_id, inbox.id), messages.clone().into());
                    self.unified_slices.insert((account_id, inbox.id), messages);
                }
            }
            // The folders counting filters file into (#116) as well, so the
            // tray menu can list their unread mail before any sync.
            let counted = folders.iter().filter(|f| {
                !matches!(
                    f.kind,
                    FolderKind::Inbox | FolderKind::Trash | FolderKind::Junk
                ) && self.filters.iter().any(|r| {
                    r.count_unread
                        && r.account_email.eq_ignore_ascii_case(&c.email)
                        && r.dest_path == f.path
                })
            });
            for f in counted {
                let messages = cache.load_messages(account_id, &f.path, f.id);
                if !messages.is_empty() {
                    self.message_cache
                        .insert((account_id, f.id), messages.into());
                }
            }
            for f in &folders {
                if f.unread > 0 {
                    self.folder_unread.insert((account_id, f.id), f.unread);
                }
            }
            self.folders.insert(account_id, folders);
        }
    }

    /// Spawn one worker per configured account (or a single mock worker when no
    /// account is configured).
    fn spawn_workers(&mut self, sender: &ComponentSender<Self>) {
        self.workers.clear();
        self.message_list
            .emit(MessageListInput::SetOwnAddresses(self.own_addresses()));
        // account_id is the config index + 1 (a load-bearing invariant), so we keep
        // every account's slot but only spawn a worker for enabled ones — disabled
        // accounts simply have no worker (no sync, no sidebar presence). With no
        // accounts configured, the app is blank — the sample/demo data only appears
        // when explicitly requested via HYLKI_DEMO (so removing all real accounts
        // doesn't fall back to fake content).
        if self.config.is_empty() {
            if demo_mode() {
                for account_id in [1, 2, 3] {
                    self.workers
                        .insert(account_id, Self::spawn_worker(account_id, None, sender));
                }
            }
        } else {
            self.claim_cached_accounts();
            for (i, account) in self.config.iter().enumerate() {
                if !account.enabled {
                    continue;
                }
                let account_id = i as u32 + 1;
                let worker = Self::spawn_worker(account_id, Some(account.clone()), sender);
                self.workers.insert(account_id, worker);
                // Anything left queued by a previous run has to show up now, not
                // only after the next failed send.
                self.send_to(account_id, MailRequest::LoadOutbox);
            }
        }
    }

    fn spawn_worker(
        account_id: u32,
        account: Option<AccountConfig>,
        sender: &ComponentSender<Self>,
    ) -> UnboundedSender<MailRequest> {
        let input = sender.input_sender().clone();
        worker::spawn(account_id, account, move |event| {
            let _ = input.send(map_event(account_id, event));
        })
    }

    /// Tear down all workers and reconnect from the current config.
    pub(super) fn reconnect_all(&mut self, sender: &ComponentSender<Self>) {
        self.accounts.clear();
        self.folders.clear();
        self.selected = None;
        self.unified = false;
        self.tag_view = None;
        self.smart_view = None;
        self.unified_slices.clear();
        self.message_cache.clear();
        self.body_cache.clear();
        self.attachments.clear();
        self.sync_attachment_drawer();
        self.attachments_loading = false;
        self.attachment_cache.clear();
        self.current = None;
        self.busy.clear();
        self.update_busy_indicator();
        self.show_message(None, false);
        self.message_list.emit(MessageListInput::SetLoading);
        // The sidebar still shows the view selected; it picks it again, and
        // says so, once the account behind it has listed its folders.
        // Without this the list stayed empty until another folder was
        // clicked and the first one clicked again.
        self.sidebar.emit(SidebarInput::Reannounce);
        self.rebuild_sidebar();
        self.spawn_workers(sender);
    }

    /// Spin the header's Refresh while any account syncs (the rail's own
    /// refresh button mirrors this via `SidebarInput::SetBusy`).
    fn set_header_refresh_busy(&self, busy: bool) {
        let face = if busy { "spinner" } else { "icon" };
        for (spinner, stack) in [
            (&self.sidebar_refresh_spinner, &self.sidebar_refresh_stack),
            (&self.peek_refresh_spinner, &self.peek_refresh_stack),
        ] {
            spinner.set_spinning(busy);
            stack.set_visible_child_name(face);
        }
    }

    /// Spin the refresh button (header and rail) while anything is happening
    /// in the background — an account sync, or bulk moves/deletions still
    /// being applied on the server. The at-a-glance "working on something"
    /// signal; the status bar has the words.
    pub(super) fn update_busy_indicator(&self) {
        let busy = !self.busy.is_empty() || self.bulk_pending > 0;
        self.sidebars_emit(SidebarInput::SetBusy(busy));
        self.set_header_refresh_busy(busy);
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_workers(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::Refresh => {
                self.resend_launcher_count();
                // The monitor can be wrong about the network (one with no
                // default route): a Refresh asked for while it says offline
                // tries the servers anyway (#442).
                if !crate::desktop::power::online() {
                    crate::desktop::power::try_anyway();
                }
                if self.unified {
                    let reqs = self.unified_targets();
                    for (account_id, folder_id, path) in reqs {
                        self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
                    }
                    self.refresh_unified_starred(&sender);
                } else {
                    match self.selected.clone() {
                        Some(sel) => self.send_to(
                            sel.account_id,
                            MailRequest::LoadMessages {
                                folder_id: sel.folder_id,
                                path: sel.path,
                            },
                        ),
                        None => {
                            for w in self.workers.values() {
                                let _ = w.send(MailRequest::Reconnect);
                            }
                        }
                    }
                }
                // Only the visible folder (and the IDLE-watched inbox) re-synced
                // above; every other folder's unread chip would drift until it
                // was selected. Have each account re-check them all.
                for w in self.workers.values() {
                    let _ = w.send(MailRequest::RefreshUnread);
                }
                // A tag view lists what the index holds, and the index only
                // learns a folder's flags when that folder syncs: have the
                // accounts in scope re-read their keywords (#166).
                self.sync_tag_keywords();
                // The same for the stars and read state a Smart Folder or
                // the Starred view lists (#532).
                self.sync_view_flags();
            }

            AppMsg::SystemResumed => {
                // The dock may have come back from the lock screen without
                // the badge; nothing else would send it again.
                self.resend_launcher_count();
                // logind and the clock check both report the same wake.
                let now = std::time::Instant::now();
                if self
                    .last_wake
                    .is_some_and(|t| now - t < std::time::Duration::from_secs(30))
                {
                    return;
                }
                self.last_wake = Some(now);
                // Realign the auto-fetch timer to now; its monotonic countdown
                // did not advance during sleep.
                self.arm_auto_fetch(&sender);
                // Still rejoining the Wi-Fi: a reconnect now would fail and
                // leave the account offline until the next auto-fetch.
                // NetworkBack does it once the network is up.
                if !crate::desktop::power::online() {
                    return;
                }
                self.reconnect_workers(&sender);
                // The network monitor can lag the wake by a moment and call a
                // dead network up; one more pass shortly after catches that.
                let input = sender.input_sender().clone();
                gtk::glib::timeout_add_seconds_local_once(20, move || {
                    let _ = input.send(AppMsg::Refresh);
                });
            }

            AppMsg::NetworkBack => {
                self.show_network_state();
                // Errors from while it was down say nothing about now.
                self.notifications.emit(NotifyInput::ClearConnectivity);
                self.reconnect_workers(&sender);
                // What waited in the Outbox goes now. An IMAP account sends
                // it as its session comes back, but JMAP, Graph and POP3 only
                // send when asked, so their mail sat there until a manual
                // retry (#442).
                let queued: Vec<u32> = self
                    .outbox_by_account
                    .iter()
                    .filter(|(_, items)| !items.is_empty())
                    .map(|(id, _)| *id)
                    .collect();
                for account_id in queued {
                    self.send_to(account_id, MailRequest::FlushOutbox { id: None });
                }
            }

            AppMsg::NetworkLost => self.show_network_state(),

            AppMsg::MeteredChanged => {
                // The flag has already flipped, so before it the connection
                // was the other way.
                let was = self.metered_save_data && !crate::desktop::power::metered();
                self.saving_data_changed(was);
            }

            AppMsg::LauncherCountLost => self.resend_launcher_count(),

            AppMsg::WindowInFront => {
                self.arrivals.clear();
                self.resend_launcher_count();
            }
            _ => unreachable!("not routed to on_workers"),
        }
    }

    /// The header Refresh's icon/spinner faces, and its click.
    pub(super) fn init_refresh_buttons(
        &self,
        widgets: &AppModelWidgets,
        sender: &ComponentSender<Self>,
    ) {
        {
            fill_refresh_faces(&self.sidebar_refresh_stack, &self.sidebar_refresh_spinner);
            self.sidebar_refresh
                .set_child(Some(&self.sidebar_refresh_stack));
            let s = sender.input_sender().clone();
            self.sidebar_refresh.connect_clicked(move |_| {
                let _ = s.send(AppMsg::Refresh);
            });
            // Long-press slides the status bar down — the place to see what
            // the spinner is spinning about. Claiming the sequence keeps the
            // release from also firing the button's click (a refresh).
            let long = gtk::GestureLongPress::new();
            let s = sender.input_sender().clone();
            long.connect_pressed(move |gesture, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let _ = s.send(AppMsg::ToggleNotifications);
            });
            self.sidebar_refresh.add_controller(long);
        }
        // The peek panel's Refresh: the same icon/spinner faces.
        fill_refresh_faces(&self.peek_refresh_stack, &self.peek_refresh_spinner);
        // Started with no network: orange from the first frame (#442).
        if !crate::desktop::power::online() {
            self.show_network_state();
        }
        if self.rail_active {
            widgets
                .sidebar_split
                .set_min_sidebar_width(SIDEBAR_RAIL_WIDTH);
            widgets
                .sidebar_split
                .set_max_sidebar_width(SIDEBAR_RAIL_WIDTH);
            set_sidebar_header_compact(
                &widgets.sidebar_header,
                &widgets.app_title,
                &widgets.sidebar_menu,
                &self.sidebar_refresh,
                true,
            );
        }
    }

    pub(super) fn prime_and_connect(&mut self, sender: &ComponentSender<Self>) {
        self.prime_from_cache();
        self.refresh_tag_css();
        if self.filters.iter().any(|r| r.needs_contacts()) {
            self.saved_addresses.refresh();
        }
        self.message_list
            .emit(MessageListInput::SetTags(self.tags.clone()));
        self.message_view
            .emit(MessageViewInput::SetTags(self.tags.clone()));
        // What each mailbox of the user's own shows (#189), before the first
        // cached message is painted: the sidebar refreshes this whenever the
        // accounts change, but nothing has rebuilt it yet at this point.
        self.refresh_own_faces();
        self.warm_own_gravatars(sender);
        self.spawn_workers(sender);
    }
}

/// Watch GNOME Online Accounts so a change there (account removed, Mail
/// toggled) is reflected in Hylki live, no restart needed. The watcher
/// debounces signal bursts and snapshots GOA on its own thread;
/// reconciliation happens on GoaChanged.
pub(super) fn watch_system_events(sender: &ComponentSender<AppModel>) {
    crate::services::goa::watch_changes({
        let s = sender.input_sender().clone();
        move |state| {
            let _ = s.send(AppMsg::GoaChanged(state));
        }
    });
    // Watch for resume-from-sleep: suspended IMAP sockets die silently, so
    // on wake we reconnect every worker and refresh, otherwise no new mail
    // arrives until the next auto-fetch (#322). logind is instant but out
    // of the Flatpak's reach; the clock check covers it there.
    crate::desktop::power::watch_resume({
        let s = sender.input_sender().clone();
        move || {
            let _ = s.send(AppMsg::SystemResumed);
        }
    });
    crate::desktop::power::watch_clock_gap({
        let s = sender.input_sender().clone();
        move || {
            let _ = s.send(AppMsg::SystemResumed);
        }
    });
    // A dock that has just (re)started holds no badge until we send it.
    crate::desktop::launcher_badge::watch_dock({
        let s = sender.input_sender().clone();
        move || {
            let _ = s.send(AppMsg::LauncherCountLost);
        }
    });
    crate::desktop::power::watch_network({
        let s = sender.input_sender().clone();
        move |change| {
            let _ = s.send(match change {
                crate::desktop::power::NetworkChange::Back => AppMsg::NetworkBack,
                crate::desktop::power::NetworkChange::Lost => AppMsg::NetworkLost,
                crate::desktop::power::NetworkChange::Metered(_) => AppMsg::MeteredChanged,
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::children::sidebar_output_msg;
    use crate::app::types::UnifiedView;
    use crate::ui::sidebar::SidebarOutput;

    #[test]
    fn sidebar_and_worker_outputs_map_to_app_messages() {
        let dbg = |m: AppMsg| format!("{m:?}");
        assert_eq!(
            dbg(sidebar_output_msg(SidebarOutput::UnifiedSelected)),
            dbg(AppMsg::UnifiedSelected(UnifiedView::Kind(
                FolderKind::Inbox
            )))
        );
        assert_eq!(
            dbg(sidebar_output_msg(SidebarOutput::UnifiedTagsSelected)),
            dbg(AppMsg::TagSelected {
                keyword: None,
                account: None
            })
        );
        assert_eq!(
            dbg(map_event(3, WorkerEvent::BulkComplete)),
            dbg(AppMsg::BulkComplete)
        );
        assert_eq!(
            dbg(map_event(
                3,
                WorkerEvent::AttachmentsScanned {
                    folder_path: "INBOX".into(),
                    added: 1,
                    remaining: 2
                }
            )),
            dbg(AppMsg::AttachmentsScanned {
                account_id: 3,
                folder_path: "INBOX".into(),
                added: 1,
                remaining: 2
            })
        );
    }
}
