//! The On This Computer account (#552): folders kept on this computer, in
//! the Maildir tree [`maildir`] writes, with no server behind them. Mail
//! comes in by being moved here from another account, and leaves the same
//! way; nothing is sent from it.
//!
//! The files are the mail. The cache holds what the list and the reader
//! show of them, as it does for a server, and can be dropped at any time:
//! each folder is read again from disk when it opens, and in the background
//! every message is rendered into the cache, so the whole archive is in the
//! search index the way an account keeping all its mail offline (#536) is.

use std::cmp::Reverse;
use std::collections::HashMap;

use tokio::sync::mpsc;

use crate::cache::Cache;
use crate::config::{AccountConfig, OfflineMail};
use crate::i18n::{i18n, i18n_f};
use crate::models::{Folder, FolderKind, Message};
use crate::worker::chores::{auto_empty_due, auto_empty_roles, role_folder_path};
use crate::worker::folders::{
    chip_count_of, chip_counts_all, classify_with_source, number_folders, snoozed_counts,
};
use crate::worker::index_queue::index_pause;
use crate::worker::offline::{keep, Offline};
use crate::worker::{summary_from_raw, MailRequest, WorkerEvent};

pub(super) mod copy;
mod handle;
mod import;
mod maildir;
mod ops;

use maildir::{Entry, Maildir, INBOX};

/// Messages one background step renders into the cache.
const INDEX_BATCH: usize = 20;

/// How long after a copy from another account lands its folder is read.
const COPIES_SETTLE: std::time::Duration = std::time::Duration::from_secs(3);

/// A folder read for the first time with more new messages than this says
/// so in the status bar while it reads them.
const QUIET_READ: usize = 200;

struct LocalWorker<E> {
    account_id: u32,
    account: AccountConfig,
    store: Maildir,
    cache: Option<Cache>,
    emit: E,
    /// The background rendering of every message into the cache.
    offline: Offline,
    /// The folders as last listed.
    folders: Vec<Folder>,
    /// Folders not read into the cache yet this session: the background
    /// reads them before it renders anything, so search finds mail in a
    /// folder never opened.
    unread_folders: Vec<String>,
    /// When Junk and Trash were last emptied of old mail.
    last_auto_empty: Option<std::time::Instant>,
    /// Mail being imported from another program's folders.
    import: import::Import,
    /// What other accounts had left to copy here when it was last said.
    copies_left: Option<usize>,
}

/// Run the On This Computer account until the UI drops its sender.
pub(super) async fn run_local(
    account_id: u32,
    mut account: AccountConfig,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent),
) {
    // Everything is on disk already: the cache keeps all of it rendered,
    // and an attachment is read from the file when it is opened.
    account.offline_mail = OfflineMail::All;
    account.offline_attachments = false;
    emit(WorkerEvent::account(account_id, &account));
    let opened = crate::config::local_mail_dir()
        .ok_or_else(|| "no data directory".to_string())
        .and_then(|dir| Maildir::open(dir).map_err(|e| e.to_string()));
    let store = match opened {
        Ok(store) => store,
        Err(e) => {
            emit(WorkerEvent::error(i18n_f(
                "Could not open the folders on this computer: {e}",
                &[("e", &e)],
            )));
            while rx.recv().await.is_some() {}
            return;
        }
    };
    tracing::info!("local mail in {}", store.root().display());
    let cache = Cache::open()
        .map_err(|e| tracing::warn!("cache unavailable: {e}"))
        .ok();
    let mut worker = LocalWorker::new(account_id, account, store, cache, emit);
    // Copies from other accounts (#552) are shown a few seconds after the
    // first lands, so a batch is one look at the folders, not twenty.
    let mut copies_due: Option<tokio::time::Instant> = None;
    loop {
        let indexing = worker.cache.is_some();
        // A folder to read goes at once; the rendering keeps its pauses.
        let wait = if worker.unread_folders.is_empty() {
            worker.offline.wait()
        } else {
            None
        };
        let importing = !worker.import.is_empty();
        let req = tokio::select! {
            biased;
            req = rx.recv() => req,
            () = copy::DELIVERED.notified(), if copies_due.is_none() => {
                worker.report_copies();
                copies_due = Some(tokio::time::Instant::now() + COPIES_SETTLE);
                continue;
            }
            () = tokio::time::sleep_until(copies_due.unwrap_or_else(tokio::time::Instant::now)), if copies_due.is_some() => {
                copies_due = None;
                worker.show_copies();
                continue;
            }
            () = async {}, if importing => {
                worker.import_step();
                continue;
            }
            _ = index_pause(wait), if indexing && !importing => {
                worker.index_step();
                continue;
            }
        };
        let Some(req) = req else { break };
        worker.handle(req).await;
    }
}

/// Put a message file's flags and tags on its summary; true when that
/// changed it.
fn apply_entry(m: &mut Message, e: &Entry) -> bool {
    let before = (
        m.unread,
        m.starred,
        m.answered,
        m.forwarded,
        m.keywords.clone(),
    );
    m.unread = !e.has('S');
    m.starred = e.has('F');
    m.answered = e.has('R');
    m.forwarded = e.has('P')
        || e.keywords
            .iter()
            .any(|k| k.eq_ignore_ascii_case("$forwarded"));
    m.keywords = e.keywords.clone();
    before
        != (
            m.unread,
            m.starred,
            m.answered,
            m.forwarded,
            m.keywords.clone(),
        )
}

/// A folder's name in the sidebar: its last level, the Inbox translated.
fn leaf_name(path: &str) -> String {
    if path == INBOX {
        return i18n("Inbox");
    }
    path.rsplit('/').next().unwrap_or(path).to_string()
}

impl<E: Fn(WorkerEvent)> LocalWorker<E> {
    /// The worker over `store`, its folders listed and queued to be read.
    fn new(
        account_id: u32,
        account: AccountConfig,
        store: Maildir,
        cache: Option<Cache>,
        emit: E,
    ) -> Self {
        let mut worker = LocalWorker {
            account_id,
            account,
            store,
            cache,
            emit,
            offline: Offline::default(),
            folders: Vec::new(),
            unread_folders: Vec::new(),
            last_auto_empty: None,
            import: import::Import::default(),
            copies_left: None,
        };
        (worker.emit)(WorkerEvent::Hierarchy(
            crate::models::FolderHierarchy::maildir(),
        ));
        worker.refresh_folders();
        worker.unread_folders = worker.folders.iter().map(|f| f.path.clone()).collect();
        worker
    }

    /// List the folders with their counts, number them as every backend
    /// does, and show them.
    fn refresh_folders(&mut self) {
        let mut list: Vec<(Folder, u32)> = self
            .store
            .folder_paths()
            .into_iter()
            .map(|path| {
                let kind = classify_with_source(&path, Some("/"), &[]).0;
                let folder = Folder {
                    id: 0,
                    account_id: self.account_id,
                    name: leaf_name(&path),
                    path,
                    kind,
                    unread: 0,
                    childless: false,
                };
                (folder, 0)
            })
            .collect();
        number_folders(&mut list, |f| &f.0, |f| &mut f.0);
        let mut folders: Vec<Folder> = list.iter().map(|f| f.0.clone()).collect();
        crate::models::assign_folder_roles(&self.account.folder_roles, &mut folders);
        for ((f, total), assigned) in list.iter_mut().zip(folders) {
            f.kind = assigned.kind;
            let (all, unseen) = self.store.counts(&f.path);
            *total = all;
            f.unread = if chip_counts_all(f.kind) { all } else { unseen };
        }
        // The Inbox is the tree's top, which Maildir always has, but mail
        // copied from other accounts goes to folders of their own, so an
        // empty one only takes room. It shows again once something is in
        // it. Numbered with the rest first, so the others keep their ids.
        list.retain(|(f, total)| {
            !(f.path == INBOX && *total == 0)
                && !crate::models::folder_is_hidden(
                    &f.path,
                    Some("/"),
                    &self.account.hidden_folders,
                )
        });
        let snoozed = snoozed_counts(&list, |(f, total)| (f, *total));
        let folders: Vec<Folder> = list.into_iter().map(|(f, _)| f).collect();
        if let Some(c) = self.cache.as_ref() {
            c.save_folders(self.account_id, &folders);
        }
        self.folders = folders.clone();
        (self.emit)(WorkerEvent::Folders(folders));
        for ev in snoozed {
            (self.emit)(ev);
        }
    }

    /// The listed folder at `path`.
    fn folder(&self, path: &str) -> Option<&Folder> {
        self.folders.iter().find(|f| f.path == path)
    }

    fn folder_id(&self, path: &str) -> u32 {
        self.folder(path).map_or(0, |f| f.id)
    }

    /// Read a folder from disk into the cache: the summaries the cache has
    /// are kept, a file it has not seen is parsed, and what the file names
    /// say of read, starred and tags wins. Returns the folder, newest first.
    fn read_folder(&mut self, folder_id: u32, path: &str) -> Result<Vec<Message>, String> {
        let account_id = self.account_id;
        let entries = self.store.scan(path).map_err(|e| e.to_string())?;
        let mut known: HashMap<u32, Message> = self
            .cache
            .as_ref()
            .map(|c| c.load_messages(account_id, path, folder_id))
            .unwrap_or_default()
            .into_iter()
            .map(|m| (m.uid, m))
            .collect();
        let fresh = entries
            .iter()
            .filter(|e| !known.contains_key(&e.uid))
            .count();
        if fresh > QUIET_READ {
            (self.emit)(WorkerEvent::Status(i18n_f(
                "Reading {n} messages…",
                &[("n", &fresh.to_string())],
            )));
        }
        let mut messages = Vec::with_capacity(entries.len());
        // New and changed rows only: a large folder is not rewritten whole
        // each time a message comes in.
        let mut changed = Vec::new();
        for e in &entries {
            let (mut m, new) = match known.remove(&e.uid) {
                Some(m) => (m, false),
                None => match self.store.read(path, e.uid) {
                    Ok(raw) => (summary_from_raw(account_id, folder_id, e.uid, &raw), true),
                    Err(err) => {
                        tracing::warn!("local mail: could not read {path}/{}: {err}", e.uid);
                        continue;
                    }
                },
            };
            m.folder_id = folder_id;
            if apply_entry(&mut m, e) || new {
                changed.push(m.clone());
            }
            messages.push(m);
        }
        // Gone from disk: deleted or moved by another program.
        if let Some(c) = self.cache.as_ref() {
            for uid in known.keys() {
                c.delete_message(account_id, path, *uid);
            }
            if !changed.is_empty() {
                c.upsert_messages(account_id, path, &changed);
            }
        }
        if fresh > QUIET_READ {
            (self.emit)(WorkerEvent::Status(String::new()));
        }
        messages.sort_by_key(|m| Reverse(m.timestamp));
        self.unread_folders.retain(|p| p != path);
        Ok(messages)
    }

    /// A folder's list: the cache at once, then the folder as it is on
    /// disk, unless that is what the cache said.
    fn on_load_messages(&mut self, folder_id: u32, path: String) {
        let account_id = self.account_id;
        let cached = self
            .cache
            .as_ref()
            .map(|c| c.load_messages(account_id, &path, folder_id))
            .unwrap_or_default();
        let shown = !cached.is_empty();
        if shown {
            (self.emit)(WorkerEvent::Messages {
                folder_id,
                messages: cached.clone(),
            });
        }
        match self.read_folder(folder_id, &path) {
            Ok(messages) => {
                let kind = self.folder(&path).map_or(FolderKind::Custom, |f| f.kind);
                let unread = chip_count_of(kind, &messages);
                let same = shown
                    && messages.len() == cached.len()
                    && messages.iter().all(|m| {
                        cached.iter().any(|c| {
                            c.uid == m.uid
                                && c.unread == m.unread
                                && c.starred == m.starred
                                && c.keywords == m.keywords
                        })
                    });
                if !same {
                    (self.emit)(WorkerEvent::Messages {
                        folder_id,
                        messages,
                    });
                }
                (self.emit)(WorkerEvent::FolderUnread { folder_id, unread });
            }
            Err(e) => (self.emit)(WorkerEvent::error(i18n_f(
                "Could not fetch mail: {e}",
                &[("e", &e)],
            ))),
        }
        (self.emit)(WorkerEvent::BackfillDone { folder_id });
        (self.emit)(WorkerEvent::FolderSynced { folder_id });
    }

    /// Say how much other accounts have left to copy here (#552), when it
    /// changed since last said.
    fn report_copies(&mut self) {
        let left = copy::pending_total();
        if self.copies_left != Some(left) {
            self.copies_left = Some(left);
            (self.emit)(WorkerEvent::CopyProgress { left });
        }
    }

    /// Copies from other accounts (#552) have landed: list the folders
    /// again, and show each folder whose files the cache does not know
    /// yet, as opening it would.
    fn show_copies(&mut self) {
        self.report_copies();
        self.refresh_folders();
        let Some(c) = self.cache.as_ref() else {
            return;
        };
        let changed: Vec<(u32, String)> = self
            .folders
            .iter()
            .filter(|f| {
                self.store.counts(&f.path).0 as usize != c.count_messages(self.account_id, &f.path)
            })
            .map(|f| (f.id, f.path.clone()))
            .collect();
        for (folder_id, path) in changed {
            self.on_load_messages(folder_id, path);
        }
    }

    /// The unread counts again, from the file names. A folder holding
    /// another number of files than the cache knows (mail copied in from
    /// another account, #552, or put there by hand) is read again in the
    /// background.
    fn on_refresh_unread(&mut self) {
        self.auto_empty();
        self.refresh_folders();
        for f in &self.folders {
            (self.emit)(WorkerEvent::FolderUnread {
                folder_id: f.id,
                unread: f.unread,
            });
        }
        let Some(c) = self.cache.as_ref() else {
            return;
        };
        for f in &self.folders {
            let on_disk = self.store.counts(&f.path).0 as usize;
            if on_disk != c.count_messages(self.account_id, &f.path)
                && !self.unread_folders.contains(&f.path)
            {
                self.unread_folders.push(f.path.clone());
            }
        }
    }

    /// Delete what has been in Junk or Trash longer than the account's
    /// choice (#140), counted from when it was moved there.
    fn auto_empty(&mut self) {
        if !auto_empty_due(&self.account, &mut self.last_auto_empty) {
            return;
        }
        for (kind, role, days) in auto_empty_roles(&self.account) {
            let Some(path) = role_folder_path(&self.account, &self.folders, kind, role) else {
                continue;
            };
            let cutoff = crate::datefmt::now() - i64::from(days) * 86_400;
            let uids = self.store.older_than(&path, cutoff);
            if uids.is_empty() {
                continue;
            }
            self.on_purge(&path, &uids);
            tracing::info!(
                "auto-empty: deleted {} message(s) older than {days} days from {path} on this computer",
                uids.len()
            );
        }
    }

    /// One step of the background work: a folder not read yet, or else a
    /// few messages rendered into the cache. Nothing is pushed to the UI.
    fn index_step(&mut self) {
        if let Some(path) = self.unread_folders.first().cloned() {
            let folder_id = self.folder_id(&path);
            if let Err(e) = self.read_folder(folder_id, &path) {
                tracing::warn!("local mail: could not read {path}: {e}");
                self.unread_folders.retain(|p| *p != path);
            }
            return;
        }
        let Some(c) = self.cache.as_ref() else {
            return;
        };
        if !self.offline.fill(c, self.account_id, &self.account) {
            return;
        }
        let Some((path, uids)) = self.offline.next_batch(INDEX_BATCH) else {
            return;
        };
        for uid in uids {
            let kept = self
                .store
                .read(&path, uid)
                .is_ok_and(|raw| keep(c, self.account_id, &path, uid, &raw, false));
            if !kept {
                self.offline.not_kept(&path, uid);
            }
        }
        self.offline.succeeded();
    }
}

#[cfg(test)]
mod tests;
