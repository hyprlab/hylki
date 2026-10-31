//! Tests of the JMAP path: identities, base URLs, summaries, the index, and
//! sessions and calls against a local server; the live ones are ignored.

use std::collections::{BTreeMap, HashMap};

use super::*;
use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::models::{FolderKind, MailboxQuota, Message};
use crate::worker::compose::build::build_email;
use crate::worker::ids::hash_uid;
use crate::worker::index_queue::IndexQueue;
use crate::worker::jmap::folders::{jmap_create_mailbox, jmap_list_folders};
use crate::worker::jmap::index::{
    jmap_backfill_with, jmap_index_plan, jmap_sync_folder, JmapBackfillPhase,
};
use crate::worker::jmap::messages::{
    jmap_get_messages, jmap_list_messages, jmap_message, jmap_query_ids,
};
use crate::worker::jmap::ops::{
    crlf_line_endings, jmap_destroy_all, jmap_import_draft, jmap_import_email,
    jmap_import_template, jmap_set_emails, jmap_strip_attachment,
};
use crate::worker::jmap::send::{jmap_identities, jmap_identity_for, jmap_submit, pick_identity};
use crate::worker::jmap::wire::{
    args, call, fill_template, jmap_auth, jmap_call, jmap_connect, jmap_download,
    jmap_storage_quota, origin_of, JmapSession, CAP_CORE, CAP_MAIL,
};
use crate::worker::render::attachments::extract_attachments;
use crate::worker::test_support::sample_account;
use crate::worker::{OutgoingMessage, WorkerEvent};

/// #414: lone LFs become CRLF; CRLFs stay as they are.
#[test]
fn submission_bytes_get_crlf_line_endings() {
    assert_eq!(&*crlf_line_endings(b"a\r\nb"), b"a\r\nb");
    assert!(matches!(
        crlf_line_endings(b"a\r\nb"),
        std::borrow::Cow::Borrowed(_)
    ));
    assert_eq!(
        &*crlf_line_endings(b"\na\nb\r\nc\n"),
        b"\r\na\r\nb\r\nc\r\n"
    );
}

/// #346: the From picks its identity by address, then by a domain's
/// catch-all, before falling back to the first.
#[test]
fn an_identity_is_picked_by_address_then_by_domain() {
    let list = vec![
        serde_json::json!({ "id": "me", "email": "me@example.com" }),
        serde_json::json!({ "id": "any", "email": "*@example.org" }),
        serde_json::json!({ "id": "work", "email": "Work@Example.com" }),
    ];
    assert_eq!(pick_identity(&list, "work@example.com"), Some("work"));
    assert_eq!(pick_identity(&list, "shop+x@EXAMPLE.org"), Some("any"));
    assert_eq!(pick_identity(&list, "other@elsewhere.net"), Some("me"));
    assert_eq!(pick_identity(&[], "me@example.com"), None);
}

/// An email as `Email/get` answers it with [`EMAIL_PROPS`].
fn jmap_email(id: &str, at: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "blobId": format!("B-{id}"), "threadId": format!("T-{id}"),
        "mailboxIds": { "MB": true }, "keywords": { "$seen": true },
        "hasAttachment": false, "receivedAt": at,
        "subject": format!("Subject {id}"), "preview": "Hello there",
        "from": [{ "name": "Ada", "email": "ada@example.org" }],
        "replyTo": null, "to": [{ "email": "me@example.org" }], "cc": null,
        "messageId": [format!("<{id}@example.org>")], "inReplyTo": null, "references": null,
        "header:X-Priority:asText": null, "header:Importance:asText": null, "header:Priority:asText": null,
    })
}

fn jmap_row(id: &str, ts: i64, folder_id: u32) -> Message {
    let (mut m, _, _) =
        jmap_message(&jmap_email(id, "2026-09-01T09:00:00Z"), 1, folder_id).expect("summary");
    m.timestamp = ts;
    m
}

fn rfc3339(at: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(at)
        .unwrap()
        .timestamp()
}

/// A JMAP API on loopback answering `answers.len()` POSTs in order, each
/// with its `methodResponses`; hands back the request bodies.
fn serve_api(
    answers: Vec<serde_json::Value>,
) -> (JmapSession, std::thread::JoinHandle<Vec<serde_json::Value>>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let origin = format!("http://{}", listener.local_addr().expect("addr"));
    let handle = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        for responses in answers {
            let Ok((stream, _)) = listener.accept() else {
                break;
            };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            reader.read_line(&mut line).expect("request line");
            let mut length = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = h.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).expect("body");
            bodies.push(serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null));
            let reply = serde_json::json!({ "methodResponses": responses, "sessionState": "x" })
                .to_string();
            let mut out = stream;
            let _ = write!(
                out,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
        bodies
    });
    let s = JmapSession {
        api_url: format!("{origin}/jmap/api/"),
        download_url: String::new(),
        upload_url: String::new(),
        event_source_url: None,
        account: "a1".into(),
        auth: "Bearer t".into(),
        quota: false,
        top_level: true,
        max_depth: None,
    };
    (s, handle)
}

fn query_page(ids: &[&str], state: &str, total: usize, position: usize) -> serde_json::Value {
    serde_json::json!([["Email/query", {
        "accountId": "a1", "queryState": state, "canCalculateChanges": true,
        "position": position, "ids": ids, "total": total,
    }, "0"]])
}

fn state_with(path: &str, folder_id: u32, mailbox: &str) -> JmapState {
    let mut state = JmapState::new(&sample_account());
    state
        .folders
        .insert(path.to_string(), (folder_id, mailbox.to_string()));
    state
}

/// #429: what a complete id listing asks of the cache.
#[test]
fn the_index_plan_fetches_the_missing_and_drops_what_went() {
    let server: Vec<String> = ["M1", "M2", "M3", "M2"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let since = rfc3339("2026-09-10T00:00:00Z");
    let cached: HashMap<u32, i64> = [
        (hash_uid("M1"), 1),
        (hash_uid("GONE"), 1),
        (hash_uid("NEW"), since + 60),
    ]
    .into_iter()
    .collect();
    let (gone, missing) = jmap_index_plan(&server, &cached, since, true);
    assert_eq!(gone, vec![hash_uid("GONE")]);
    assert_eq!(missing, vec!["M2".to_string(), "M3".to_string()]);
    let (gone, _) = jmap_index_plan(&server, &cached, since, false);
    assert!(gone.is_empty(), "an unsteady listing prunes nothing");
}

/// #429: the whole background pass over a folder: the ids in two
/// pages, the missing mail in one get; the cache keeps what it had and
/// loses what the server lost.
#[tokio::test]
async fn the_background_index_fills_and_prunes_a_mailbox() {
    let got: Vec<serde_json::Value> = (2..=5)
        .map(|i| jmap_email(&format!("M{i}"), &format!("2026-09-0{}T09:00:00Z", 9 - i)))
        .collect();
    let (s, server) = serve_api(vec![
        query_page(&["M1", "M2", "M3"], "q1", 5, 0),
        query_page(&["M4", "M5"], "q1", 5, 3),
        serde_json::json!([["Email/get", { "accountId": "a1", "state": "e1", "list": got, "notFound": [] }, "0"]]),
    ]);
    let cache = Cache::in_memory().expect("cache");
    let now = crate::datefmt::now();
    cache.upsert_messages(
        1,
        "Archive",
        &[
            jmap_row("M1", rfc3339("2026-09-08T09:00:00Z"), 3),
            jmap_row("GONE", rfc3339("2026-01-01T00:00:00Z"), 3),
            jmap_row("NEW", now + 60, 3),
        ],
    );
    let mut state = state_with("Archive", 3, "MB-archive");
    state.queue_backfill("Archive", true, false);
    let events = std::cell::RefCell::new(Vec::new());
    let emit = |e: WorkerEvent| events.borrow_mut().push(e);
    for _ in 0..3 {
        assert!(jmap_backfill_with(&s, 1, Some(&cache), &mut state, &emit).await);
    }
    assert!(state.backfill.is_empty(), "the folder's index is done");
    let mut have: Vec<u32> = cache.cached_uids(1, "Archive").into_iter().collect();
    have.sort_unstable();
    let mut want: Vec<u32> = ["M1", "M2", "M3", "M4", "M5", "NEW"]
        .iter()
        .map(|id| hash_uid(id))
        .collect();
    want.sort_unstable();
    assert_eq!(have, want);
    assert_eq!(
        state.uids.get(&hash_uid("M4")).map(|(_, b)| b.as_str()),
        Some("B-M4")
    );
    let kinds: Vec<&str> = events
        .into_inner()
        .iter()
        .map(|e| match e {
            WorkerEvent::Messages { folder_id: 3, .. } => "messages",
            WorkerEvent::MessagesAppend { folder_id: 3, .. } => "append",
            WorkerEvent::BackfillDone { folder_id: 3 } => "done",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["messages", "append", "done"]);
    let bodies = server.join().expect("server");
    let first = &bodies[0]["methodCalls"][0][1];
    assert_eq!(
        first["filter"],
        serde_json::json!({ "inMailbox": "MB-archive" })
    );
    assert_eq!(first["position"], 0);
    assert_eq!(bodies[1]["methodCalls"][0][1]["position"], 3);
    assert_eq!(
        bodies[2]["methodCalls"][0][1]["ids"],
        serde_json::json!(["M2", "M3", "M4", "M5"])
    );
}

/// #429: a folder that changed between pages is listed again from the
/// top, positions having moved.
#[tokio::test]
async fn a_listing_starts_over_when_the_folder_changes() {
    let (s, server) = serve_api(vec![
        query_page(&["M1", "M2"], "q1", 4, 0),
        query_page(&["M3", "M4"], "q2", 4, 2),
        query_page(&["M0", "M1", "M2", "M3", "M4"], "q2", 5, 0),
    ]);
    let mut state = state_with("Inbox", 1, "MB-inbox");
    state.queue_backfill("Inbox", true, false);
    let emit = |_: WorkerEvent| {};
    for _ in 0..3 {
        assert!(jmap_backfill_with(&s, 1, None, &mut state, &emit).await);
    }
    match &state.backfill.front().expect("still to fetch").phase {
        JmapBackfillPhase::Fetching { missing } => assert_eq!(missing.len(), 5),
        JmapBackfillPhase::Listing { .. } => panic!("still listing"),
    }
    let bodies = server.join().expect("server");
    assert_eq!(bodies[2]["methodCalls"][0][1]["position"], 0);
}

/// #429: re-listing the newest page leaves the older cached mail be.
#[tokio::test]
async fn a_mailbox_load_merges_over_the_cache() {
    let (s, server) = serve_api(vec![serde_json::json!([
        ["Email/query", { "accountId": "a1", "queryState": "q1", "position": 0, "ids": ["A", "B"], "total": 400 }, "0"],
        ["Email/get", { "accountId": "a1", "state": "e1", "list": [
            jmap_email("A", "2026-10-02T09:00:00Z"),
            jmap_email("B", "2026-10-01T09:00:00Z"),
        ], "notFound": [] }, "1"],
    ])]);
    let cache = Cache::in_memory().expect("cache");
    cache.upsert_messages(
        1,
        "Inbox",
        &[
            jmap_row("OLD", rfc3339("2025-01-01T00:00:00Z"), 1),
            jmap_row("DEL", rfc3339("2026-10-01T12:00:00Z"), 1),
        ],
    );
    let mut state = state_with("Inbox", 1, "MB-inbox");
    let (messages, complete) = jmap_sync_folder(&s, 1, 1, "Inbox", Some(&cache), &mut state)
        .await
        .expect("listed");
    assert!(!complete, "the total says there is more");
    let mut uids: Vec<u32> = messages.iter().map(|m| m.uid).collect();
    uids.sort_unstable();
    let mut want = vec![hash_uid("A"), hash_uid("B"), hash_uid("OLD")];
    want.sort_unstable();
    assert_eq!(uids, want);
    let bodies = server.join().expect("server");
    assert_eq!(bodies[0]["methodCalls"][0][1]["calculateTotal"], true);
}

#[test]
fn the_accounts_own_mail_quota_wins() {
    let list = serde_json::json!([
        { "resourceType": "count", "scope": "account", "used": 5, "hardLimit": 100, "types": ["Email"] },
        { "resourceType": "octets", "scope": "domain", "used": 9, "hardLimit": 1000, "types": ["Email"] },
        { "resourceType": "octets", "scope": "account", "used": 2048, "hardLimit": 4096, "types": ["Email", "CalendarEvent"] },
        { "resourceType": "octets", "scope": "account", "used": 1, "hardLimit": 2, "types": ["ContactCard"] },
    ]);
    assert_eq!(
        jmap_storage_quota(&list),
        Some(MailboxQuota {
            used: 2048,
            limit: 4096
        })
    );
    // A soft limit stands in for a missing hard one; no limit, no quota.
    let soft = serde_json::json!([{ "resourceType": "octets", "scope": "account", "used": 10, "softLimit": 50 }]);
    assert_eq!(
        jmap_storage_quota(&soft),
        Some(MailboxQuota {
            used: 10,
            limit: 50
        })
    );
    let none = serde_json::json!([{ "resourceType": "octets", "scope": "account", "used": 10 }]);
    assert_eq!(jmap_storage_quota(&none), None);
}

fn account(host: &str, port: u16) -> AccountConfig {
    AccountConfig {
        imap_host: host.into(),
        imap_port: port,
        ..sample_account()
    }
}

#[test]
fn base_url_from_host_or_url() {
    assert_eq!(
        jmap_base_url(&account("mail.example.org", 443)),
        "https://mail.example.org"
    );
    assert_eq!(
        jmap_base_url(&account("mail.example.org", 0)),
        "https://mail.example.org"
    );
    assert_eq!(
        jmap_base_url(&account("mail.example.org", 8443)),
        "https://mail.example.org:8443"
    );
    assert_eq!(
        jmap_base_url(&account("http://127.0.0.1:8080/", 443)),
        "http://127.0.0.1:8080"
    );
    assert_eq!(
        jmap_base_url(&account(" https://jmap.example.org ", 993)),
        "https://jmap.example.org"
    );
}

#[test]
fn token_accounts_send_a_bearer_token() {
    let basic = AccountConfig {
        username: "me@example.org".into(),
        password: "pw".into(),
        ..account("mail.example.org", 443)
    };
    assert_eq!(jmap_auth(&basic), "Basic bWVAZXhhbXBsZS5vcmc6cHc=");
    // Fastmail's API token (#356): no username, and the token as typed,
    // minus stray whitespace from a paste.
    let token = AccountConfig {
        jmap_token: true,
        password: " fmu1-abc \n".into(),
        ..basic
    };
    assert_eq!(jmap_auth(&token), "Bearer fmu1-abc");
    let url = account("https://api.fastmail.com/jmap/session", 443);
    assert_eq!(jmap_base_url(&url), "https://api.fastmail.com/jmap/session");
    assert_eq!(origin_of(&jmap_base_url(&url)), "https://api.fastmail.com");
}

/// An HTTP server on loopback for `requests` requests: answers each by its path
/// and hands back what was asked for, with the Authorization header.
fn serve(
    requests: usize,
    answers: Vec<(&'static str, u16, String)>,
) -> (String, std::thread::JoinHandle<Vec<(String, String)>>) {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let origin = format!("http://{}", listener.local_addr().expect("addr"));
    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..requests {
            let Ok((stream, _)) = listener.accept() else {
                break;
            };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            reader.read_line(&mut line).expect("request line");
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let mut auth = String::new();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
                if let Some(v) = h
                    .strip_prefix("Authorization: ")
                    .or_else(|| h.strip_prefix("authorization: "))
                {
                    auth = v.trim().to_string();
                }
            }
            let (status, body) = answers
                .iter()
                .find(|(p, _, _)| *p == path)
                .map(|(_, s, b)| (*s, b.clone()))
                .unwrap_or((404, String::new()));
            let mut out = stream;
            let _ = write!(
                out,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            seen.push((path, auth));
        }
        seen
    });
    (origin, handle)
}

fn session_json() -> String {
    serde_json::json!({
        "apiUrl": "/jmap/api/",
        "downloadUrl": "/jmap/download/{accountId}/{blobId}/{name}?accept={type}",
        "uploadUrl": "/jmap/upload/{accountId}/",
        "primaryAccounts": { "urn:ietf:params:jmap:mail": "a1" },
        "capabilities": { "urn:ietf:params:jmap:mail": {} },
        "accounts": {},
    })
    .to_string()
}

#[test]
fn a_session_url_is_used_as_given() {
    // Fastmail's shape (#356): the session resource named outright, and
    // signed in to with a bearer token.
    let (origin, server) = serve(1, vec![("/jmap/session", 200, session_json())]);
    let acc = AccountConfig {
        jmap_token: true,
        password: "fmu1-token".into(),
        ..account(&format!("{origin}/jmap/session"), 443)
    };
    let s = jmap_connect(&acc).expect("session");
    assert_eq!(s.api_url, format!("{origin}/jmap/api/"));
    assert_eq!(s.account, "a1");
    let seen = server.join().expect("server");
    assert_eq!(
        seen,
        vec![("/jmap/session".to_string(), "Bearer fmu1-token".to_string())]
    );
}

#[test]
fn a_path_that_is_no_session_falls_back_to_well_known() {
    // A server behind a path prefix: the prefix itself is no session, so
    // the well-known path under it is asked, as before.
    let (origin, server) = serve(2, vec![("/mail/.well-known/jmap", 200, session_json())]);
    let acc = AccountConfig {
        username: "me".into(),
        password: "pw".into(),
        ..account(&format!("{origin}/mail"), 443)
    };
    let s = jmap_connect(&acc).expect("session");
    assert_eq!(s.api_url, format!("{origin}/jmap/api/"));
    let paths: Vec<String> = server
        .join()
        .expect("server")
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    assert_eq!(
        paths,
        vec!["/mail".to_string(), "/mail/.well-known/jmap".to_string()]
    );
}

#[test]
fn origin_is_scheme_host_port() {
    assert_eq!(
        origin_of("https://mail.example.test/jmap/"),
        "https://mail.example.test"
    );
    assert_eq!(origin_of("http://127.0.0.1:8080"), "http://127.0.0.1:8080");
    assert_eq!(origin_of("/jmap/"), "/jmap/");
}

#[test]
fn template_slots_are_filled() {
    let t = "https://h/download/{accountId}/{blobId}/{name}?type={type}";
    assert_eq!(
        fill_template(
            t,
            &[
                ("accountId", "a"),
                ("blobId", "b"),
                ("name", "m.eml"),
                ("type", "x")
            ]
        ),
        "https://h/download/a/b/m.eml?type=x"
    );
}

#[test]
fn summary_maps_keywords_and_threading() {
    let v = serde_json::json!({
        "id": "M1", "blobId": "B1", "threadId": "T1",
        "receivedAt": "2026-09-21T10:00:00Z",
        "subject": "Hello", "preview": "  first   words ",
        "from": [{ "name": "Ada", "email": "ada@example.org" }],
        "replyTo": [{ "email": "ada@example.org" }],
        "to": [{ "email": "me@example.org" }, { "email": "you@example.org" }],
        "keywords": { "$seen": true, "$flagged": true, "$answered": true, "Work": true, "$Junk": false },
        "hasAttachment": true,
        "messageId": ["<A@x>"], "inReplyTo": ["<B@x>"], "references": ["<C@x>", "<B@x>"],
    });
    let (m, id, blob) = jmap_message(&v, 7, 3).unwrap();
    assert_eq!((id.as_str(), blob.as_str()), ("M1", "B1"));
    assert_eq!(m.uid, hash_uid("M1"));
    assert!(!m.unread && m.starred && m.has_attachment);
    assert!(m.answered && !m.forwarded);
    assert_eq!(m.keywords, vec!["Work"]);
    assert_eq!(m.reply_to, "", "a Reply-To equal to From is dropped");
    assert_eq!(m.to, "me@example.org, you@example.org");
    assert_eq!(m.message_id, "a@x");
    assert_eq!(m.references, "b@x c@x");
    assert_eq!(m.preview, "first words");
    assert_eq!(m.timestamp, 1_789_984_800);
}

/// The whole wire path against a real server: `JMAP_LIVE=url,user,pw`
/// (a Stalwart container, say) and `cargo test jmap::tests::live -- --ignored --nocapture`.
/// Lists, downloads, flags, moves, drafts and sends a message to the
/// account itself, and leaves the mailbox as it found it, bar the Sent copy.
#[test]
#[ignore]
fn live() {
    let Ok(spec) = std::env::var("JMAP_LIVE") else {
        return;
    };
    let mut parts = spec.splitn(3, ',');
    let acc = AccountConfig {
        imap_host: parts.next().unwrap_or_default().into(),
        imap_port: 443,
        username: parts.next().unwrap_or_default().into(),
        password: parts.next().unwrap_or_default().into(),
        // JMAP_LIVE_TOKEN=1: the third field is a bearer token (#356).
        jmap_token: std::env::var_os("JMAP_LIVE_TOKEN").is_some(),
        ..sample_account()
    };
    let s = jmap_connect(&acc).expect("session");
    println!(
        "api {} download {} upload {} events {:?}",
        s.api_url, s.download_url, s.upload_url, s.event_source_url
    );
    let folders = jmap_list_folders(&s, 1, &BTreeMap::new()).expect("folders");
    for f in &folders {
        println!(
            "folder {:?} {} kind={:?} unread={}",
            f.folder.path, f.mailbox_id, f.folder.kind, f.folder.unread
        );
    }
    let inbox = folders
        .iter()
        .find(|f| f.folder.kind == FolderKind::Inbox)
        .expect("inbox");
    let trash = folders
        .iter()
        .find(|f| f.folder.kind == FolderKind::Trash)
        .expect("trash");
    let drafts = folders
        .iter()
        .find(|f| f.folder.kind == FolderKind::Drafts)
        .expect("drafts");
    let sent = folders
        .iter()
        .find(|f| f.folder.kind == FolderKind::Sent)
        .expect("sent");
    let listed = jmap_list_messages(&s, &inbox.mailbox_id, 1, inbox.folder.id).expect("list");
    for (m, id, _) in &listed {
        println!(
            "message uid={} id={id} unread={} {:?} <{}> refs={:?}",
            m.uid, m.unread, m.subject, m.message_id, m.references
        );
    }
    let (first, id, blob) = listed.first().cloned().expect("a message in the inbox");
    let raw = jmap_download(&s, &blob).expect("raw");
    assert!(
        raw.starts_with(b"From:")
            || raw
                .windows(6)
                .any(|w| w == b"\r\nFrom:" || w == b"\nFrom: "),
        "raw RFC 5322"
    );
    println!("raw {} bytes", raw.len());
    // Flags: seen on, then back off.
    jmap_set_emails(
        &s,
        serde_json::json!({ id.clone(): { "keywords/$seen": true } }),
        &[],
    )
    .expect("seen");
    let again = jmap_get_messages(&s, std::slice::from_ref(&id), 1, inbox.folder.id).expect("get");
    assert!(!again[0].0.unread, "marked seen");
    jmap_set_emails(
        &s,
        serde_json::json!({ id.clone(): { "keywords/$seen": null } }),
        &[],
    )
    .expect("unseen");
    // Move to Trash and back.
    jmap_set_emails(
        &s,
        serde_json::json!({ id.clone(): { "mailboxIds": { trash.mailbox_id.clone(): true } } }),
        &[],
    )
    .expect("move");
    assert!(jmap_list_messages(&s, &trash.mailbox_id, 1, 0)
        .expect("trash")
        .iter()
        .any(|(_, i, _)| i == &id));
    jmap_set_emails(
        &s,
        serde_json::json!({ id.clone(): { "mailboxIds": { inbox.mailbox_id.clone(): true } } }),
        &[],
    )
    .expect("move back");
    // Locate by Message-ID header.
    let found = jmap_query_ids(
        &s,
        serde_json::json!({ "header": ["Message-ID", first.message_id.clone()] }),
        5,
    )
    .expect("locate");
    println!(
        "header filter for {:?} found {found:?} (Stalwart: none; the cache answers first)",
        first.message_id
    );
    // Draft in, draft out.
    let draft = b"From: me <me@example.test>\r\nSubject: draft\r\n\r\nbody\r\n";
    let did = jmap_import_draft(&s, draft, &drafts.mailbox_id).expect("draft");
    assert!(jmap_list_messages(&s, &drafts.mailbox_id, 1, 0)
        .expect("drafts")
        .iter()
        .any(|(_, i, _)| i == &did));
    jmap_destroy_all(&s, &[did]).expect("drop draft");
    // Send to ourselves: the Sent copy is the submitted message.
    let me = acc.username.clone();
    let msg = format!("From: {me}\r\nTo: {me}\r\nSubject: live test\r\nMessage-ID: <live-{}@hylki.test>\r\n\r\nsent over JMAP\r\n", crate::datefmt::now());
    let sid = jmap_submit(
        &s,
        msg.as_bytes(),
        &me,
        std::slice::from_ref(&me),
        &sent.mailbox_id,
    )
    .expect("submit");
    assert!(jmap_list_messages(&s, &sent.mailbox_id, 1, 0)
        .expect("sent")
        .iter()
        .any(|(_, i, _)| i == &sid));
    println!("submitted {sid}");
}

/// A template (#360), with `JMAP_LIVE` as for [`live`]: a top-level
/// Templates mailbox the save makes is taken for the role by its name,
/// and the template in it is read mail, not a draft. Leaves the account
/// as it was.
#[test]
#[ignore]
fn live_template() {
    let Ok(spec) = std::env::var("JMAP_LIVE") else {
        return;
    };
    let mut parts = spec.splitn(3, ',');
    let acc = AccountConfig {
        imap_host: parts.next().unwrap_or_default().into(),
        imap_port: 443,
        username: parts.next().unwrap_or_default().into(),
        password: parts.next().unwrap_or_default().into(),
        jmap_token: std::env::var_os("JMAP_LIVE_TOKEN").is_some(),
        ..sample_account()
    };
    let s = jmap_connect(&acc).expect("session");
    let before = jmap_list_folders(&s, 1, &BTreeMap::new()).expect("folders");
    assert!(
        !before.iter().any(|f| f.folder.path == "Templates"),
        "start without one"
    );
    let made = jmap_create_mailbox(&s, "Templates", None).expect("create");
    let folders = jmap_list_folders(&s, 1, &BTreeMap::new()).expect("folders");
    let templates = folders
        .iter()
        .find(|f| f.folder.path == "Templates")
        .expect("listed");
    assert_eq!(templates.folder.kind, FolderKind::Templates);
    assert_eq!(templates.mailbox_id, made);
    let raw = b"From: me <me@example.test>\r\nSubject: template\r\n\r\nbody\r\n";
    let id = jmap_import_template(&s, raw, &templates.mailbox_id).expect("import");
    let responses = jmap_call(
        &s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(
            0,
            "Email/get",
            serde_json::json!({ "accountId": s.account, "ids": [id], "properties": ["keywords"] }),
        )],
    )
    .expect("get");
    let keywords = args(&responses, 0)["list"][0]["keywords"].clone();
    println!("template keywords {keywords}");
    assert_eq!(keywords["$seen"], serde_json::json!(true));
    assert!(keywords.get("$draft").is_none());
    jmap_destroy_all(&s, &[id]).expect("drop template");
    jmap_call(
        &s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(
            0,
            "Mailbox/set",
            serde_json::json!({ "accountId": s.account, "destroy": [made] }),
        )],
    )
    .expect("drop mailbox");
}

/// The server's identities (#346), with `JMAP_LIVE` as for [`live`]:
/// listed for the composer, and the account's own picked to send as.
#[test]
#[ignore]
fn live_identities() {
    let Ok(spec) = std::env::var("JMAP_LIVE") else {
        return;
    };
    let mut parts = spec.splitn(3, ',');
    let acc = AccountConfig {
        imap_host: parts.next().unwrap_or_default().into(),
        imap_port: 443,
        username: parts.next().unwrap_or_default().into(),
        password: parts.next().unwrap_or_default().into(),
        // JMAP_LIVE_TOKEN=1: the third field is a bearer token (#356).
        jmap_token: std::env::var_os("JMAP_LIVE_TOKEN").is_some(),
        ..sample_account()
    };
    let s = jmap_connect(&acc).expect("session");
    let list = jmap_identities(&s).expect("Identity/get");
    println!("identities {list:?}");
    let events = std::cell::RefCell::new(Vec::new());
    emit_jmap_identities(&s, &|e| events.borrow_mut().push(e));
    let got = events.borrow();
    let Some(WorkerEvent::Identities(ids)) = got.first() else {
        panic!("no Identities event")
    };
    assert!(
        ids.iter()
            .any(|(_, a)| a.eq_ignore_ascii_case(&acc.username)),
        "{ids:?}"
    );
    assert!(jmap_identity_for(&s, &acc.username).is_ok());
}

/// A signed message with a Unix-ending text file attached, sent to the
/// account itself (#414), with `JMAP_LIVE` as for [`live`]. Stalwart
/// takes bare newlines; Cyrus (Fastmail) is the server that refuses them.
#[test]
#[ignore]
fn live_signed_send() {
    let Ok(spec) = std::env::var("JMAP_LIVE") else {
        return;
    };
    if !crate::mail::pgp::available() {
        return;
    }
    let mut parts = spec.splitn(3, ',');
    let acc = AccountConfig {
        imap_host: parts.next().unwrap_or_default().into(),
        imap_port: 443,
        username: parts.next().unwrap_or_default().into(),
        password: parts.next().unwrap_or_default().into(),
        jmap_token: std::env::var_os("JMAP_LIVE_TOKEN").is_some(),
        ..sample_account()
    };
    let home = std::env::temp_dir().join(format!("hylki-gpg-jmap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::env::set_var("HYLKI_GNUPGHOME", &home);
    let me = acc.username.clone();
    let account = AccountConfig {
        email: me.clone(),
        ..acc.clone()
    };
    crate::mail::pgp::generate_key(&crate::mail::pgp::Gpg::system(), "Me", &me, "never", "")
        .expect("own key");
    let file = home.join("notes.txt");
    std::fs::write(&file, b"one\ntwo\n").unwrap();
    let msg = OutgoingMessage {
        to: me.clone(),
        subject: "live signed test".into(),
        body: "signed over JMAP".into(),
        attachments: vec![file.to_string_lossy().to_string()],
        sign: true,
        ..crate::worker::test_support::sample_outgoing()
    };
    let raw = build_email(&account, &msg).expect("builds").formatted();
    let s = jmap_connect(&acc).expect("session");
    let folders = jmap_list_folders(&s, 1, &BTreeMap::new()).expect("folders");
    let sent = folders
        .iter()
        .find(|f| f.folder.kind == FolderKind::Sent)
        .expect("sent");
    let sid =
        jmap_submit(&s, &raw, &me, std::slice::from_ref(&me), &sent.mailbox_id).expect("submit");
    println!("submitted {sid}");
    std::fs::remove_dir_all(&home).ok();
}

/// Removing an attachment over JMAP (#289), with `JMAP_LIVE` as for
/// [`live`]: the copy keeps the other file, the keywords and the arrival
/// time, and the original is destroyed. Leaves the inbox as it was.
#[test]
#[ignore]
fn live_strip_attachment() {
    let Ok(spec) = std::env::var("JMAP_LIVE") else {
        return;
    };
    let mut parts = spec.splitn(3, ',');
    let acc = AccountConfig {
        imap_host: parts.next().unwrap_or_default().into(),
        imap_port: 443,
        username: parts.next().unwrap_or_default().into(),
        password: parts.next().unwrap_or_default().into(),
        // JMAP_LIVE_TOKEN=1: the third field is a bearer token (#356).
        jmap_token: std::env::var_os("JMAP_LIVE_TOKEN").is_some(),
        ..sample_account()
    };
    let s = jmap_connect(&acc).expect("session");
    let folders = jmap_list_folders(&s, 1, &BTreeMap::new()).expect("folders");
    let inbox = folders
        .iter()
        .find(|f| f.folder.kind == FolderKind::Inbox)
        .expect("inbox");
    let raw = super::super::strip::MIXED.replace(
        "r1@example.org",
        &format!("strip-{}@hylki.test", crate::datefmt::now()),
    );
    let id = jmap_import_email(
        &s,
        raw.as_bytes(),
        serde_json::json!({
            "mailboxIds": { inbox.mailbox_id.clone(): true },
            "keywords": { "$seen": true, "$flagged": true },
            "receivedAt": "2020-01-01T10:00:00Z",
        }),
    )
    .expect("import");
    let blob = jmap_list_messages(&s, &inbox.mailbox_id, 1, inbox.folder.id)
        .expect("list")
        .into_iter()
        .find(|(_, i, _)| i == &id)
        .map(|(_, _, b)| b)
        .expect("listed");
    let mut state = JmapState::new(&sample_account());
    state.uids = HashMap::from([(hash_uid(&id), (id.clone(), blob))]);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (new_id, _) = rt
        .block_on(jmap_strip_attachment(
            &s,
            &mut state,
            &inbox.folder.path,
            hash_uid(&id),
            "report.pdf",
            18,
        ))
        .expect("strip");
    assert_ne!(new_id, id);
    let responses = jmap_call(
        &s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(
            0,
            "Email/get",
            serde_json::json!({
                "accountId": s.account,
                "ids": [id, new_id],
                "properties": ["keywords", "receivedAt", "mailboxIds", "blobId"],
            }),
        )],
    )
    .expect("get");
    let got = args(&responses, 0);
    println!("{got}");
    assert_eq!(
        got["notFound"],
        serde_json::json!([id]),
        "the original is gone"
    );
    let copy = &got["list"][0];
    assert_eq!(
        copy["keywords"],
        serde_json::json!({ "$seen": true, "$flagged": true })
    );
    assert_eq!(copy["receivedAt"], "2020-01-01T10:00:00Z");
    assert_eq!(
        copy["mailboxIds"],
        serde_json::json!({ inbox.mailbox_id.clone(): true })
    );
    let bytes = jmap_download(&s, copy["blobId"].as_str().unwrap()).expect("download");
    let names: Vec<String> = extract_attachments(&bytes)
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert_eq!(names, vec!["data.csv"]);
    jmap_destroy_all(&s, &[new_id]).expect("clean up");
}

#[test]
fn errors_name_the_call() {
    // A method-level error is an Err naming the call it answered.
    let responses = serde_json::json!([["error", { "type": "unknownMethod" }, "1"]]);
    let names = ["Mailbox/get".to_string(), "Email/query".to_string()];
    let resp = responses.as_array().unwrap();
    let tag = resp[0][2].as_str().unwrap();
    assert_eq!(names[tag.parse::<usize>().unwrap()], "Email/query");
}
