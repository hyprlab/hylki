//! The unified views (All Inboxes, Starred, Favorites ...): one list merged
//! from many folders, rebuilt only when what it holds changed.

use std::collections::{HashMap, HashSet};

use relm4::prelude::*;

use crate::models::{FolderKind, Message};
use crate::ui::message_list::MessageListInput;

use super::demo::demo_mode;
use super::model::AppModel;
use super::msg::AppMsg;
use super::types::UnifiedView;

/// A unified view, and the account it is narrowed to, as one number: the
/// key the message list keeps it under.
fn view_key_of(view: UnifiedView, account: Option<u32>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{view:?}{account:?}").hash(&mut h);
    h.finish()
}

/// What a list holds, as one number: every message's place, state and
/// tags, whatever order they come in. A few milliseconds for tens of
/// thousands of messages, against a merge and a regrouping many times that.
fn content_fp<'a>(msgs: impl Iterator<Item = &'a Message>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut sum = 0u64;
    let mut n = 0u64;
    for m in msgs {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (m.account_id, m.folder_id, m.uid, m.id, m.timestamp).hash(&mut h);
        (m.unread, m.starred, m.answered, m.forwarded).hash(&mut h);
        (
            &m.keywords,
            &m.labels,
            m.subject.len(),
            m.preview.len(),
            m.from_addr.len(),
        )
            .hash(&mut h);
        (m.message_id.len(), m.references.len()).hash(&mut h);
        sum = sum.wrapping_add(h.finish());
        n += 1;
    }
    sum ^ n.rotate_left(32)
}

/// How many of a folder's newest messages a list not in memory shows at
/// once, read from the index while the rest of the folder is.
pub(super) const LIST_NEWEST: usize = 300;

/// How long after an account's folders arrive the unified rows' folders are
/// read into memory: past the start's own rush of work.
const PRELOAD_AFTER_SECS: u32 = 5;

impl AppModel {
    /// Merge the unified view's slices for the list shortly (and hand it the
    /// new thread links), once for every answer that lands meanwhile. Each
    /// merge regroups the whole view, which across accounts can be tens of
    /// thousands of messages.
    pub(super) fn queue_unified_emit(&self, sender: &ComponentSender<Self>) {
        if self.unified_emit_queued.replace(true) {
            return;
        }
        // Shortly after the first answer, so the rest of a run joins it, and
        // never sooner after the last merge than the view's size allows: an
        // account that keeps sending batches (a large folder catching up)
        // must not keep the window busy regrouping. A second for a small
        // view, up to five for one of tens of thousands of messages.
        let size = self.unified_content().count();
        let gap = std::time::Duration::from_millis((size as u64 / 16).clamp(1000, 5000));
        let since = self.unified_emitted_at.get().elapsed();
        let wait = std::time::Duration::from_millis(150).max(gap.saturating_sub(since));
        let s = sender.input_sender().clone();
        gtk::glib::timeout_add_local_once(wait, move || {
            let _ = s.send(AppMsg::EmitUnified);
        });
    }

    /// `PrebuildViews` shortly, once for however many folder reads land
    /// meanwhile.
    fn schedule_prebuild(&mut self, sender: &ComponentSender<Self>) {
        if std::mem::replace(&mut self.prebuild_due, true) {
            return;
        }
        let s = sender.input_sender().clone();
        gtk::glib::timeout_add_seconds_local_once(2, move || {
            let _ = s.send(AppMsg::PrebuildViews);
        });
    }

    /// Hand the message list the next unified row to build ahead: one whose
    /// folders are all in memory, not on show, and not kept as it is. The
    /// next follows a moment later, so the window keeps drawing between.
    fn prebuild_next(&mut self, sender: &ComponentSender<Self>) {
        while let Some(view) = self.prebuild_queue.pop() {
            let UnifiedView::Kind(kind) = view else {
                continue;
            };
            let key = view_key_of(view, None);
            if self.unified && self.unified_view_key() == key {
                continue;
            }
            let targets: Vec<(u32, u32)> = self
                .accounts
                .iter()
                .filter(|a| self.in_unified(a.id))
                .filter_map(|a| self.folder_of_kind(a.id, kind).map(|f| (a.id, f.id)))
                .collect();
            if targets.is_empty() || targets.iter().any(|k| !self.message_cache.contains_key(k)) {
                continue;
            }
            let content = || {
                targets
                    .iter()
                    .filter_map(|k| self.message_cache.get(k))
                    .flatten()
            };
            if content().count() > crate::ui::message_list::KEEP_AT_MOST {
                continue;
            }
            let fp = content_fp(content());
            if self.message_list.model().has_view(key, fp) {
                continue;
            }
            let mut messages: Vec<Message> = content().cloned().collect();
            messages.sort_by_key(|m| std::cmp::Reverse(m.timestamp));
            tracing::debug!(
                "prebuild: {view:?}, {} messages ({})",
                messages.len(),
                self.message_list.model().view_miss(key, fp)
            );
            self.message_list
                .emit(MessageListInput::Prebuild { key, fp, messages });
            let s = sender.input_sender().clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                let _ = s.send(AppMsg::PrebuildNext);
            });
            return;
        }
    }

    /// `preload` a few seconds from now, once for however many accounts'
    /// folders arrive meanwhile.
    pub(super) fn schedule_preload(&mut self, sender: &ComponentSender<Self>) {
        if std::mem::replace(&mut self.preload_due, true) {
            return;
        }
        let s = sender.input_sender().clone();
        gtk::glib::timeout_add_seconds_local_once(PRELOAD_AFTER_SECS, move || {
            let _ = s.send(AppMsg::Preload);
        });
    }

    /// Read the folders the unified rows merge (Inboxes, Sent, Drafts,
    /// Archive and the rest switched on, every account in the unified
    /// section) into memory off the main thread, those not there yet. The
    /// first visit to a row after a start then opens from memory as later
    /// ones do; the folders were going to be held from that visit on anyway.
    fn preload(&mut self, sender: &ComponentSender<Self>) {
        if self.cache.is_none() || demo_mode() && self.config.is_empty() {
            return;
        }
        let mut kinds = self.unified_kinds_listed().listed();
        kinds.push(FolderKind::Inbox);
        let wanted: Vec<(u32, u32, String, FolderKind)> = self
            .accounts
            .iter()
            .filter(|a| self.in_unified(a.id))
            .flat_map(|a| self.folders.get(&a.id).into_iter().flatten())
            .filter(|f| kinds.contains(&f.kind))
            .filter(|f| !self.message_cache.contains_key(&(f.account_id, f.id)))
            .map(|f| (f.account_id, f.id, f.path.clone(), f.kind))
            .collect();
        if wanted.is_empty() {
            self.schedule_prebuild(sender);
            return;
        }
        let s = sender.input_sender().clone();
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            let lists: Vec<((u32, u32), Vec<Message>)> = match crate::cache::Cache::open() {
                Ok(cache) => {
                    // A row too large to be built ahead is not read ahead
                    // either: it is read at its visit, as any folder is (#504).
                    let mut sizes: HashMap<FolderKind, usize> = HashMap::new();
                    for (a, _, path, kind) in &wanted {
                        *sizes.entry(*kind).or_default() += cache.count_messages(*a, path);
                    }
                    wanted
                        .into_iter()
                        .filter(|(.., kind)| sizes[kind] <= crate::ui::message_list::KEEP_AT_MOST)
                        .map(|(a, f, path, _)| ((a, f), cache.load_messages(a, &path, f)))
                        .collect()
                }
                Err(_) => Vec::new(),
            };
            tracing::debug!(
                "preload: {} folders, {} messages, read in {:?}",
                lists.len(),
                lists.iter().map(|(_, l)| l.len()).sum::<usize>(),
                t.elapsed()
            );
            let _ = s.send(AppMsg::UnifiedFoldersLoaded { lists });
        });
    }

    /// The open unified view, as one number: which view, and for which
    /// account when narrowed to one.
    pub(super) fn unified_view_key(&self) -> u64 {
        view_key_of(self.unified_view, self.unified_account)
    }

    /// What the unified view holds, as one number: every message's place,
    /// state and tags, whatever order the slices are in. A few milliseconds
    /// for tens of thousands of messages, against a merge and a regrouping
    /// many times that.
    pub(super) fn unified_fp(&self) -> u64 {
        content_fp(self.unified_content())
    }

    /// Every message of the open unified view: its slices, and for a folder
    /// not copied yet, the folder's cache.
    fn unified_content(&self) -> impl Iterator<Item = &Message> {
        let filled = self
            .unified_slices
            .iter()
            .filter(|(k, _)| !self.unified_unfilled.contains(k))
            .flat_map(|(_, v)| v.iter());
        let unfilled = self
            .unified_unfilled
            .iter()
            .filter_map(|k| self.message_cache.get(k))
            .flat_map(|v| v.iter());
        filled.chain(unfilled)
    }

    /// Whether the open unified view reads its folders from the folder
    /// caches rather than from copies. A row of whole folders has nothing a
    /// copy would add, and an Archive's copy was tens of megabytes (#504).
    /// Starred keeps its slices: they are a folder's starred messages, not
    /// the folder.
    pub(super) fn unified_reads_through(&self) -> bool {
        matches!(self.unified_view, UnifiedView::Kind(kind) if kind != FolderKind::Starred)
    }

    /// The folders on show: the open unified view's, or the open folder.
    pub(super) fn folders_on_show(&self) -> HashSet<(u32, u32)> {
        if self.unified {
            self.unified_targets()
                .into_iter()
                .map(|(a, f, _)| (a, f))
                .collect()
        } else {
            self.selected
                .iter()
                .map(|s| (s.account_id, s.folder_id))
                .collect()
        }
    }

    /// Whether a folder of `len` messages is too large to keep in memory
    /// off screen: alone, or with the other folders of its unified row,
    /// more than [`crate::ui::message_list::KEEP_AT_MOST`]. Inboxes and the
    /// folders whose unread mail is counted are always kept: what arrives
    /// next is compared with them.
    pub(super) fn too_large_off_screen(
        &self,
        key: (u32, u32),
        len: usize,
        on_show: &HashSet<(u32, u32)>,
    ) -> bool {
        use crate::ui::message_list::KEEP_AT_MOST;
        let kind_of = |k: &(u32, u32)| self.folder_kind(k.0, k.1).unwrap_or(FolderKind::Custom);
        let kind = kind_of(&key);
        if kind == FolderKind::Inbox || on_show.contains(&key) {
            return false;
        }
        let row = match kind {
            FolderKind::Custom => len,
            _ => {
                len + self
                    .message_cache
                    .iter()
                    .filter(|(k, _)| **k != key && kind_of(k) == kind)
                    .map(|(_, l)| l.len())
                    .sum::<usize>()
            }
        };
        row > KEEP_AT_MOST && !self.counted_folders(key.0).iter().any(|c| c.id == key.1)
    }

    /// Let go of the folders [`Self::too_large_off_screen`] picks out, once
    /// their view is left. Read for one visit, an Archive of 81,000 messages
    /// stayed for the rest of the session, with the heap it was copied
    /// through (#504). Its next visit reads the index, as its first did.
    pub(super) fn shed_large_folders(&mut self) {
        let on_show = self.folders_on_show();
        let shed: Vec<(u32, u32)> = self
            .message_cache
            .iter()
            .filter(|(k, l)| self.too_large_off_screen(**k, l.len(), &on_show))
            .map(|(k, _)| *k)
            .collect();
        if shed.is_empty() {
            return;
        }
        let n: usize = shed
            .iter()
            .filter_map(|k| self.message_cache.remove(k))
            .map(|l| l.len())
            .sum();
        tracing::debug!("let go of {} folders off screen, {n} messages", shed.len());
        // Handed back once the list has let go of its rows too, a frame or
        // so on, rather than at the next half-minute trim: a system monitor
        // watched over the switch should see the memory go.
        gtk::glib::timeout_add_seconds_local_once(3, || {
            if let Some(held) = crate::desktop::memory_report::trim_if_worthwhile() {
                tracing::debug!(target: "hylki::memory", "trimmed the heap: {} was freed but held", crate::desktop::memory_report::human_bytes(held as u64));
            }
        });
    }

    /// Copy the folders `open_unified_in` left to the folder caches into
    /// the unified slices.
    pub(super) fn fill_unified_slices(&mut self) {
        let t = std::time::Instant::now();
        if self.unified_reads_through() {
            // Read where they are; only a newest-first slice goes.
            for key in &self.unified_unfilled {
                self.unified_slices.remove(key);
            }
            return;
        }
        for key in std::mem::take(&mut self.unified_unfilled) {
            match self.message_cache.get(&key) {
                Some(c) if !c.is_empty() => {
                    self.unified_slices.insert(key, c.to_vec());
                }
                _ => {
                    self.unified_slices.remove(&key);
                }
            }
        }
        tracing::debug!("unified: slices filled in {:?}", t.elapsed());
    }

    /// Push every account's inbox slice to the list as one date-sorted run. The
    /// slices arrive independently (cache seed, then each account's load), so the
    /// whole merged list is re-emitted each time one of them changes.
    pub(super) fn emit_unified(&self) {
        let t = std::time::Instant::now();
        let fp = self.unified_fp();
        if self.list_fp.get() == Some(fp)
            && self.message_list.model().view_key() == Some(self.unified_view_key())
        {
            tracing::debug!("unified: unchanged, not merged ({:?})", t.elapsed());
            return;
        }
        self.list_fp.set(Some(fp));
        self.message_list.emit(MessageListInput::SetViewFp(fp));
        self.unified_emitted_at.set(t);
        let mut merged: Vec<Message> = self.unified_content().cloned().collect();
        merged.sort_by_key(|m| std::cmp::Reverse(m.timestamp));
        tracing::debug!(
            "unified: merged {} messages in {:?}",
            merged.len(),
            t.elapsed()
        );
        self.message_list
            .emit(MessageListInput::SetMessages { messages: merged });
    }

    /// Whether an account's mail belongs in the open unified view: it is in
    /// the unified section, and the view is not narrowed to another one.
    fn in_unified_view(&self, account_id: u32) -> bool {
        self.in_unified(account_id) && self.unified_account.is_none_or(|a| a == account_id)
    }

    /// The folders the open unified view merges: (account, folder id, path).
    pub(super) fn unified_targets(&self) -> Vec<(u32, u32, String)> {
        match self.unified_view {
            UnifiedView::Kind(kind) => self
                .accounts
                .iter()
                .filter(|a| self.in_unified_view(a.id))
                .filter_map(|a| {
                    self.folder_of_kind(a.id, kind)
                        .map(|f| (a.id, f.id, f.path.clone()))
                })
                .collect(),
            UnifiedView::Filtered => self
                .unified_folder_refs()
                .into_iter()
                .map(|r| (r.account_id, r.folder.id, r.folder.path))
                .collect(),
            UnifiedView::Favorites => self
                .favorite_folder_refs()
                .into_iter()
                .map(|r| (r.account_id, r.folder.id, r.folder.path))
                .collect(),
        }
    }

    /// Whether a folder is one the open unified view merges.
    pub(super) fn is_unified_target(&self, account_id: u32, folder_id: u32) -> bool {
        match self.unified_view {
            UnifiedView::Kind(kind) => {
                self.in_unified_view(account_id)
                    && self
                        .folder_of_kind(account_id, kind)
                        .is_some_and(|f| f.id == folder_id)
            }
            UnifiedView::Filtered => self
                .unified_folder_refs()
                .iter()
                .any(|r| r.account_id == account_id && r.folder.id == folder_id),
            UnifiedView::Favorites => self
                .favorite_folder_refs()
                .iter()
                .any(|r| r.account_id == account_id && r.folder.id == folder_id),
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_unified(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::FillUnifiedSlices => self.fill_unified_slices(),

            AppMsg::Preload => {
                self.preload_due = false;
                self.preload(&sender);
            }

            AppMsg::PrebuildViews => {
                self.prebuild_due = false;
                let mut views: Vec<UnifiedView> = Vec::new();
                if self.multi_account() {
                    if self.unified_inboxes_shown() {
                        views.push(UnifiedView::Kind(FolderKind::Inbox));
                    }
                    // Starred is built from the flags of every folder, and
                    // is small: its first visit builds it.
                    views.extend(
                        self.unified_kinds_listed()
                            .listed()
                            .into_iter()
                            .filter(|k| *k != FolderKind::Starred)
                            .map(UnifiedView::Kind),
                    );
                }
                // Taken from the end: the Inboxes first.
                views.reverse();
                self.prebuild_queue = views;
                self.prebuild_next(&sender);
            }

            AppMsg::PrebuildNext => self.prebuild_next(&sender),

            AppMsg::UnifiedFoldersLoaded { lists } => self.on_unified_folders_loaded(lists, sender),

            AppMsg::PushUnreadCounts => self.push_unread_counts(),

            AppMsg::EmitUnifiedNow => {
                self.list_fp.set(None);
                if self.unified {
                    self.emit_unified();
                }
            }

            AppMsg::EmitUnified => {
                self.unified_emit_queued.set(false);
                if self.thread_links_due.replace(false) {
                    self.push_thread_links();
                }
                if self.unified && self.unified_dirty.replace(false) {
                    self.emit_unified();
                }
            }
            _ => unreachable!("not routed to on_unified"),
        }
    }

    /// Folders not in memory yet, read whole from the index off the main
    /// thread: for the unified view being opened, or ahead of it (`Preload`).
    fn on_unified_folders_loaded(
        &mut self,
        lists: Vec<((u32, u32), Vec<Message>)>,
        sender: ComponentSender<Self>,
    ) {
        let open = self.unified;
        let mut landed = false;
        for ((account_id, folder_id), messages) in lists {
            // The worker's own answer may have come first. An empty
            // folder is recorded as read too: building a unified row
            // ahead waits on all of its folders.
            if self
                .message_cache
                .get(&(account_id, folder_id))
                .is_some_and(|c| !c.is_empty())
            {
                continue;
            }
            if messages.is_empty() {
                self.message_cache
                    .entry((account_id, folder_id))
                    .or_default();
                continue;
            }
            let messages = self.overlay_local_state(account_id, folder_id, messages);
            if open && self.is_unified_target(account_id, folder_id) {
                if self.unified_reads_through() {
                    self.unified_slices.remove(&(account_id, folder_id));
                    self.unified_unfilled.insert((account_id, folder_id));
                } else {
                    self.unified_unfilled.remove(&(account_id, folder_id));
                    self.unified_slices
                        .insert((account_id, folder_id), messages.clone());
                }
                landed = true;
            } else if !open
                && self
                    .selected
                    .as_ref()
                    .is_some_and(|s| (s.account_id, s.folder_id) == (account_id, folder_id))
            {
                // The folder on show had only its newest so far.
                self.message_list.emit(MessageListInput::SetMessages {
                    messages: messages.clone(),
                });
            }
            self.message_cache
                .insert((account_id, folder_id), messages.into());
        }
        // A view left before its folders landed.
        self.shed_large_folders();
        if landed {
            self.emit_unified();
            self.push_index_complete();
        }
        self.schedule_prebuild(&sender);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::msg;

    #[test]
    fn view_keys_differ_by_view_and_account() {
        let inbox = UnifiedView::Kind(FolderKind::Inbox);
        assert_eq!(view_key_of(inbox, None), view_key_of(inbox, None));
        assert_ne!(view_key_of(inbox, None), view_key_of(inbox, Some(1)));
        assert_ne!(
            view_key_of(inbox, None),
            view_key_of(UnifiedView::Favorites, None)
        );
    }

    /// The fingerprint ignores order, and (B29) only hashes the lengths of
    /// the text fields: an equal-length edit leaves it unchanged.
    #[test]
    fn content_fingerprint_ignores_order_and_same_length_text() {
        let a = msg(1, 1, 1);
        let mut b = msg(1, 1, 2);
        b.subject = "abc".into();
        let fp = content_fp([&a, &b].into_iter());
        assert_eq!(fp, content_fp([&b, &a].into_iter()));
        let mut b2 = b.clone();
        b2.subject = "xyz".into();
        assert_eq!(fp, content_fp([&a, &b2].into_iter()));
        let mut b3 = b.clone();
        b3.unread = true;
        assert_ne!(fp, content_fp([&a, &b3].into_iter()));
        assert_ne!(content_fp([&a].into_iter()), content_fp(std::iter::empty()));
    }
}
