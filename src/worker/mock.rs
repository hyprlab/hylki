//! The offline sample backend (no account configured, or `--demo`): the
//! same request and event protocol as a real account, answered from the
//! built-in demo mail.

use std::time::Duration;

use tokio::sync::mpsc;

use crate::backend::{MailBackend, MockBackend};
use crate::models::{KeywordFinding, MailboxQuota};
use crate::worker::{MailRequest, WorkerEvent};

/// The files a demo message "carries": a 1x1 PNG and a short text note. The
/// Q3 roadmap thread carries a drawn PDF instead of the PNG, whose first page
/// makes a real thumbnail in the drawer where a transparent pixel reads as an
/// empty square (tools/gen-demo-pdfs.py draws them).
fn demo_attachment_files(message_id: u32) -> Vec<crate::models::Attachment> {
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let first = match message_id {
        23 => crate::models::Attachment {
            name: "Q3-roadmap-draft.pdf".into(),
            data: include_bytes!("../../data/demo/q3-roadmap-draft.pdf").to_vec(),
        },
        21 => crate::models::Attachment {
            name: "migration-timeline.pdf".into(),
            data: include_bytes!("../../data/demo/migration-timeline.pdf").to_vec(),
        },
        _ => crate::models::Attachment {
            name: "palette-hover.png".into(),
            data: PNG_1X1.to_vec(),
        },
    };
    vec![
        first,
        crate::models::Attachment {
            name: "notes.txt".into(),
            data: b"Hover palette: reserved-space fade vs. layout shift.\n".to_vec(),
        },
    ]
}

/// The demo's sender verdict for a message that has a header block: the
/// real check, run over those headers, so the card's seal and the
/// Unsubscribe banner appear as they would for fetched mail.
fn mock_sender_check(path: &str, message_id: u32, emit: &impl Fn(WorkerEvent)) {
    if let Some(raw) = crate::backend::demo_raw(message_id) {
        let check = crate::mail::verify::check_sender(raw.as_bytes());
        emit(WorkerEvent::SenderChecked {
            path: path.to_string(),
            message_id,
            check,
        });
    }
}

pub(super) async fn run_mock(
    account_id: u32,
    mut rx: mpsc::UnboundedReceiver<MailRequest>,
    emit: impl Fn(WorkerEvent) + Clone + 'static,
) {
    let backend = MockBackend::new();

    if let Some(account) = backend.accounts().into_iter().find(|a| a.id == account_id) {
        emit(WorkerEvent::Account(account));
    }
    emit(WorkerEvent::Folders(backend.folders(account_id)));
    // HYLKI_DEMO_ARRIVAL=<secs>: a reply lands in account 1's Inbox
    // conversation after that long, as a sync would bring it — for
    // watching an open conversation take in a new message.
    if account_id == 1 {
        if let Some(secs) = std::env::var("HYLKI_DEMO_ARRIVAL")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
        {
            let emit = emit.clone();
            let mut messages = backend.messages(1);
            tokio::task::spawn_local(async move {
                tokio::time::sleep(Duration::from_secs(secs)).await;
                messages.insert(0, crate::backend::demo_arrival());
                emit(WorkerEvent::Messages {
                    folder_id: 1,
                    messages,
                });
            });
        }
    }

    while let Some(req) = rx.recv().await {
        match req {
            // The demo's mail has no server to ask; its text is all in the list.
            MailRequest::FindText { gen, .. } => emit(WorkerEvent::TextFound {
                gen,
                hits: Vec::new(),
                done: true,
            }),
            MailRequest::Locate { message_id } => {
                // No folders to search here beyond what the cache already
                // held: answer "not found" so the app can report the miss.
                emit(WorkerEvent::Located {
                    message_id: message_id.clone(),
                    hit: None,
                });
            }
            // Demo mode seeds its in-memory cache up front (see
            // `backend::seed_demo_cache`), so every attachment is already
            // described and there is nothing to ask a server about.
            MailRequest::ScanAttachments { folder_path } => {
                emit(WorkerEvent::AttachmentsScanned {
                    folder_path,
                    added: 0,
                    remaining: 0,
                });
            }
            MailRequest::Quota => emit(WorkerEvent::Quota(Some(MailboxQuota {
                used: 2_310_000_000,
                limit: 15_000_000_000,
            }))),
            // What a scan of a lived-in mailbox turns up: Thunderbird's
            // built-ins and a few of the user's own, plus one the demo's tags
            // already know (dropped by the app before the report).
            MailRequest::FindKeywords => {
                let f = |keyword: &str, count: usize, folders: &[&str]| KeywordFinding {
                    keyword: keyword.to_string(),
                    name: None,
                    color: None,
                    count,
                    folders: folders.iter().map(|s| s.to_string()).collect(),
                };
                emit(WorkerEvent::KeywordsFound(vec![
                    f("$label1", 14, &["Inbox", "Archive"]),
                    f("$label4", 6, &["Inbox"]),
                    f("Receipts", 23, &["Inbox", "Archive"]),
                    f("travel_plans", 4, &["Inbox"]),
                    f("Work", 9, &["Inbox"]),
                ]));
            }
            MailRequest::LoadRelated { message_id, .. } => {
                emit(WorkerEvent::Related {
                    message_id,
                    messages: Vec::new(),
                });
            }
            MailRequest::LoadThreadSummaries { groups } => {
                emit(WorkerEvent::ThreadSummaries {
                    summaries: backend.thread_summaries(account_id, &groups),
                });
            }
            MailRequest::LoadMessages { folder_id, .. }
            | MailRequest::SyncFolder { folder_id, .. } => {
                let messages = backend.messages(folder_id);
                // Demo messages carry their bodies in the listing, so the
                // app never asks for one: the verdicts that would come with
                // a fetched body are served with the listing instead.
                let ids: Vec<u32> = messages.iter().map(|m| m.id).collect();
                emit(WorkerEvent::Messages {
                    folder_id,
                    messages,
                });
                let path = backend
                    .folders(account_id)
                    .into_iter()
                    .find(|f| f.id == folder_id)
                    .map(|f| f.path)
                    .unwrap_or_default();
                for id in ids {
                    mock_sender_check(&path, id, &emit);
                }
                // HYLKI_DEMO_SYNC_DELAY=<secs> holds the "been to the server"
                // signal back, so the progress a long sync shows (the manual
                // filter run's dialog, #198) can be watched here.
                match std::env::var("HYLKI_DEMO_SYNC_DELAY")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                {
                    Some(secs) => {
                        let emit = emit.clone();
                        tokio::task::spawn_local(async move {
                            tokio::time::sleep(Duration::from_secs(secs)).await;
                            emit(WorkerEvent::FolderSynced { folder_id });
                        });
                    }
                    None => emit(WorkerEvent::FolderSynced { folder_id }),
                }
            }
            MailRequest::LoadBody {
                message_id,
                ref path,
                ..
            } => {
                let body = backend
                    .message(message_id)
                    .map(|m| m.body)
                    .unwrap_or_default();
                emit(WorkerEvent::Body {
                    message_id,
                    path: path.clone(),
                    body,
                });
                mock_sender_check(path, message_id, &emit);
            }
            MailRequest::LoadBodies {
                ref items,
                ref path,
            } => {
                for (message_id, _) in items {
                    let body = backend
                        .message(*message_id)
                        .map(|m| m.body)
                        .unwrap_or_default();
                    emit(WorkerEvent::Body {
                        message_id: *message_id,
                        path: path.clone(),
                        body,
                    });
                    mock_sender_check(path, *message_id, &emit);
                }
            }
            MailRequest::LoadSource { message_id, .. } => {
                let text = backend
                    .message(message_id)
                    .map(|m| m.body)
                    .unwrap_or_default();
                emit(WorkerEvent::Source { text });
            }
            MailRequest::LoadAttachments {
                message_id,
                ref path,
                ..
            } => {
                // A demo message flagged as carrying attachments gets two
                // small files, so the reader's per-card rows and the drawer
                // have something to show (#213).
                // A meeting request carries its own calendar part and
                // nothing else (#223).
                let invite = crate::backend::demo_raw(message_id)
                    .and_then(|raw| crate::mail::invite::detect_raw(raw.as_bytes()));
                let items = if let Some(inv) = invite {
                    vec![crate::models::Attachment {
                        name: "invite.ics".to_string(),
                        data: inv.ics.into_bytes(),
                    }]
                } else if backend
                    .message(message_id)
                    .is_some_and(|m| m.has_attachment)
                {
                    demo_attachment_files(message_id)
                } else {
                    Vec::new()
                };
                emit(WorkerEvent::Attachments {
                    path: path.to_string(),
                    message_id,
                    items,
                });
            }
            // Mutations are no-ops offline; the UI updates optimistically.
            MailRequest::SetSeen { .. }
            | MailRequest::SetFlagged { .. }
            | MailRequest::SetKeyword { .. }
            | MailRequest::MarkAllRead { .. }
            | MailRequest::MarkSpam { .. }
            | MailRequest::MarkHam { .. }
            | MailRequest::MoveMessage { .. }
            | MailRequest::CreateFolder { .. }
            | MailRequest::RenameFolder { .. }
            | MailRequest::DeleteFolder { .. }
            | MailRequest::SetHiddenFolders { .. }
            | MailRequest::FlushOutbox { .. }
            | MailRequest::DeleteOutbox { .. }
            | MailRequest::RefreshUnread
            | MailRequest::RefreshKeywords { .. }
            | MailRequest::RefreshFlags { .. }
            | MailRequest::Reconnect => {}
            // The demo's folders are fixed, but an emptied one reads as empty.
            MailRequest::EmptyFolder { folder_id, .. } => {
                emit(WorkerEvent::Messages {
                    folder_id,
                    messages: Vec::new(),
                });
                emit(WorkerEvent::FolderUnread {
                    folder_id,
                    unread: 0,
                });
            }
            // The demo backend sends nothing, so its Outbox is always empty.
            MailRequest::LoadOutbox => emit(WorkerEvent::Outbox { items: Vec::new() }),
            // Signal completion so the demo's bulk spinner clears.
            MailRequest::MoveMessages { .. }
            | MailRequest::MarkHamMany { .. }
            | MailRequest::PurgeMessages { .. }
            | MailRequest::UndoMove { .. } => emit(WorkerEvent::BulkComplete),
            // The demo keeps no snoozed mail to bring back (#527).
            MailRequest::Unsnooze { message_ids, .. } => emit(WorkerEvent::Unsnoozed {
                message_ids,
                done: true,
            }),
            MailRequest::SaveDraft { autosave, .. } => emit(WorkerEvent::DraftSaved {
                autosave,
                message_id: None,
            }),
            MailRequest::Settle { path, uids } => emit(WorkerEvent::MovesSettled { path, uids }),
            MailRequest::ExportRaw { token, uid, .. } => emit(WorkerEvent::RawExported {
                token,
                // The sample's own raw form where it has one, else one made
                // from what the list shows of it.
                raw: crate::backend::demo_raw(uid)
                    .or_else(|| {
                        crate::backend::MockBackend::new().message(uid).map(|m| {
                            format!(
                                "From: {} <{}>\r\nSubject: {}\r\nDate: {}\r\n\r\n{}\r\n",
                                m.from_name, m.from_addr, m.subject, m.date, m.body
                            )
                        })
                    })
                    .map(String::into_bytes)
                    .ok_or_else(|| "no such demo message".to_string()),
            }),
            MailRequest::ImportRaw { token, .. } => emit(WorkerEvent::RawImported {
                token,
                result: Ok(()),
            }),
            MailRequest::DeleteAttachment {
                message_id,
                path,
                uid,
                name,
                size,
            } => emit(WorkerEvent::AttachmentDeleted {
                message_id,
                path,
                uid,
                new_uid: Some(uid),
                name,
                size,
            }),
            // Pretend the send succeeded so the compose flow is demoable offline.
            MailRequest::Send { ticket, .. } => {
                emit(WorkerEvent::Sent);
                emit(WorkerEvent::SendDone { ticket, sent: true });
            }
        }
    }
}
