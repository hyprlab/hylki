//! Whole sessions, as an assistant has them: the protocol over a socket, the
//! tools behind it, a real cache and a stand-in for the app at the other end
//! of the line. Mostly about what the user did not allow staying out of reach.

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::app::AppMsg;
use crate::cache::Cache;
use crate::config::{AccountGrant, AssistantCap, AssistantPolicy};
use crate::mcp::server::{KnownAccount, Shared};
use crate::mcp::{AppCall, Organized};
use crate::models::{Folder, FolderKind, Message};

const ADA: &str = "ada@example.org";
const BO: &str = "bo@example.org";

fn known_accounts() -> Vec<KnownAccount> {
    let known = |id, address: &str| KnownAccount {
        id,
        address: address.into(),
        name: String::new(),
        sends: true,
    };
    vec![known(1, ADA), known(2, BO)]
}

fn message(uid: u32, subject: &str) -> Message {
    Message {
        id: uid,
        uid,
        subject: subject.into(),
        from_addr: "sender@example.net".into(),
        message_id: format!("{uid}@example.net"),
        timestamp: 1_700_000_000 + i64::from(uid),
        preview: format!("preview of {subject}"),
        ..Message::blank()
    }
}

fn folder(id: u32, path: &str, kind: FolderKind) -> Folder {
    Folder {
        id,
        path: path.into(),
        name: path.into(),
        kind,
        account_id: 0,
        unread: 0,
        childless: false,
    }
}

/// Two accounts' worth of mail. In Ada's: the Inbox, Junk and a private
/// folder. Uid 5 has its text on disk; uid 6 does not.
fn seeded_cache() -> Cache {
    let cache = Cache::in_memory().unwrap();
    cache.save_folders(
        1,
        &[
            folder(1, "INBOX", FolderKind::Inbox),
            folder(2, "Spam", FolderKind::Junk),
            folder(3, "Private", FolderKind::Custom),
            folder(4, "Bin", FolderKind::Trash),
            folder(5, "Vault", FolderKind::Custom),
            // iCloud: a plain folder called Trash, beside the real one.
            folder(6, "Trash", FolderKind::Custom),
            // On This Computer: the same name, once for each account copied.
            folder(7, "iCloud/Saved", FolderKind::Custom),
            folder(8, "Work/Saved", FolderKind::Custom),
        ],
    );
    cache.save_folders(2, &[folder(1, "INBOX", FolderKind::Inbox)]);
    cache.upsert_messages(
        1,
        "INBOX",
        &[message(5, "Lunch"), message(6, "Not downloaded")],
    );
    cache.upsert_messages(1, "Spam", &[message(7, "Prize")]);
    cache.upsert_messages(1, "Private", &[message(8, "Diagnosis")]);
    cache.upsert_messages(2, "INBOX", &[message(9, "Bo's mail")]);
    let mut sealed = message(11, "Sealed");
    sealed.preview = crate::models::ENCRYPTED_PREVIEW.into();
    cache.upsert_messages(1, "Vault", &[sealed]);
    cache.upsert_messages(1, "Trash", &[message(21, "In the plain Trash folder")]);
    cache.upsert_messages(1, "iCloud/Saved", &[message(22, "One copy")]);
    cache.upsert_messages(1, "Work/Saved", &[message(23, "Other copy")]);
    cache.save_body(1, "INBOX", 5, "<p>Friday at noon?</p>");
    // Another account's mail, under the number this one has now: no such
    // folder is on the account.
    let mut stale = message(31, "Lunch of another account");
    stale.starred = true;
    cache.upsert_messages(1, "[Gmail]/All Mail", &[stale]);
    cache
}

fn ada_may(caps: &[AssistantCap]) -> AssistantPolicy {
    let mut policy = AssistantPolicy {
        enabled: true,
        ..Default::default()
    };
    let grant: &mut AccountGrant = policy.grant_mut(ADA);
    grant.on = true;
    for cap in AssistantCap::ALL {
        grant.set_switch(cap, caps.contains(&cap));
    }
    policy
}

/// What the stand-in for the app was asked.
#[derive(Default)]
struct AppSaw {
    fetched: usize,
    drafts: usize,
    organized: Vec<String>,
    activity: Vec<(String, bool)>,
    /// What each account's server answers: hits, or why not. An account not
    /// here has no worker, so its answer never comes.
    server: std::collections::HashMap<u32, Result<Vec<(String, u32)>, String>>,
    /// What servers were asked: account, words, folders.
    asked: Vec<(u32, Vec<String>, Vec<String>)>,
}

struct Session {
    read: BufReader<tokio::net::unix::OwnedReadHalf>,
    write: tokio::net::unix::OwnedWriteHalf,
    policy: Arc<RwLock<AssistantPolicy>>,
    saw: Arc<Mutex<AppSaw>>,
    next_id: u32,
}

impl Session {
    /// A session past its handshake. `raw` is what the account's server keeps
    /// for any message asked of it.
    async fn start(policy: AssistantPolicy, raw: Result<Vec<u8>, String>) -> Session {
        use rmcp::ServiceExt;
        let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
        let (app, calls) = relm4::channel::<AppMsg>();
        let policy = Arc::new(RwLock::new(policy));
        let shared = Arc::new(Shared {
            policy: policy.clone(),
            memo: Default::default(),
            accounts: known_accounts,
        });
        let saw = Arc::new(Mutex::new(AppSaw::default()));
        {
            let saw = saw.clone();
            std::thread::spawn(move || {
                while let Some(call) = calls.recv_sync() {
                    let AppMsg::Assistant(call) = call else {
                        panic!("not for the assistant: {call:?}")
                    };
                    let mut saw = saw.lock().unwrap();
                    match call {
                        AppCall::FetchRaw { done, .. } => {
                            saw.fetched += 1;
                            let _ = done.send(raw.clone());
                        }
                        AppCall::AskServer { account_id, ask } => {
                            saw.asked
                                .push((account_id, ask.terms.clone(), ask.paths.clone()));
                            match saw.server.get(&account_id).cloned() {
                                Some(Ok(hits)) => {
                                    let _ = ask.answer.send(crate::worker::ServerFound {
                                        account_id,
                                        found: hits.len(),
                                        hits,
                                        done: true,
                                        ..Default::default()
                                    });
                                }
                                Some(Err(why)) => ask.refuse(account_id, why),
                                // No worker: the channel closes unanswered.
                                None => drop(ask),
                            }
                        }
                        AppCall::Draft { done, .. } => {
                            saw.drafts += 1;
                            let _ = done.send(Ok("opened".into()));
                        }
                        AppCall::Organize { targets, done, .. } => {
                            let changed: Vec<String> =
                                targets.iter().map(|t| t.id.clone()).collect();
                            saw.organized.extend(changed.clone());
                            let _ = done.send(Organized {
                                changed,
                                skipped: Vec::new(),
                            });
                        }
                        AppCall::Activity(a) => saw.activity.push((a.tool, a.ok)),
                        AppCall::ShowWaiting => {}
                    }
                }
            });
        }
        let mail = super::Mail::new(seeded_cache(), app, shared);
        tokio::spawn(async move {
            let service = mail.serve(theirs).await.unwrap();
            let _ = service.waiting().await;
        });
        let (read, write) = ours.into_split();
        let mut session = Session {
            read: BufReader::new(read),
            write,
            policy,
            saw,
            next_id: 1,
        };
        session
            .request(
                "initialize",
                json!({"protocolVersion":"2025-06-18","capabilities":{},
                       "clientInfo":{"name":"test","version":"0"}}),
            )
            .await;
        session
            .send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        session
    }

    async fn send(&mut self, v: Value) {
        self.write
            .write_all(format!("{v}\n").as_bytes())
            .await
            .unwrap();
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(10), self.read.read_line(&mut line))
            .await
            .expect("an answer in time")
            .unwrap();
        serde_json::from_str::<Value>(&line).unwrap()["result"].clone()
    }

    async fn call(&mut self, tool: &str, args: Value) -> Value {
        self.request("tools/call", json!({"name":tool,"arguments":args}))
            .await
    }
}

/// The text of an answer, parsed when it is JSON.
fn data(result: &Value) -> Value {
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

fn refused(result: &Value) -> bool {
    result["isError"] == true
}

fn id_of(account: &str, path: &str, uid: u32) -> String {
    super::message_id(account, path, uid)
}

fn subjects(result: &Value) -> Vec<String> {
    data(result)
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|m| m["subject"].as_str().unwrap().to_string())
        .collect()
}

/// A PGP/MIME encrypted message, as the server keeps it.
fn encrypted_raw() -> Vec<u8> {
    b"From: a@example.net\r\nSubject: Sealed\r\nMIME-Version: 1.0\r\n\
      Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"b\"\r\n\r\n\
      --b\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n\r\n\
      --b\r\nContent-Type: application/octet-stream\r\n\r\n\
      -----BEGIN PGP MESSAGE-----\r\n\r\nhQEMA0ciphertext\r\n-----END PGP MESSAGE-----\r\n\r\n--b--\r\n"
        .to_vec()
}

fn plain_raw(text: &str) -> Vec<u8> {
    format!("From: a@example.net\r\nSubject: Plain\r\nContent-Type: text/plain\r\n\r\n{text}\r\n")
        .into_bytes()
}

#[tokio::test]
async fn the_tools_offered_cannot_send_or_erase() {
    let mut s = Session::start(ada_may(&AssistantCap::ALL), Err("none".into())).await;
    let tools = s.request("tools/list", json!({})).await;
    let tools = tools["tools"].as_array().unwrap();
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "archive_messages",
            "compose_message",
            "find_contacts",
            "forward_message",
            "hylki_status",
            "list_accounts",
            "list_attachments",
            "list_folders",
            "list_messages",
            "list_tags",
            "move_messages",
            "read_attachment",
            "read_message",
            "read_thread",
            "reply_to_message",
            "search",
            "search_server",
            "set_read",
            "set_starred",
            "set_tag",
            "trash_messages",
        ]
    );
    // What a client may run without asking, it can tell from the tool.
    let read_only = |name: &str| {
        tools.iter().find(|t| t["name"] == name).unwrap()["annotations"]["readOnlyHint"] == true
    };
    assert!(read_only("read_message") && read_only("search"));
    assert!(!read_only("trash_messages") && !read_only("compose_message"));
}

#[tokio::test]
async fn nothing_is_reachable_until_the_user_allows_it() {
    let mut s = Session::start(
        AssistantPolicy {
            enabled: true,
            ..Default::default()
        },
        Ok(plain_raw("text")),
    )
    .await;
    let id = id_of(ADA, "INBOX", 5);
    for (tool, args) in [
        ("list_accounts", json!({})),
        ("list_folders", json!({})),
        ("list_messages", json!({})),
        ("search", json!({"query":"lunch"})),
        ("read_message", json!({"id":id})),
        ("list_attachments", json!({"id":id})),
        ("find_contacts", json!({"query":"ada"})),
        ("compose_message", json!({"to":["a@example.org"]})),
        ("reply_to_message", json!({"id":id})),
        ("set_read", json!({"ids":[id],"read":true})),
        ("trash_messages", json!({"ids":[id]})),
    ] {
        let result = s.call(tool, args).await;
        let changed = !data(&result)["changed"]
            .as_array()
            .is_none_or(Vec::is_empty);
        assert!(refused(&result) || !changed, "{tool}: {result}");
        assert!(!result.to_string().contains("Lunch"), "{tool}: {result}");
    }
    let saw = s.saw.lock().unwrap();
    assert_eq!((saw.fetched, saw.drafts), (0, 0));
    assert!(saw.organized.is_empty());
}

#[tokio::test]
async fn an_account_not_shared_does_not_exist() {
    let mut s = Session::start(ada_may(&AssistantCap::ALL), Ok(plain_raw("text"))).await;
    let accounts = s.call("list_accounts", json!({})).await;
    assert!(!accounts.to_string().contains(BO), "{accounts}");
    assert!(!subjects(&s.call("list_messages", json!({})).await).contains(&"Bo's mail".to_string()));
    let named = s.call("list_messages", json!({"account":BO})).await;
    assert!(refused(&named), "{named}");
    let read = s
        .call("read_message", json!({"id":id_of(BO,"INBOX",9)}))
        .await;
    assert!(
        refused(&read) && !read.to_string().contains("Bo's mail"),
        "{read}"
    );
    let search = s.call("search", json!({"query":"mail"})).await;
    assert!(subjects(&search).is_empty(), "{search}");
    let changed = s
        .call("set_read", json!({"ids":[id_of(BO,"INBOX",9)],"read":true}))
        .await;
    assert_eq!(data(&changed)["changed"], json!([]), "{changed}");
    assert!(s.saw.lock().unwrap().organized.is_empty());
}

#[tokio::test]
async fn each_capability_stands_alone() {
    let id = id_of(ADA, "INBOX", 5);
    let mut s = Session::start(ada_may(&[AssistantCap::List]), Ok(plain_raw("text"))).await;
    // Lists, but not the text behind them, nor a search of it.
    assert_eq!(
        subjects(&s.call("list_messages", json!({})).await),
        ["Not downloaded", "Lunch"]
    );
    let read = s.call("read_message", json!({"id":id})).await;
    assert!(
        refused(&read) && data(&read).to_string().contains("read message text"),
        "{read}"
    );
    assert!(subjects(&s.call("search", json!({"query":"noon"})).await).is_empty());
    for (tool, args, cap) in [
        ("list_attachments", json!({"id":id}), "read attachments"),
        ("reply_to_message", json!({"id":id}), "prepare messages"),
        ("forward_message", json!({"id":id}), "prepare messages"),
    ] {
        let result = s.call(tool, args).await;
        assert!(
            refused(&result) && result.to_string().contains(cap),
            "{tool}: {result}"
        );
    }
    for (tool, args, cap) in [
        (
            "set_read",
            json!({"ids":[id],"read":true}),
            "mark mail read or unread",
        ),
        (
            "set_starred",
            json!({"ids":[id],"starred":true}),
            "add and remove stars",
        ),
        ("archive_messages", json!({"ids":[id]}), "move and archive"),
        ("trash_messages", json!({"ids":[id]}), "move mail to Trash"),
    ] {
        let result = s.call(tool, args).await;
        assert!(result.to_string().contains(cap), "{tool}: {result}");
        assert_eq!(data(&result)["changed"], json!([]), "{tool}: {result}");
    }
    assert!(refused(
        &s.call("move_messages", json!({"ids":[id],"folder":"Private"}))
            .await
    ));

    // Allowed, each does its one thing.
    let mut s = Session::start(
        ada_may(&[AssistantCap::List, AssistantCap::Star]),
        Ok(plain_raw("text")),
    )
    .await;
    let starred = s
        .call("set_starred", json!({"ids":[id],"starred":true}))
        .await;
    assert_eq!(data(&starred)["changed"], json!([id]), "{starred}");
    let read = s.call("set_read", json!({"ids":[id],"read":true})).await;
    assert_eq!(data(&read)["changed"], json!([]), "{read}");
    assert_eq!(s.saw.lock().unwrap().organized, [id]);
}

#[tokio::test]
async fn a_new_message_needs_only_its_own_permission() {
    let mut s = Session::start(ada_may(&[AssistantCap::Draft]), Ok(plain_raw("text"))).await;
    let opened = s
        .call(
            "compose_message",
            json!({"to":["Ada <ada@example.org>"],"body":"Hi"}),
        )
        .await;
    assert_ne!(opened["isError"], true, "{opened}");
    assert_eq!(s.saw.lock().unwrap().drafts, 1);
    // A reply names a message, which takes seeing it.
    let reply = s
        .call("reply_to_message", json!({"id":id_of(ADA,"INBOX",5)}))
        .await;
    assert!(refused(&reply), "{reply}");
    // A name is refused before anything reaches the app.
    let name = s
        .call("compose_message", json!({"to":["Ada Lovelace"]}))
        .await;
    assert!(
        refused(&name) && name.to_string().contains("find_contacts"),
        "{name}"
    );
    assert_eq!(s.saw.lock().unwrap().drafts, 1);
    // And it does not start from an account not shared.
    let elsewhere = s
        .call(
            "compose_message",
            json!({"account":BO,"to":["a@example.org"]}),
        )
        .await;
    assert!(refused(&elsewhere), "{elsewhere}");
}

#[tokio::test]
async fn a_folder_kept_back_is_out_of_reach_by_every_route() {
    let mut policy = ada_may(&AssistantCap::ALL);
    policy
        .grant_mut(ADA)
        .folders
        .insert("Private".into(), false);
    let mut s = Session::start(policy, Ok(plain_raw("text"))).await;
    for hidden in ["Spam", "Private", "Bin"] {
        let folders = s.call("list_folders", json!({})).await;
        assert!(!folders.to_string().contains(hidden), "{folders}");
        let list = s.call("list_messages", json!({"folder":hidden})).await;
        assert!(refused(&list), "{hidden}: {list}");
        let search = s.call("search", json!({"query":"a","folder":hidden})).await;
        assert!(refused(&search), "{hidden}: {search}");
    }
    assert!(refused(
        &s.call("list_messages", json!({"folder":"junk"})).await
    ));
    let search = s.call("search", json!({"query":"prize"})).await;
    assert!(subjects(&search).is_empty(), "{search}");
    let search = s.call("search", json!({"query":"diagnosis"})).await;
    assert!(subjects(&search).is_empty(), "{search}");
    // An id written by hand gets no further.
    for (path, uid) in [("Spam", 7), ("Private", 8)] {
        let read = s
            .call("read_message", json!({"id":id_of(ADA,path,uid)}))
            .await;
        assert!(refused(&read), "{path}: {read}");
        let moved = s
            .call("archive_messages", json!({"ids":[id_of(ADA,path,uid)]}))
            .await;
        assert_eq!(data(&moved)["changed"], json!([]), "{path}: {moved}");
    }
    // Nor is one a place to move mail to.
    let moved = s
        .call(
            "move_messages",
            json!({"ids":[id_of(ADA,"INBOX",5)],"folder":"Private"}),
        )
        .await;
    assert!(refused(&moved), "{moved}");
    assert_eq!(s.saw.lock().unwrap().fetched, 0);
    assert!(s.saw.lock().unwrap().organized.is_empty());
}

#[tokio::test]
async fn a_path_means_that_folder_and_a_shared_name_means_none_of_them() {
    let mut s = Session::start(ada_may(&AssistantCap::ALL), Err("none".into())).await;
    // The path list_folders gives wins over the role of the same word...
    assert_eq!(
        subjects(&s.call("list_messages", json!({"folder":"Trash"})).await),
        ["In the plain Trash folder"]
    );
    // ...and the two names pick neither copy at random: moving mail by one
    // would put it in the wrong place.
    assert!(refused(
        &s.call("list_messages", json!({"folder":"Saved"})).await
    ));
    assert_eq!(
        subjects(
            &s.call("list_messages", json!({"folder":"Work/Saved"}))
                .await
        ),
        ["Other copy"]
    );
    let moved = s
        .call(
            "move_messages",
            json!({"ids":[id_of(ADA,"INBOX",5)],"folder":"Saved"}),
        )
        .await;
    assert!(refused(&moved), "{moved}");
}

#[tokio::test]
async fn a_listing_goes_further_back_with_an_offset() {
    let mut s = Session::start(ada_may(&AssistantCap::ALL), Err("none".into())).await;
    let page = |r: &Value| subjects(r);
    let all = page(&s.call("list_messages", json!({"limit":2})).await);
    assert_eq!(all, ["Not downloaded", "Lunch"]);
    let one = page(&s.call("list_messages", json!({"limit":1,"offset":1})).await);
    assert_eq!(one, ["Lunch"]);
    assert!(page(&s.call("list_messages", json!({"offset":2})).await).is_empty());
}

#[tokio::test]
async fn junk_opens_only_when_the_user_says_so() {
    let mut policy = ada_may(&AssistantCap::ALL);
    policy.grant_mut(ADA).folders.insert("Spam".into(), true);
    let mut s = Session::start(policy, Ok(plain_raw("text"))).await;
    assert_eq!(
        subjects(&s.call("list_messages", json!({"folder":"junk"})).await),
        ["Prize"]
    );
}

#[tokio::test]
async fn encrypted_mail_never_reaches_an_assistant() {
    let mut s = Session::start(ada_may(&AssistantCap::ALL), Ok(encrypted_raw())).await;
    // Nothing says so until its bytes are seen: this one's list row looks
    // like any other, and its text is not on disk.
    let id = id_of(ADA, "INBOX", 6);
    let read = s.call("read_message", json!({"id":id})).await;
    let out = data(&read);
    assert_eq!(out["encrypted"], true, "{read}");
    assert!(out.get("body").is_none(), "{read}");
    assert!(!read.to_string().contains("ciphertext"), "{read}");
    assert_eq!(
        s.saw.lock().unwrap().fetched,
        1,
        "asked for bytes, not a body"
    );

    for (tool, args) in [
        ("reply_to_message", json!({"id":id})),
        ("forward_message", json!({"id":id})),
        ("list_attachments", json!({"id":id})),
        ("read_attachment", json!({"id":id,"index":0})),
    ] {
        let result = s.call(tool, args).await;
        assert!(
            refused(&result) && result.to_string().contains("encrypted"),
            "{tool}: {result}"
        );
    }
    // Refused, so nothing was quoted into a composer, and it was not kept.
    assert_eq!(s.saw.lock().unwrap().drafts, 0);
    assert!(
        s.saw.lock().unwrap().fetched >= 2,
        "an encrypted message is not memoized"
    );
}

#[tokio::test]
async fn a_message_marked_encrypted_is_not_even_fetched() {
    let mut s = Session::start(ada_may(&AssistantCap::ALL), Ok(plain_raw("text"))).await;
    let list = data(&s.call("list_messages", json!({"folder":"Vault"})).await);
    assert_eq!(list[0]["encrypted"], true, "{list}");
    assert_eq!(list[0]["preview"], "", "{list}");
    let id = list[0]["id"].as_str().unwrap().to_string();
    let read = data(&s.call("read_message", json!({"id":id})).await);
    assert_eq!(read["encrypted"], true, "{read}");
    assert!(read.get("body").is_none(), "{read}");
    assert_eq!(s.saw.lock().unwrap().fetched, 0);
}

#[tokio::test]
async fn a_message_not_on_disk_is_downloaded_without_going_near_a_decrypter() {
    let mut s = Session::start(ada_may(&AssistantCap::ALL), Ok(plain_raw("Hello there"))).await;
    let read = s
        .call("read_message", json!({"id":id_of(ADA,"INBOX",6)}))
        .await;
    assert!(
        data(&read)["body"]
            .as_str()
            .unwrap()
            .contains("Hello there"),
        "{read}"
    );
    assert_eq!(s.saw.lock().unwrap().fetched, 1);
    // What came is kept a while: its files are each a call of their own.
    let again = s
        .call("read_message", json!({"id":id_of(ADA,"INBOX",6)}))
        .await;
    assert_ne!(again["isError"], true);
    assert_eq!(s.saw.lock().unwrap().fetched, 1);
}

#[tokio::test]
async fn a_failed_download_answers_at_once_and_says_why() {
    let mut s = Session::start(
        ada_may(&AssistantCap::ALL),
        Err("the server said no".into()),
    )
    .await;
    let read = s
        .call("read_message", json!({"id":id_of(ADA,"INBOX",6)}))
        .await;
    let out = data(&read);
    assert!(
        out["body_note"]
            .as_str()
            .unwrap()
            .contains("the server said no"),
        "{read}"
    );
    assert_eq!(out["preview"], "preview of Not downloaded");
}

#[tokio::test]
async fn text_on_disk_comes_without_what_the_sender_hid() {
    let mut s = Session::start(ada_may(&AssistantCap::ALL), Err("none".into())).await;
    let read = s
        .call("read_message", json!({"id":id_of(ADA,"INBOX",5)}))
        .await;
    assert!(
        data(&read)["body"]
            .as_str()
            .unwrap()
            .contains("Friday at noon?"),
        "{read}"
    );
    assert_eq!(s.saw.lock().unwrap().fetched, 0, "on disk: no download");
}

#[tokio::test]
async fn a_switch_turned_off_holds_from_the_next_call() {
    let id = id_of(ADA, "INBOX", 5);
    let mut s = Session::start(ada_may(&AssistantCap::ALL), Err("none".into())).await;
    assert_ne!(
        s.call("read_message", json!({"id":id})).await["isError"],
        true
    );
    s.policy.write().unwrap().grant_mut(ADA).read = false;
    assert!(refused(&s.call("read_message", json!({"id":id})).await));
    s.policy.write().unwrap().grant_mut(ADA).on = false;
    assert!(refused(&s.call("list_messages", json!({})).await));
}

#[tokio::test]
async fn one_call_changes_a_limited_number_of_messages() {
    let mut s = Session::start(ada_may(&AssistantCap::ALL), Err("none".into())).await;
    let ids: Vec<String> = (0..101).map(|i| id_of(ADA, "INBOX", i)).collect();
    assert!(refused(&s.call("trash_messages", json!({"ids":ids})).await));
    assert!(refused(&s.call("trash_messages", json!({"ids":[]})).await));
    assert!(s.saw.lock().unwrap().organized.is_empty());
}

#[tokio::test]
async fn every_call_goes_on_the_record_refused_or_not() {
    let id = id_of(ADA, "INBOX", 5);
    let mut s = Session::start(ada_may(&[AssistantCap::List]), Err("none".into())).await;
    s.call("hylki_status", json!({})).await;
    s.call("list_messages", json!({})).await;
    s.call("read_message", json!({"id":id})).await;
    // The record goes to the app's loop, on its own time.
    for _ in 0..200 {
        if s.saw.lock().unwrap().activity.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        s.saw.lock().unwrap().activity,
        [
            ("list_messages".to_string(), true),
            ("read_message".to_string(), false)
        ]
    );
}

#[tokio::test]
async fn status_says_what_is_allowed_and_what_never_is() {
    let mut s = Session::start(
        ada_may(&[AssistantCap::List, AssistantCap::Read]),
        Err("none".into()),
    )
    .await;
    let status = data(&s.call("hylki_status", json!({})).await);
    assert_eq!(status["accounts"][0]["account"], ADA);
    assert_eq!(
        status["accounts"][0]["allowed"],
        json!(["see message lists", "read message text"])
    );
    assert_eq!(status["accounts"].as_array().unwrap().len(), 1);
    assert!(status["never_allowed"].to_string().contains("encrypted"));
}

const LOOKING: &[AssistantCap] = &[
    AssistantCap::List,
    AssistantCap::Read,
    AssistantCap::SearchServer,
];

fn answered_by_server(s: &Session, hits: &[(&str, u32)]) {
    let hits = hits.iter().map(|(p, uid)| (p.to_string(), *uid)).collect();
    s.saw.lock().unwrap().server.insert(1, Ok(hits));
}

#[tokio::test]
async fn a_server_is_asked_only_with_leave_to_search_it_and_read_the_text() {
    for caps in [
        &[AssistantCap::List, AssistantCap::Read][..],
        &[AssistantCap::List, AssistantCap::SearchServer][..],
    ] {
        let mut s = Session::start(ada_may(caps), Ok(plain_raw("text"))).await;
        let result = s.call("search_server", json!({"query":"lunch"})).await;
        assert!(refused(&result), "{result}");
        assert!(result.to_string().contains("search on the mail server"));
        assert!(s.saw.lock().unwrap().asked.is_empty());
    }
}

#[tokio::test]
async fn a_server_is_asked_about_shared_folders_and_answers_only_for_them() {
    let mut policy = ada_may(LOOKING);
    policy
        .grant_mut(ADA)
        .folders
        .insert("Private".into(), false);
    let mut s = Session::start(policy, Ok(plain_raw("text"))).await;
    // Whatever a worker gets wrong, a folder kept back stays back.
    answered_by_server(
        &s,
        &[("INBOX", 5), ("Private", 8), ("Spam", 7), ("INBOX", 999)],
    );
    let result = s.call("search_server", json!({"query":"lunch"})).await;
    let answer = data(&result);
    let found: Vec<&str> = answer["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["subject"].as_str().unwrap())
        .collect();
    assert_eq!(found, ["Lunch"], "{result}");
    assert_eq!(answer["found"], 4);
    let saw = s.saw.lock().unwrap();
    let (account, words, paths) = &saw.asked[0];
    assert_eq!(
        (*account, words.as_slice()),
        (1, &["lunch".to_string()][..])
    );
    assert!(paths.contains(&"INBOX".to_string()));
    for hidden in ["Private", "Spam", "Bin"] {
        assert!(!paths.contains(&hidden.to_string()), "{paths:?}");
    }
}

#[tokio::test]
async fn what_a_server_found_can_be_read_like_any_other() {
    let mut s = Session::start(ada_may(LOOKING), Ok(plain_raw("from the server"))).await;
    answered_by_server(&s, &[("INBOX", 6)]);
    let result = s
        .call("search_server", json!({"query":"words only it knows"}))
        .await;
    let id = data(&result)["messages"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let read = s.call("read_message", json!({"id":id})).await;
    assert!(data(&read)["body"]
        .as_str()
        .unwrap()
        .contains("from the server"));
}

#[tokio::test]
async fn encrypted_mail_a_server_finds_is_named_but_not_opened() {
    let mut s = Session::start(ada_may(LOOKING), Ok(plain_raw("text"))).await;
    answered_by_server(&s, &[("Vault", 11)]);
    let result = s.call("search_server", json!({"query":"sealed"})).await;
    let message = &data(&result)["messages"][0];
    assert_eq!(message["encrypted"], true);
    let id = message["id"].as_str().unwrap().to_string();
    let read = s.call("read_message", json!({"id":id})).await;
    assert!(data(&read)["body"].is_null(), "{read}");
    assert!(data(&read)["body_note"]
        .as_str()
        .unwrap()
        .contains("encrypted"));
}

#[tokio::test]
async fn a_server_that_cannot_answer_is_said_to_have_not_answered() {
    let mut s = Session::start(ada_may(LOOKING), Ok(plain_raw("text"))).await;
    s.saw
        .lock()
        .unwrap()
        .server
        .insert(1, Err("cannot be reached".into()));
    let result = s.call("search_server", json!({"query":"lunch"})).await;
    assert!(!refused(&result), "{result}");
    let answer = data(&result);
    assert_eq!(answer["not_searched"][0]["account"], ADA);
    assert_eq!(answer["not_searched"][0]["why"], "cannot be reached");

    // No worker to ask: the channel closes with no answer.
    s.saw.lock().unwrap().server.clear();
    let result = s.call("search_server", json!({"query":"lunch"})).await;
    assert!(data(&result)["not_searched"][0]["why"]
        .as_str()
        .unwrap()
        .contains("did not finish"));
}

#[tokio::test]
async fn a_server_is_not_asked_what_it_cannot_search_for() {
    let mut s = Session::start(ada_may(LOOKING), Ok(plain_raw("text"))).await;
    let long = "x".repeat(2000);
    for query in ["", "   ", "is:unread has:attachment", long.as_str()] {
        let result = s.call("search_server", json!({"query":query})).await;
        assert!(refused(&result), "{query}: {result}");
    }
    assert!(refused(
        &s.call(
            "search_server",
            json!({"query":"x","folder":"nothing like it"})
        )
        .await
    ));
    assert!(s.saw.lock().unwrap().asked.is_empty());
}

#[tokio::test]
async fn rows_of_a_folder_the_account_does_not_have_are_out_of_reach() {
    let mut caps = LOOKING.to_vec();
    caps.push(AssistantCap::Attachments);
    let mut s = Session::start(ada_may(&caps), Ok(plain_raw("text"))).await;
    let stale = "Lunch of another account";
    let search = s.call("search", json!({"query":"lunch"})).await;
    assert!(!search.to_string().contains(stale), "{search}");
    let starred = s.call("list_messages", json!({"folder":"starred"})).await;
    assert!(!starred.to_string().contains(stale), "{starred}");
    let listed = s
        .call("list_messages", json!({"folder":"[Gmail]/All Mail"}))
        .await;
    assert!(refused(&listed), "{listed}");
    let id = id_of(ADA, "[Gmail]/All Mail", 31);
    for (tool, args) in [
        ("read_message", json!({"id":id})),
        ("list_attachments", json!({"id":id})),
    ] {
        assert!(refused(&s.call(tool, args).await), "{tool}");
    }
    let thread_id = super::threads::thread_id(ADA, "[Gmail]/All Mail", &message(31, stale));
    let thread = s.call("read_thread", json!({"thread_id":thread_id})).await;
    assert!(!thread.to_string().contains(stale), "{thread}");
    // Nor can a server's answer name one.
    answered_by_server(&s, &[("[Gmail]/All Mail", 31)]);
    let found = s.call("search_server", json!({"query":"lunch"})).await;
    assert_eq!(data(&found)["messages"], json!([]), "{found}");
}

#[tokio::test]
async fn full_access_opens_every_folder_and_still_not_what_no_setting_allows() {
    let mut policy = ada_may(&[]);
    let grant = policy.grant_mut(ADA);
    grant.full = true;
    grant.folders.insert("Private".into(), false);
    let mut s = Session::start(policy, Ok(plain_raw("text"))).await;

    let status = data(&s.call("hylki_status", json!({})).await);
    assert_eq!(
        status["accounts"][0]["allowed"].as_array().unwrap().len(),
        AssistantCap::ALL.len()
    );
    for (folder, subject) in [("Spam", "Prize"), ("Private", "Diagnosis")] {
        let listed = s.call("list_messages", json!({"folder":folder})).await;
        assert_eq!(subjects(&listed), [subject], "{listed}");
    }
    let found = s.call("search", json!({"query":"Diagnosis"})).await;
    assert_eq!(subjects(&found), ["Diagnosis"], "{found}");

    let trash = s
        .call("trash_messages", json!({"ids":[id_of(ADA, "INBOX", 5)]}))
        .await;
    assert_eq!(
        data(&trash)["changed"].as_array().unwrap().len(),
        1,
        "{trash}"
    );

    let sealed = s
        .call("read_message", json!({"id":id_of(ADA, "Vault", 11)}))
        .await;
    assert!(data(&sealed)["body"].is_null(), "{sealed}");
    let names = s.request("tools/list", json!({})).await.to_string();
    for never in ["send_message", "delete_forever", "empty_trash"] {
        assert!(!names.contains(never), "{never}");
    }
    // Another account is not shared by this one's switch.
    let other = s.call("list_messages", json!({"account":BO})).await;
    assert!(refused(&other), "{other}");
}
