//! The Tuta worker's handlers against a fake helper: folder changes,
//! erasing for good, undo and snooze by stable id, tags as labels, drafts
//! and templates, what pushed changes sync, and mail imported from another
//! account.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::index::TutaState;
use super::messages::tuta_message;
use super::push::{signal_of, PushSignal};
use super::tests::{account, fake, folder_id, folder_rows, header, ops, state_with, Asked};
use super::TutaWorker;
use crate::cache::Cache;
use crate::models::{tuta_key, DraftOrigin, FolderKind};
use crate::worker::helper::Transport;
use crate::worker::ids::hash_uid;
use crate::worker::offline::Offline;
use crate::worker::test_support::sample_outgoing;
use crate::worker::WorkerEvent;

type Events = Rc<RefCell<Vec<WorkerEvent>>>;

/// A worker on account 1 whose folders came from [`folder_rows`], with an
/// in-memory cache holding them, talking to `transport`.
fn worker(transport: Transport) -> (TutaWorker<impl Fn(WorkerEvent)>, Events) {
    let events: Events = Rc::default();
    let log = events.clone();
    let state: TutaState = state_with(transport);
    let cache = Cache::in_memory().unwrap();
    let mut folders: Vec<(String, u32)> = state
        .folders
        .iter()
        .map(|(path, (id, _))| (path.clone(), *id))
        .collect();
    folders.sort_by_key(|(_, id)| *id);
    let folders: Vec<crate::models::Folder> = folders
        .into_iter()
        .map(|(path, id)| crate::models::Folder {
            id,
            account_id: 1,
            name: path.clone(),
            kind: state.kinds[&path],
            path,
            unread: 0,
            childless: false,
        })
        .collect();
    cache.save_folders(1, &folders);
    let (_, rx) = mpsc::unbounded_channel();
    let (_, push_rx) = mpsc::unbounded_channel();
    let worker = TutaWorker {
        account_id: 1,
        account: account(),
        rx,
        emit: move |e| log.borrow_mut().push(e),
        cache: Some(cache),
        state,
        offline: Offline::default(),
        push_rx,
        push_tx: None,
        push_live: false,
        polled: Instant::now(),
        open: None,
    };
    (worker, events)
}

/// A sync reply for `request`'s folder with `extra` folders beside
/// [`folder_rows`], listing `messages` and the id snapshot `all_ids`.
fn sync_reply(request: &Value, extra: &[Value], messages: Value, all_ids: Value) -> Value {
    let mut folders = folder_rows().as_array().unwrap().clone();
    folders.extend(extra.iter().cloned());
    let folder = request["folder"].as_str().unwrap_or("f/inbox");
    json!({"folders": folders, "folder": folder, "messages": messages, "all_ids": all_ids,
        "labels": [{"id": "t/1", "name": "Work", "color": "#2ec27e"}]})
}

fn errors(events: &Events) -> Vec<String> {
    events
        .borrow()
        .iter()
        .filter_map(|e| match e {
            WorkerEvent::Error { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn request(asked: &Asked, op: &str) -> Value {
    asked
        .lock()
        .unwrap()
        .iter()
        .find(|r| r["op"] == op)
        .cloned()
        .unwrap_or_else(|| panic!("no {op} request"))
}

/// A row of `path` cached for the message `id`: its uid.
fn cache_row(w: &TutaWorker<impl Fn(WorkerEvent)>, path: &str, id: &str) -> u32 {
    let folder = folder_id(&w.state, path);
    let (m, _) = tuta_message(&header(id, 1_000_000, false), 1, folder).unwrap();
    w.cache
        .as_ref()
        .unwrap()
        .upsert_messages(1, path, std::slice::from_ref(&m));
    m.uid
}

#[tokio::test]
async fn a_folder_is_made_under_its_parent_and_the_tree_synced() {
    let (transport, asked) = fake(|op, request| match op {
        "create_folder" => Ok(json!({"id": "f/new"})),
        "sync" => Ok(sync_reply(
            request,
            &[json!({"id": "f/new", "role": "other", "label": "Q1/Q2", "parent": "f/work"})],
            json!([]),
            json!([]),
        )),
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    // A '/' in the name is its look-alike in the path.
    w.on_create_folder("Work/Q1\u{2215}Q2".into()).await;
    assert_eq!(ops(&asked), ["create_folder", "sync"]);
    let made = request(&asked, "create_folder");
    assert_eq!(made["name"], "Q1/Q2");
    assert_eq!(made["parent"], "f/work");
    assert!(w.state.folders.contains_key("Work/Q1\u{2215}Q2"));
    assert!(errors(&events).is_empty());
    assert!(events
        .borrow()
        .iter()
        .any(|e| matches!(e, WorkerEvent::Folders(f) if f.iter().any(|f| f.name == "Q1\u{2215}Q2"))));
}

#[tokio::test]
async fn a_folder_the_server_refuses_is_said_and_the_list_put_back() {
    let (transport, asked) = fake(|op, _| match op {
        "create_folder" => Err("folder_exists"),
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    w.on_create_folder("Work".into()).await;
    assert_eq!(ops(&asked), ["create_folder"]);
    let said = errors(&events);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("already a folder"), "{said:?}");
    assert!(matches!(events.borrow().last(), Some(WorkerEvent::Folders(_))));
    // A folder under one that is gone is not asked for at all.
    w.on_create_folder("Gone/Child".into()).await;
    assert_eq!(ops(&asked), ["create_folder"]);
    assert_eq!(errors(&events).len(), 2);
}

#[tokio::test]
async fn a_folder_is_renamed_moved_and_deleted() {
    let (transport, asked) = fake(|op, request| match op {
        "rename_folder" => Ok(json!({})),
        "delete_folder" => Ok(json!({"outcome": "trashed"})),
        "sync" => Ok(sync_reply(request, &[], json!([]), json!([]))),
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    w.on_rename_folder("Work".into(), "Jobs".into()).await;
    let renamed = asked.lock().unwrap()[0].clone();
    assert_eq!(renamed["id"], "f/work");
    assert_eq!(renamed["name"], "Jobs");
    assert!(renamed.get("parent").is_none());
    // Moved under the Inbox, the name kept.
    w.on_rename_folder("Work".into(), "Inbox/Work".into()).await;
    let moved = asked.lock().unwrap()[2].clone();
    assert_eq!(moved["parent"], "f/inbox");
    assert!(moved.get("name").is_none());
    w.on_delete_folder("Work".into()).await;
    assert_eq!(
        ops(&asked),
        ["rename_folder", "sync", "rename_folder", "sync", "delete_folder", "sync"]
    );
    assert_eq!(request(&asked, "delete_folder")["id"], "f/work");
    assert!(errors(&events).is_empty());
}

#[tokio::test]
async fn mail_is_erased_from_the_trash_and_by_way_of_it() {
    let (transport, asked) = fake(|op, request| match op {
        "delete" => {
            assert_eq!(request["folder"], "f/trash");
            let rows: Vec<Value> = request["ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| json!({"id": id, "code": null}))
                .collect();
            Ok(json!({"outcomes": rows}))
        }
        "move" => Ok(json!({"outcomes": [{"id": "w/1", "code": null, "new_id": "w/1"}]})),
        "sync" => Ok(sync_reply(
            request,
            &[],
            json!([header("w/1", 1_000_000, false)]),
            json!(["w/1"]),
        )),
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    let in_trash = cache_row(&w, "Trash", "t/1");
    w.state.uids.insert(in_trash, "t/1".into());
    w.on_purge_messages("Trash".into(), vec![in_trash]).await;
    assert_eq!(ops(&asked), ["delete"]);
    let trash = folder_id(&w.state, "Trash");
    assert!(w
        .cache
        .as_ref()
        .unwrap()
        .load_messages(1, "Trash", trash)
        .is_empty());
    // Outside the Trash: moved there, then erased under the id it has.
    let in_work = cache_row(&w, "Work", "w/1");
    w.state.uids.insert(in_work, "w/1".into());
    w.on_purge_messages("Work".into(), vec![in_work]).await;
    assert_eq!(ops(&asked), ["delete", "move", "sync", "delete"]);
    assert_eq!(asked.lock().unwrap()[3]["ids"], json!(["w/1"]));
    assert!(errors(&events).is_empty());
    assert!(matches!(
        events.borrow().last(),
        Some(WorkerEvent::BulkComplete)
    ));
}

#[tokio::test]
async fn a_refused_erase_is_said_and_the_folder_listed_again() {
    let (transport, asked) = fake(|op, request| match op {
        "delete" => Ok(json!({"outcomes": [{"id": "t/1", "code": "delete_failed"}]})),
        "sync" => Ok(sync_reply(request, &[], json!([]), json!([]))),
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    let uid = cache_row(&w, "Trash", "t/1");
    w.state.uids.insert(uid, "t/1".into());
    w.on_purge_messages("Trash".into(), vec![uid]).await;
    assert_eq!(ops(&asked), ["delete", "sync"]);
    assert_eq!(errors(&events).len(), 1);
}

#[tokio::test]
async fn only_the_trash_or_spam_is_emptied() {
    let (transport, asked) = fake(|op, request| match op {
        "empty_folder" => {
            assert_eq!(request["id"], "f/spam");
            Ok(json!({"deleted": 3}))
        }
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    cache_row(&w, "Spam", "s/1");
    let spam = folder_id(&w.state, "Spam");
    w.on_empty_folder(spam, "Spam".into()).await;
    assert_eq!(ops(&asked), ["empty_folder"]);
    assert!(w
        .cache
        .as_ref()
        .unwrap()
        .load_messages(1, "Spam", spam)
        .is_empty());
    assert!(events.borrow().iter().any(
        |e| matches!(e, WorkerEvent::Messages { folder_id, messages } if *folder_id == spam && messages.is_empty())
    ));
    // Anywhere else nothing is asked; the server would refuse it.
    let (transport, asked) = fake(|op, request| match op {
        "sync" => Ok(sync_reply(request, &[], json!([]), json!([]))),
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    let work = folder_id(&w.state, "Work");
    w.on_empty_folder(work, "Work".into()).await;
    assert_eq!(ops(&asked), ["sync"]);
    assert_eq!(errors(&events).len(), 1);
}

#[tokio::test]
async fn a_draft_is_erased_as_a_draft() {
    let (transport, asked) = fake(|op, request| match op {
        "delete_draft" => {
            assert_eq!(request["id"], "d/1");
            Ok(json!({}))
        }
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    let uid = cache_row(&w, "Drafts", "d/1");
    w.state.uids.insert(uid, "d/1".into());
    w.on_purge_messages("Drafts".into(), vec![uid]).await;
    assert_eq!(ops(&asked), ["delete_draft"]);
    assert!(errors(&events).is_empty());
}

#[tokio::test]
async fn an_undo_finds_moved_mail_by_its_id_and_message_id() {
    // l/1 was read (its Message-ID known), l/2 never was; l/3 has gone
    // from Work since the move.
    let (transport, asked) = fake(|op, request| match op {
        "sync" if request["limit"] == 1 => Ok(sync_reply(
            request,
            &[],
            json!([]),
            json!(["l/1", "l/2"]),
        )),
        "sync" => Ok(sync_reply(request, &[], json!([]), json!([]))),
        "move" => {
            let rows: Vec<Value> = request["ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| json!({"id": id, "code": null, "new_id": id}))
                .collect();
            Ok(json!({"outcomes": rows}))
        }
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    let read = cache_row(&w, "Work", "l/1");
    w.cache
        .as_ref()
        .unwrap()
        .set_threading(1, "Work", read, "read@example.com", "", "");
    let inbox = folder_id(&w.state, "Inbox");
    let keys = vec![
        "read@example.com".to_string(),
        tuta_key(hash_uid("l/2")),
        tuta_key(hash_uid("l/3")),
    ];
    w.on_undo_move("Work".into(), "Inbox".into(), inbox, keys)
        .await;
    assert_eq!(ops(&asked), ["sync", "move", "sync"]);
    let moved = request(&asked, "move");
    assert_eq!(moved["ids"], json!(["l/1", "l/2"]));
    assert_eq!(moved["destination"], "f/inbox");
    assert!(errors(&events).is_empty());
    assert!(matches!(
        events.borrow().last(),
        Some(WorkerEvent::BulkComplete)
    ));
}

#[tokio::test]
async fn snoozed_mail_waits_in_a_folder_made_for_it_and_comes_back_unread() {
    let snoozed = json!({"id": "f/snoozed", "role": "other", "label": "Snoozed", "parent": ""});
    let made = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = made.clone();
    let (transport, asked) = fake(move |op, request| {
        let extra = if seen.load(std::sync::atomic::Ordering::SeqCst) {
            vec![snoozed.clone()]
        } else {
            Vec::new()
        };
        match op {
            "create_folder" => {
                seen.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(json!({"id": "f/snoozed"}))
            }
            "sync" if request["limit"] == 1 => {
                Ok(sync_reply(request, &extra, json!([]), json!(["l/1"])))
            }
            "sync" => Ok(sync_reply(
                request,
                &extra,
                json!([header("l/1", 1_000_000, false)]),
                json!(["l/1"]),
            )),
            "move" => Ok(json!({"outcomes": [{"id": "l/1", "code": null, "new_id": "l/1"}]})),
            "mark_read" => Ok(json!({"outcomes": [{"id": "l/1", "code": null}]})),
            _ => Err("unsupported_operation"),
        }
    });
    let (mut w, events) = worker(transport);
    let uid = cache_row(&w, "Inbox", "l/1");
    w.state.uids.insert(uid, "l/1".into());
    // The first snooze makes the folder, as an IMAP move would.
    w.move_to("Inbox".into(), vec![uid], "Snoozed".into(), |e| e.to_string())
        .await;
    assert_eq!(ops(&asked), ["create_folder", "sync", "move", "sync"]);
    assert_eq!(request(&asked, "create_folder")["name"], "Snoozed");
    assert_eq!(w.state.kinds["Snoozed"], FolderKind::Snoozed);
    assert!(events
        .borrow()
        .iter()
        .any(|e| matches!(e, WorkerEvent::SnoozedCount { path, count: 1 } if path == "Snoozed")));
    // Its time comes: found by its key, unread again, and back.
    let inbox = folder_id(&w.state, "Inbox");
    w.on_unsnooze("Snoozed".into(), "Inbox".into(), inbox, vec![tuta_key(uid)])
        .await;
    assert_eq!(
        ops(&asked)[4..],
        ["sync", "mark_read", "move", "sync"]
    );
    assert_eq!(request(&asked, "mark_read")["is_read"], false);
    assert!(matches!(
        events.borrow().last(),
        Some(WorkerEvent::Unsnoozed { done: true, message_ids }) if message_ids == &[tuta_key(uid)]
    ));
}

#[tokio::test]
async fn unsnoozing_mail_no_longer_there_is_done_and_a_failure_is_retried() {
    let (transport, _) = fake(|op, request| match op {
        "sync" => Ok(sync_reply(request, &[], json!([]), json!([]))),
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    w.on_unsnooze("Work".into(), "Inbox".into(), 1, vec![tuta_key(7)])
        .await;
    assert!(matches!(
        events.borrow().last(),
        Some(WorkerEvent::Unsnoozed { done: true, .. })
    ));
    let (transport, _) = fake(|_, _| Err("transport_failed"));
    let (mut w, events) = worker(transport);
    w.on_unsnooze("Work".into(), "Inbox".into(), 1, vec![tuta_key(7)])
        .await;
    assert!(matches!(
        events.borrow().last(),
        Some(WorkerEvent::Unsnoozed { done: false, .. })
    ));
}

#[tokio::test]
async fn a_tag_is_a_label_made_on_first_use_and_taken_off_by_name() {
    let (transport, asked) = fake(|op, request| match op {
        "create_label" => Ok(json!({"id": "t/9"})),
        "apply_labels" => Ok(json!({"outcomes": [{"id": request["ids"][0], "code": null}]})),
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    let uid = cache_row(&w, "Inbox", "l/1");
    w.state.uids.insert(uid, "l/1".into());
    let keyword = "hylki-test-tag-label";
    w.on_set_keyword("Inbox".into(), uid, String::new(), keyword.into(), true)
        .await;
    assert_eq!(ops(&asked), ["create_label", "apply_labels"]);
    let made = request(&asked, "create_label");
    assert_eq!(made["name"], keyword);
    assert!(made["color"].as_str().unwrap().starts_with('#'));
    let applied = request(&asked, "apply_labels");
    assert_eq!(applied["ids"], json!(["l/1"]));
    assert_eq!(applied["add"], json!(["t/9"]));
    assert_eq!(applied["remove"], json!([]));
    let inbox = folder_id(&w.state, "Inbox");
    let row = |w: &TutaWorker<_>| {
        w.cache
            .as_ref()
            .unwrap()
            .load_messages(1, "Inbox", inbox)
            .into_iter()
            .find(|m| m.uid == uid)
            .unwrap()
    };
    assert_eq!(row(&w).labels, [keyword]);
    // The label made is used again rather than made twice.
    w.on_set_keyword("Inbox".into(), uid, String::new(), keyword.into(), true)
        .await;
    assert_eq!(ops(&asked), ["create_label", "apply_labels", "apply_labels"]);
    w.on_set_keyword("Inbox".into(), uid, String::new(), keyword.into(), false)
        .await;
    assert_eq!(asked.lock().unwrap()[3]["remove"], json!(["t/9"]));
    assert!(row(&w).labels.is_empty());
    assert!(errors(&events).is_empty());
    let settled = events
        .borrow()
        .iter()
        .filter(|e| matches!(e, WorkerEvent::TagSettled { .. }))
        .count();
    assert_eq!(settled, 3);
    // The labels are the tags the account has.
    w.on_find_keywords();
    let found = events
        .borrow()
        .iter()
        .find_map(|e| match e {
            WorkerEvent::KeywordsFound(found) => Some(found.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].keyword, keyword);
}

#[test]
fn a_tag_goes_by_its_name_and_comes_off_by_name_or_keyword() {
    use super::labels::{keyword_of, label_for, standing_for};
    use super::wire::LabelRow;
    let tags = [crate::config::Tag {
        name: "To Do".into(),
        keyword: "To_Do".into(),
        color: "#e66100".into(),
    }];
    assert_eq!(
        label_for("to_do", &tags),
        ("To Do".to_string(), "#e66100".to_string())
    );
    let (name, color) = label_for("other", &tags);
    assert_eq!(name, "other");
    assert!(color.starts_with('#') && color.len() == 7);
    let label = |id: &str, name: &str| LabelRow {
        id: id.into(),
        name: name.into(),
        color: String::new(),
    };
    let labels = [label("1", "to do"), label("2", "To_Do"), label("3", "Else")];
    let standing: Vec<&str> = standing_for(labels.iter(), "To Do", "To_Do")
        .iter()
        .map(|l| l.id.as_str())
        .collect();
    assert_eq!(standing, ["1", "2"]);
    assert_eq!(keyword_of(&labels[0], &tags), "To_Do");
    assert_eq!(keyword_of(&labels[2], &tags), "Else");
}

#[tokio::test]
async fn a_draft_is_saved_on_the_server_and_found_again_by_its_message_id() {
    let (transport, asked) = fake(|op, request| match op {
        "save_draft" => Ok(json!({"id": "d/1"})),
        "sync" => Ok(sync_reply(
            request,
            &[],
            json!([header("d/1", 1_000_000, false)]),
            json!(["d/1"]),
        )),
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    let message = crate::worker::OutgoingMessage {
        to: "ann@example.com".into(),
        bcc: "hidden@example.com".into(),
        ..sample_outgoing()
    };
    w.on_save_draft(Box::new(message.clone()), "Drafts".into(), Some(7), false)
        .await;
    assert_eq!(ops(&asked), ["save_draft", "sync"]);
    let saved = request(&asked, "save_draft");
    assert!(saved.get("draft_id").is_none());
    assert!(saved.get("delivery").is_none());
    assert_eq!(saved["confidential"], false);
    assert_eq!(saved["bcc"][0]["address"], "hidden@example.com");
    let message_id = events
        .borrow()
        .iter()
        .find_map(|e| match e {
            WorkerEvent::DraftSaved {
                autosave: Some(7),
                message_id: Some(id),
            } => Some(id.clone()),
            _ => None,
        })
        .expect("draft saved");
    let drafts = folder_id(&w.state, "Drafts");
    let rows = w
        .cache
        .as_ref()
        .unwrap()
        .load_messages(1, "Drafts", drafts);
    assert_eq!(rows[0].message_id, message_id);
    // The next save updates that draft in place.
    let again = crate::worker::OutgoingMessage {
        draft_origin: Some(DraftOrigin {
            account_id: 1,
            folder_id: drafts,
            path: "Drafts".into(),
            uid: hash_uid("d/1"),
        }),
        ..message
    };
    w.on_save_draft(Box::new(again.clone()), "Drafts".into(), Some(7), false)
        .await;
    assert_eq!(asked.lock().unwrap()[2]["draft_id"], "d/1");
    // Sent, the draft goes.
    let (transport, sent) = fake(|op, request| match op {
        "send" => Ok(json!({"message_id": "x", "confidential": false, "recipients": []})),
        "delete_draft" => Ok(json!({})),
        "sync" => Ok(sync_reply(request, &[], json!([]), json!([]))),
        _ => Err("unsupported_operation"),
    });
    w.state.transport = transport;
    w.on_send(Box::new(again), 3).await;
    assert_eq!(ops(&sent)[..3], ["send", "delete_draft", "sync"]);
    assert_eq!(request(&sent, "delete_draft")["id"], "d/1");
    assert!(events
        .borrow()
        .iter()
        .any(|e| matches!(e, WorkerEvent::SendDone { ticket: 3, sent: true })));
}

#[tokio::test]
async fn a_reply_to_tuta_mail_names_the_message_id_its_listing_gave() {
    // Mail between Tuta accounts: no header block, only the id Tuta keeps.
    let (transport, asked) = fake(|op, request| match op {
        "sync" => {
            let mut listed = header("l/1", 1_000_000, false);
            listed["message_id"] = json!("Abc.1+X@Tuta.com");
            Ok(sync_reply(request, &[], json!([listed]), json!(["l/1"])))
        }
        "send" => Ok(json!({"message_id": "y", "recipients": [], "parent_marked": true})),
        _ => Err("unsupported_operation"),
    });
    let (mut w, _) = worker(transport);
    let inbox = folder_id(&w.state, "Inbox");
    w.on_load_messages(inbox, "Inbox".into(), true).await;
    let rows = w.cache.as_ref().unwrap().load_messages(1, "Inbox", inbox);
    let row = rows
        .iter()
        .find(|m| m.uid == hash_uid("l/1"))
        .expect("listed");
    assert_eq!(row.message_id, "abc.1+x@tuta.com");
    // A reply as the composer fills it from the row: the chain ends with
    // the row's own id.
    let reply = crate::worker::OutgoingMessage {
        to: row.from_addr.clone(),
        subject: "Re: Hello".into(),
        in_reply_to: row.message_id.clone(),
        references: format!("{} {}", row.references, row.message_id),
        ..sample_outgoing()
    };
    w.on_send(Box::new(reply), 1).await;
    let sent = request(&asked, "send");
    // Spelt as Tuta spells it, which is what the helper finds the parent by.
    assert_eq!(sent["in_reply_to"], "Abc.1+X@Tuta.com");
    assert_eq!(sent["references"], "Abc.1+X@Tuta.com");
    assert_eq!(sent["action"], "reply");
}

#[tokio::test]
async fn a_reply_draft_and_a_forward_name_what_they_answer() {
    let (transport, asked) = fake(|op, request| match op {
        "save_draft" => Ok(json!({"id": "d/1"})),
        "send" => Ok(json!({"message_id": "x", "recipients": [], "parent_marked": true})),
        "sync" => Ok(sync_reply(request, &[], json!([]), json!([]))),
        _ => Err("unsupported_operation"),
    });
    let (mut w, _) = worker(transport);
    // A reply-all saved as a draft: the composer's threading ids, read back
    // from the built draft.
    let reply = crate::worker::OutgoingMessage {
        to: "ann@example.com".into(),
        cc: "bob@example.com".into(),
        in_reply_to: "parent@example.com".into(),
        references: "root@example.com parent@example.com".into(),
        ..sample_outgoing()
    };
    w.on_save_draft(Box::new(reply), "Drafts".into(), None, false)
        .await;
    let saved = request(&asked, "save_draft");
    assert_eq!(saved["in_reply_to"], "parent@example.com");
    assert_eq!(saved["references"], "root@example.com parent@example.com");
    assert_eq!(saved["action"], "reply_all");
    // A forward sent now names what it forwards.
    let forward = crate::worker::OutgoingMessage {
        to: "ann@example.com".into(),
        forwards: "fwd@example.com".into(),
        ..sample_outgoing()
    };
    w.on_send(Box::new(forward), 1).await;
    let sent = request(&asked, "send");
    assert_eq!(sent["in_reply_to"], "fwd@example.com");
    assert_eq!(sent["action"], "forward");
    assert!(sent.get("references").is_none());
}

#[tokio::test]
async fn a_template_is_a_draft_moved_to_a_templates_folder() {
    let templates = json!({"id": "f/tpl", "role": "other", "label": "Templates", "parent": ""});
    let made = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = made.clone();
    let (transport, asked) = fake(move |op, request| {
        let extra = if seen.load(std::sync::atomic::Ordering::SeqCst) {
            vec![templates.clone()]
        } else {
            Vec::new()
        };
        match op {
            "create_folder" => {
                seen.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(json!({"id": "f/tpl"}))
            }
            "save_draft" => Ok(json!({"id": "d/5"})),
            "move" => Ok(json!({"outcomes": [{"id": "d/5", "code": null, "new_id": "d/5"}]})),
            "sync" => Ok(sync_reply(
                request,
                &extra,
                json!([header("d/5", 1_000_000, false)]),
                json!(["d/5"]),
            )),
            _ => Err("unsupported_operation"),
        }
    });
    let (mut w, events) = worker(transport);
    w.on_save_draft(Box::new(sample_outgoing()), "Templates".into(), None, true)
        .await;
    assert_eq!(
        ops(&asked),
        ["create_folder", "sync", "save_draft", "move", "sync", "sync"]
    );
    assert_eq!(request(&asked, "move")["destination"], "f/tpl");
    assert_eq!(w.state.kinds["Templates"], FolderKind::Templates);
    assert!(errors(&events).is_empty(), "{:?}", errors(&events));
    assert!(events.borrow().iter().any(|e| matches!(
        e,
        WorkerEvent::DraftSaved {
            message_id: Some(_),
            ..
        }
    )));
}

#[test]
fn watch_lines_become_signals() {
    let line = |v: Value| v.as_object().unwrap().clone();
    assert_eq!(
        signal_of(&line(json!({"version": 1, "event": "ready"}))),
        Some(PushSignal::Live)
    );
    assert_eq!(
        signal_of(&line(
            json!({"version": 1, "event": "mail_changed", "folder": "f/work"})
        )),
        Some(PushSignal::Mail("f/work".into()))
    );
    assert_eq!(
        signal_of(&line(json!({"version": 1, "event": "reconnect"}))),
        Some(PushSignal::Mail(String::new()))
    );
    assert_eq!(
        signal_of(&line(json!({"version": 1, "event": "folders_changed"}))),
        Some(PushSignal::Folders)
    );
    assert_eq!(
        signal_of(&line(json!({"version": 1, "event": "newer"}))),
        None
    );
}

#[tokio::test]
async fn a_pushed_change_syncs_the_folder_it_names_and_a_vague_one_everything() {
    let (transport, asked) = fake(|op, request| match op {
        "sync" => Ok(sync_reply(request, &[], json!([]), json!([]))),
        _ => Err("unsupported_operation"),
    });
    let (mut w, _) = worker(transport);
    w.on_pushed(PushSignal::Mail("f/work".into())).await;
    let folders = |asked: &Asked| -> Vec<Value> {
        asked
            .lock()
            .unwrap()
            .iter()
            .map(|r| r["folder"].clone())
            .collect()
    };
    assert_eq!(folders(&asked), [json!("f/work")]);
    // Live again after a drop: the Inbox, the tree and the open folder.
    w.open = Some("Work".into());
    w.on_pushed(PushSignal::Ended).await;
    assert!(!w.push_live);
    asked.lock().unwrap().clear();
    w.on_pushed(PushSignal::Live).await;
    assert!(w.push_live);
    assert_eq!(folders(&asked), [Value::Null, json!("f/work")]);
}

#[test]
fn the_push_thread_reports_what_watch_says_and_stops_with_the_worker() {
    use crate::worker::helper::test_support::streaming_helper;
    let helper = streaming_helper(
        "{\"version\":1,\"event\":\"ready\"}\n{\"version\":1,\"event\":\"mail_changed\",\"folder\":\"f/work\"}\n{\"version\":1,\"error\":{\"code\":\"session_revoked\"}}\n",
        true,
    );
    let (transport, _) = fake(|_, _| Err("unsupported_operation"));
    let client = super::tests::client_for(transport);
    let (tx, mut rx) = mpsc::unbounded_channel();
    super::push::spawn_tuta_push(client, Some(helper.path()), tx);
    let mut got = Vec::new();
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    while got.len() < 3 && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(signal) => got.push(signal),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    assert_eq!(
        got,
        [
            PushSignal::Live,
            PushSignal::Mail("f/work".into()),
            PushSignal::Ended
        ]
    );
    assert_eq!(helper.stdin()["op"], "watch");
    // A revoked sign-in is final: the thread is gone, and says nothing more.
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

/// A message another account hands over (#265).
const IMPORTED: &[u8] = b"From: Ann <ann@example.com>\r\n\
Subject: Hello\r\n\
Message-ID: <moved@example.com>\r\n\
References: <root@example.com>\r\n\
\r\n\
Moved across.\r\n";

#[tokio::test]
async fn mail_from_another_account_is_imported_and_its_folder_listed() {
    use base64::Engine as _;
    let (transport, asked) = fake(|op, request| match op {
        "import" => Ok(json!({"id": "w/9"})),
        "sync" => Ok(sync_reply(
            request,
            &[],
            json!([header("w/9", 1_000_000, true)]),
            json!(["w/9"]),
        )),
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    w.on_import_raw(7, "Work".into(), IMPORTED.to_vec(), false, true)
        .await;
    assert_eq!(ops(&asked), ["import", "sync"]);
    let asked_import = request(&asked, "import");
    assert_eq!(asked_import["folder"], "f/work");
    assert_eq!(
        asked_import["unread"], true,
        "an unread message stays unread"
    );
    let sent = base64::engine::general_purpose::STANDARD
        .decode(asked_import["mime_base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(sent, IMPORTED);
    assert_eq!(request(&asked, "sync")["folder"], "f/work");
    assert!(events.borrow().iter().any(|e| matches!(
        e,
        WorkerEvent::RawImported {
            token: 7,
            result: Ok(())
        }
    )));
    // The new row carries what an undo finds it by, and the star Tuta
    // does not keep, in the list and in the cache alike.
    let uid = hash_uid("w/9");
    let listed = events
        .borrow()
        .iter()
        .find_map(|e| match e {
            WorkerEvent::Messages { messages, .. } => {
                messages.iter().find(|m| m.uid == uid).cloned()
            }
            _ => None,
        })
        .expect("the imported row is listed");
    assert_eq!(listed.message_id, "moved@example.com");
    assert!(listed.references.contains("root@example.com"));
    assert!(listed.starred);
    let folder = folder_id(&w.state, "Work");
    let cached = w
        .cache
        .as_ref()
        .unwrap()
        .load_messages(1, "Work", folder)
        .into_iter()
        .find(|m| m.uid == uid)
        .unwrap();
    assert_eq!(cached.message_id, "moved@example.com");
    assert!(cached.starred);
    assert!(errors(&events).is_empty());
}

#[tokio::test]
async fn an_import_the_server_refuses_is_reported_and_nothing_listed() {
    let (transport, asked) = fake(|op, _| match op {
        "import" => Err("import_refused"),
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    w.on_import_raw(3, "Inbox".into(), IMPORTED.to_vec(), true, false)
        .await;
    assert_eq!(ops(&asked), ["import"]);
    assert_eq!(request(&asked, "import")["unread"], false);
    let said = events
        .borrow()
        .iter()
        .find_map(|e| match e {
            WorkerEvent::RawImported { token: 3, result } => Some(result.clone()),
            _ => None,
        })
        .expect("answered");
    assert!(said.unwrap_err().contains("import mail"));
}

#[tokio::test]
async fn an_import_without_an_id_still_lands_and_the_folder_is_listed() {
    let (transport, asked) = fake(|op, request| match op {
        "import" => Ok(json!({"id": null})),
        "sync" => Ok(sync_reply(request, &[], json!([]), json!([]))),
        _ => Err("unsupported_operation"),
    });
    let (mut w, events) = worker(transport);
    w.on_import_raw(4, "Inbox".into(), IMPORTED.to_vec(), true, false)
        .await;
    assert_eq!(ops(&asked), ["import", "sync"]);
    assert!(events.borrow().iter().any(|e| matches!(
        e,
        WorkerEvent::RawImported {
            token: 4,
            result: Ok(())
        }
    )));
}

#[tokio::test]
async fn an_import_is_refused_here_when_too_large_or_the_folder_is_gone() {
    let (transport, asked) = fake(|_, _| Err("unsupported_operation"));
    let (mut w, events) = worker(transport);
    let huge = vec![b'a'; super::wire::MAX_IMPORT_BYTES + 1];
    w.on_import_raw(5, "Inbox".into(), huge, true, false).await;
    w.on_import_raw(6, "Gone".into(), IMPORTED.to_vec(), true, false)
        .await;
    assert!(ops(&asked).is_empty(), "the helper is not asked");
    let results: Vec<Result<(), String>> = events
        .borrow()
        .iter()
        .filter_map(|e| match e {
            WorkerEvent::RawImported { result, .. } => Some(result.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 2);
    assert!(results[0].as_ref().unwrap_err().contains("too large"));
    assert!(results[1]
        .as_ref()
        .unwrap_err()
        .contains("no longer on the server"));
}
