//! #554 against a real server: `JMAP_LIVE=url,user,pw cargo test
//! jmap::hierarchy_tests -- --ignored --nocapture`.

use std::sync::{Arc, Mutex};

use super::*;
use crate::config::AccountConfig;
use crate::models::FolderHierarchy;
use crate::worker::jmap::wire::{args, call, jmap_call, jmap_session, CAP_CORE, CAP_MAIL};
use crate::worker::test_support::sample_account;
use crate::worker::WorkerEvent;

/// A mailbox made at the top with a '/' in its name, one made inside it,
/// that one moved out to the top under a new name: each where the paths
/// say, the names as typed.
#[test]
#[ignore]
fn live_mailboxes_nest_move_and_keep_their_names() {
    let Ok(spec) = std::env::var("JMAP_LIVE") else {
        return;
    };
    let mut parts = spec.splitn(3, ',');
    let account = AccountConfig {
        imap_host: parts.next().unwrap_or_default().into(),
        imap_port: 443,
        username: parts.next().unwrap_or_default().into(),
        password: parts.next().unwrap_or_default().into(),
        jmap_token: std::env::var_os("JMAP_LIVE_TOKEN").is_some(),
        ..sample_account()
    };
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = events.clone();
    let emit = move |ev: WorkerEvent| {
        let line = match ev {
            WorkerEvent::Hierarchy(h) => format!("hierarchy {h:?}"),
            WorkerEvent::Error { .. } => format!("{ev:?}"),
            _ => return,
        };
        log.lock().unwrap().push(line);
    };
    let (_tx, rx) = mpsc::unbounded_channel();
    let (push_tx, push_rx) = mpsc::unbounded_channel::<()>();
    let mut w = JmapWorker {
        account_id: 1,
        account: account.clone(),
        rx,
        emit,
        cache: None,
        state: JmapState::new(&account),
        last_auto_empty: None,
        last_list_clean: None,
        push_tx,
        push_rx,
        push_enabled: false,
        push_started: false,
        offline: Default::default(),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let s = jmap_session(&w.account, &mut w.state, &w.emit)
            .await
            .expect("session");
        eprintln!(
            "session: top_level {} max_depth {:?}",
            s.top_level, s.max_depth
        );
        let h = FolderHierarchy::slashed(s.top_level);
        let pid = std::process::id();
        let top = h
            .child("", &h.leaf_for(&format!("h554 a/b {pid}")).unwrap())
            .unwrap();
        w.on_create_folder(top.clone()).await;
        let child = h.child(&top, &h.leaf_for("Café").unwrap()).unwrap();
        w.on_create_folder(child.clone()).await;
        assert!(w.state.folders.contains_key(&top), "{top:?} listed");
        assert!(w.state.folders.contains_key(&child), "{child:?} listed");

        let name_of = |id: &str, s: &crate::worker::jmap::wire::JmapSession| {
            let r = jmap_call(
                s,
                &[CAP_CORE, CAP_MAIL],
                vec![call(
                    0,
                    "Mailbox/get",
                    serde_json::json!({ "accountId": s.account, "ids": [id] }),
                )],
            )
            .unwrap();
            let m = &args(&r, 0)["list"][0];
            (
                m["name"].as_str().unwrap_or_default().to_string(),
                m["parentId"].as_str().map(String::from),
            )
        };
        let top_id = w.state.folders[&top].1.clone();
        let child_id = w.state.folders[&child].1.clone();
        assert_eq!(name_of(&top_id, &s), (format!("h554 a/b {pid}"), None));
        assert_eq!(
            name_of(&child_id, &s),
            ("Café".into(), Some(top_id.clone()))
        );

        // Out to the top under a new name: one Mailbox/set.
        let moved = h
            .child("", &h.leaf_for(&format!("h554 moved {pid}")).unwrap())
            .unwrap();
        w.on_rename_folder(child.clone(), moved.clone()).await;
        assert!(w.state.folders.contains_key(&moved), "{moved:?} listed");
        assert!(!w.state.folders.contains_key(&child));
        assert_eq!(
            name_of(&w.state.folders[&moved].1, &s),
            (format!("h554 moved {pid}"), None)
        );
        // And back inside, keeping its name.
        let back = h.child(&top, h.leaf(&moved)).unwrap();
        w.on_rename_folder(moved.clone(), back.clone()).await;
        assert_eq!(
            name_of(&w.state.folders[&back].1, &s).1,
            Some(top_id.clone())
        );
        // Inside a mailbox that is gone: refused, nothing made at the top.
        w.on_create_folder(format!("no such {pid}/x")).await;
        assert!(!w.state.folders.contains_key(&format!("no such {pid}/x")));
        assert!(!w
            .state
            .folders
            .keys()
            .any(|p| p.contains(&format!("no such {pid}"))));

        for path in [back, top] {
            let id = w.state.folders[&path].1.clone();
            jmap_call(
                &s,
                &[CAP_CORE, CAP_MAIL],
                vec![call(
                    0,
                    "Mailbox/set",
                    serde_json::json!({ "accountId": s.account, "destroy": [id] }),
                )],
            )
            .unwrap();
        }
        eprintln!("events: {:#?}", events.lock().unwrap());
    });
}
