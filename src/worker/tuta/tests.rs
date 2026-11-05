//! The Tuta backend against a fake helper: folder and message mapping,
//! the uid map, moves, read marks and the ways a send can end.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::folders::tuta_folders;
use super::index::{apply_first_page, carry_over, TutaState, TUTA_BACKFILL_CHUNK};
use super::messages::tuta_message;
use super::mime::mime_message;
use super::ops::{tuta_move_uids, tuta_set_seen};
use super::push::{spawn_tuta_push, PushSignal};
use super::send::{draft_parameters, tuta_flush_outbox, tuta_send_raw};
use super::wire::{sync_page, FolderRow};
use super::TutaWorker;
use crate::cache::Cache;
use crate::config::{AccountConfig, Protocol};
use crate::models::{DraftOrigin, FolderKind, Message};
use crate::worker::helper::send::{send_outcome, send_parameters, SendOutcome};
use crate::worker::helper::{HelperClient, HelperError, Object, Transport};
use crate::worker::ids::hash_uid;
use crate::worker::test_support::{queued, sample_account, sample_outgoing};
use crate::worker::{OutgoingMessage, WorkerEvent};

fn row(id: &str, role: &str, label: &str, parent: &str) -> FolderRow {
    FolderRow {
        id: id.into(),
        role: role.into(),
        label: label.into(),
        parent: parent.into(),
    }
}

pub(super) fn account() -> AccountConfig {
    AccountConfig {
        email: "me@tuta.com".into(),
        protocol: Protocol::Tuta,
        helper_session: json!({"login": "me@tuta.com", "user_id": "u", "access_token": "t"})
            .to_string(),
        ..sample_account()
    }
}

/// What the fake helper was asked, in order.
pub(super) type Asked = Arc<Mutex<Vec<Value>>>;

/// A transport answering each request with `reply(op, request)`: a result
/// object, or an error code.
pub(super) fn fake(
    reply: impl Fn(&str, &Value) -> Result<Value, &'static str> + Send + Sync + 'static,
) -> (Transport, Asked) {
    let asked: Asked = Arc::default();
    let log = asked.clone();
    let transport: Transport = Arc::new(move |request: &Value| {
        log.lock().unwrap().push(request.clone());
        let op = request["op"].as_str().unwrap_or_default();
        let reply = match reply(op, request) {
            Ok(result) => json!({"version": 1, "result": result}),
            Err(code) => json!({"version": 1, "error": {"code": code}}),
        };
        match reply {
            Value::Object(reply) => Ok(reply),
            _ => Err(HelperError::new("invalid_response")),
        }
    });
    (transport, asked)
}

pub(super) fn ops(asked: &Asked) -> Vec<String> {
    asked
        .lock()
        .unwrap()
        .iter()
        .map(|r| r["op"].as_str().unwrap_or_default().to_string())
        .collect()
}

pub(super) fn header(id: &str, ms: i64, unread: bool) -> Value {
    json!({
        "id": id, "subject": "Hello", "date_ms": ms,
        "sender": {"name": "Ann", "address": "ann@example.com"},
        "recipient": {"name": "", "address": "me@tuta.com"},
        "is_unread": unread, "confidential": false,
        "conversation": "Conv1", "has_attachments": true, "reply_type": 1
    })
}

pub(super) fn folder_rows() -> Value {
    json!([
        {"id": "f/inbox", "role": "inbox", "label": "Inbox", "parent": ""},
        {"id": "f/drafts", "role": "drafts", "label": "Drafts", "parent": ""},
        {"id": "f/sent", "role": "sent", "label": "Sent", "parent": ""},
        {"id": "f/trash", "role": "trash", "label": "Trash", "parent": ""},
        {"id": "f/spam", "role": "junk", "label": "Spam", "parent": ""},
        {"id": "f/work", "role": "other", "label": "Work", "parent": ""}
    ])
}

/// A state whose folders came from [`folder_rows`], talking to `transport`.
pub(super) fn state_with(transport: Transport) -> TutaState {
    let account = account();
    let mut state = TutaState::new(&account);
    state.transport = transport;
    let page = sync_page(
        json!({"folders": folder_rows(), "folder": "f/inbox", "messages": []})
            .as_object()
            .unwrap(),
    )
    .unwrap();
    let list = tuta_folders(&page.folders, 1, &Default::default(), &[], &[]);
    state.adopt_folders(&list);
    state
}

pub(super) fn folder_id(state: &TutaState, path: &str) -> u32 {
    state.folders[path].0
}

#[test]
fn folder_roles_become_kinds_and_parents_become_paths() {
    let rows = [
        row("l/1", "inbox", "Inbox", ""),
        row("l/2", "sent", "Sent", ""),
        row("l/3", "junk", "Spam", ""),
        row("l/4", "other", "Projects", ""),
        row("l/5", "other", "2026/Q1", "l/4"),
        row("l/6", "other", "Orphan", "l/missing"),
        row("l/7", "newer-role", "Templates", ""),
        row("l/8", "other", "Snoozed", ""),
        row("l/9", "other", "Templates", "l/4"),
        row("l/10", "other", "templates", ""),
    ];
    let list = tuta_folders(&rows, 3, &Default::default(), &[], &[]);
    let by_path = |p: &str| list.iter().find(|f| f.folder.path == p).unwrap();
    assert_eq!(by_path("Inbox").folder.kind, FolderKind::Inbox);
    assert_eq!(by_path("Inbox").folder.id, 1);
    assert_eq!(by_path("Sent").folder.kind, FolderKind::Sent);
    assert_eq!(by_path("Spam").folder.kind, FolderKind::Junk);
    // A system folder goes by its role alone; a user folder by name only
    // as the Templates or Snoozed folder Hylki makes at the top.
    assert_eq!(by_path("Templates").folder.kind, FolderKind::Custom);
    assert_eq!(by_path("templates").folder.kind, FolderKind::Templates);
    assert_eq!(by_path("Snoozed").folder.kind, FolderKind::Snoozed);
    assert_eq!(
        by_path("Projects/Templates").folder.kind,
        FolderKind::Custom
    );
    let child = list
        .iter()
        .find(|f| f.tuta_id == "l/5")
        .expect("the sub-folder is listed");
    // A '/' in a label is not a level of its own.
    assert!(child.folder.path.starts_with("Projects/"));
    assert_ne!(child.folder.path, "Projects/2026/Q1");
    assert!(list.iter().all(|f| f.tuta_id != "l/6"));
    assert!(list.iter().all(|f| f.folder.account_id == 3));
    // Hidden folders (#239) and manual roles (#82) apply.
    let roles = [("archive".to_string(), "Projects".to_string())].into();
    let list = tuta_folders(&rows, 3, &roles, &["Spam".to_string()], &[]);
    assert!(list.iter().all(|f| f.folder.path != "Spam"));
    let projects = list.iter().find(|f| f.folder.path == "Projects").unwrap();
    assert_eq!(projects.folder.kind, FolderKind::Archive);
}

#[test]
fn a_header_becomes_a_threaded_row() {
    let (m, id) = tuta_message(&header("L1/E1", 1_700_000_000_123, true), 4, 2).unwrap();
    assert_eq!(id, "L1/E1");
    assert_eq!(m.uid, hash_uid("L1/E1"));
    assert_eq!((m.account_id, m.folder_id), (4, 2));
    assert_eq!(m.timestamp, 1_700_000_000);
    assert_eq!(m.from_addr, "ann@example.com");
    assert!(m.unread && m.answered && !m.forwarded && m.has_attachment);
    // No Message-ID in this listing; the conversation threads it, and never
    // reaches a wire header.
    assert!(m.message_id.is_empty());
    assert_eq!(m.references, "tuta-conv:Conv1");
    assert!(crate::worker::ids::is_conversation_token(&m.references));
    assert!(tuta_message(&json!({"date_ms": 1}), 1, 1).is_none());
    assert!(tuta_message(&json!({"id": "a/b"}), 1, 1).is_none());
}

#[test]
fn a_listed_message_id_is_stored_as_the_cache_stores_one() {
    let mut listed = header("l/1", 1_000, true);
    listed["message_id"] = json!("Abc.1+X@Tuta.com");
    let (m, _) = tuta_message(&listed, 1, 1).unwrap();
    assert_eq!(m.message_id, "abc.1+x@tuta.com");
    // Its spelling comes back for the reply that names it.
    let cache = Cache::in_memory().unwrap();
    assert_eq!(cache.exact_msgids(&m.message_id), "Abc.1+X@Tuta.com");
    let mut null = header("l/2", 1_000, true);
    null["message_id"] = Value::Null;
    assert!(tuta_message(&null, 1, 1).unwrap().0.message_id.is_empty());
}

#[test]
fn a_sync_reply_is_checked_before_it_is_used() {
    let ok = json!({"folders": folder_rows(), "folder": "f/inbox", "messages": [],
        "all_ids": ["a/1", "a/2"],
        "labels": [{"id": "t/1", "name": "Work", "color": "#2ec27e"}]});
    let page = sync_page(ok.as_object().unwrap()).unwrap();
    assert_eq!(page.all_ids.unwrap(), ["a/1", "a/2"]);
    assert_eq!(page.labels.unwrap()[0].name, "Work");
    // A helper that lists no labels says nothing about them.
    let older = json!({"folders": [], "folder": "f/inbox", "messages": []});
    assert!(sync_page(older.as_object().unwrap())
        .unwrap()
        .labels
        .is_none());
    for bad in [
        json!({"folder": "f/inbox", "messages": []}),
        json!({"folders": [{"id": ""}], "folder": "f/inbox", "messages": []}),
        json!({"folders": [], "folder": "f/inbox", "messages": [], "all_ids": [1]}),
        json!({"folders": [], "folder": "f/inbox", "messages": [], "all_ids": "a/1"}),
        json!({"folders": [], "messages": []}),
        json!({"folders": [], "folder": "f/inbox", "messages": [], "labels": {}}),
        json!({"folders": [], "folder": "f/inbox", "messages": [],
            "labels": [{"id": "t/1", "name": ""}]}),
    ] {
        let err = sync_page(bad.as_object().unwrap()).unwrap_err();
        assert_eq!(err.code, "invalid_response", "{bad}");
    }
}

#[test]
fn a_listing_names_the_labels_a_row_wears() {
    let (transport, _) = fake(|_, _| Ok(json!({})));
    let mut state = state_with(transport);
    let mut labeled = header("l/1", 1_000, true);
    labeled["labels"] = json!(["t/1", "t/2", "t/gone"]);
    let page = sync_page(
        json!({"folders": folder_rows(), "folder": "f/inbox",
            "messages": [labeled, header("l/2", 900, true)],
            "labels": [{"id": "t/1", "name": "Work", "color": "#2ec27e"},
                {"id": "t/2", "name": "R&D", "color": ""}]})
        .as_object()
        .unwrap(),
    )
    .unwrap();
    let rows = super::index::page_messages(&page, 1, 1, &mut state);
    // Written as Gmail's are, so a tag named like one shows as that tag.
    assert_eq!(rows[0].labels, ["Work", "R&-D"]);
    assert!(rows[1].labels.is_empty());
    let tag = crate::config::Tag {
        name: "R&D".into(),
        keyword: "rnd".into(),
        color: "#1c71d8".into(),
    };
    assert!(rows[0].has_tag(&tag));
    assert_eq!(state.labels.len(), 2);
}

#[test]
fn the_index_plan_finds_runs_of_missing_mail() {
    let ids: Vec<String> = (0..6).map(|i| format!("l/{i}")).collect();
    // 1 and 4 are cached; "gone" was cached long ago and is not listed.
    let mut cached = std::collections::HashMap::new();
    cached.insert(hash_uid("l/1"), 0);
    cached.insert(hash_uid("l/4"), 0);
    cached.insert(hash_uid("gone"), 0);
    let (gone, runs) =
        crate::worker::index_queue::snapshot_plan(&ids, &cached, 10_000, TUTA_BACKFILL_CHUNK);
    assert_eq!(gone, [hash_uid("gone")]);
    assert_eq!(runs, [(0, 1), (2, 2), (5, 1)]);
}

#[test]
fn a_first_page_fills_the_uid_map_and_prunes_by_the_snapshot() {
    let (transport, _) = fake(|_, _| Ok(json!({})));
    let mut state = state_with(transport);
    let inbox = folder_id(&state, "Inbox");
    let cache = Cache::in_memory().unwrap();
    // A row the server no longer has, and one with a star and a Message-ID
    // only this computer knows.
    let (mut kept, _) = tuta_message(&header("l/1", 1_000_000, true), 1, inbox).unwrap();
    kept.starred = true;
    kept.message_id = "abc@example.com".into();
    let (gone, _) = tuta_message(&header("l/old", 500_000, true), 1, inbox).unwrap();
    cache.upsert_messages(1, "Inbox", &[kept, gone]);
    let page = sync_page(
        json!({"folders": folder_rows(), "folder": "f/inbox",
            "messages": [header("l/1", 1_000_000, false)],
            "all_ids": ["l/1", "l/2"]})
        .as_object()
        .unwrap(),
    )
    .unwrap();
    let (rows, complete) =
        apply_first_page(&page, 1, inbox, "Inbox", 2_000, Some(&cache), &mut state);
    assert!(!complete, "l/2 is not in the page");
    assert_eq!(state.uids.get(&hash_uid("l/2")).unwrap(), "l/2");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.uid, hash_uid("l/1"));
    assert!(!row.unread, "the server's read state wins");
    assert!(row.starred, "the star is kept here");
    assert_eq!(row.message_id, "abc@example.com");
}

#[test]
fn a_listing_keeps_what_only_the_cache_knows() {
    let (mut listed, _) = tuta_message(&header("l/1", 1_000, true), 1, 1).unwrap();
    let cached = Message {
        starred: true,
        message_id: "id@x".into(),
        references: "parent@x tuta-conv:Conv1".into(),
        preview: "Hi there".into(),
        ..listed.clone()
    };
    carry_over(std::slice::from_mut(&mut listed), &[cached]);
    assert!(listed.starred);
    assert_eq!(listed.message_id, "id@x");
    assert_eq!(listed.references, "parent@x tuta-conv:Conv1");
    assert_eq!(listed.preview, "Hi there");
}

#[test]
fn a_read_bodys_message_id_wins_over_the_listed_one() {
    let mut header = header("l/1", 1_000, true);
    header["message_id"] = json!("Listed@x");
    let (mut listed, _) = tuta_message(&header, 1, 1).unwrap();
    let cached = Message {
        message_id: "body@x".into(),
        references: "parent@x tuta-conv:Conv1".into(),
        ..listed.clone()
    };
    carry_over(std::slice::from_mut(&mut listed), &[cached]);
    assert_eq!(listed.message_id, "body@x");
    assert_eq!(listed.references, "parent@x tuta-conv:Conv1");
    // Never read: the listed one stands.
    let (mut fresh, _) = tuta_message(&header, 1, 1).unwrap();
    carry_over(std::slice::from_mut(&mut fresh), &[]);
    assert_eq!(fresh.message_id, "listed@x");
}

#[tokio::test]
async fn a_move_drops_the_source_row_and_lists_the_destination_again() {
    // A Tuta mail keeps its id through a move, and the helper says so: the
    // destination's listing has it under the same uid, with what its read
    // body showed carried over.
    let (transport, asked) = fake(|op, request| match op {
        "move" => Ok(json!({"outcomes": [
            {"id": "l/1", "code": null, "new_id": "l/1", "source": "f/inbox"}
        ]})),
        "sync" => {
            assert_eq!(request["folder"], "f/work");
            Ok(json!({"folders": folder_rows(), "folder": "f/work",
                "messages": [header("l/1", 1_000_000, false)], "all_ids": ["l/1"]}))
        }
        _ => Err("unsupported_operation"),
    });
    let mut state = state_with(transport);
    let account = account();
    let cache = Cache::in_memory().unwrap();
    let inbox = folder_id(&state, "Inbox");
    let (mut m, id) = tuta_message(&header("l/1", 1_000_000, true), 1, inbox).unwrap();
    m.message_id = "read@example.com".into();
    state.uids.insert(m.uid, id);
    cache.upsert_messages(1, "Inbox", std::slice::from_ref(&m));
    let events = std::cell::RefCell::new(Vec::new());
    tuta_move_uids(
        &account,
        1,
        &mut state,
        "Inbox",
        &[m.uid],
        "Work",
        Some(&cache),
        &|e| events.borrow_mut().push(e),
    )
    .await
    .unwrap();
    assert_eq!(ops(&asked), ["move", "sync"]);
    assert_eq!(asked.lock().unwrap()[0]["destination"], "f/work");
    assert!(cache.load_messages(1, "Inbox", inbox).is_empty());
    assert_eq!(state.uids[&m.uid], "l/1");
    let work = folder_id(&state, "Work");
    let moved = cache.load_messages(1, "Work", work);
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].uid, m.uid);
    assert_eq!(moved[0].message_id, "read@example.com");
    assert!(events
        .into_inner()
        .iter()
        .any(|e| matches!(e, WorkerEvent::Messages { folder_id, .. } if *folder_id == work)));
}

#[test]
fn a_mutation_reply_is_checked_row_by_row() {
    let ids = ["l/1".to_string(), "l/2".to_string()];
    let rows = super::wire::outcomes(
        json!({"outcomes": [
            {"id": "l/1", "code": null, "new_id": "l/1"},
            {"id": "l/2", "code": "not_attempted", "new_id": null}
        ]})
        .as_object()
        .unwrap(),
        &ids,
    )
    .unwrap();
    assert_eq!(rows[0].new_id.as_deref(), Some("l/1"));
    assert_eq!(rows[1].code.as_deref(), Some("not_attempted"));
    for bad in [
        json!({"outcomes": [{"id": "l/1", "code": null}]}),
        json!({"outcomes": [{"id": "l/1", "code": 1}, {"id": "l/2", "code": null}]}),
        json!({"outcomes": [{"id": "l/1", "code": null, "new_id": ""},
            {"id": "l/2", "code": null}]}),
        json!({"outcomes": [{"id": "l/2", "code": null}, {"id": "l/1", "code": null}]}),
    ] {
        let err = super::wire::outcomes(bad.as_object().unwrap(), &ids).unwrap_err();
        assert_eq!(err.code, "invalid_response", "{bad}");
    }
    // A row's code off the helper's list reads as `request_failed`, so no
    // server text reaches a log or the screen.
    let (transport, _) = fake(|_, _| {
        Ok(json!({"outcomes": [{"id": "l/1", "code": "Server says: quota of user x@y"}]}))
    });
    let (applied, error) =
        super::wire::mark_read(&client_for(transport), &["l/1".to_string()], true);
    assert!(applied.is_empty());
    assert_eq!(error.unwrap().code, "request_failed");
}

#[tokio::test]
async fn a_move_that_stops_part_way_keeps_what_did_not_go() {
    let (transport, asked) = fake(|op, _| match op {
        "move" => Ok(json!({"outcomes": [
            {"id": "l/1", "code": null},
            {"id": "l/2", "code": "request_failed"}
        ]})),
        "sync" => Ok(json!({"folders": folder_rows(), "folder": "f/trash",
            "messages": [], "all_ids": []})),
        _ => Err("unsupported_operation"),
    });
    let mut state = state_with(transport);
    let cache = Cache::in_memory().unwrap();
    let inbox = folder_id(&state, "Inbox");
    let mut uids = Vec::new();
    for id in ["l/1", "l/2"] {
        let (m, id) = tuta_message(&header(id, 1_000_000, true), 1, inbox).unwrap();
        state.uids.insert(m.uid, id);
        cache.upsert_messages(1, "Inbox", std::slice::from_ref(&m));
        uids.push(m.uid);
    }
    let err = tuta_move_uids(
        &account(),
        1,
        &mut state,
        "Inbox",
        &uids,
        "Trash",
        Some(&cache),
        &|_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "request_failed");
    assert_eq!(ops(&asked), ["move", "sync"]);
    let left = cache.load_messages(1, "Inbox", inbox);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].uid, hash_uid("l/2"));
}

#[tokio::test]
async fn read_marks_follow_the_helpers_outcomes() {
    let (transport, asked) = fake(|op, request| match op {
        "mark_read" => {
            assert_eq!(request["is_read"], true);
            Ok(json!({"outcomes": [
                {"id": "l/1", "code": null},
                {"id": "l/2", "code": "request_failed"},
                {"id": "l/3", "code": "not_attempted"}
            ]}))
        }
        _ => Err("unsupported_operation"),
    });
    let mut state = state_with(transport);
    let cache = Cache::in_memory().unwrap();
    let inbox = folder_id(&state, "Inbox");
    let mut uids = Vec::new();
    for id in ["l/1", "l/2", "l/3"] {
        let (m, id) = tuta_message(&header(id, 1_000_000, true), 1, inbox).unwrap();
        state.uids.insert(m.uid, id);
        cache.upsert_messages(1, "Inbox", std::slice::from_ref(&m));
        uids.push(m.uid);
    }
    let applied = tuta_set_seen(
        &account(),
        1,
        &mut state,
        "Inbox",
        &uids,
        true,
        Some(&cache),
        &|_| {},
    )
    .await;
    assert_eq!(applied, [hash_uid("l/1")]);
    assert_eq!(ops(&asked), ["mark_read"]);
    let rows = cache.load_messages(1, "Inbox", inbox);
    let unread = |id: &str| rows.iter().find(|m| m.uid == hash_uid(id)).unwrap().unread;
    assert!(!unread("l/1"));
    assert!(unread("l/2") && unread("l/3"));
}

/// A minimal built message, as `build_email` writes one.
const RAW: &[u8] = b"From: Me <me@tuta.com>\r\nTo: Ann <ann@example.com>\r\nCc: bob@example.com\r\n\
Subject: Plans\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nSee you <soon>\r\n";

#[test]
fn send_parameters_come_from_the_built_message_and_its_envelope() {
    let envelope = [
        "ann@example.com".to_string(),
        "BOB@example.com".to_string(),
        "secret@example.com".to_string(),
    ];
    let p = send_parameters(RAW, &envelope, "").unwrap();
    assert_eq!(p["sender"], "me@tuta.com");
    assert_eq!(p["sender_name"], "Me");
    assert_eq!(
        p["to"],
        json!([{"name": "Ann", "address": "ann@example.com"}])
    );
    assert_eq!(p["cc"], json!([{"name": "", "address": "bob@example.com"}]));
    // Bcc is only in the envelope.
    assert_eq!(
        p["bcc"],
        json!([{"name": "", "address": "secret@example.com"}])
    );
    assert_eq!(p["subject"], "Plans");
    let body = p["body_html"].as_str().unwrap();
    assert!(body.starts_with("<pre>See you &lt;soon&gt;"), "{body}");
    assert_eq!(p["delivery"], "ordinary");
    assert_eq!(p["ordinary_confirmed"], true);
    assert_eq!(p["attachments"], json!([]));
    // New mail sits in no conversation yet.
    for key in ["in_reply_to", "references", "action"] {
        assert!(!p.contains_key(key), "{key}");
    }
}

pub(super) fn client_for(transport: Transport) -> HelperClient {
    let account = account();
    HelperClient::new(
        super::SPEC,
        transport,
        &account.email,
        &account.helper_session,
    )
    .unwrap()
}

#[tokio::test]
async fn each_way_a_send_ends_is_told_apart() {
    let confidential = json!({"message_id": "x", "sent_mail": "s/1", "confidential": true,
        "recipients": [{"address": "ann@tuta.com", "key_version": 2,
            "protocol": "tuta_crypt", "fingerprint": "ab12"}]});
    for (answer, expected) in [
        (Ok(json!({"message_id": "x", "sent_mail": "s/1"})), "sent"),
        (Ok(confidential), "sent"),
        (Err("send_uncertain"), "uncertain"),
        (Err("helper_failed"), "uncertain"),
        (Err("timeout"), "uncertain"),
        (Err("recipient_not_found"), "refused"),
        (Err("secure_external_unsupported"), "refused"),
        (Err("invalid_recipient"), "refused"),
        (Err("invalid_thread"), "refused"),
        (Err("send_failed"), "failed"),
        (Err("transport_failed"), "failed"),
        (Err("session_revoked"), "failed"),
    ] {
        let reply = answer.clone();
        let (transport, asked) = fake(move |op, _| match op {
            "send" => reply.clone(),
            _ => Err("unsupported_operation"),
        });
        let outcome = tuta_send_raw(
            client_for(transport),
            RAW.to_vec(),
            vec!["ann@example.com".into()],
            String::new(),
        )
        .await;
        let got = match &outcome {
            SendOutcome::Sent => "sent",
            SendOutcome::Uncertain => "uncertain",
            SendOutcome::Refused(_) => "refused",
            SendOutcome::Failed(_) => "failed",
        };
        assert_eq!(got, expected, "{answer:?}");
        assert_eq!(ops(&asked), ["send"]);
    }
    // A Tuta address with no key is refused with words of its own, and
    // never sent unencrypted instead.
    for code in ["recipient_not_found", "secure_external_unsupported"] {
        match send_outcome(&HelperError::new(code)) {
            SendOutcome::Refused(why) => assert!(why.contains("Tuta"), "{why}"),
            other => panic!("{other:?}"),
        }
    }
    // The old refusal of every Tuta-only send is gone: such a send goes.
    assert!(!super::wire::SPEC
        .codes
        .contains(&"confidential_send_unsupported"));
    // The helper missing sent nothing: it can wait in the Outbox.
    assert!(matches!(
        send_outcome(&HelperError::new("helper_missing")),
        SendOutcome::Failed(_)
    ));
}

/// A built reply to `rcpts`, its threading headers as a client wrote them:
/// one References id is malformed.
fn reply_raw(rcpts: &[&str], in_reply_to: &str) -> Vec<u8> {
    format!(
        "From: Me <me@tuta.com>\r\nTo: {}\r\nMessage-ID: <Abc.2@tuta.com>\r\n\
         In-Reply-To: {in_reply_to}\r\n\
         References: <Root.1@Example.com> <bad id> <Parent.1@Example.com>\r\n\
         Subject: Re: Plans\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nYes\r\n",
        rcpts.join(", ")
    )
    .into_bytes()
}

/// The threading fields of a helper request, null where absent.
fn thread_fields(request: &Value) -> [Value; 3] {
    ["in_reply_to", "references", "action"].map(|k| request.get(k).cloned().unwrap_or(Value::Null))
}

#[tokio::test]
async fn a_reply_or_forward_tells_the_helper_what_it_answers() {
    // What the helper was sent for one send.
    let sent = |raw: Vec<u8>, rcpts: &[&str], forwards: &str| {
        let (transport, asked) = fake(|op, _| match op {
            "send" => Ok(json!({"message_id": "x", "sent_mail": "s/1", "parent_marked": true})),
            _ => Err("unsupported_operation"),
        });
        let envelope: Vec<String> = rcpts.iter().map(|r| r.to_string()).collect();
        let forwards = forwards.to_string();
        async move {
            let outcome = tuta_send_raw(client_for(transport), raw, envelope, forwards).await;
            assert_eq!(outcome, SendOutcome::Sent);
            let request = asked.lock().unwrap()[0].clone();
            request
        }
    };
    let parent = "<Parent.1@Example.com>";

    // A reply: the ids as they arrived, less the brackets, and less one the
    // helper would refuse the send over.
    let reply = sent(
        reply_raw(&["ann@example.com"], parent),
        &["ann@example.com"],
        "",
    )
    .await;
    assert_eq!(
        thread_fields(&reply),
        [
            json!("Parent.1@Example.com"),
            json!("Root.1@Example.com Parent.1@Example.com"),
            json!("reply")
        ]
    );

    // To more than one distinct address, it is a reply-all.
    let rcpts = ["ann@example.com", "bob@example.com", "ANN@example.com"];
    let all = sent(reply_raw(&rcpts[..2], parent), &rcpts, "").await;
    assert_eq!(all["in_reply_to"], "Parent.1@Example.com");
    assert_eq!(all["action"], "reply_all");
    // The same address twice is still one person.
    let twice = sent(reply_raw(&["ann@example.com"], parent), &rcpts[..1], "").await;
    assert_eq!(twice["action"], "reply");

    // A forward names what it forwards, whoever it goes to.
    let forward = sent(
        RAW.to_vec(),
        &["ann@example.com", "bob@example.com"],
        "Fwd.1@Example.com",
    )
    .await;
    assert_eq!(
        thread_fields(&forward),
        [json!("Fwd.1@Example.com"), Value::Null, json!("forward")]
    );

    // New mail carries none of them.
    let new = sent(RAW.to_vec(), &["ann@example.com"], "").await;
    assert_eq!(thread_fields(&new), [Value::Null, Value::Null, Value::Null]);

    // A malformed In-Reply-To is dropped rather than failing the send; the
    // References still place it.
    let odd = sent(
        reply_raw(&["ann@example.com"], "<a b@x>"),
        &["ann@example.com"],
        "",
    )
    .await;
    assert_eq!(
        thread_fields(&odd),
        [
            Value::Null,
            json!("Root.1@Example.com Parent.1@Example.com"),
            json!("reply")
        ]
    );
}

#[test]
fn a_draft_names_what_it_answers_too() {
    let reply = reply_raw(
        &["ann@example.com", "bob@example.com"],
        "<Parent.1@Example.com>",
    );
    let p = draft_parameters(&reply, "").unwrap();
    assert_eq!(p["in_reply_to"], "Parent.1@Example.com");
    assert_eq!(p["action"], "reply_all");
    let forward = draft_parameters(RAW, "Fwd.1@Example.com").unwrap();
    assert_eq!(forward["action"], "forward");
    let new = draft_parameters(RAW, "").unwrap();
    assert!(!new.contains_key("in_reply_to") && !new.contains_key("action"));
}

#[tokio::test]
async fn a_queued_forward_keeps_what_it_forwards() {
    let (transport, asked) = fake(|op, _| match op {
        "send" => Ok(json!({"message_id": "x", "sent_mail": "s/1"})),
        "sync" => Err("request_failed"),
        _ => Err("unsupported_operation"),
    });
    let mut state = state_with(transport);
    let cache = Cache::in_memory().unwrap();
    let id = cache
        .queue_outbox(
            1,
            "me@tuta.com",
            &["ann@example.com".into()],
            "",
            "Fwd: Plans",
            "",
            RAW,
            None,
            "offline",
            None,
        )
        .unwrap();
    cache.set_outbox_forwards(id, "fwd.1@example.com");
    tuta_flush_outbox(Some(&cache), 1, &account(), None, &mut state, &|_| {}).await;
    let request = asked.lock().unwrap()[0].clone();
    assert_eq!(request["op"], "send");
    assert_eq!(request["in_reply_to"], "fwd.1@example.com");
    assert_eq!(request["action"], "forward");
    assert!(cache.outbox_items(1).is_empty());
}

#[tokio::test]
async fn an_uncertain_outbox_message_is_never_sent_again() {
    let (transport, asked) = fake(|op, _| match op {
        "send" => Err("send_uncertain"),
        _ => Err("unsupported_operation"),
    });
    let mut state = state_with(transport);
    let account = account();
    let cache = Cache::in_memory().unwrap();
    let item = queued("me@tuta.com", &["ann@example.com"]);
    let id = cache
        .queue_outbox(
            1,
            &item.from_addr,
            &item.rcpts,
            "",
            "Notes",
            "",
            RAW,
            None,
            "offline",
            None,
        )
        .unwrap();
    // The first flush tries it, and it may have gone: marked, and kept.
    tuta_flush_outbox(Some(&cache), 1, &account, None, &mut state, &|_| {}).await;
    assert_eq!(ops(&asked), ["send"]);
    let items = cache.outbox_items(1);
    assert_eq!(items.len(), 1);
    assert!(items[0].is_send_uncertain());
    // No later flush sends it, whether every due message or this one.
    let events = std::cell::RefCell::new(Vec::new());
    tuta_flush_outbox(Some(&cache), 1, &account, None, &mut state, &|_| {}).await;
    tuta_flush_outbox(Some(&cache), 1, &account, Some(id), &mut state, &|e| {
        events.borrow_mut().push(e)
    })
    .await;
    assert_eq!(ops(&asked), ["send"], "it was not sent again");
    assert!(events
        .into_inner()
        .iter()
        .any(|e| matches!(e, WorkerEvent::Error { .. })));
    assert_eq!(cache.outbox_items(1).len(), 1);
}

#[test]
fn mail_between_tuta_accounts_reads_with_the_message_id_tuta_keeps() {
    let reply = |message_id: Value, headers: &str| {
        json!({
            "header": {"id": "l/1", "subject": "Plans", "date_ms": 1_700_000_000_000_i64,
                "sender": {"name": "", "address": "ann@tuta.com"}, "message_id": message_id},
            "html": "<p>Hi</p>", "headers": headers, "to": [], "cc": [], "reply_to": [],
            "attachments": []
        })
    };
    let parse = |reply: Value| {
        let raw = mime_message(reply.as_object().unwrap()).unwrap();
        crate::worker::summary_from_raw(1, 1, 1, &raw)
    };
    // No header block: the kept id, exactly as Tuta spells it.
    let internal = parse(reply(json!("Abc.1+X@Tuta.com"), ""));
    assert_eq!(internal.message_id, "abc.1+x@tuta.com");
    let cache = Cache::in_memory().unwrap();
    assert_eq!(cache.exact_msgids(&internal.message_id), "Abc.1+X@Tuta.com");
    // An original header wins; nothing is made up without either.
    let external = parse(reply(
        json!("other@x"),
        "Message-ID: <Orig@example.com>\r\n",
    ));
    assert_eq!(external.message_id, "orig@example.com");
    assert!(parse(reply(Value::Null, "")).message_id.is_empty());
    assert!(parse(reply(json!("a b@x"), "")).message_id.is_empty());
}

#[test]
fn a_message_reply_reads_as_mime() {
    let reply = json!({
        "header": {"id": "l/1", "subject": "Grüße", "date_ms": 1_700_000_000_000_i64,
            "sender": {"name": "Ann, A.", "address": "ann@example.com"}},
        "html": "<p>Hi</p>",
        "headers": "Message-ID: <Abc@Example.com>\r\nReferences: <p@x>\r\n <q@x>\r\nX-Other: 1\r\n",
        "to": [{"name": "", "address": "me@tuta.com"}],
        "cc": [], "reply_to": [],
        "attachments": [{"name": "a.txt", "mime_type": "text/plain", "cid": null,
            "data": "aGVsbG8="}]
    });
    let raw = mime_message(reply.as_object().unwrap()).unwrap();
    let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
    assert_eq!(parsed.subject(), Some("Grüße"));
    assert_eq!(parsed.message_id(), Some("Abc@Example.com"));
    assert_eq!(
        parsed
            .from()
            .and_then(|a| a.first())
            .and_then(|a| a.address()),
        Some("ann@example.com")
    );
    assert_eq!(parsed.attachments().count(), 1);
    assert_eq!(parsed.body_html(0).as_deref(), Some("<p>Hi</p>"));
    // A reply missing its body is refused, not shown empty.
    let mut broken: Object = reply.as_object().unwrap().clone();
    broken.remove("html");
    assert_eq!(mime_message(&broken).unwrap_err().code, "invalid_response");
}

/// The ids of the mail among the newest in `folder` whose subject holds
/// `token`.
fn token_mail(
    client: &HelperClient,
    folder: &str,
    token: &str,
) -> Result<Vec<String>, HelperError> {
    let page = super::wire::sync(client, Some(folder), 25, 0)?;
    Ok(page
        .headers
        .iter()
        .filter(|h| h["subject"].as_str().is_some_and(|s| s.contains(token)))
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect())
}

/// Remove whatever the live test made, found by `token`, which every
/// subject and name it makes carries, so no other mail is ever touched:
/// drafts deleted, other mail moved to the Trash and erased there, then
/// folders and labels deleted. What could not be removed, as lines.
fn live_sweep(client: &HelperClient, token: &str) -> Vec<String> {
    use super::wire::{delete_draft, delete_folder, delete_ids, move_ids, sync};
    let mut left = Vec::new();
    let page = match sync(client, None, 1, 0) {
        Ok(page) => page,
        Err(e) => return vec![format!("sync: {}", e.code)],
    };
    let role = |role: &str| {
        page.folders
            .iter()
            .find(|f| f.role == role)
            .map(|f| f.id.clone())
    };
    let mut note = |what: &str, e: HelperError| left.push(format!("{what}: {}", e.code));
    if let Some(drafts) = role("drafts") {
        for id in token_mail(client, &drafts, token).unwrap_or_default() {
            if let Err(e) = delete_draft(client, &id) {
                note("delete a draft", e);
            }
        }
    }
    if let Some(trash) = role("trash") {
        for folder in ["inbox", "sent"].iter().filter_map(|r| role(r)) {
            let ids = token_mail(client, &folder, token).unwrap_or_default();
            if let (_, Some(e)) = move_ids(client, &ids, &trash) {
                note("move to the Trash", e);
            }
        }
        let ids = token_mail(client, &trash, token).unwrap_or_default();
        if let (_, Some(e)) = delete_ids(client, &ids, &trash) {
            note("erase in the Trash", e);
        }
    }
    // A folder holding mail goes to the Trash rather than away; deleted
    // again there, it goes for good.
    for _ in 0..3 {
        let Ok(page) = sync(client, None, 1, 0) else {
            break;
        };
        let made: Vec<_> = page
            .folders
            .iter()
            .filter(|f| f.label.contains(token))
            .collect();
        if made.is_empty() {
            break;
        }
        for folder in made {
            if let Err(e) = delete_folder(client, &folder.id) {
                note("delete a folder", e);
            }
        }
    }
    for label in page
        .labels
        .iter()
        .flatten()
        .filter(|l| l.name.contains(token))
    {
        let mut parameters = Object::new();
        parameters.insert("id".into(), label.id.as_str().into());
        if let Err(e) = client.request("delete_label", parameters) {
            note("delete a label", e);
        }
    }
    match sync(client, None, 1, 0) {
        Ok(page) => {
            for role in ["inbox", "sent", "drafts", "trash"] {
                let found = page.folders.iter().find(|f| f.role == role);
                if let Some(folder) = found {
                    match token_mail(client, &folder.id, token) {
                        Ok(ids) if ids.is_empty() => {}
                        Ok(ids) => left.push(format!("{} message(s) left in {role}", ids.len())),
                        Err(e) => left.push(format!("list {role}: {}", e.code)),
                    }
                }
            }
            if page.folders.iter().any(|f| f.label.contains(token)) {
                left.push("a folder is left".into());
            }
            if page.labels.iter().flatten().any(|l| l.name.contains(token)) {
                left.push("a label is left".into());
            }
        }
        Err(e) => left.push(format!("sync after cleanup: {}", e.code)),
    }
    left
}

/// The live test's sign-in, held for its whole run: on the way out,
/// however the test ended, it sweeps away what the test made (unless the
/// test already did) and then ends the session.
struct LiveGuard {
    client: HelperClient,
    token: String,
    swept: bool,
}

impl LiveGuard {
    fn sweep(&mut self) -> Vec<String> {
        self.swept = true;
        live_sweep(&self.client, &self.token)
    }
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        if !self.swept {
            let left = self.sweep();
            if !left.is_empty() {
                eprintln!("live cleanup left: {left:?}");
            }
        }
        if let Err(e) = self.client.request("revoke", Object::new()) {
            eprintln!("could not end the live session: {}", e.code);
        }
    }
}

/// One live step's outcome: what went wrong, said in a line.
type Step = Result<(), String>;

fn ensure(ok: bool, what: impl FnOnce() -> String) -> Step {
    if ok {
        Ok(())
    } else {
        Err(what())
    }
}

type LiveWorker = TutaWorker<Box<dyn Fn(WorkerEvent)>>;

/// A real Tuta worker on the live account, and what it said.
struct Live {
    w: LiveWorker,
    events: Rc<RefCell<Vec<WorkerEvent>>>,
    /// The `send` replies, as the helper gave them: the worker keeps only
    /// whether the send went, and the test checks how it went.
    sends: Arc<Mutex<Vec<Object>>>,
    email: String,
    token: String,
}

impl Live {
    /// The events since the last call, taken.
    fn take_events(&self) -> Vec<WorkerEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    /// The errors the worker said since the last call to this or
    /// [`take_events`](Self::take_events).
    fn errors(&self) -> Vec<String> {
        self.take_events()
            .into_iter()
            .filter_map(|e| match e {
                WorkerEvent::Error { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    fn no_errors(&self, step: &str) -> Step {
        let errors = self.errors();
        ensure(errors.is_empty(), || format!("{step}: {errors:?}"))
    }

    fn path(&self, kind: FolderKind) -> Result<String, String> {
        self.w
            .state
            .path_of(kind)
            .ok_or_else(|| format!("no {kind:?} folder"))
    }

    fn tuta_id(&self, path: &str) -> Result<String, String> {
        self.w
            .state
            .folders
            .get(path)
            .map(|(_, id)| id.clone())
            .ok_or_else(|| format!("{path} is not listed"))
    }

    /// The token's mail in `path`, listed through the worker's own sync.
    async fn token_rows(&mut self, path: &str) -> Result<Vec<Message>, String> {
        let (folder_id, _) = self.w.state.folders.get(path).cloned().ok_or("no folder")?;
        let w = &mut self.w;
        let messages = super::index::tuta_load_folder(
            &w.account,
            1,
            folder_id,
            path,
            w.cache.as_ref(),
            &mut w.state,
            &w.emit,
        )
        .await
        .map_err(|e| format!("list {path}: {}", e.code))?;
        Ok(messages
            .into_iter()
            .filter(|m| m.subject.contains(&self.token))
            .collect())
    }

    /// The token's mail in `path` once the server has filed it there.
    async fn arrived(&mut self, path: &str) -> Result<Vec<Message>, String> {
        for attempt in 0..10 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
            let rows = self.token_rows(path).await?;
            if !rows.is_empty() {
                return Ok(rows);
            }
        }
        Err(format!("nothing reached {path}"))
    }

    /// The label ids the listing of the Tuta folder `folder` gives the
    /// mail `id`.
    async fn labels_on(&self, folder: &str, id: &str) -> Result<Vec<String>, String> {
        let client = self.w.state.client(&self.w.account).map_err(|e| e.code)?;
        let (folder, id) = (folder.to_string(), id.to_string());
        let page = super::index::run(move || super::wire::sync(&client, Some(&folder), 25, 0))
            .await
            .map_err(|e| format!("sync: {}", e.code))?;
        let header = page
            .headers
            .iter()
            .find(|h| h["id"] == id.as_str())
            .ok_or("the mail is not listed")?;
        Ok(header["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l.as_str().map(str::to_string))
            .collect())
    }

    /// Send a message to `to` through the worker: the helper's reply.
    async fn send_to(&mut self, to: &str, kind: &str) -> Result<Object, String> {
        let message = OutgoingMessage {
            to: to.into(),
            subject: format!("Hylki live {kind} {}", self.token),
            body: "Sent by Hylki's live Tuta test.".into(),
            ..sample_outgoing()
        };
        self.sends.lock().unwrap().clear();
        self.w.on_send(Box::new(message), 7).await;
        let events = self.take_events();
        let sent = events.iter().any(|e| {
            matches!(
                e,
                WorkerEvent::SendDone {
                    ticket: 7,
                    sent: true
                }
            )
        });
        let errors: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                WorkerEvent::Error { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        ensure(sent, || format!("not sent: {errors:?}"))?;
        let reply = self.sends.lock().unwrap().pop().ok_or("no send reply")?;
        reply
            .get("result")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| "a send reply without a result".into())
    }
}

/// Confidential to the account itself: one key row for its own address,
/// both copies filed, and the Inbox copy read back. The Inbox copy's uid.
async fn live_confidential_send(live: &mut Live) -> Result<u32, String> {
    let email = live.email.clone();
    let result = live.send_to(&email, "confidential").await?;
    ensure(
        result.get("confidential") == Some(&Value::Bool(true)),
        || "not sent confidentially".into(),
    )?;
    let rows = result
        .get("recipients")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    ensure(rows.len() == 1, || {
        format!("{} recipient key rows", rows.len())
    })?;
    let row = &rows[0];
    ensure(
        row["address"]
            .as_str()
            .is_some_and(|a| a.eq_ignore_ascii_case(&email)),
        || "the key row names another address".into(),
    )?;
    let protocol = row["protocol"].as_str().unwrap_or_default();
    ensure(matches!(protocol, "tuta_crypt" | "rsa"), || {
        format!("protocol {protocol:?}")
    })?;
    ensure(row["key_version"].is_u64(), || "no key version".into())?;
    let fingerprint = row["fingerprint"].as_str().unwrap_or_default();
    ensure(
        fingerprint.len() == 64 && fingerprint.bytes().all(|b| b.is_ascii_hexdigit()),
        || "the fingerprint is not SHA-256 hex".into(),
    )?;
    eprintln!(
        "confidential send: {protocol} key, version {}",
        row["key_version"]
    );
    let (sent, inbox) = (live.path(FolderKind::Sent)?, live.path(FolderKind::Inbox)?);
    live.arrived(&sent).await?;
    let uid = live.arrived(&inbox).await?[0].uid;
    // The recipient's copy decrypts through the bucket key, as any other.
    let raw = live.w.raw_of(&inbox, uid).await?;
    let (body, _, _) = crate::worker::render_raw(&raw);
    ensure(body.contains("live Tuta test"), || {
        "the Inbox copy reads back without its body".into()
    })?;
    Ok(uid)
}

/// Ordinary to an address outside Tuta: no key rows.
async fn live_external_send(live: &mut Live, to: &str) -> Step {
    let result = live.send_to(to, "external").await?;
    ensure(
        result.get("confidential") == Some(&Value::Bool(false)),
        || "an external send went confidentially".into(),
    )?;
    ensure(
        result
            .get("recipients")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        || "an ordinary send reported keys".into(),
    )?;
    let sent = live.path(FolderKind::Sent)?;
    live.arrived(&sent).await.map(|_| ())
}

/// Make a folder, rename it and delete it, as the sidebar does.
async fn live_folders(live: &mut Live) -> Step {
    let made_path = |live: &Live| {
        live.w
            .state
            .folders
            .keys()
            .find(|p| p.contains(&live.token))
            .cloned()
    };
    let name = format!("Hylki live {}", live.token);
    live.w.on_create_folder(name.clone()).await;
    live.no_errors("create")?;
    ensure(made_path(live).as_deref() == Some(name.as_str()), || {
        "the made folder is not listed".into()
    })?;
    let renamed = format!("{name} renamed");
    live.w.on_rename_folder(name, renamed.clone()).await;
    live.no_errors("rename")?;
    ensure(made_path(live).as_deref() == Some(renamed.as_str()), || {
        "the folder is not listed under its new name".into()
    })?;
    live.w.on_delete_folder(renamed).await;
    live.no_errors("delete")?;
    ensure(made_path(live).is_none(), || {
        "the folder is still listed".into()
    })
}

/// Tag the Inbox copy, which makes its label, take the tag off, and
/// delete the label (no Hylki action does; it is the helper's op).
async fn live_labels(live: &mut Live, uid: u32) -> Step {
    let inbox = live.path(FolderKind::Inbox)?;
    let folder = live.tuta_id(&inbox)?;
    let id = live.w.state.uids.get(&uid).cloned().ok_or("no id")?;
    let keyword = format!("hylki-live-{}", live.token);
    let tag = |add| (inbox.clone(), uid, String::new(), keyword.clone(), add);
    let (path, uid, message_id, word, add) = tag(true);
    live.w
        .on_set_keyword(path, uid, message_id, word, add)
        .await;
    live.no_errors("tag")?;
    let label = live
        .w
        .state
        .labels
        .values()
        .find(|l| l.name == keyword)
        .cloned()
        .ok_or("no label was made")?;
    let on = live.labels_on(&folder, &id).await?;
    ensure(on.contains(&label.id), || {
        "the label is not on the mail".into()
    })?;
    let (path, uid, message_id, word, add) = tag(false);
    live.w
        .on_set_keyword(path, uid, message_id, word, add)
        .await;
    live.no_errors("untag")?;
    let on = live.labels_on(&folder, &id).await?;
    ensure(!on.contains(&label.id), || {
        "the label is still on the mail".into()
    })?;
    let client = live.w.state.client(&live.w.account).map_err(|e| e.code)?;
    let label_id = label.id.clone();
    let listed = super::index::run(move || {
        let mut parameters = Object::new();
        parameters.insert("id".into(), label_id.as_str().into());
        client.request("delete_label", parameters)?;
        client.request("list_labels", Object::new())
    })
    .await
    .map_err(|e| format!("delete the label: {}", e.code))?;
    live.w.state.labels.remove(&label.id);
    let left = listed
        .get("labels")
        .and_then(Value::as_array)
        .is_some_and(|labels| labels.iter().any(|l| l["id"] == label.id.as_str()));
    ensure(!left, || "the label is still listed".into())
}

/// Save a draft, update it in place, and delete it, as the composer and
/// the list do.
async fn live_drafts(live: &mut Live) -> Step {
    let drafts = live.path(FolderKind::Drafts)?;
    let (folder_id, _) = live.w.state.folders[&drafts].clone();
    let email = live.email.clone();
    let draft = |subject: String| OutgoingMessage {
        to: email.clone(),
        subject,
        body: "Saved by Hylki's live Tuta test.".into(),
        ..sample_outgoing()
    };
    let saved = |events: &[WorkerEvent]| {
        events.iter().any(|e| {
            matches!(
                e,
                WorkerEvent::DraftSaved {
                    message_id: Some(_),
                    ..
                }
            )
        })
    };
    let first = draft(format!("Hylki live draft {}", live.token));
    live.w
        .on_save_draft(Box::new(first), drafts.clone(), None, false)
        .await;
    let events = live.take_events();
    ensure(saved(&events), || "the draft was not saved".into())?;
    let rows = live.token_rows(&drafts).await?;
    ensure(rows.len() == 1, || format!("{} drafts listed", rows.len()))?;
    let uid = rows[0].uid;
    let updated = format!("Hylki live draft {} updated", live.token);
    let second = OutgoingMessage {
        draft_origin: Some(DraftOrigin {
            account_id: 1,
            folder_id,
            path: drafts.clone(),
            uid,
        }),
        ..draft(updated.clone())
    };
    live.w
        .on_save_draft(Box::new(second), drafts.clone(), None, false)
        .await;
    let events = live.take_events();
    ensure(saved(&events), || "the update was not saved".into())?;
    let rows = live.token_rows(&drafts).await?;
    ensure(rows.len() == 1 && rows[0].uid == uid, || {
        "the draft was not updated in place".into()
    })?;
    ensure(rows[0].subject == updated, || {
        "the update is not listed".into()
    })?;
    live.w.on_purge_messages(drafts.clone(), vec![uid]).await;
    live.no_errors("delete")?;
    let rows = live.token_rows(&drafts).await?;
    ensure(rows.is_empty(), || "the draft is still listed".into())
}

/// The next push signal, within a minute.
async fn next_signal(rx: &mut mpsc::UnboundedReceiver<PushSignal>) -> Option<PushSignal> {
    tokio::time::timeout(std::time::Duration::from_secs(60), rx.recv())
        .await
        .ok()
        .flatten()
}

/// Move the Inbox copy to the Trash, which also starts the cleanup, and
/// wait for the push thread to report a mail change in either folder.
async fn live_watch_move(
    live: &mut Live,
    rx: &mut mpsc::UnboundedReceiver<PushSignal>,
    uid: u32,
) -> Step {
    let (inbox, trash) = (live.path(FolderKind::Inbox)?, live.path(FolderKind::Trash)?);
    let (inbox_id, trash_id) = (live.tuta_id(&inbox)?, live.tuta_id(&trash)?);
    // What the earlier steps changed is let through first, so the change
    // seen next is the move's.
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    while rx.try_recv().is_ok() {}
    live.w
        .move_to(inbox, vec![uid], trash, |e| e.to_string())
        .await;
    live.no_errors("move")?;
    loop {
        match next_signal(rx).await {
            Some(PushSignal::Mail(folder)) => {
                let named = if folder == trash_id {
                    "the Trash"
                } else if folder == inbox_id {
                    "the Inbox"
                } else if folder.is_empty() {
                    "no folder"
                } else {
                    return Err("a change in another folder".into());
                };
                eprintln!("watch: mail_changed naming {named}");
                return Ok(());
            }
            Some(PushSignal::Live | PushSignal::Folders) => {}
            Some(PushSignal::Ended) => return Err("the watch ended".into()),
            None => return Err("no mail_changed within a minute".into()),
        }
    }
}

/// Erase every message the test sent, wherever it is now, through the
/// list's Delete Permanently (by way of the Trash).
async fn live_purge(live: &mut Live) -> Step {
    for kind in [FolderKind::Inbox, FolderKind::Sent, FolderKind::Trash] {
        let path = live.path(kind)?;
        let uids: Vec<u32> = live
            .token_rows(&path)
            .await?
            .iter()
            .map(|m| m.uid)
            .collect();
        if !uids.is_empty() {
            live.w.on_purge_messages(path, uids).await;
            live.no_errors("delete permanently")?;
        }
    }
    Ok(())
}

/// A live test's sign-in: the guard that cleans up after it, a real worker
/// on the account, and the `send` requests it made.
struct LiveStart {
    guard: LiveGuard,
    live: Live,
    requests: Arc<Mutex<Vec<Value>>>,
}

/// Sign in with `TUTA_LIVE`, `TUTA_LIVE_PASSWORD` and `TUTA_LIVE_TOTP`, as
/// the live tests do; `None` without `TUTA_LIVE`.
fn live_start() -> Option<LiveStart> {
    use crate::worker::helper::{login, process_transport, HelperLogin};
    let Ok(email) = std::env::var("TUTA_LIVE") else {
        eprintln!("TUTA_LIVE not set");
        return None;
    };
    let password = std::env::var("TUTA_LIVE_PASSWORD").expect("TUTA_LIVE_PASSWORD");
    let totp = std::env::var("TUTA_LIVE_TOTP").ok();
    let inner = process_transport(super::SPEC);
    let sign_in = HelperLogin {
        email: email.clone(),
        password,
        totp,
        ..HelperLogin::default()
    };
    let session =
        login(&inner, super::SPEC, &sign_in).unwrap_or_else(|e| panic!("login: {}", e.code));
    let token = crate::rng::from_alphabet(12, b"0123456789abcdef").unwrap();
    let guard = LiveGuard {
        client: HelperClient::new(super::SPEC, inner.clone(), &email, &session).unwrap(),
        token: token.clone(),
        swept: false,
    };
    let sends: Arc<Mutex<Vec<Object>>> = Arc::default();
    let requests: Arc<Mutex<Vec<Value>>> = Arc::default();
    let (log, asked) = (sends.clone(), requests.clone());
    let transport: Transport = Arc::new(move |request: &Value| {
        let reply = inner(request);
        if request["op"] == "send" {
            asked.lock().unwrap().push(request.clone());
            if let Ok(reply) = &reply {
                log.lock().unwrap().push(reply.clone());
            }
        }
        reply
    });
    let account = AccountConfig {
        email: email.clone(),
        protocol: Protocol::Tuta,
        helper_session: session,
        push: Some(false),
        ..sample_account()
    };
    let events: Rc<RefCell<Vec<WorkerEvent>>> = Rc::default();
    let sink = events.clone();
    let mut state = TutaState::new(&account);
    state.transport = transport;
    let (_, rx) = mpsc::unbounded_channel();
    let (_, push_rx) = mpsc::unbounded_channel();
    let w: LiveWorker = TutaWorker {
        account_id: 1,
        account,
        rx,
        emit: Box::new(move |e| sink.borrow_mut().push(e)),
        cache: Some(Cache::in_memory().unwrap()),
        state,
        offline: Default::default(),
        push_rx,
        push_tx: None,
        push_live: false,
        polled: std::time::Instant::now(),
        open: None,
    };
    let live = Live {
        w,
        events,
        sends,
        email,
        token,
    };
    Some(LiveStart {
        guard,
        live,
        requests,
    })
}

/// The conversation tokens of the token's mail in `path` whose subject
/// starts with `subject`.
async fn live_conversations(
    live: &mut Live,
    path: &str,
    subject: &str,
) -> Result<Vec<String>, String> {
    Ok(live
        .token_rows(path)
        .await?
        .iter()
        .filter(|m| m.subject.starts_with(subject))
        .map(|m| m.references.clone())
        .collect())
}

/// A mail to the account itself, then a reply to its Inbox copy filled in
/// from the cached row, as the composer fills one. The steps' outcomes.
async fn live_reply_steps(live: &mut Live, requests: &Mutex<Vec<Value>>) -> Step {
    live.w.sync_folders().await;
    live.no_errors("sync")?;
    let (inbox, sent) = (live.path(FolderKind::Inbox)?, live.path(FolderKind::Sent)?);
    ensure(live.token_rows(&inbox).await?.is_empty(), || {
        "the token is already in use".into()
    })?;
    let email = live.email.clone();
    let first = live.send_to(&email, "thread").await?;
    let sent_id = first
        .get("message_id")
        .and_then(Value::as_str)
        .ok_or("the first send has no Message-ID")?
        .to_string();
    let parent = live.arrived(&inbox).await?.remove(0);
    let sent_copy = live.arrived(&sent).await?.remove(0);
    // What a reply needs is the id the Inbox copy is listed with: Tuta's own
    // app answers a mail by its own conversation entry's id. Whether that is
    // the id the send reported is only noted (shapes, never the ids).
    eprintln!(
        "listed ids: inbox empty {}, sent copy empty {}, inbox equals the send's {}, sent copy equals it {}",
        parent.message_id.is_empty(),
        sent_copy.message_id.is_empty(),
        parent.message_id.eq_ignore_ascii_case(&sent_id),
        sent_copy.message_id.eq_ignore_ascii_case(&sent_id),
    );
    ensure(!parent.message_id.is_empty(), || {
        "the Inbox copy is listed without a Message-ID".into()
    })?;
    let exact = live
        .w
        .cache
        .as_ref()
        .ok_or("no cache")?
        .exact_msgids(&parent.message_id);
    let chain = format!("{} {}", parent.references, parent.message_id);
    let reply = OutgoingMessage {
        to: email.clone(),
        subject: format!("Re: {}", parent.subject),
        body: "A reply from Hylki's live Tuta test.".into(),
        in_reply_to: parent.message_id.clone(),
        references: chain,
        ..sample_outgoing()
    };
    requests.lock().unwrap().clear();
    live.sends.lock().unwrap().clear();
    live.w.on_send(Box::new(reply), 8).await;
    let went = live.take_events().iter().any(|e| {
        matches!(
            e,
            WorkerEvent::SendDone {
                ticket: 8,
                sent: true
            }
        )
    });
    ensure(went, || "the reply was not sent".into())?;
    let asked = requests.lock().unwrap().pop().ok_or("no reply request")?;
    ensure(asked["in_reply_to"] == exact.as_str(), || {
        "the reply does not name its parent as Tuta spells it".into()
    })?;
    ensure(asked["action"] == "reply", || {
        format!("action {}", asked["action"])
    })?;
    let answer = live.sends.lock().unwrap().pop().ok_or("no reply answer")?;
    let marked = answer["result"]["parent_marked"].clone();
    eprintln!("reply: parent_marked {marked}");
    ensure(marked == Value::Bool(true), || {
        "the parent was not marked".into()
    })?;
    // Both in one Tuta conversation: every copy of the reply is listed in a
    // conversation a copy of the parent is in.
    let mut replies = Vec::new();
    for _ in 0..10 {
        replies = live_conversations(live, &inbox, "Re: ").await?;
        replies.extend(live_conversations(live, &sent, "Re: ").await?);
        if replies.len() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    let mut parents = live_conversations(live, &inbox, "Hylki live").await?;
    parents.extend(live_conversations(live, &sent, "Hylki live").await?);
    eprintln!(
        "conversations: {} parent copies, {} reply copies, {} distinct",
        parents.len(),
        replies.len(),
        parents
            .iter()
            .chain(&replies)
            .collect::<std::collections::HashSet<_>>()
            .len()
    );
    ensure(replies.len() == 2, || {
        format!("{} reply copies listed", replies.len())
    })?;
    ensure(replies.iter().all(|r| parents.contains(r)), || {
        "the reply is in a conversation of its own".into()
    })
}

/// Replying to mail between Tuta accounts through the worker, live, with
/// the sign-in [`live_start`] reads: a mail to the account itself, then a
/// reply to its Inbox copy as the composer fills one from the cached row.
/// The reply must name its parent by the Message-ID Tuta keeps, spelt as
/// Tuta spells it; the helper must mark the parent replied; and both must
/// sit in one Tuta conversation. One sign-in and two sends; both mails are
/// erased by way of the Trash and the session ended.
#[test]
#[ignore]
fn live_tuta_reply_threads() {
    let Some(LiveStart {
        mut guard,
        mut live,
        requests,
    }) = live_start()
    else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (steps, purged) = runtime.block_on(async {
        let steps = live_reply_steps(&mut live, &requests).await;
        (steps, live_purge(&mut live).await)
    });
    let left = guard.sweep();
    eprintln!("reply threads: {steps:?}; delete permanently: {purged:?}");
    eprintln!("cleanup sweep left: {left:?}");
    assert!(steps.is_ok() && purged.is_ok(), "a live step failed");
    assert!(left.is_empty(), "the cleanup left something behind");
}

/// Against a real Tuta account, through the Tuta worker itself:
/// `TUTA_LIVE=<address>` with `TUTA_LIVE_PASSWORD` (and `TUTA_LIVE_TOTP`
/// for an account with two-step verification) signs in, lists the folders,
/// a page of the Inbox and one body. `TUTA_LIVE_WRITE=1` adds, each under a
/// random token in its subject or name: a confidential send to the account
/// itself; with `TUTA_LIVE_EXTERNAL_TO` (not a Tuta address), an ordinary
/// send there; a folder made, renamed and deleted; a label put on the sent
/// mail, taken off and deleted; a draft saved, updated and deleted; and a
/// `watch` that is ready and reports the move of the sent mail to the
/// Trash. Every message sent is then erased by way of the Trash, and a
/// sweep for the token removes whatever a failed step left, then ends the
/// session, also when the test panics. The helper must be on PATH
/// (`native/tuta/target/debug`).
#[test]
#[ignore]
fn live_tuta_account() {
    let Some(LiveStart {
        mut guard,
        mut live,
        ..
    }) = live_start()
    else {
        return;
    };
    let write = std::env::var("TUTA_LIVE_WRITE").as_deref() == Ok("1");
    let external = std::env::var("TUTA_LIVE_EXTERNAL_TO").ok();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let steps = runtime.block_on(async {
        let mut steps: Vec<(&str, Step)> = Vec::new();
        live.w.sync_folders().await;
        live.no_errors("sync").unwrap();
        let inbox = live.path(FolderKind::Inbox).unwrap();
        let rows = live.token_rows(&inbox).await.unwrap();
        assert!(rows.is_empty(), "the token is already in use");
        let listed =
            live.w
                .cache
                .as_ref()
                .unwrap()
                .load_messages(1, &inbox, live.w.state.folders[&inbox].0);
        eprintln!(
            "{} folders, {} Inbox messages listed",
            live.w.state.folders.len(),
            listed.len()
        );
        if let Some(first) = listed.first() {
            let raw = live.w.raw_of(&inbox, first.uid).await.unwrap();
            let (body, _, _) = crate::worker::render_raw(&raw);
            assert!(!body.is_empty());
        }
        if !write {
            return steps;
        }
        let (push_tx, mut push_rx) = mpsc::unbounded_channel();
        spawn_tuta_push(live.w.state.client(&live.w.account).unwrap(), None, push_tx);
        let ready = match next_signal(&mut push_rx).await {
            Some(PushSignal::Live) => Ok(()),
            other => Err(format!("{other:?} instead of ready")),
        };
        steps.push(("watch ready", ready));
        let sent = live_confidential_send(&mut live).await;
        steps.push((
            "confidential send",
            sent.as_ref().map(|_| ()).map_err(Clone::clone),
        ));
        if let Some(to) = external.as_deref() {
            steps.push(("external send", live_external_send(&mut live, to).await));
        }
        steps.push(("folders", live_folders(&mut live).await));
        if let Ok(uid) = sent {
            steps.push(("labels", live_labels(&mut live, uid).await));
        }
        steps.push(("drafts", live_drafts(&mut live).await));
        if let Ok(uid) = sent {
            let moved = live_watch_move(&mut live, &mut push_rx, uid).await;
            steps.push(("watch mail_changed", moved));
        }
        steps.push(("delete permanently", live_purge(&mut live).await));
        steps
    });
    let left = guard.sweep();
    for (step, outcome) in &steps {
        match outcome {
            Ok(()) => eprintln!("{step}: ok"),
            Err(why) => eprintln!("{step}: FAILED: {why}"),
        }
    }
    eprintln!("cleanup sweep left: {left:?}");
    assert!(steps.iter().all(|(_, o)| o.is_ok()), "a live step failed");
    assert!(left.is_empty(), "the cleanup left something behind");
}
