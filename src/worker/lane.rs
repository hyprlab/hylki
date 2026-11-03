//! The cache lane: a thread in front of each account's worker that answers
//! from the disk cache whatever it can, so cached mail opens while the worker
//! is busy with a sync.

use tokio::sync::mpsc;

use crate::cache::Cache;
use crate::models::{FolderKind, Message};
use crate::worker::{MailRequest, WorkerEvent};

/// The fast lane in front of an account's worker: everything the disk cache
/// can answer is answered here, at once, and only what needs the network goes
/// on to the worker.
///
/// The worker handles one request at a time, and a folder sync or a
/// prefetch can hold it for a good while. Without this lane a click on a
/// message whose body was cached long ago would still queue behind that
/// work — at startup, the whole initial sync — and the reader would sit on
/// its spinner over mail that is already on disk. Cached mail must open
/// instantly whatever the worker is doing, so the lane runs on its own
/// thread with its own cache connection (WAL mode; readers never block).
///
/// Order is kept for whatever is forwarded: the lane is a single loop, so
/// two requests that both reach the worker arrive in the order the UI sent
/// them. The worker keeps its own cache-first arms, which cost a miss each
/// for a forwarded request and still serve its prefetch paths.
///
/// The offline sample backend (`protocol == None`) has no cache; everything
/// passes straight through. POP3 keeps its whole conversation in the inbox
/// and answers `LoadRelated` itself.
pub(super) fn cache_lane(
    account_id: u32,
    protocol: Option<crate::config::Protocol>,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    tx: mpsc::UnboundedSender<MailRequest>,
    emit: impl Fn(WorkerEvent) + Clone + Send + 'static,
) {
    let cache = protocol.and_then(|_| {
        Cache::open()
            .map_err(|e| tracing::warn!("cache lane unavailable: {e}"))
            .ok()
    });
    let summaries = spawn_badge_thread(account_id, protocol, cache.as_ref(), &emit);
    while let Some(req) = rx.blocking_recv() {
        let Some(c) = cache.as_ref() else {
            if tx.send(req).is_err() {
                break;
            }
            continue;
        };
        let forward = answer_from_cache(c, account_id, protocol, req, summaries.as_ref(), &emit);
        if let Some(req) = forward {
            if tx.send(req).is_err() {
                break;
            }
        }
    }
}

/// The conversation groups the badge thread is asked to count.
type BadgeQueue = mpsc::UnboundedSender<Vec<(String, Vec<String>)>>;

/// The thread that fills the list's conversation badges, and the sender
/// that feeds it; `None` for POP3 or when its cache connection did not
/// open.
fn spawn_badge_thread(
    account_id: u32,
    protocol: Option<crate::config::Protocol>,
    cache: Option<&Cache>,
    emit: &(impl Fn(WorkerEvent) + Clone + Send + 'static),
) -> Option<BadgeQueue> {
    // Filling the list's conversation badges may scan the account once per
    // thread. It must not hold cached bodies, or even cache misses, behind it.
    // Give that work its own WAL reader; forwarded network requests keep their
    // original ordering in this lane.
    let summary_cache = cache
        .filter(|_| protocol != Some(crate::config::Protocol::Pop3))
        .and_then(|c| c.read_connection().ok());
    if let Some(cache) = summary_cache {
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<(String, Vec<String>)>>();
        let emit = emit.clone();
        std::thread::Builder::new()
            .name(format!("hylki-threads-{account_id}"))
            .spawn(move || {
                while let Some(groups) = rx.blocking_recv() {
                    let start = std::time::Instant::now();
                    let summaries = thread_summaries_with_members(&cache, account_id, &groups);
                    tracing::debug!(account_id, groups = groups.len(), elapsed = ?start.elapsed(),
                        "conversation badges loaded");
                    emit(WorkerEvent::ThreadSummaries { summaries });
                }
            })
            .expect("failed to spawn conversation cache thread");
        Some(tx)
    } else {
        None
    }
}

/// One request the lane can answer from the cache, answered; what is left
/// of it for the worker comes back.
fn answer_from_cache(
    c: &Cache,
    account_id: u32,
    protocol: Option<crate::config::Protocol>,
    req: MailRequest,
    summaries: Option<&BadgeQueue>,
    emit: &impl Fn(WorkerEvent),
) -> Option<MailRequest> {
    match req {
        MailRequest::LoadBody {
            message_id,
            path,
            uid,
        } => {
            if serve_cached_body(c, account_id, &path, uid, message_id, emit) {
                None
            } else {
                Some(MailRequest::LoadBody {
                    message_id,
                    path,
                    uid,
                })
            }
        }
        MailRequest::LoadBodies { items, path } => {
            let items: Vec<(u32, u32)> = items
                .into_iter()
                .filter(|(message_id, uid)| {
                    !serve_cached_body(c, account_id, &path, *uid, *message_id, emit)
                })
                .collect();
            if items.is_empty() {
                None
            } else {
                Some(MailRequest::LoadBodies { items, path })
            }
        }
        MailRequest::LoadAttachments {
            message_id,
            path,
            uid,
            download,
        } => {
            let items = c.load_attachments(account_id, &path, uid);
            if !items.is_empty() {
                emit(WorkerEvent::Attachments {
                    path: path.to_string(),
                    message_id,
                    items,
                });
                None
            } else if !download && protocol != Some(crate::config::Protocol::Local) {
                // On this computer (#552) the file is right here: there is
                // nothing to wait for the user to ask to download.
                emit(WorkerEvent::AttachmentsPending {
                    path: path.to_string(),
                    message_id,
                });
                None
            } else {
                Some(MailRequest::LoadAttachments {
                    message_id,
                    path,
                    uid,
                    download,
                })
            }
        }
        MailRequest::LoadRelated { message_id, ids }
            if protocol != Some(crate::config::Protocol::Pop3) =>
        {
            emit(WorkerEvent::Related {
                message_id,
                messages: related_from_cache(c, account_id, &ids),
            });
            None
        }
        MailRequest::LoadThreadSummaries { groups }
            if protocol != Some(crate::config::Protocol::Pop3) =>
        {
            // Fall back to the existing lane if the extra cache connection
            // could not open. Do not lose the list's response.
            let fallback = match summaries {
                Some(tx) => tx.send(groups).err().map(|e| e.0),
                None => Some(groups),
            };
            if let Some(groups) = fallback {
                emit(WorkerEvent::ThreadSummaries {
                    summaries: thread_summaries_with_members(c, account_id, &groups),
                });
            }
            None
        }
        other => Some(other),
    }
}

/// Emit a message's cached body (and its cached sender check, if any).
/// Returns false when the cache has no body for it.
pub(super) fn serve_cached_body(
    cache: &Cache,
    account_id: u32,
    path: &str,
    uid: u32,
    message_id: u32,
    emit: &impl Fn(WorkerEvent),
) -> bool {
    let Some(body) = cache.load_body(account_id, path, uid) else {
        return false;
    };
    // A blank cached by an earlier build for a message the server would not
    // hand over (#226) is a miss, so the message is asked for again.
    if body == "(empty message)" {
        return false;
    }
    emit(WorkerEvent::Body {
        message_id,
        path: path.to_string(),
        body,
    });
    if let Some(check) = cache.load_sender_check(account_id, path, uid) {
        emit(WorkerEvent::SenderChecked {
            path: path.to_string(),
            message_id,
            check,
        });
    }
    true
}

/// Assemble a conversation from the cache across every folder of the account:
/// the messages whose Message-ID or references match `ids`.
///
/// Folder ids are positional over the same ordered folder list the UI was
/// given, so a cached row's path maps back to the id the app knows it by —
/// which is what lets the reader fetch a related message's body and say which
/// folder it came from. Deleting or binning a message is a decision about it,
/// so Trash and Junk copies are left out: a conversation shouldn't quietly put
/// them back on screen. A template is no part of any conversation (#360).
pub(super) fn related_from_cache(cache: &Cache, account_id: u32, ids: &[String]) -> Vec<Message> {
    let skipped = [FolderKind::Trash, FolderKind::Junk, FolderKind::Templates];
    related_in(
        cache,
        account_id,
        &cache.load_folders(account_id),
        ids,
        &skipped,
    )
}

/// [`related_from_cache`] over an already loaded folder list, leaving out the
/// folders of the `skipped` kinds.
fn related_in(
    cache: &Cache,
    account_id: u32,
    folders: &[crate::models::Folder],
    ids: &[String],
    skipped: &[FolderKind],
) -> Vec<Message> {
    cache
        .messages_by_thread_ids(account_id, ids)
        .into_iter()
        .filter_map(|(path, mut m)| {
            let f = folders.iter().find(|f| f.path == path)?;
            if skipped.contains(&f.kind) {
                return None;
            }
            m.folder_id = f.id;
            Some(m)
        })
        .collect()
}

/// The list's [`Cache::thread_summaries`], with the members of every
/// conversation bigger than one message, so a row can open out into the parts
/// of it filed in other folders (#309). Drafts stay out of the rows as they
/// stay out of the newest message: a reply still being written is not part of
/// the conversation yet.
pub(super) fn thread_summaries_with_members(
    cache: &Cache,
    account_id: u32,
    groups: &[(String, Vec<String>)],
) -> Vec<(String, crate::models::ThreadSummary)> {
    let mut summaries = cache.thread_summaries(account_id, groups);
    if summaries.iter().all(|(_, s)| s.count < 2) {
        return summaries;
    }
    let folders = cache.load_folders(account_id);
    let skipped = [
        FolderKind::Trash,
        FolderKind::Junk,
        FolderKind::Drafts,
        FolderKind::Templates,
    ];
    for (tag, summary) in &mut summaries {
        if summary.count < 2 {
            continue;
        }
        if let Some((_, ids)) = groups.iter().find(|(t, _)| t == tag) {
            summary.members = related_in(cache, account_id, &folders, ids, &skipped);
        }
    }
    summaries
}
