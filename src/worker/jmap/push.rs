//! New mail: the EventSource push channel, the poll that stands in for it,
//! and bodies served from the cache or the server.

use std::time::Duration;

use tokio::sync::mpsc;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::i18n_f;
use crate::worker::jmap::chores::emit_jmap_body_hits;
use crate::worker::jmap::folders::{jmap_refresh_unread, refresh_jmap_folders};
use crate::worker::jmap::index::{jmap_load_folder, JmapState};
use crate::worker::jmap::ops::jmap_fetch_raw;
use crate::worker::jmap::wire::{fill_template, jmap_err, jmap_wire, jmap_wired, JmapSession};
use crate::worker::render::body_cacheable;
use crate::worker::{render_raw, WorkerEvent};

/// Keep the server's EventSource open on a thread of its own and tick the
/// channel whenever mail or mailboxes change. The stream is re-opened after
/// a pause when it drops; the thread ends once the worker has gone.
pub(super) fn spawn_jmap_push(s: JmapSession, tx: mpsc::UnboundedSender<()>) {
    let Some(template) = s.event_source_url.clone() else {
        return;
    };
    let url = fill_template(
        &template,
        &[
            ("types", "Email,Mailbox"),
            ("closeafter", "no"),
            ("ping", "30"),
        ],
    );
    std::thread::Builder::new()
        .name("jmap-push".into())
        .spawn(move || {
            use std::io::BufRead;
            loop {
                if tx.is_closed() {
                    return;
                }
                // Offline: wait for the network rather than redial into
                // nothing every 30 seconds (#442).
                if !crate::desktop::power::reaches_url(&url) {
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
                jmap_wire("GET events");
                let resp = ureq::AgentBuilder::new()
                    .timeout_connect(Duration::from_secs(20))
                    // The server pings every 30 s; well past that means the
                    // connection is dead.
                    .timeout_read(Duration::from_secs(120))
                    .build()
                    .get(&url)
                    .set("Authorization", &s.auth)
                    .set("Accept", "text/event-stream")
                    .call()
                    .map_err(jmap_err);
                jmap_wired("GET events", &resp);
                if let Ok(resp) = resp {
                    let reader = std::io::BufReader::new(resp.into_reader());
                    let mut event = String::new();
                    for line in reader.lines() {
                        let Ok(line) = line else { break };
                        if let Some(name) = line.strip_prefix("event:") {
                            event = name.trim().to_string();
                        } else if line.is_empty() {
                            if event == "state" && tx.send(()).is_err() {
                                return;
                            }
                            event.clear();
                        }
                    }
                }
                if tx.is_closed() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(30));
            }
        })
        .ok();
}

/// One poll tick or push signal: refresh the Inbox and the folder counts.
pub(super) async fn jmap_poll_inbox(
    s: &JmapSession,
    account_id: u32,
    account: &AccountConfig,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) {
    if state.inbox.is_none() {
        refresh_jmap_folders(s, account_id, cache, state, emit).await;
    }
    let Some((folder_id, path)) = state.inbox.clone() else {
        return;
    };
    if let Ok(messages) = jmap_load_folder(s, account_id, folder_id, &path, cache, state).await {
        let unread = messages.iter().filter(|m| m.unread).count() as u32;
        emit_jmap_body_hits(s, account, folder_id, &path, &messages, state, emit).await;
        emit(WorkerEvent::Messages {
            folder_id,
            messages,
        });
        emit(WorkerEvent::FolderUnread {
            folder_id,
            unread,
            total: None,
        });
    }
    jmap_refresh_unread(s, account_id, cache, state, emit).await;
}

/// A body from the cache, or none.
pub(super) fn serve_cached(
    cache: Option<&Cache>,
    account_id: u32,
    path: &str,
    uid: u32,
    message_id: u32,
    emit: &impl Fn(WorkerEvent),
) -> bool {
    let Some(c) = cache else { return false };
    let Some(body) = c.load_body(account_id, path, uid) else {
        return false;
    };
    emit(WorkerEvent::Body {
        message_id,
        path: path.to_string(),
        body,
    });
    if let Some(check) = c.load_sender_check(account_id, path, uid) {
        emit(WorkerEvent::SenderChecked {
            path: path.to_string(),
            message_id,
            check,
        });
    }
    true
}

/// Fetch, render and cache one body.
pub(super) async fn jmap_deliver_body(
    s: &JmapSession,
    account_id: u32,
    message_id: u32,
    path: &str,
    uid: u32,
    cache: Option<&Cache>,
    state: &mut JmapState,
    emit: &impl Fn(WorkerEvent),
) {
    match jmap_fetch_raw(s, state, path, uid).await {
        Ok(raw) => {
            let (body, check, _) = render_raw(&raw);
            if let Some(c) = cache {
                if body_cacheable(&check) {
                    c.save_body(account_id, path, uid, &body);
                }
                c.save_sender_check(account_id, path, uid, &check);
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
        }
        Err(e) => emit(WorkerEvent::net_error(i18n_f(
            "Could not load message: {e}",
            &[("e", &e)],
        ))),
    }
}
