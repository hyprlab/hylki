//! Tests of the Graph path: summaries, the index plan, and the background
//! index and folder loads against a local HTTP server standing in for Graph.

use super::*;
use crate::cache::Cache;
use crate::models::Message;
use crate::worker::graph::index::{
    graph_backfill_with, graph_index_plan, graph_sync_folder, GraphRun, GRAPH_BACKFILL_CHUNK,
};
use crate::worker::graph::messages::{
    graph_due, graph_find_by_msgid, graph_id_page, graph_ids_url, graph_message, GRAPH_FIRST_PAGE,
};
use crate::worker::ids::hash_uid;
use crate::worker::index_queue::IndexQueue;
use crate::worker::reconcile::window_gone;
use crate::worker::WorkerEvent;

/// A Graph message summary as `/messages` lists it.
fn graph_msg(id: &str, at: &str) -> serde_json::Value {
    serde_json::json!({
        "@odata.etag": "W/\"CQAAABYAAAD\"",
        "id": id,
        "receivedDateTime": at,
        "subject": format!("Subject {id}"),
        "bodyPreview": "Hello there",
        "isRead": true,
        "hasAttachments": false,
        "internetMessageId": format!("<{id}@example.com>"),
        "conversationId": format!("AAQk-{id}"),
        "importance": "normal",
        "categories": [],
        "flag": { "flagStatus": "notFlagged" },
        "from": { "emailAddress": { "name": "Ada", "address": "ada@example.com" } },
        "toRecipients": [{ "emailAddress": { "name": "Me", "address": "me@example.com" } }],
        "ccRecipients": [],
        "replyTo": [],
    })
}

/// The same message as an id-only listing names it.
fn graph_id(id: &str, at: &str) -> serde_json::Value {
    serde_json::json!({ "@odata.etag": "W/\"CQAAABYAAAD\"", "id": id, "receivedDateTime": at })
}

fn day(d: u32) -> String {
    format!("2026-09-{d:02}T09:00:00Z")
}

/// A cached row for `id`, received at `ts`.
fn row(id: &str, ts: i64, folder_id: u32) -> Message {
    let v = graph_msg(id, "2026-09-01T09:00:00Z");
    let (mut m, _) = graph_message(&v, 1, folder_id).expect("summary");
    m.timestamp = ts;
    m
}

fn ts(at: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(at)
        .unwrap()
        .timestamp()
}

/// One answer of the loopback server: the first unused one whose `key`
/// is part of the request's path and query.
struct Answer {
    key: String,
    status: u16,
    headers: &'static str,
    body: String,
}

fn answer(key: &str, status: u16, body: serde_json::Value) -> Answer {
    Answer {
        key: key.to_string(),
        status,
        headers: "",
        body: body.to_string(),
    }
}

/// An HTTP server on loopback for `answers.len()` requests, as in the
/// JMAP tests; hands back the request targets it was sent. Each body is
/// formatted with `{origin}` replaced, for the next-page links.
fn serve(answers: Vec<Answer>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let origin = format!("http://{}", listener.local_addr().expect("addr"));
    let o = origin.clone();
    let handle = std::thread::spawn(move || {
        let mut answers: Vec<Option<Answer>> = answers.into_iter().map(Some).collect();
        let mut seen = Vec::new();
        for _ in 0..answers.len() {
            let Ok((stream, _)) = listener.accept() else {
                break;
            };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            reader.read_line(&mut line).expect("request line");
            let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
            }
            let hit = answers
                .iter_mut()
                .find(|a| a.as_ref().is_some_and(|a| target.contains(&a.key)))
                .and_then(Option::take);
            let (status, headers, body) = match hit {
                Some(a) => (a.status, a.headers, a.body.replace("{origin}", &o)),
                None => (404, "", String::new()),
            };
            let mut out = stream;
            let _ = write!(
                out,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            seen.push(target);
        }
        seen
    });
    (origin, handle)
}

fn state_for(origin: &str, path: &str, folder_id: u32, gid: &str) -> GraphState {
    let mut state = GraphState::new(&crate::worker::test_support::sample_account());
    state.base = origin.to_string();
    state
        .folders
        .insert(path.to_string(), (folder_id, gid.to_string()));
    state
}

/// #429: missing mail comes in runs, newest first, none longer than a
/// chunk; a cached row the server has not got is gone, unless it is
/// newer than the listing.
/// Exchange's last verb is the replied or forwarded arrow (#454).
#[test]
fn the_last_verb_marks_replied_and_forwarded() {
    let verb = |value: Option<&str>| {
        let mut v = graph_msg("M1", "2026-09-10T00:00:00Z");
        if let Some(value) = value {
            v["singleValueExtendedProperties"] =
                serde_json::json!([{ "id": "Integer 0x1081", "value": value }]);
        }
        let (m, _) = graph_message(&v, 1, 1).unwrap();
        (m.answered, m.forwarded)
    };
    assert_eq!(verb(None), (false, false));
    assert_eq!(verb(Some("102")), (true, false));
    assert_eq!(verb(Some("103")), (true, false), "reply all");
    assert_eq!(verb(Some("104")), (false, true));
    assert_eq!(verb(Some("0")), (false, false));
}

#[test]
fn the_index_plan_fetches_runs_and_drops_what_went() {
    let server: Vec<(String, String)> = (1..=8).map(|i| (format!("M{i}"), day(9 - i))).collect();
    let since = ts("2026-09-10T00:00:00Z");
    let cached: std::collections::HashMap<u32, i64> = [
        (hash_uid("M1"), ts(&day(8))),
        (hash_uid("M2"), ts(&day(7))),
        (hash_uid("M5"), ts(&day(4))),
        (hash_uid("GONE"), ts("2026-01-01T00:00:00Z")),
        (hash_uid("NEW"), since + 60),
    ]
    .into_iter()
    .collect();
    let (gone, runs) = graph_index_plan(&server, &cached, since);
    assert_eq!(gone, vec![hash_uid("GONE")]);
    assert_eq!(
        runs,
        vec![
            GraphRun {
                ids: vec!["M3".into(), "M4".into()],
                newest: day(6),
                oldest: day(5)
            },
            GraphRun {
                ids: vec!["M6".into(), "M7".into(), "M8".into()],
                newest: day(3),
                oldest: day(1)
            },
        ]
    );

    // A long run is cut into chunks.
    let many: Vec<(String, String)> = (0..GRAPH_BACKFILL_CHUNK + 5)
        .map(|i| (format!("X{i}"), day(1)))
        .collect();
    let (_, runs) = graph_index_plan(&many, &Default::default(), since);
    assert_eq!(
        runs.iter().map(|r| r.ids.len()).collect::<Vec<_>>(),
        vec![GRAPH_BACKFILL_CHUNK, 5]
    );
}

/// #429: a listing of the newest page speaks only for the mail it
/// covers; a complete one for the whole folder.
#[test]
fn a_window_listing_keeps_older_rows() {
    let listed = vec![row("A", 300, 1), row("B", 200, 1)];
    let cached: std::collections::HashMap<u32, i64> = [
        (hash_uid("A"), 300),
        (hash_uid("B"), 200),
        (hash_uid("DEL"), 250),
        (hash_uid("OLD"), 100),
    ]
    .into_iter()
    .collect();
    assert_eq!(window_gone(&cached, &listed, false), vec![hash_uid("DEL")]);
    let mut all = vec![hash_uid("DEL"), hash_uid("OLD")];
    all.sort_unstable();
    assert_eq!(window_gone(&cached, &listed, true), all);
    assert!(window_gone(&cached, &[], false).is_empty());
}

/// #429: a throttled request waits as asked and goes again.
#[test]
fn a_throttled_request_is_retried() {
    let (origin, server) = serve(vec![
        Answer {
            key: "/messages".into(),
            status: 429,
            headers: "Retry-After: 0\r\n",
            body: serde_json::json!({ "error": { "code": "ApplicationThrottled", "message": "slow down" } })
                .to_string(),
        },
        answer("/messages", 200, serde_json::json!({ "value": [graph_id("M1", &day(1))] })),
    ]);
    let (ids, next) = graph_id_page("t", &graph_ids_url(&origin, "F")).expect("retried");
    assert_eq!(ids, vec![("M1".to_string(), day(1))]);
    assert_eq!(next, None);
    assert_eq!(server.join().expect("server").len(), 2);
}

/// #429: the whole background pass over a folder, against recorded
/// answers: the ids in two pages, a short run fetched message by
/// message (one gone meanwhile), a long one by its dates; the cache
/// keeps what it had, loses what the server lost, and the list hears
/// of each step and of the end.
#[tokio::test]
async fn the_background_index_fills_and_prunes_a_folder() {
    let page1: Vec<serde_json::Value> = (1..=4)
        .map(|i| graph_id(&format!("M{i}"), &day(9 - i)))
        .collect();
    let page2: Vec<serde_json::Value> = (5..=8)
        .map(|i| graph_id(&format!("M{i}"), &day(9 - i)))
        .collect();
    let ranged: Vec<serde_json::Value> = (5..=8)
        .map(|i| graph_msg(&format!("M{i}"), &day(9 - i)))
        .collect();
    let (origin, server) = serve(vec![
        answer(
            "$top=1000&$orderby=receivedDateTime%20desc&$select=id,receivedDateTime",
            200,
            serde_json::json!({
                "value": page1,
                "@odata.nextLink": "{origin}/me/mailFolders/F-archive/messages?$select=id,receivedDateTime&$skip=4",
            }),
        ),
        answer("$skip=4", 200, serde_json::json!({ "value": page2 })),
        answer("/me/messages/M3?", 200, graph_msg("M3", &day(6))),
        answer(
            "/me/messages/M4?",
            404,
            serde_json::json!({ "error": { "code": "ErrorItemNotFound", "message": "Not found." } }),
        ),
        answer(
            "$filter=receivedDateTime%20ge%20",
            200,
            serde_json::json!({ "value": ranged }),
        ),
    ]);
    let cache = Cache::in_memory().expect("cache");
    let now = crate::datefmt::now();
    cache.upsert_messages(
        1,
        "Archive",
        &[
            row("M1", ts(&day(8)), 4),
            row("M2", ts(&day(7)), 4),
            row("M5", ts(&day(4)), 4),
            row("GONE", ts("2026-01-01T00:00:00Z"), 4),
            row("NEW", now + 60, 4),
        ],
    );
    let mut state = state_for(&origin, "Archive", 4, "F-archive");
    state.queue_backfill("Archive", true, false);
    let events = std::cell::RefCell::new(Vec::new());
    let emit = |e: WorkerEvent| events.borrow_mut().push(e);
    for _ in 0..4 {
        assert!(graph_backfill_with("t", 1, Some(&cache), &mut state, &emit).await);
    }
    assert!(state.backfill.is_empty(), "the folder's index is done");

    let mut have: Vec<u32> = cache.cached_uids(1, "Archive").into_iter().collect();
    have.sort_unstable();
    let mut want: Vec<u32> = ["M1", "M2", "M3", "M5", "M6", "M7", "M8", "NEW"]
        .iter()
        .map(|id| hash_uid(id))
        .collect();
    want.sort_unstable();
    assert_eq!(have, want);
    assert_eq!(
        state.uids.get(&hash_uid("M7")).map(String::as_str),
        Some("M7")
    );

    let events = events.into_inner();
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            WorkerEvent::Messages { folder_id: 4, .. } => "messages",
            WorkerEvent::MessagesAppend { folder_id: 4, .. } => "append",
            WorkerEvent::BackfillDone { folder_id: 4 } => "done",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["messages", "append", "append", "done"]);

    let seen = server.join().expect("server");
    assert_eq!(seen.len(), 5);
    assert!(
        seen[4].contains("ge%202026-09-01T09%3A00%3A00Z%20and%20receivedDateTime%20le%202026-09-03T09%3A00%3A00Z"),
        "{}",
        seen[4]
    );
}

/// #429: re-listing the newest page leaves the older cached mail be,
/// and says the folder holds more than the page.
#[tokio::test]
async fn a_folder_load_merges_over_the_cache() {
    let page = |from: usize| -> Vec<serde_json::Value> {
        (from..from + 100)
            .map(|i| {
                graph_msg(
                    &format!("N{i}"),
                    &format!("2026-10-01T{:02}:{:02}:00Z", 23 - i / 60, 59 - i % 60),
                )
            })
            .collect()
    };
    let (origin, server) = serve(vec![
        answer(
            "$top=100&$orderby",
            200,
            serde_json::json!({ "value": page(0), "@odata.nextLink": "{origin}/me/mailFolders/F-inbox/messages?$skip=100" }),
        ),
        answer(
            "$skip=100",
            200,
            serde_json::json!({ "value": page(100), "@odata.nextLink": "{origin}/me/mailFolders/F-inbox/messages?$skip=200" }),
        ),
        answer(
            "$skip=200",
            200,
            serde_json::json!({ "value": page(200), "@odata.nextLink": "{origin}/me/mailFolders/F-inbox/messages?$skip=300" }),
        ),
    ]);
    let cache = Cache::in_memory().expect("cache");
    // An old message from the background index, and a new one deleted
    // on the server since it was cached.
    cache.upsert_messages(
        1,
        "Inbox",
        &[
            row("OLD", ts("2025-01-01T00:00:00Z"), 1),
            row("DELETED", ts("2026-10-01T23:59:30Z"), 1),
        ],
    );
    let mut state = state_for(&origin, "Inbox", 1, "F-inbox");
    let (messages, complete) = graph_sync_folder("t", 1, 1, "Inbox", Some(&cache), &mut state)
        .await
        .expect("listed");
    assert!(!complete, "the server has more than the first page");
    assert_eq!(messages.len(), GRAPH_FIRST_PAGE + 1);
    assert!(messages.iter().any(|m| m.uid == hash_uid("OLD")));
    assert!(!messages.iter().any(|m| m.uid == hash_uid("DELETED")));
    assert_eq!(server.join().expect("server").len(), 3);

    // Not complete and never indexed: queued, first, for the folder on
    // screen; the list keeps its "Loading more…".
    assert!(!state.index_after_load("Inbox", complete, true));
    assert_eq!(
        state.backfill.front().map(|j| j.path.as_str()),
        Some("Inbox")
    );
    // A folder that fitted in one page is complete as listed.
    assert!(state.index_after_load("Inbox", true, true));
    assert!(state.backfill.is_empty());
}

/// #429: an undone move finds an old message by its Message-ID.
#[test]
fn a_message_is_found_by_its_message_id() {
    let (origin, server) = serve(vec![answer(
        "$filter=internetMessageId",
        200,
        serde_json::json!({ "value": [graph_msg("M9", &day(1))] }),
    )]);
    let hits = graph_find_by_msgid("t", &origin, "F-trash", "M9@Example.com").expect("found");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].1, "M9");
    let seen = server.join().expect("server");
    assert!(
        seen[0].contains("$filter=internetMessageId%20eq%20%27%3CM9%40Example.com%3E%27"),
        "{}",
        seen[0]
    );
}

/// #334: a flagged message's due date, in UTC; none once the flag is
/// complete or cleared, or without a date.
#[test]
fn a_due_date_is_read_only_from_an_open_flag() {
    let flag = |status: &str| {
        serde_json::json!({
            "flagStatus": status,
            "dueDateTime": { "dateTime": "2026-10-05T04:00:00.0000000", "timeZone": "UTC" },
        })
    };
    assert_eq!(graph_due(&flag("flagged")), 1_791_172_800);
    assert_eq!(graph_due(&flag("complete")), 0);
    assert_eq!(graph_due(&flag("notFlagged")), 0);
    assert_eq!(
        graph_due(&serde_json::json!({ "flagStatus": "flagged" })),
        0
    );
}

/// #536: kept offline, a message's MIME comes down from `$value` and is
/// written to the cache with its attachment; a cached message the index
/// did not list is gone from the server and asked for at no URL, and the
/// Trash is left out.
#[test]
fn the_offline_period_is_downloaded_message_by_message() {
    use crate::worker::graph::offline::keep_next;
    use crate::worker::offline::Offline;

    let raw = "From: Ann <ann@example.test>\r\nSubject: kept\r\n\
               MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n\
               --b\r\nContent-Type: text/plain\r\n\r\nbody\r\n\
               --b\r\nContent-Type: application/octet-stream\r\n\
               Content-Disposition: attachment; filename=a.bin\r\n\
               Content-Transfer-Encoding: base64\r\n\r\naGVsbG8=\r\n--b--\r\n";
    let (origin, server) = serve(vec![Answer {
        key: "/me/messages/M1/$value".into(),
        status: 200,
        headers: "",
        body: raw.into(),
    }]);
    let mut state = state_for(&origin, "Inbox", 1, "F-inbox");
    state.uids.insert(hash_uid("M1"), "M1".into());
    let cache = Cache::in_memory().unwrap();
    cache.save_folders(
        1,
        &[
            crate::models::Folder {
                id: 1,
                account_id: 1,
                name: "Inbox".into(),
                path: "Inbox".into(),
                kind: crate::models::FolderKind::Inbox,
                unread: 0,
            },
            crate::models::Folder {
                id: 2,
                account_id: 1,
                name: "Deleted Items".into(),
                path: "Deleted Items".into(),
                kind: crate::models::FolderKind::Trash,
                unread: 0,
            },
        ],
    );
    cache.upsert_messages(
        1,
        "Inbox",
        &[row("M1", ts(&day(3)), 1), row("M9", ts(&day(2)), 1)],
    );
    cache.upsert_messages(1, "Deleted Items", &[row("M7", ts(&day(4)), 2)]);
    let account = crate::config::AccountConfig {
        offline_mail: crate::config::OfflineMail::All,
        offline_attachments: true,
        ..crate::worker::test_support::sample_account()
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut offline = Offline::default();
    rt.block_on(async {
        while offline.fill(&cache, 1, &account) {
            keep_next("t", &account, 1, &cache, &state, &mut offline).await;
        }
    });
    assert_eq!(server.join().expect("server").len(), 1);
    assert!(cache.load_body(1, "Inbox", hash_uid("M1")).is_some());
    assert_eq!(cache.load_attachments(1, "Inbox", hash_uid("M1")).len(), 1);
    assert!(offline.was_tried("Inbox", hash_uid("M9")));
    assert!(cache
        .load_body(1, "Deleted Items", hash_uid("M7"))
        .is_none());
    assert!(offline.wait().is_some(), "the period is complete");
}
