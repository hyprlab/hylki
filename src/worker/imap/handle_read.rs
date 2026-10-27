//! The handlers for requests that read: folder lists, bodies, sources,
//! attachments, raw exports, searches across folders, quota and keywords.

use async_imap::types::{Fetch, Flag};
use futures::TryStreamExt;

use super::{ensured, After, Emit, ImapWorker};
use crate::cache::Cache;
use crate::i18n::{i18n, i18n_f};
use crate::models::{KeywordFinding, Message, SenderCheck};
use crate::worker::imap::backfill::Backfill;
use crate::worker::imap::bodies::settle_missing_bodies;
use crate::worker::imap::connect::connect;
use crate::worker::imap::flag_sync::refresh_flags;
use crate::worker::imap::folders::{cached_folder_kind, imap_storage_quota, list_folders};
use crate::worker::imap::keywords::refresh_keywords;
use crate::worker::imap::prefetch::{
    folder_is_gallery, prefetch_status, queue_attachment_prefetch, queue_body_prefetch,
};
use crate::worker::imap::retry::{
    load_bodies_retry, load_body_retry, load_messages_retry, load_raw_retry, load_source_retry,
};
use crate::worker::imap::structure::structure_attachments;
use crate::worker::imap::sweep::{emit_body_hits, selected_chip_count};
use crate::worker::imap::watch::{watch_active_folder, watchable};
use crate::worker::imap::wire::{exam, fetch_uids, search_uids, sel, uid_set};
use crate::worker::imap::{BODY_FETCH_BATCH, FIRST_PAGE, SCAN_CHUNK, SCAN_SPLIT_BUDGET};
use crate::worker::preempt::interrupt_read_for_reader;
use crate::worker::render::attachments::attachments_of;
use crate::worker::render::body_cacheable;
use crate::worker::{is_system_keyword, MailRequest, WorkerEvent};

impl<E: Emit> ImapWorker<E> {
    /// A folder's list (`LoadMessages`, or a quiet `SyncFolder` when
    /// `background`): the newest window, its queues and chips, and the
    /// events that close the folder's load.
    pub(super) async fn on_load_messages(
        &mut self,
        folder_id: u32,
        path: String,
        background: bool,
    ) -> After {
        if !background {
            (self.emit)(WorkerEvent::Status(i18n("Syncing…")));
        }
        // Fast first page (or a recent-window refresh over the cached
        // index); the background backfill indexes the rest of the folder.
        // Reads retry once across a reconnect, so an idle-dropped session
        // recovers transparently instead of surfacing an EOF.
        let result = interrupt_read_for_reader(
            load_messages_retry(
                self.account_id,
                &mut self.session,
                &self.account,
                folder_id,
                &path,
                &mut self.use_envelope,
                self.cache.as_ref(),
            ),
            &mut self.rx,
            &mut self.backlog,
        )
        .await;
        let Some(result) = result else {
            tracing::debug!("folder sync yielded to the reader: {path}");
            self.backlog.push_back(if background {
                MailRequest::SyncFolder { folder_id, path }
            } else {
                MailRequest::LoadMessages { folder_id, path }
            });
            return After::Skip;
        };
        match result {
            Ok(messages) => self.on_listed(folder_id, &path, messages, background).await,
            Err(e) => {
                (self.emit)(WorkerEvent::net_error(i18n_f(
                    "Could not load {path}: {e}",
                    &[("path", &(path).to_string()), ("e", &(e).to_string())],
                )));
                // The load failed, so nothing more is on its way for
                // this folder: end its index (#218) rather than leave
                // the list spinning on a backfill that will not run.
                (self.emit)(WorkerEvent::BackfillDone { folder_id });
            }
        }
        // However it went, this folder has been to the server (#198).
        (self.emit)(WorkerEvent::FolderSynced { folder_id });
        (self.emit)(WorkerEvent::Status(prefetch_status(self.prefetch.len())));
        After::Done
    }

    /// A folder's list as loaded: the work it queues, the push watch on a
    /// folder the user opened, the body-rule hits, then the list itself,
    /// the end of its index when it is the whole folder, and its unread
    /// chip.
    async fn on_listed(
        &mut self,
        folder_id: u32,
        path: &str,
        messages: Vec<Message>,
        background: bool,
    ) {
        self.queue_folder_work(folder_id, path, &messages);
        if !background {
            self.adopt_open_folder(folder_id, path);
        }
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        emit_body_hits(
            session,
            account,
            account_id,
            folder_id,
            path,
            &messages,
            cache.as_ref(),
            emit,
        )
        .await;
        // A load that came back with less than a full first
        // page *is* the whole folder — an uncached folder asks
        // for the newest FIRST_PAGE of however many the server
        // holds, and a cached one is reconciled against the
        // server's complete UID set — so its index is already
        // complete and the background backfill has nothing to
        // add. Saying so now (#218) instead of waiting for that
        // job's turn matters because the job sits behind every
        // other folder in the mailbox, and behind the body
        // prefetch, unread sweep and attachment prefetch ahead
        // of it in the idle ladder: a small inbox could wait
        // hours for a "Loading more…" spinner that had nothing
        // left to load. Larger folders are unaffected — the
        // list stops showing the spinner once it holds a full
        // page anyway.
        let complete = messages.len() < FIRST_PAGE as usize;
        emit(WorkerEvent::Messages {
            folder_id,
            messages,
        });
        if complete {
            emit(WorkerEvent::BackfillDone { folder_id });
        }
        // Refresh the true unread count (catches new mail and
        // reads from other clients beyond the loaded window).
        if let Some(sess) = session.as_mut() {
            let kind = cached_folder_kind(cache.as_ref(), account_id, path);
            if let Some(unread) = selected_chip_count(sess, kind).await {
                emit(WorkerEvent::FolderUnread { folder_id, unread });
            }
        }
    }

    /// The cache and the background queues after a folder's list: the
    /// list upserted, the folder's full index queued once a session, and
    /// its recent bodies and attachments queued for download.
    fn queue_folder_work(&mut self, folder_id: u32, path: &str, messages: &[Message]) {
        let account_id = self.account_id;
        let cache = &self.cache;
        if let Some(c) = cache.as_ref() {
            // Upsert (not replace) so the background-indexed tail
            // isn't wiped by a fast first-page load.
            c.upsert_messages(account_id, path, messages);
        }
        // Ensure this folder gets fully indexed (if not already
        // queued this session).
        if self.backfill_seen.insert(path.to_string()) {
            self.backfill.push_back(Backfill {
                folder_id,
                gallery: folder_is_gallery(cache.as_ref(), account_id, path),
                path: path.to_string(),
                remaining: None,
                failures: 0,
            });
        }
        // Pre-download recent bodies so opening them is instant.
        queue_body_prefetch(&mut self.body_prefetch, path, messages, &self.body_emitted);
        // Queue background attachment pre-downloads for recent
        // messages that have them — older ones download on demand.
        queue_attachment_prefetch(
            &mut self.prefetch,
            path,
            messages,
            cache.as_ref(),
            account_id,
        );
    }

    /// A folder the user opened is the one IDLE sits on, and opening it is
    /// "use": keep it push-watched for the next lease hour, so changes
    /// still land instantly after the user moves on to another folder.
    fn adopt_open_folder(&mut self, folder_id: u32, path: &str) {
        self.idle_folder = Some((folder_id, path.to_string()));
        let kind = self.cache.as_ref().and_then(|c| {
            c.load_folders(self.account_id)
                .into_iter()
                .find(|f| f.path == path)
                .map(|f| f.kind)
        });
        if kind.is_some_and(watchable) {
            watch_active_folder(&mut self.watchers, &self.account, path, 0, &self.emit);
        }
    }

    /// One message's body, with its sender check and attachment flag.
    pub(super) async fn on_load_body(&mut self, message_id: u32, path: String, uid: u32) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        match load_body_retry(session, account, &path, uid).await {
            Ok(rendered) => {
                deliver_body(
                    cache.as_ref(),
                    account_id,
                    &path,
                    message_id,
                    uid,
                    rendered,
                    emit,
                );
            }
            Err(e) => {
                emit(WorkerEvent::net_error(i18n_f(
                    "Could not load message: {e}",
                    &[("e", &(e).to_string())],
                )));
            }
        }
        After::Done
    }

    /// The bodies of a conversation the cache could not answer, fetched in
    /// batches of [`BODY_FETCH_BATCH`].
    pub(super) async fn on_load_bodies(
        &mut self,
        path: String,
        pending_bodies: Vec<(u32, u32)>,
    ) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        for group in pending_bodies.chunks(BODY_FETCH_BATCH) {
            // A failed batch leaves no session behind (the retry only
            // hands one back on success), so the rest of the conversation
            // needs one before it can go on.
            if session.is_none() {
                *session = connect(account).await.ok();
            }
            if session.is_none() {
                break; // still offline; what's left loads on reopen
            }
            let uids: Vec<u32> = group.iter().map(|(_, uid)| *uid).collect();
            match load_bodies_retry(session, account, &path, &uids).await {
                Ok(mut fetched) => {
                    // The UIDs the server answered without a body.
                    let mut missing: Vec<(u32, u32)> = Vec::new();
                    for (message_id, uid) in group {
                        let Some(rendered) = fetched.remove(uid) else {
                            missing.push((*message_id, *uid));
                            continue;
                        };
                        deliver_body(
                            cache.as_ref(),
                            account_id,
                            &path,
                            *message_id,
                            *uid,
                            rendered,
                            emit,
                        );
                    }
                    if !missing.is_empty() {
                        settle_missing_bodies(
                            session,
                            account_id,
                            &path,
                            missing,
                            cache.as_ref(),
                            emit,
                        )
                        .await;
                    }
                }
                Err(e) => {
                    emit(WorkerEvent::net_error(i18n_f(
                        "Could not load conversation: {e}",
                        &[("e", &(e).to_string())],
                    )));
                }
            }
        }
        After::Done
    }

    /// A message's raw source for the source view.
    pub(super) async fn on_load_source(&mut self, path: String, uid: u32) -> After {
        let account = &self.account;
        let session = &mut self.session;
        let emit = &self.emit;
        match load_source_retry(session, account, &path, uid).await {
            Ok(text) => emit(WorkerEvent::Source { text }),
            Err(e) => {
                emit(WorkerEvent::net_error(i18n_f(
                    "Could not load source: {e}",
                    &[("e", &(e).to_string())],
                )));
            }
        }
        After::Done
    }

    /// A message's attachments, downloaded because the user asked.
    pub(super) async fn on_load_attachments(
        &mut self,
        message_id: u32,
        path: String,
        uid: u32,
    ) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        match load_raw_retry(session, account, &path, uid).await {
            Ok(raw) => {
                let (items, cacheable) = attachments_of(&raw);
                if let Some(c) = cache.as_ref() {
                    if cacheable {
                        c.save_attachments(account_id, &path, uid, &items);
                        c.mark_attachments_checked(account_id, &path, uid);
                    }
                }
                emit(WorkerEvent::Attachments {
                    path: path.to_string(),
                    message_id,
                    items,
                });
            }
            Err(e) => {
                emit(WorkerEvent::net_error(i18n_f(
                    "Could not load attachments: {e}",
                    &[("e", &(e).to_string())],
                )));
            }
        }
        After::Done
    }

    /// A message's raw bytes, for Save As or for the reader.
    pub(super) async fn on_export_raw(&mut self, token: u64, path: String, uid: u32) -> After {
        let account = &self.account;
        let session = &mut self.session;
        let emit = &self.emit;
        let raw = load_raw_retry(session, account, &path, uid)
            .await
            .map_err(|e| e.to_string());
        emit(WorkerEvent::RawExported { token, raw });
        After::Done
    }

    /// Find a message by its Message-ID in any folder (a `mid:` link).
    pub(super) async fn on_locate(&mut self, message_id: String) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let session = &mut self.session;
        let emit = &self.emit;
        // A folder at a time: SELECT, then a HEADER search. Servers
        // whose header index lags (see UndoMove below) are the reason
        // the cache is asked first; this is the fallback for a message
        // Hylki has never listed.
        let sess = ensured(session);
        let mut hit: Option<(String, u32)> = None;
        match list_folders(
            account_id,
            sess,
            &account.folder_roles,
            &account.hidden_folders,
        )
        .await
        {
            Ok(folders) => {
                for f in folders {
                    if sel(sess, &f.path).await.is_err() {
                        continue;
                    }
                    let Ok(set) =
                        search_uids(sess, format!("HEADER Message-ID \"{message_id}\"")).await
                    else {
                        continue;
                    };
                    if let Some(uid) = set.into_iter().max() {
                        hit = Some((f.path.clone(), uid));
                        break;
                    }
                }
            }
            Err(e) => {
                tracing::warn!("[account {account_id}] locate: listing folders failed: {e}")
            }
        }
        emit(WorkerEvent::Located {
            message_id: message_id.clone(),
            hit,
        });
        After::Done
    }

    /// Describe a batch of a folder's unscanned attachments for the gallery.
    pub(super) async fn on_scan_attachments(&mut self, folder_path: String) -> After {
        let account_id = self.account_id;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        // Ask the server what a batch of messages holds, without
        // downloading any of it. BODYSTRUCTURE is a few hundred bytes
        // per message, so a whole archive costs single-digit megabytes
        // — the difference between a gallery that reaches back two
        // decades and one that shows only what has been opened.
        let Some(c) = cache.as_ref() else {
            return After::Skip;
        };
        let uids = c.unscanned_attachment_uids(account_id, folder_path.as_str(), SCAN_CHUNK);
        if uids.is_empty() {
            emit(WorkerEvent::AttachmentsScanned {
                folder_path: folder_path.clone(),
                added: 0,
                remaining: 0,
            });
            return After::Skip;
        }
        let sess = ensured(session);
        if sel(sess, folder_path.as_str()).await.is_err() {
            return After::Skip;
        }

        // One bad response must not cost the whole batch. A batch that
        // will not parse is halved and retried, down to the single
        // message responsible: its neighbours are described normally
        // and only it is written off. Smaller responses also parse more
        // often, so the split usually resolves the failure outright.
        let mut added = 0u32;
        let mut pending: Vec<Vec<u32>> = vec![uids];
        let mut budget = SCAN_SPLIT_BUDGET;
        while let Some(batch) = pending.pop() {
            if budget == 0 {
                break; // the rest keeps its place in the queue
            }
            budget -= 1;
            let set = uid_set(&batch);
            // BODYSTRUCTURE on its own, not beside ENVELOPE: the
            // servers that make us fall back to raw headers (iCloud)
            // usually choke on the ENVELOPE half — an unescaped quote
            // in a Message-ID — so asking for the structure alone
            // often parses where the combined fetch did not.
            let fetched: Result<Vec<Fetch>, _> = match fetch_uids(sess, &set, "BODYSTRUCTURE").await
            {
                Ok(stream) => stream.try_collect().await,
                Err(e) => Err(e),
            };
            match fetched {
                Ok(fetches) => {
                    // Every UID asked about is answered, including the
                    // ones holding nothing: an empty list is still an
                    // answer, and recording it stops the message being
                    // asked about on every pass forever.
                    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
                    for f in &fetches {
                        let Some(uid) = f.uid else { continue };
                        seen.insert(uid);
                        let metas = f
                            .bodystructure()
                            .map(structure_attachments)
                            .unwrap_or_default();
                        added += metas.len() as u32;
                        c.save_attachment_meta(account_id, folder_path.as_str(), uid, &metas);
                    }
                    for uid in batch.iter().filter(|u| !seen.contains(u)) {
                        c.save_attachment_meta(account_id, folder_path.as_str(), *uid, &[]);
                    }
                }
                // A dropped connection says nothing about these
                // messages. Leave them unscanned and let the next pass
                // have them, rather than recording an answer the server
                // never gave.
                Err(async_imap::error::Error::ConnectionLost) => {
                    pending.clear();
                    session.take();
                    break;
                }
                Err(e) if batch.len() > 1 => {
                    let mid = batch.len() / 2;
                    pending.push(batch[mid..].to_vec());
                    pending.push(batch[..mid].to_vec());
                    tracing::debug!(
                        "attachment scan of {folder_path}: splitting {} after {e}",
                        batch.len()
                    );
                }
                Err(e) => {
                    // One message whose structure our parser will not
                    // take. Record it as holding nothing rather than
                    // retrying it forever; opening it still fetches the
                    // message in full and lists what it really holds.
                    tracing::warn!("attachment scan of {folder_path} uid {}: {e}", batch[0]);
                    c.save_attachment_meta(account_id, folder_path.as_str(), batch[0], &[]);
                }
            }
        }

        emit(WorkerEvent::AttachmentsScanned {
            folder_path: folder_path.clone(),
            added,
            remaining: c.unscanned_attachment_count(account_id, folder_path.as_str()),
        });
        After::Done
    }

    /// The keywords in use across the account, for the tag finder.
    pub(super) async fn on_find_keywords(&mut self) -> After {
        let account_id = self.account_id;
        let account = &self.account;
        let session = &mut self.session;
        let emit = &self.emit;
        // Every folder, read-only: EXAMINE answers with the mailbox's
        // FLAGS line, which lists the keywords in use there, and a
        // KEYWORD search per candidate says how many messages carry
        // it (some servers list every keyword ever defined, so an
        // unused one is dropped here).
        let sess = ensured(session);
        let mut found: Vec<KeywordFinding> = Vec::new();
        match list_folders(
            account_id,
            sess,
            &account.folder_roles,
            &account.hidden_folders,
        )
        .await
        {
            Ok(folders) => {
                for f in &folders {
                    let Ok(mb) = exam(sess, &f.path).await else {
                        continue;
                    };
                    let candidates: Vec<String> = mb
                        .flags
                        .iter()
                        .filter_map(|fl| match fl {
                            Flag::Custom(k) if !is_system_keyword(k) => Some(k.to_string()),
                            _ => None,
                        })
                        .collect();
                    for kw in candidates {
                        let n = match search_uids(sess, format!("KEYWORD {kw}")).await {
                            Ok(set) => set.len(),
                            Err(_) => 0,
                        };
                        if n == 0 {
                            continue;
                        }
                        match found
                            .iter_mut()
                            .find(|x| x.keyword.eq_ignore_ascii_case(&kw))
                        {
                            Some(x) => {
                                x.count += n;
                                if !x.folders.contains(&f.name) {
                                    x.folders.push(f.name.clone());
                                }
                            }
                            None => found.push(KeywordFinding {
                                keyword: kw,
                                name: None,
                                color: None,
                                count: n,
                                folders: vec![f.name.clone()],
                            }),
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("[account {account_id}] find tags: listing folders failed: {e}")
            }
        }
        emit(WorkerEvent::KeywordsFound(found));
        After::Done
    }

    /// The mailbox size and its limit, where the server offers QUOTA.
    pub(super) async fn on_quota(&mut self) -> After {
        let account_id = self.account_id;
        let session = &mut self.session;
        let emit = &self.emit;
        // RFC 2087/9208: the quota root the inbox belongs to is the
        // account's own, and its STORAGE resource is the mailbox size.
        let sess = ensured(session);
        let offered = sess
            .capabilities()
            .await
            .is_ok_and(|caps| caps.has_str("QUOTA"));
        let quota = if offered {
            match sess.get_quota_root("INBOX").await {
                Ok((_, quotas)) => imap_storage_quota(&quotas),
                Err(e) => {
                    tracing::warn!("[account {account_id}] quota: GETQUOTAROOT failed: {e}");
                    None
                }
            }
        } else {
            None
        };
        tracing::debug!(target: "hylki::imap", "quota: {quota:?} (server {} QUOTA)", if offered { "offers" } else { "lacks" });
        emit(WorkerEvent::Quota(quota));
        After::Done
    }

    /// Bring the cached keywords of every folder in step with the server (#166).
    pub(super) async fn on_refresh_keywords(&mut self, keywords: Vec<String>) -> After {
        let account_id = self.account_id;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let sess = ensured(session);
        let paths = refresh_keywords(account_id, sess, cache.as_ref(), &keywords).await;
        emit(WorkerEvent::KeywordsSynced { paths });
        After::Done
    }

    /// Bring the cached stars and read state of `paths` in step with the
    /// server (#532).
    pub(super) async fn on_refresh_flags(&mut self, paths: Vec<String>) -> After {
        let account_id = self.account_id;
        let cache = &self.cache;
        let session = &mut self.session;
        let emit = &self.emit;
        let sess = ensured(session);
        let paths = refresh_flags(account_id, sess, cache.as_ref(), &paths).await;
        emit(WorkerEvent::FlagsSynced { paths });
        After::Done
    }
}

/// A fetched body to the cache (unless it is OpenPGP mail) and to the
/// reader: `Body`, then `SenderChecked`, then whether it has attachments.
fn deliver_body(
    cache: Option<&Cache>,
    account_id: u32,
    path: &str,
    message_id: u32,
    uid: u32,
    (body, check, has_attachments): (String, SenderCheck, bool),
    emit: &impl Fn(WorkerEvent),
) {
    if let Some(c) = cache {
        if body_cacheable(&check) {
            c.save_body(account_id, path, uid, &body);
        }
        c.save_sender_check(account_id, path, uid, &check);
        c.set_has_attachment(account_id, path, uid, has_attachments);
    }
    emit(WorkerEvent::Body {
        message_id,
        path: path.to_string(),
        body,
    });
    emit(WorkerEvent::SenderChecked {
        path: path.to_string(),
        message_id,
        check,
    });
    emit(if has_attachments {
        WorkerEvent::HasAttachments {
            path: path.to_string(),
            message_id,
        }
    } else {
        WorkerEvent::NoAttachments {
            path: path.to_string(),
            message_id,
        }
    });
}
