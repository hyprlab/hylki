//! The Proton backend against a fake helper: folder and message mapping,
//! the event log, moves, read marks, stars, the ways a send can end, and
//! the session the helper rotates on every call.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::folders::proton_folders;
use super::index::{
    apply_changes, apply_first_page, carry_over, proton_backfill_with, ProtonState,
};
use super::messages::proton_message;
use super::ops::{
    proton_fetch_raw, proton_move_uids, proton_poll, proton_set_seen, proton_set_starred,
};
use super::send::{proton_flush_outbox, proton_send_parameters, proton_send_raw};
use super::wire::{sync_page, Changes, FolderRow};
use super::{keep_rotated_session, SPEC};
use crate::cache::Cache;
use crate::config::{AccountConfig, LiveSession, Protocol};
use crate::models::{FolderKind, Message};
use crate::worker::helper::send::{send_outcome, send_parameters, SendOutcome};
use crate::worker::helper::{login, HelperClient, HelperError, HelperLogin, Object, Transport};
use crate::worker::ids::hash_uid;
use crate::worker::test_support::{queued, sample_account};
use crate::worker::WorkerEvent;

const EMAIL: &str = "me@proton.me";

fn session(token: &str) -> Value {
    json!({"login": EMAIL, "user_id": "u", "uid": "s", "access_token": token,
        "refresh_token": format!("r-{token}"), "password_mode": 1, "key_pass": "a2V5"})
}

fn account() -> AccountConfig {
    AccountConfig {
        email: EMAIL.into(),
        protocol: Protocol::Proton,
        helper_session: session("first").to_string(),
        ..sample_account()
    }
}

/// What the fake helper was asked, in order.
type Asked = Arc<Mutex<Vec<Value>>>;

fn ok(result: Value) -> Value {
    json!({ "result": result })
}

fn fail(code: &str) -> Value {
    json!({"error": {"code": code}})
}

/// A transport answering each request with `reply(op, request)`, a reply
/// envelope less its version.
fn fake(reply: impl Fn(&str, &Value) -> Value + Send + Sync + 'static) -> (Transport, Asked) {
    let asked: Asked = Arc::default();
    let log = asked.clone();
    let transport: Transport = Arc::new(move |request: &Value| {
        log.lock().unwrap().push(request.clone());
        let op = request["op"].as_str().unwrap_or_default();
        let mut reply = reply(op, request);
        reply["version"] = 1.into();
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

fn header(id: &str, ms: i64, unread: bool, labels: &[&str]) -> Value {
    json!({
        "id": id, "subject": "Hello", "date_ms": ms,
        "sender": {"name": "Ann", "address": "ann@example.com"},
        "recipient": {"name": "", "address": EMAIL},
        "is_unread": unread, "confidential": true,
        "message_id": format!("Mid-{id}@Example.com"),
        "label_ids": labels, "is_starred": labels.contains(&"10"),
        "is_draft": false, "has_attachments": true, "size": 10
    })
}

fn folder_row(id: &str, role: &str, label: &str, parent: &str, kind: &str, unread: u32) -> Value {
    json!({"id": id, "role": role, "label": label, "parent": parent, "kind": kind,
        "total": unread, "unread": unread})
}

fn folder_rows() -> Value {
    json!([
        folder_row("0", "inbox", "Inbox", "", "system", 3),
        folder_row("8", "drafts", "Drafts", "", "system", 0),
        folder_row("7", "sent", "Sent", "", "system", 0),
        folder_row("10", "starred", "Starred", "", "system", 0),
        folder_row("6", "archive", "Archive", "", "system", 0),
        folder_row("4", "junk", "Spam", "", "system", 0),
        folder_row("3", "trash", "Trash", "", "system", 0),
        folder_row("5", "other", "All Mail", "", "system", 0),
        folder_row("Work", "other", "Work", "", "folder", 0),
        folder_row("Imp", "other", "Important", "", "label", 0)
    ])
}

fn rows_of(value: Value) -> Vec<FolderRow> {
    sync_page(
        json!({"folders": value, "folder": "0", "messages": [], "event_id": "E0"})
            .as_object()
            .unwrap(),
    )
    .unwrap()
    .folders
}

/// A sync result listing `folder`.
fn listing(folder: &str, headers: Vec<Value>, all_ids: Option<Vec<&str>>, event: &str) -> Value {
    json!({"folders": folder_rows(), "folder": folder, "messages": headers,
        "exists": 0, "offset": 0, "all_ids": all_ids, "next_cursor": null, "event_id": event})
}

/// A state whose folders came from [`folder_rows`], talking to `transport`
/// on a session of its own.
fn state_with(transport: Transport) -> ProtonState {
    let account = account();
    let mut state = ProtonState::new(&account);
    state.transport = transport;
    state.live = Arc::new(LiveSession::default());
    let list = proton_folders(&rows_of(folder_rows()), 1, &Default::default(), &[]);
    state.adopt_folders(&list);
    state
}

fn folder_id(state: &ProtonState, path: &str) -> u32 {
    state.folders[path].0
}

/// Put a listed message into `path`'s cache and the uid map.
fn seed(state: &mut ProtonState, cache: &Cache, path: &str, h: Value) -> Message {
    let (m, id) = proton_message(&h, 1, folder_id(state, path)).unwrap();
    state.uids.insert(m.uid, id);
    cache.upsert_messages(1, path, std::slice::from_ref(&m));
    m
}

#[test]
fn folder_roles_become_kinds_and_labels_are_listed_too() {
    let mut rows = rows_of(folder_rows());
    rows.push(
        rows_of(json!([folder_row(
            "Q1", "other", "2026/Q1", "Work", "folder", 2
        )]))[0]
            .clone(),
    );
    rows.push(
        rows_of(json!([folder_row(
            "Lost", "other", "Lost", "nowhere", "folder", 0
        )]))[0]
            .clone(),
    );
    let list = proton_folders(&rows, 3, &Default::default(), &[]);
    let by_path = |p: &str| list.iter().find(|f| f.folder.path == p).unwrap();
    assert_eq!(by_path("Inbox").folder.kind, FolderKind::Inbox);
    assert_eq!(by_path("Inbox").folder.unread, 3, "the server's count");
    assert_eq!(by_path("Starred").folder.kind, FolderKind::Starred);
    assert_eq!(by_path("Spam").folder.kind, FolderKind::Junk);
    assert_eq!(by_path("All Mail").folder.kind, FolderKind::Custom);
    assert_eq!(by_path("Important").label_id, "Imp");
    let child = list.iter().find(|f| f.label_id == "Q1").unwrap();
    assert!(child.folder.path.starts_with("Work/"));
    assert_ne!(
        child.folder.path, "Work/2026/Q1",
        "a '/' in a name is no level"
    );
    assert!(list.iter().all(|f| f.label_id != "Lost"));
    // Hidden folders (#239) and manual roles (#82) apply.
    let roles = [("archive".to_string(), "Work".to_string())].into();
    let list = proton_folders(&rows, 3, &roles, &["Spam".to_string()]);
    assert!(list.iter().all(|f| f.folder.path != "Spam"));
    let work = list.iter().find(|f| f.folder.path == "Work").unwrap();
    assert_eq!(work.folder.kind, FolderKind::Archive);
}

#[test]
fn a_header_becomes_a_row_threaded_by_its_message_id() {
    let (m, id) = proton_message(
        &header("AbC==", 1_700_000_000_123, true, &["0", "10"]),
        4,
        2,
    )
    .unwrap();
    assert_eq!(id, "AbC==");
    assert_eq!(m.uid, hash_uid("AbC=="));
    assert_eq!((m.account_id, m.folder_id), (4, 2));
    assert_eq!(m.timestamp, 1_700_000_000);
    assert!(m.unread && m.starred && m.has_attachment);
    assert_eq!(m.message_id, "mid-abc==@example.com");
    // No synthetic token: the References come with the body.
    assert!(m.references.is_empty());
    assert!(proton_message(&json!({"date_ms": 1}), 1, 1).is_none());
    assert!(proton_message(&json!({"id": "x"}), 1, 1).is_none());
}

#[test]
fn a_sync_reply_is_checked_before_it_is_used() {
    let good = json!({"folders": folder_rows(), "folder": "0", "messages": [],
        "all_ids": ["a", "b"], "next_cursor": "b", "event_id": "E1",
        "changes": {"refresh": false, "more": true, "deleted": ["c"], "updated": []}});
    let page = sync_page(good.as_object().unwrap()).unwrap();
    assert_eq!(page.all_ids.unwrap(), ["a", "b"]);
    assert_eq!(page.next_cursor.as_deref(), Some("b"));
    assert_eq!(page.event_id, "E1");
    let changes = page.changes.unwrap();
    assert!(changes.more && !changes.refresh);
    assert_eq!(changes.deleted, ["c"]);
    for bad in [
        json!({"folder": "0", "messages": [], "event_id": "E"}),
        json!({"folders": [{"id": ""}], "folder": "0", "messages": [], "event_id": "E"}),
        json!({"folders": [], "folder": "0", "messages": []}),
        json!({"folders": [], "folder": "0", "messages": [], "event_id": "E", "all_ids": [1]}),
        json!({"folders": [], "folder": "0", "messages": [], "event_id": "E", "next_cursor": 3}),
        json!({"folders": [], "folder": "0", "messages": [], "event_id": "E",
            "changes": {"refresh": false, "deleted": [], "updated": []}}),
    ] {
        let err = sync_page(bad.as_object().unwrap()).unwrap_err();
        assert_eq!(err.code, "invalid_response", "{bad}");
    }
}

#[test]
fn a_first_page_prunes_by_the_snapshot_and_keeps_what_a_body_showed() {
    let (transport, _) = fake(|_, _| ok(json!({})));
    let mut state = state_with(transport);
    let inbox = folder_id(&state, "Inbox");
    let cache = Cache::in_memory().unwrap();
    let mut kept = seed(
        &mut state,
        &cache,
        "Inbox",
        header("m1", 1_000_000, true, &["0"]),
    );
    kept.references = "parent@x".into();
    kept.preview = "Hi there".into();
    kept.starred = true;
    cache.upsert_messages(1, "Inbox", std::slice::from_ref(&kept));
    seed(
        &mut state,
        &cache,
        "Inbox",
        header("old", 500_000, true, &["0"]),
    );
    let page = sync_page(
        listing(
            "0",
            vec![header("m1", 1_000_000, false, &["0"])],
            Some(vec!["m1", "m2"]),
            "E1",
        )
        .as_object()
        .unwrap(),
    )
    .unwrap();
    let (rows, complete) =
        apply_first_page(&page, 1, inbox, "Inbox", 2_000, Some(&cache), &mut state);
    assert!(!complete, "m2 is not in the page");
    assert_eq!(state.uids.get(&hash_uid("m2")).unwrap(), "m2");
    assert_eq!(rows.len(), 1, "the old row is gone");
    let row = &rows[0];
    assert!(!row.unread, "the server's read state wins");
    assert!(!row.starred, "and its star");
    assert_eq!(row.references, "parent@x");
    assert_eq!(row.message_id, "mid-m1@example.com");

    let (mut listed, _) = proton_message(&header("m1", 1, true, &["0"]), 1, 1).unwrap();
    listed.message_id.clear();
    carry_over(std::slice::from_mut(&mut listed), &[kept.clone()]);
    assert_eq!(listed.message_id, kept.message_id);
}

#[test]
fn changes_land_in_every_folder_whose_label_a_message_carries() {
    let (transport, _) = fake(|_, _| ok(json!({})));
    let mut state = state_with(transport);
    let cache = Cache::in_memory().unwrap();
    let moved = seed(
        &mut state,
        &cache,
        "Inbox",
        header("moved", 1_000, true, &["0", "5"]),
    );
    seed(
        &mut state,
        &cache,
        "All Mail",
        header("moved", 1_000, true, &["0", "5"]),
    );
    let gone = seed(
        &mut state,
        &cache,
        "Inbox",
        header("gone", 2_000, true, &["0"]),
    );
    seed(
        &mut state,
        &cache,
        "Archive",
        header("gone", 2_000, true, &["6"]),
    );
    let changes = Changes {
        refresh: false,
        more: false,
        deleted: vec!["gone".into()],
        updated: vec![
            // Archived elsewhere, read and starred there.
            header("moved", 1_000, false, &["6", "5", "10"]),
            header("new", 3_000, true, &["0", "5"]),
        ],
    };
    let touched = apply_changes(&changes, 1, Some(&cache), &mut state);
    let paths: Vec<&str> = touched.iter().map(|(_, p)| p.as_str()).collect();
    for path in ["Inbox", "Archive", "All Mail", "Starred"] {
        assert!(paths.contains(&path), "{path} in {paths:?}");
    }
    let uids = |path: &str| -> Vec<u32> {
        cache
            .load_messages(1, path, folder_id(&state, path))
            .iter()
            .map(|m| m.uid)
            .collect()
    };
    assert_eq!(uids("Inbox"), [hash_uid("new")]);
    assert_eq!(uids("Archive"), [moved.uid]);
    assert_eq!(uids("Starred"), [moved.uid]);
    assert!(uids("All Mail").contains(&moved.uid) && uids("All Mail").contains(&hash_uid("new")));
    let archived = &cache.load_messages(1, "Archive", folder_id(&state, "Archive"))[0];
    assert!(!archived.unread && archived.starred);
    assert!(!state.uids.contains_key(&gone.uid));
    assert_eq!(state.uids[&hash_uid("new")], "new");
}

#[tokio::test]
async fn a_poll_follows_the_event_log_from_where_it_left_off() {
    let (transport, asked) = fake(|op, request| {
        assert_eq!(op, "sync");
        match request["since"].as_str() {
            None => ok(listing(
                "0",
                vec![header("a", 1_000, true, &["0"])],
                Some(vec!["a"]),
                "E1",
            )),
            Some("E1") => {
                let mut page = listing("0", vec![], None, "E2");
                page["changes"] = json!({"refresh": false, "more": true, "deleted": ["a"],
                    "updated": []});
                ok(page)
            }
            Some("E2") => {
                let mut page = listing("0", vec![], None, "E3");
                page["changes"] = json!({"refresh": true, "more": false, "deleted": [],
                    "updated": []});
                ok(page)
            }
            Some(other) => panic!("asked since {other}"),
        }
    });
    let mut state = state_with(transport);
    state.backfill.clear();
    state
        .listed
        .insert("Inbox".into(), std::time::Instant::now());
    let cache = Cache::in_memory().unwrap();
    let account = account();
    proton_poll(&account, 1, Some(&cache), &mut state, &|_| {})
        .await
        .unwrap();
    assert_eq!(state.event_id.as_deref(), Some("E1"));
    assert_eq!(
        cache
            .load_messages(1, "Inbox", folder_id(&state, "Inbox"))
            .len(),
        1
    );
    // The next poll asks from E1; more waits, so it asks again from E2,
    // where the server lost track: every folder is listed in full again.
    let events = RefCell::new(Vec::new());
    proton_poll(&account, 1, Some(&cache), &mut state, &|e| {
        events.borrow_mut().push(e)
    })
    .await
    .unwrap();
    assert_eq!(ops(&asked), ["sync", "sync", "sync"]);
    assert_eq!(state.event_id.as_deref(), Some("E3"));
    assert!(cache
        .load_messages(1, "Inbox", folder_id(&state, "Inbox"))
        .is_empty());
    assert!(state.listed.is_empty());
    assert!(!state.backfill.is_empty() && state.backfill.iter().all(|j| j.full));
    assert!(events
        .into_inner()
        .iter()
        .any(|e| matches!(e, WorkerEvent::Messages { messages, .. } if messages.is_empty())));
}

#[tokio::test]
async fn the_index_plans_from_the_snapshot_or_walks_a_large_folder() {
    let (transport, asked) = fake(|_, request| {
        match (
            request["folder"].as_str(),
            request["offset"].as_u64(),
            request["cursor"].as_str(),
        ) {
            (Some("0"), None, None) => ok(listing(
                "0",
                vec![header("n1", 3_000, true, &["0"])],
                Some(vec!["n1", "c2", "n3"]),
                "E1",
            )),
            (Some("0"), Some(2), None) => ok(listing(
                "0",
                vec![header("n3", 1_000, true, &["0"])],
                None,
                "E1",
            )),
            (Some("6"), None, None) => {
                let mut page = listing("6", vec![header("x1", 3_000, true, &["6"])], None, "E1");
                page["next_cursor"] = "x1".into();
                ok(page)
            }
            (Some("6"), None, Some("x1")) => ok(listing(
                "6",
                vec![header("x2", 2_000, true, &["6"])],
                None,
                "E1",
            )),
            other => panic!("asked {other:?}"),
        }
    });
    let mut state = state_with(transport);
    state
        .backfill
        .retain(|j| j.path == "Inbox" || j.path == "Archive");
    let cache = Cache::in_memory().unwrap();
    seed(
        &mut state,
        &cache,
        "Inbox",
        header("c2", 2_000, true, &["0"]),
    );
    let account = account();
    for _ in 0..4 {
        assert!(proton_backfill_with(&account, 1, Some(&cache), &mut state, &|_| {}).await);
    }
    assert!(state.backfill.is_empty(), "both folders indexed");
    assert_eq!(
        cache
            .load_messages(1, "Inbox", folder_id(&state, "Inbox"))
            .len(),
        3
    );
    assert_eq!(
        cache
            .load_messages(1, "Archive", folder_id(&state, "Archive"))
            .len(),
        2
    );
    assert_eq!(ops(&asked).len(), 4);
    // A listing without `since` before any event is known starts the log.
    assert_eq!(state.event_id.as_deref(), Some("E1"));
}

#[tokio::test]
async fn a_move_drops_the_applied_rows_and_lists_the_destination_since_the_last_event() {
    let (transport, asked) = fake(|op, request| match op {
        "move" => {
            assert_eq!(request["destination"], "6");
            ok(
                json!({"outcomes": [{"id": "m1", "code": null}, {"id": "m2", "code": "request_failed"}]}),
            )
        }
        "sync" => {
            assert_eq!(request["folder"], "6");
            assert_eq!(request["since"], "E1");
            let mut page = listing("6", vec![header("m1", 1_000, false, &["6"])], None, "E2");
            page["changes"] =
                json!({"refresh": false, "more": false, "deleted": [], "updated": []});
            ok(page)
        }
        _ => fail("invalid_request"),
    });
    let mut state = state_with(transport);
    state.event_id = Some("E1".into());
    let cache = Cache::in_memory().unwrap();
    let one = seed(
        &mut state,
        &cache,
        "Inbox",
        header("m1", 1_000, true, &["0"]),
    );
    let two = seed(
        &mut state,
        &cache,
        "Inbox",
        header("m2", 2_000, true, &["0"]),
    );
    let events = RefCell::new(Vec::new());
    let err = proton_move_uids(
        &account(),
        1,
        &mut state,
        "Inbox",
        &[one.uid, two.uid],
        "Archive",
        Some(&cache),
        &|e| events.borrow_mut().push(e),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "request_failed");
    assert_eq!(ops(&asked), ["move", "sync"]);
    let left: Vec<u32> = cache
        .load_messages(1, "Inbox", folder_id(&state, "Inbox"))
        .iter()
        .map(|m| m.uid)
        .collect();
    assert_eq!(left, [two.uid]);
    let archive = folder_id(&state, "Archive");
    let filed = cache.load_messages(1, "Archive", archive);
    assert_eq!(filed.len(), 1);
    assert_eq!(filed[0].uid, one.uid, "Proton keeps the id across a move");
    assert_eq!(state.event_id.as_deref(), Some("E2"));
    assert!(events
        .into_inner()
        .iter()
        .any(|e| matches!(e, WorkerEvent::Messages { folder_id, .. } if *folder_id == archive)));
}

#[tokio::test]
async fn a_body_is_the_helpers_mime_and_fills_in_the_threading() {
    use base64::Engine as _;
    let mime = "Message-ID: <Mid-m1@Example.com>\r\nReferences: <Parent@x>\r\n\
        From: Ann <ann@example.com>\r\nSubject: Hello\r\nContent-Type: text/plain\r\n\r\nHi there\r\n";
    let encoded = base64::engine::general_purpose::STANDARD.encode(mime);
    let (transport, asked) = fake(move |op, request| match (op, request["id"].as_str()) {
        ("message", Some("m1")) => ok(json!({"header": header("m1", 1_000, true, &["0"]),
            "mime": encoded, "attachments": []})),
        ("message", _) => ok(json!({"header": {}, "mime": "not base64!"})),
        _ => fail("invalid_request"),
    });
    let mut state = state_with(transport);
    let cache = Cache::in_memory().unwrap();
    let one = seed(
        &mut state,
        &cache,
        "Inbox",
        header("m1", 1_000, true, &["0"]),
    );
    let two = seed(
        &mut state,
        &cache,
        "Inbox",
        header("m2", 1_000, true, &["0"]),
    );
    let account = account();
    let raw = proton_fetch_raw(
        &account,
        1,
        &mut state,
        "Inbox",
        one.uid,
        Some(&cache),
        &|_| {},
    )
    .await
    .unwrap();
    assert_eq!(raw, mime.as_bytes());
    let row = cache
        .load_messages(1, "Inbox", folder_id(&state, "Inbox"))
        .into_iter()
        .find(|m| m.uid == one.uid)
        .unwrap();
    assert_eq!(row.references, "parent@x");
    assert_eq!(row.message_id, "mid-m1@example.com");
    let err = proton_fetch_raw(
        &account,
        1,
        &mut state,
        "Inbox",
        two.uid,
        Some(&cache),
        &|_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "invalid_response");
    assert_eq!(asked.lock().unwrap()[0]["id"], "m1");
}

#[tokio::test]
async fn read_marks_and_stars_follow_the_helpers_outcomes() {
    let (transport, asked) = fake(|op, request| match op {
        "mark_read" => {
            assert_eq!(request["is_read"], true);
            ok(json!({"outcomes": [
                {"id": "m1", "code": null},
                {"id": "m2", "code": "request_failed"},
                {"id": "m3", "code": null}
            ]}))
        }
        "star" if request["is_starred"] == true => {
            ok(json!({"outcomes": [{"id": "m1", "code": null}]}))
        }
        "star" => ok(json!({"outcomes": [{"id": "m1", "code": "request_failed"}]})),
        _ => fail("invalid_request"),
    });
    let mut state = state_with(transport);
    let cache = Cache::in_memory().unwrap();
    let inbox = folder_id(&state, "Inbox");
    let uids: Vec<u32> = ["m1", "m2", "m3"]
        .iter()
        .map(|id| seed(&mut state, &cache, "Inbox", header(id, 1_000, true, &["0"])).uid)
        .collect();
    let account = account();
    let applied = proton_set_seen(
        &account,
        1,
        &mut state,
        "Inbox",
        &uids,
        true,
        Some(&cache),
        &|_| {},
    )
    .await;
    assert_eq!(
        applied,
        [uids[0], uids[2]],
        "a refused id does not stop the rest"
    );
    let unread = |uid: u32| {
        cache
            .load_messages(1, "Inbox", inbox)
            .iter()
            .find(|m| m.uid == uid)
            .unwrap()
            .unread
    };
    assert!(!unread(uids[0]) && unread(uids[1]) && !unread(uids[2]));
    let starred = |uid: u32| {
        cache
            .load_messages(1, "Inbox", inbox)
            .iter()
            .find(|m| m.uid == uid)
            .unwrap()
            .starred
    };
    proton_set_starred(
        &account,
        1,
        &mut state,
        "Inbox",
        uids[0],
        true,
        Some(&cache),
        &|_| {},
    )
    .await
    .unwrap();
    assert!(starred(uids[0]));
    // Refused: the star the list drew goes back.
    let err = proton_set_starred(
        &account,
        1,
        &mut state,
        "Inbox",
        uids[0],
        false,
        Some(&cache),
        &|_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "request_failed");
    assert!(starred(uids[0]));
    assert_eq!(ops(&asked), ["mark_read", "star", "star"]);
}

#[tokio::test]
async fn every_rotated_session_is_kept_failures_included() {
    let (transport, asked) = fake(|_, request| {
        match request["session"]["access_token"].as_str() {
            Some("first") => {
                let mut reply = ok(listing("0", vec![], Some(vec![]), "E1"));
                reply["result"]["session"] = session("second");
                reply
            }
            // The refresh went through, then the request failed: the new
            // session still comes back, in the error.
            Some("second") => {
                json!({"error": {"code": "transport_failed", "session": session("third")}})
            }
            Some("third") => fail("transport_failed"),
            other => panic!("presented {other:?}"),
        }
    });
    let mut state = state_with(transport);
    let account = account();
    let mut kept = account.helper_session.clone();
    let events = RefCell::new(Vec::new());
    let emit = |e| events.borrow_mut().push(e);
    proton_poll(&account, 1, None, &mut state, &emit)
        .await
        .unwrap();
    keep_rotated_session(&state, EMAIL, &mut kept, &emit);
    assert_eq!(kept, session("second").to_string());
    let err = proton_poll(&account, 1, None, &mut state, &emit)
        .await
        .unwrap_err();
    assert_eq!(err.code, "transport_failed");
    keep_rotated_session(&state, EMAIL, &mut kept, &emit);
    assert_eq!(state.live.latest(), session("third").to_string());
    proton_poll(&account, 1, None, &mut state, &emit)
        .await
        .unwrap_err();
    // Nothing new to keep.
    keep_rotated_session(&state, EMAIL, &mut kept, &emit);
    let kept_events = events
        .into_inner()
        .into_iter()
        .filter(|e| matches!(e, WorkerEvent::HelperSession { email } if email == EMAIL))
        .count();
    assert_eq!(kept_events, 2);
    let presented: Vec<Value> = asked
        .lock()
        .unwrap()
        .iter()
        .map(|r| r["session"]["access_token"].clone())
        .collect();
    assert_eq!(presented, ["first", "second", "third"]);
    // The event says which account, never the session itself.
    let shown = format!(
        "{:?}",
        WorkerEvent::HelperSession {
            email: EMAIL.into()
        }
    );
    assert!(!shown.contains("third"), "{shown}");
}

#[test]
fn proton_sign_in_refusals_reach_the_caller_by_code() {
    let sign_in = HelperLogin {
        email: EMAIL.into(),
        password: "pw".into(),
        ..HelperLogin::default()
    };
    let (transport, _) = fake(|_, _| {
        json!({"error": {"code": "human_verification_required",
            "hv": {"token": "challenge", "methods": ["captcha"]}}})
    });
    let err = login(&transport, SPEC, &sign_in).unwrap_err();
    assert!(err.needs_human_verification());
    assert_eq!(err.hv_challenge().unwrap().methods, ["captcha"]);
    for (code, check) in [
        (
            "mailbox_password_required",
            HelperError::needs_mailbox_password as fn(&HelperError) -> bool,
        ),
        ("second_factor_required", HelperError::needs_second_factor),
    ] {
        let (transport, _) = fake(move |_, _| fail(code));
        let err = login(&transport, SPEC, &sign_in).unwrap_err();
        assert!(check(&err), "{code}");
    }
    for code in [
        "mailbox_password_failed",
        "human_verification_failed",
        "invalid_destination",
        "message_too_large",
    ] {
        let (transport, _) = fake(move |_, _| fail(code));
        assert_eq!(login(&transport, SPEC, &sign_in).unwrap_err().code, code);
        assert!(!HelperError::new(code).message().is_empty());
    }
    // The solved challenge goes back with the retry.
    let (transport, asked) = fake(|_, _| ok(json!({"session": session("s")})));
    let retry = HelperLogin {
        totp: Some("123456".into()),
        hv_token: Some("solved".into()),
        hv_type: Some("captcha".into()),
        ..sign_in
    };
    login(&transport, SPEC, &retry).unwrap();
    let request = asked.lock().unwrap()[0].clone();
    assert_eq!(request["hv_token"], "solved");
    assert_eq!(request["hv_type"], "captcha");
    assert_eq!(request["totp"], "123456");
    assert!(request.get("mailbox_password").is_none());
}

/// A minimal built message, as `build_email` writes one.
const RAW: &[u8] = b"From: Me <me@proton.me>\r\nTo: Ann <ann@example.com>\r\n\
Message-ID: <Abc.1@proton.me>\r\nSubject: Plans\r\nMIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\nSee you\r\n";

#[test]
fn a_send_carries_the_message_id_it_was_built_with() {
    let p = proton_send_parameters(RAW, &["ann@example.com".to_string()], "").unwrap();
    assert_eq!(p["message_id"], "Abc.1@proton.me");
    assert_eq!(p["sender"], "me@proton.me");
    assert_eq!(p["delivery"], "ordinary");
    assert_eq!(p["ordinary_confirmed"], true);
}

/// A reply as `build_email` writes one, to `rcpts`.
fn reply_raw(rcpts: &[&str]) -> Vec<u8> {
    format!(
        "From: Me <me@proton.me>\r\nTo: {}\r\nMessage-ID: <Abc.2@proton.me>\r\n\
         In-Reply-To: <Parent.1@Example.com>\r\n\
         References: <Root.1@Example.com> <bad id> <Parent.1@Example.com>\r\n\
         Subject: Re: Plans\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nYes\r\n",
        rcpts.join(", ")
    )
    .into_bytes()
}

#[tokio::test]
async fn a_reply_or_forward_tells_the_helper_what_it_answers() {
    // What the helper was sent, per case.
    let sent = |raw: Vec<u8>, rcpts: &[&str], forwards: &str| {
        let (transport, asked) = fake(|_, _| ok(json!({"message_id": "x", "sent_mail": "s"})));
        let envelope: Vec<String> = rcpts.iter().map(|r| r.to_string()).collect();
        let forwards = forwards.to_string();
        async move {
            let outcome = proton_send_raw(client_for(transport), raw, envelope, forwards).await;
            assert_eq!(outcome, SendOutcome::Sent);
            let request = asked.lock().unwrap()[0].clone();
            request
        }
    };
    let keys = |request: &Value| -> Vec<Value> {
        ["in_reply_to", "references", "action"]
            .iter()
            .map(|k| request.get(*k).cloned().unwrap_or(Value::Null))
            .collect()
    };

    let reply = sent(reply_raw(&["ann@example.com"]), &["ann@example.com"], "").await;
    // As they arrived, less the brackets, and less an id the helper would
    // refuse the send over.
    assert_eq!(
        keys(&reply),
        [
            json!("Parent.1@Example.com"),
            json!("Root.1@Example.com Parent.1@Example.com"),
            json!("reply")
        ]
    );
    assert_eq!(reply["message_id"], "Abc.2@proton.me");

    let rcpts = ["ann@example.com", "bob@example.com", "ANN@example.com"];
    let all = sent(reply_raw(&rcpts[..2]), &rcpts, "").await;
    assert_eq!(all["action"], "reply_all");

    let forward = sent(RAW.to_vec(), &["ann@example.com"], "Fwd.1@Example.com").await;
    assert_eq!(
        keys(&forward),
        [json!("Fwd.1@Example.com"), Value::Null, json!("forward")]
    );

    let new = sent(RAW.to_vec(), &["ann@example.com"], "").await;
    assert_eq!(keys(&new), [Value::Null, Value::Null, Value::Null]);

    // The threading is shared, but Tuta's helper refuses fields it does not
    // know: its request never carries the Message-ID.
    let tuta = send_parameters(
        &reply_raw(&["ann@example.com"]),
        &["ann@example.com".into()],
        "",
    )
    .unwrap();
    assert_eq!(tuta["in_reply_to"], "Parent.1@Example.com");
    assert!(!tuta.contains_key("message_id"));
}

#[tokio::test]
async fn a_queued_forward_keeps_what_it_forwards() {
    let (transport, asked) = fake(|_, _| ok(json!({"message_id": "x", "sent_mail": "s"})));
    let mut state = state_with(transport);
    let cache = Cache::in_memory().unwrap();
    let id = cache
        .queue_outbox(
            1,
            EMAIL,
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
    proton_flush_outbox(Some(&cache), 1, &account(), None, &mut state, &|_| {}).await;
    let request = asked.lock().unwrap()[0].clone();
    assert_eq!(request["in_reply_to"], "fwd.1@example.com");
    assert_eq!(request["action"], "forward");
    assert!(cache.outbox_items(1).is_empty());
}

fn client_for(transport: Transport) -> HelperClient {
    let account = account();
    HelperClient::shared(
        SPEC,
        transport,
        &account.email,
        Arc::new(LiveSession::default()),
        &account.helper_session,
    )
    .unwrap()
}

#[tokio::test]
async fn each_way_a_send_ends_is_told_apart() {
    for (answer, expected) in [
        (ok(json!({"message_id": "x", "sent_mail": "s"})), "sent"),
        (fail("send_uncertain"), "uncertain"),
        (fail("timeout"), "uncertain"),
        (fail("message_too_large"), "refused"),
        (fail("invalid_recipient"), "refused"),
        (fail("send_failed"), "failed"),
        (fail("session_revoked"), "failed"),
    ] {
        let reply = answer.clone();
        let (transport, asked) = fake(move |_, _| reply.clone());
        let outcome = proton_send_raw(
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
        assert_eq!(got, expected, "{answer}");
        assert_eq!(ops(&asked), ["send"]);
    }
    assert!(matches!(
        send_outcome(&HelperError::new("helper_missing")),
        SendOutcome::Failed(_)
    ));
}

#[tokio::test]
async fn an_uncertain_outbox_message_is_never_sent_again() {
    let (transport, asked) = fake(|_, _| fail("send_uncertain"));
    let mut state = state_with(transport);
    let account = account();
    let cache = Cache::in_memory().unwrap();
    let item = queued(EMAIL, &["ann@example.com"]);
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
    proton_flush_outbox(Some(&cache), 1, &account, None, &mut state, &|_| {}).await;
    assert_eq!(ops(&asked), ["send"]);
    assert!(cache.outbox_items(1)[0].is_send_uncertain());
    proton_flush_outbox(Some(&cache), 1, &account, Some(id), &mut state, &|_| {}).await;
    assert_eq!(ops(&asked), ["send"], "it was not sent again");
}

/// The ids of the mail among the newest in `folder` whose subject holds
/// `token`.
fn token_mail(
    client: &HelperClient,
    folder: &str,
    token: &str,
) -> Result<Vec<String>, HelperError> {
    let page = super::wire::sync(client, Some(folder), 10, super::wire::Start::Newest, None)?;
    Ok(page
        .headers
        .iter()
        .filter(|h| h["subject"].as_str().is_some_and(|s| s.contains(token)))
        .filter_map(|h| h["id"].as_str().map(str::to_string))
        .collect())
}

/// Move the live test's sent copy from Sent to Trash through the backend's
/// move op, giving the server a few seconds to file it. How many it moved.
fn trash_sent_copy(client: &HelperClient, token: &str) -> Result<usize, HelperError> {
    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        let ids = token_mail(client, "7", token)?;
        if !ids.is_empty() {
            return match super::wire::move_ids(client, &ids, "3") {
                (moved, None) => Ok(moved.len()),
                (_, Some(error)) => Err(error),
            };
        }
    }
    Ok(0)
}

/// Against a real Proton account: `PROTON_LIVE=<address>` with
/// `PROTON_LIVE_PASSWORD`, `PROTON_LIVE_TOTP` when the account has two-step
/// verification, `PROTON_LIVE_MAILBOX_PASSWORD` for a two-password account,
/// and `PROTON_LIVE_HV_TOKEN` with `PROTON_LIVE_HV_TYPE` for a challenge
/// already solved, signs in, lists the folders, a page of the Inbox and one
/// body. With `PROTON_LIVE_WRITE=1` it also sends one message to
/// `PROTON_LIVE_EXTERNAL_TO` under a subject carrying a random token, then
/// moves its copy from Sent to Trash -- whether or not the send succeeded
/// -- and checks Sent holds no copy anymore. The helper must be on PATH
/// (`go build -o <dir>/hylki-proton` in `native/proton`). The session it
/// makes is ended again before it returns, and is never printed.
#[test]
#[ignore]
fn live_proton_account() {
    use crate::worker::helper::process_transport;
    let Ok(email) = std::env::var("PROTON_LIVE") else {
        eprintln!("PROTON_LIVE not set");
        return;
    };
    let env = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
    let sign_in = HelperLogin {
        email: email.clone(),
        password: env("PROTON_LIVE_PASSWORD").expect("PROTON_LIVE_PASSWORD"),
        totp: env("PROTON_LIVE_TOTP"),
        mailbox_password: env("PROTON_LIVE_MAILBOX_PASSWORD"),
        hv_token: env("PROTON_LIVE_HV_TOKEN"),
        hv_type: env("PROTON_LIVE_HV_TYPE"),
    };
    let transport = process_transport(SPEC);
    let session = login(&transport, SPEC, &sign_in).unwrap_or_else(|e| {
        let methods = e.hv_challenge().map(|hv| hv.methods).unwrap_or_default();
        panic!("login: {} {methods:?}", e.code)
    });
    // A session of the test's own, rotated as the calls go.
    let client = HelperClient::shared(
        SPEC,
        transport,
        &email,
        Arc::new(LiveSession::default()),
        &session,
    )
    .unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let page = super::wire::sync(&client, None, 5, super::wire::Start::Newest, None)
            .unwrap_or_else(|e| panic!("sync: {}", e.code));
        let folders = proton_folders(&page.folders, 1, &Default::default(), &[]);
        assert!(folders.iter().any(|f| f.folder.kind == FolderKind::Inbox));
        assert!(!page.event_id.is_empty());
        eprintln!(
            "{} folders, {} ids in the Inbox",
            folders.len(),
            page.all_ids.as_ref().map_or(0, Vec::len)
        );
        let rows: Vec<_> = page
            .headers
            .iter()
            .filter_map(|h| proton_message(h, 1, 1))
            .collect();
        if let Some((_, id)) = rows.first() {
            let (raw, _) =
                super::wire::message(&client, id).unwrap_or_else(|e| panic!("message: {}", e.code));
            let (body, _, _) = crate::worker::render_raw(&raw);
            assert!(!body.is_empty());
        }
        let since = super::wire::sync(
            &client,
            None,
            1,
            super::wire::Start::Newest,
            Some(&page.event_id),
        )
        .unwrap_or_else(|e| panic!("sync since: {}", e.code));
        assert!(since.changes.is_some());
        if std::env::var("PROTON_LIVE_WRITE").as_deref() == Ok("1") {
            let to = env("PROTON_LIVE_EXTERNAL_TO").expect("PROTON_LIVE_EXTERNAL_TO");
            let token = crate::rng::from_alphabet(12, b"0123456789abcdef").unwrap();
            let raw = format!(
                "From: <{email}>\r\nTo: <{to}>\r\nSubject: Hylki live test {token}\r\n\
                 MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
                 Sent by Hylki's live Proton test.\r\n"
            );
            let parameters = proton_send_parameters(raw.as_bytes(), &[to], "").unwrap();
            let sent_reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                client.request("send", parameters)
            }));
            // Runs whatever the send did -- succeeded, failed or panicked --
            // so no run leaves its copy behind in Sent.
            let trashed = trash_sent_copy(&client, &token);
            match sent_reply {
                Err(panic) => std::panic::resume_unwind(panic),
                Ok(reply) => {
                    reply.unwrap_or_else(|e| panic!("send: {}", e.code));
                }
            }
            let trashed = trashed.unwrap_or_else(|e| panic!("trash the sent copy: {}", e.code));
            assert!(trashed > 0, "the sent copy never reached Sent");
            let left = token_mail(&client, "7", &token)
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
