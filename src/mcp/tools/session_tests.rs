//! A whole session, as an assistant has one: the protocol over a socket,
//! the tools behind it, and a stand-in for the app at the other end of
//! the line the drafting tools take.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::app::AppMsg;
use crate::mcp::AppCall;

/// The client's half of a session with a fresh, empty cache.
async fn session() -> (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
    relm4::Receiver<AppMsg>,
) {
    use rmcp::ServiceExt;
    let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
    let (app, calls) = relm4::channel::<AppMsg>();
    let cache = crate::cache::Cache::in_memory().unwrap();
    tokio::spawn(async move {
        let service = super::Mail::new(cache, app).serve(theirs).await.unwrap();
        let _ = service.waiting().await;
    });
    let (read, write) = ours.into_split();
    (BufReader::new(read), write, calls)
}

async fn send(w: &mut tokio::net::unix::OwnedWriteHalf, v: serde_json::Value) {
    w.write_all(format!("{v}\n").as_bytes()).await.unwrap();
}

async fn answer(r: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> serde_json::Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), r.read_line(&mut line))
        .await
        .expect("an answer in time")
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

async fn call(
    r: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    w: &mut tokio::net::unix::OwnedWriteHalf,
    id: u32,
    tool: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    send(
        w,
        serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":tool,"arguments":args}}),
    )
    .await;
    answer(r).await["result"].clone()
}

#[tokio::test]
async fn an_assistant_drafts_through_the_app_and_cannot_guess_an_address() {
    let (mut r, mut w, calls) = session().await;
    send(
        &mut w,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"test","version":"0"}}}),
    )
    .await;
    let init = answer(&mut r).await;
    assert!(init["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("nothing is ever sent"));
    send(
        &mut w,
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;

    send(
        &mut w,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;
    let mut names: Vec<String> = answer(&mut r).await["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "compose_message",
            "find_contacts",
            "forward_message",
            "hylki_status",
            "list_accounts",
            "list_folders",
            "list_messages",
            "read_message",
            "read_thread",
            "reply_to_message",
            "search",
        ]
    );

    // A name is refused before anything reaches the app.
    let refused = call(
        &mut r,
        &mut w,
        3,
        "compose_message",
        serde_json::json!({"to":["Ada Lovelace"],"body":"Hi"}),
    )
    .await;
    assert_eq!(refused["isError"], true);
    assert!(refused.to_string().contains("find_contacts"));

    // An address goes to the app, which opens it and says so.
    let app = std::thread::spawn(move || match calls.recv_sync() {
        Some(AppMsg::Assistant(AppCall::Draft { draft, done })) => {
            let _ = done.send(Ok("opened".into()));
            draft
        }
        other => panic!("not a draft: {other:?}"),
    });
    let opened = call(
        &mut r,
        &mut w,
        4,
        "compose_message",
        serde_json::json!({"to":["Ada <ada@example.org>"],"bcc":["bo@example.com"],
            "subject":"Plans\nfor Friday","body":"Hi Ada,\n\nFriday?"}),
    )
    .await;
    assert_ne!(opened["isError"], true, "{opened}");
    let draft = app.join().unwrap();
    assert_eq!(draft.to, ["Ada <ada@example.org>"]);
    assert_eq!(draft.bcc, ["bo@example.com"]);
    assert_eq!(draft.subject, "Plans for Friday");
    assert_eq!(draft.text, "Hi Ada,\n\nFriday?");
    assert!(matches!(
        draft.kind,
        crate::mcp::draft::DraftKind::New { account_id: None }
    ));

    // A message that is not there is answered as such.
    let missing = call(
        &mut r,
        &mut w,
        5,
        "reply_to_message",
        serde_json::json!({"id":"nobody@example.org/INBOX/1","body":"Hi"}),
    )
    .await;
    assert_eq!(missing["isError"], true);
}
