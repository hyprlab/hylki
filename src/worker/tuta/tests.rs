//! The Tuta backend against a fake helper: folder and message mapping,
//! the uid map, moves, read marks and the ways a send can end.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::folders::tuta_folders;
use super::index::{apply_first_page, carry_over, tuta_index_plan, TutaState};
use super::messages::tuta_message;
use super::mime::mime_message;
use super::ops::{tuta_move_uids, tuta_set_seen};
use super::send::{send_outcome, send_parameters, tuta_flush_outbox, tuta_send_raw, SendOutcome};
use super::wire::{sync_page, FolderRow};
use crate::cache::Cache;
use crate::config::{AccountConfig, Protocol};
use crate::models::{FolderKind, Message};
use crate::worker::helper::{HelperClient, HelperError, Object, Transport};
use crate::worker::ids::hash_uid;
use crate::worker::test_support::{queued, sample_account};
use crate::worker::WorkerEvent;

fn row(id: &str, role: &str, label: &str, parent: &str) -> FolderRow {
    FolderRow {
        id: id.into(),
        role: role.into(),
        label: label.into(),
        parent: parent.into(),
    }
}

fn account() -> AccountConfig {
    AccountConfig {
        email: "me@tuta.com".into(),
        protocol: Protocol::Tuta,
        helper_session: json!({"login": "me@tuta.com", "user_id": "u", "access_token": "t"})
            .to_string(),
        ..sample_account()
    }
}

/// What the fake helper was asked, in order.
type Asked = Arc<Mutex<Vec<Value>>>;

/// A transport answering each request with `reply(op, request)`: a result
/// object, or an error code.
fn fake(
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

fn ops(asked: &Asked) -> Vec<String> {
    asked
        .lock()
        .unwrap()
        .iter()
        .map(|r| r["op"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn header(id: &str, ms: i64, unread: bool) -> Value {
    json!({
        "id": id, "subject": "Hello", "date_ms": ms,
        "sender": {"name": "Ann", "address": "ann@example.com"},
        "recipient": {"name": "", "address": "me@tuta.com"},
        "is_unread": unread, "confidential": false,
        "conversation": "Conv1", "has_attachments": true, "reply_type": 1
    })
}

fn folder_rows() -> Value {
    json!([
        {"id": "f/inbox", "role": "inbox", "label": "Inbox", "parent": ""},
        {"id": "f/sent", "role": "sent", "label": "Sent", "parent": ""},
        {"id": "f/trash", "role": "trash", "label": "Trash", "parent": ""},
        {"id": "f/spam", "role": "junk", "label": "Spam", "parent": ""},
        {"id": "f/work", "role": "other", "label": "Work", "parent": ""}
    ])
}

/// A state whose folders came from [`folder_rows`], talking to `transport`.
fn state_with(transport: Transport) -> TutaState {
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

fn folder_id(state: &TutaState, path: &str) -> u32 {
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
    ];
    let list = tuta_folders(&rows, 3, &Default::default(), &[], &[]);
    let by_path = |p: &str| list.iter().find(|f| f.folder.path == p).unwrap();
    assert_eq!(by_path("Inbox").folder.kind, FolderKind::Inbox);
    assert_eq!(by_path("Inbox").folder.id, 1);
    assert_eq!(by_path("Sent").folder.kind, FolderKind::Sent);
    assert_eq!(by_path("Spam").folder.kind, FolderKind::Junk);
    // Only a role names a special folder; a name never does.
    assert_eq!(by_path("Templates").folder.kind, FolderKind::Custom);
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
    // No Message-ID before the body is read; the conversation threads it,
    // and never reaches a wire header.
    assert!(m.message_id.is_empty());
    assert_eq!(m.references, "tuta-conv:Conv1");
    assert!(crate::worker::ids::is_conversation_token(&m.references));
    assert!(tuta_message(&json!({"date_ms": 1}), 1, 1).is_none());
    assert!(tuta_message(&json!({"id": "a/b"}), 1, 1).is_none());
}

#[test]
fn a_sync_reply_is_checked_before_it_is_used() {
    let ok = json!({"folders": folder_rows(), "folder": "f/inbox", "messages": [],
        "all_ids": ["a/1", "a/2"]});
    let page = sync_page(ok.as_object().unwrap()).unwrap();
    assert_eq!(page.all_ids.unwrap(), ["a/1", "a/2"]);
    for bad in [
        json!({"folder": "f/inbox", "messages": []}),
        json!({"folders": [{"id": ""}], "folder": "f/inbox", "messages": []}),
        json!({"folders": [], "folder": "f/inbox", "messages": [], "all_ids": [1]}),
        json!({"folders": [], "folder": "f/inbox", "messages": [], "all_ids": "a/1"}),
        json!({"folders": [], "messages": []}),
    ] {
        let err = sync_page(bad.as_object().unwrap()).unwrap_err();
        assert_eq!(err.code, "invalid_response", "{bad}");
    }
}

#[test]
fn the_index_plan_finds_runs_of_missing_mail() {
    let ids: Vec<String> = (0..6).map(|i| format!("l/{i}")).collect();
    // 1 and 4 are cached; "gone" was cached long ago and is not listed.
    let mut cached = std::collections::HashMap::new();
    cached.insert(hash_uid("l/1"), 0);
    cached.insert(hash_uid("l/4"), 0);
    cached.insert(hash_uid("gone"), 0);
    let (gone, runs) = tuta_index_plan(&ids, &cached, 10_000);
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

#[tokio::test]
async fn a_move_drops_the_source_row_and_lists_the_destination_again() {
    // The server files the moved mail under a new id: only the listing
    // after the move can say which.
    let (transport, asked) = fake(|op, request| match op {
        "move" => Ok(json!({"outcomes": [{"id": "l/1", "code": null}]})),
        "sync" => {
            assert_eq!(request["folder"], "f/work");
            Ok(json!({"folders": folder_rows(), "folder": "f/work",
                "messages": [header("m/9", 1_000_000, false)], "all_ids": ["m/9"]}))
        }
        _ => Err("unsupported_operation"),
    });
    let mut state = state_with(transport);
    let account = account();
    let cache = Cache::in_memory().unwrap();
    let inbox = folder_id(&state, "Inbox");
    let (m, id) = tuta_message(&header("l/1", 1_000_000, true), 1, inbox).unwrap();
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
    assert!(!state.uids.contains_key(&m.uid));
    let work = folder_id(&state, "Work");
    let moved = cache.load_messages(1, "Work", work);
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].uid, hash_uid("m/9"));
    assert!(events
        .into_inner()
        .iter()
        .any(|e| matches!(e, WorkerEvent::Messages { folder_id, .. } if *folder_id == work)));
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
    let p = send_parameters(RAW, &envelope).unwrap();
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
}

fn client_for(transport: Transport) -> HelperClient {
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
    for (answer, expected) in [
        (Ok(json!({"message_id": "x", "sent_mail": "s/1"})), "sent"),
        (Err("send_uncertain"), "uncertain"),
        (Err("helper_failed"), "uncertain"),
        (Err("timeout"), "uncertain"),
        (Err("confidential_send_unsupported"), "refused"),
        (Err("invalid_recipient"), "refused"),
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
    // A Tuta-only send is refused with words of its own.
    match send_outcome(&HelperError::new("confidential_send_unsupported")) {
        SendOutcome::Refused(why) => assert!(why.contains("Tuta"), "{why}"),
        other => panic!("{other:?}"),
    }
    // The helper missing sent nothing: it can wait in the Outbox.
    assert!(matches!(
        send_outcome(&HelperError::new("helper_missing")),
        SendOutcome::Failed(_)
    ));
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
    let page = super::wire::sync(client, Some(folder), 10, 0)?;
    Ok(page
        .headers
        .iter()
        .filter(|h| h["subject"].as_str().is_some_and(|s| s.contains(token)))
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect())
}

/// Move the live test's sent copy from Sent to Trash through the backend's
/// move op, giving the server a few seconds to file it. How many it moved.
fn trash_sent_copy(
    client: &HelperClient,
    sent: &str,
    trash: &str,
    token: &str,
) -> Result<usize, HelperError> {
    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        let ids = token_mail(client, sent, token)?;
        if !ids.is_empty() {
            return match super::wire::move_ids(client, &ids, trash) {
                (moved, None) => Ok(moved.len()),
                (_, Some(error)) => Err(error),
            };
        }
    }
    Ok(0)
}

/// Against a real Tuta account: `TUTA_LIVE=<address>` with
/// `TUTA_LIVE_PASSWORD` (and `TUTA_LIVE_TOTP` for an account with two-step
/// verification) signs in, lists the folders, a page of the Inbox and one
/// body. With `TUTA_LIVE_WRITE=1` it also sends one ordinary message to
/// `TUTA_LIVE_EXTERNAL_TO`, which must not be a Tuta address, under a
/// subject carrying a random token, and then moves its copy from Sent to
/// Trash -- whether or not the send succeeded -- and checks Sent holds no
/// copy anymore. The helper must be on PATH (`native/tuta/target/debug`).
/// The session it makes is ended again before it returns.
#[test]
#[ignore]
fn live_tuta_account() {
    use crate::worker::helper::{login, process_transport};
    let Ok(email) = std::env::var("TUTA_LIVE") else {
        eprintln!("TUTA_LIVE not set");
        return;
    };
    let password = std::env::var("TUTA_LIVE_PASSWORD").expect("TUTA_LIVE_PASSWORD");
    let totp = std::env::var("TUTA_LIVE_TOTP").ok();
    let transport = process_transport(super::SPEC);
    let session = login(&transport, super::SPEC, &email, &password, totp.as_deref())
        .unwrap_or_else(|e| panic!("login: {}", e.code));
    let client = HelperClient::new(super::SPEC, transport, &email, &session).unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let page =
            super::wire::sync(&client, None, 5, 0).unwrap_or_else(|e| panic!("sync: {}", e.code));
        let folders = tuta_folders(&page.folders, 1, &Default::default(), &[], &[]);
        assert!(folders.iter().any(|f| f.folder.kind == FolderKind::Inbox));
        eprintln!(
            "{} folders, {} ids in the Inbox",
            folders.len(),
            page.all_ids.as_ref().map_or(0, Vec::len)
        );
        let rows: Vec<_> = page
            .headers
            .iter()
            .filter_map(|h| tuta_message(h, 1, 1))
            .collect();
        if let Some((_, id)) = rows.first() {
            let reply =
                super::wire::message(&client, id).unwrap_or_else(|e| panic!("message: {}", e.code));
            let raw = mime_message(&reply).unwrap();
            let (body, _, _) = crate::worker::render_raw(&raw);
            assert!(!body.is_empty());
        }
        if std::env::var("TUTA_LIVE_WRITE").as_deref() == Ok("1") {
            let to = std::env::var("TUTA_LIVE_EXTERNAL_TO").expect("TUTA_LIVE_EXTERNAL_TO");
            let role = |role: &str| {
                page.folders
                    .iter()
                    .find(|f| f.role == role)
                    .map(|f| f.id.clone())
                    .unwrap_or_else(|| panic!("no {role} folder"))
            };
            let (sent, trash) = (role("sent"), role("trash"));
            let token = crate::rng::from_alphabet(12, b"0123456789abcdef").unwrap();
            let raw = format!(
                "From: <{email}>\r\nTo: <{to}>\r\nSubject: Hylki live test {token}\r\n\
                 MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
                 Sent by Hylki's live Tuta test.\r\n"
            );
            let parameters = send_parameters(raw.as_bytes(), &[to]).unwrap();
            let sent_reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                client.request("send", parameters)
            }));
            // Runs whatever the send did -- succeeded, failed or panicked --
            // so no run leaves its copy behind in Sent.
            let trashed = trash_sent_copy(&client, &sent, &trash, &token);
            match sent_reply {
                Err(panic) => std::panic::resume_unwind(panic),
                Ok(reply) => {
                    reply.unwrap_or_else(|e| panic!("send: {}", e.code));
                }
            }
            let trashed = trashed.unwrap_or_else(|e| panic!("trash the sent copy: {}", e.code));
            assert!(trashed > 0, "the sent copy never reached Sent");
            let left = token_mail(&client, &sent, &token)
                .unwrap_or_else(|e| panic!("sync Sent: {}", e.code));
            assert!(left.is_empty(), "the sent copy is still in Sent");
            eprintln!("sent, and its copy moved from Sent to Trash");
        }
    }));
    let _ = client.request("revoke", Object::new());
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
