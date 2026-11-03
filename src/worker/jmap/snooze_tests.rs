//! #527 against a real server: `JMAP_LIVE=url,user,pw cargo test
//! jmap::snooze_tests -- --ignored --nocapture`.

use std::sync::{Arc, Mutex};

use super::*;
use crate::config::AccountConfig;
use crate::worker::jmap::folders::refresh_jmap_folders;
use crate::worker::jmap::messages::jmap_list_messages;
use crate::worker::jmap::ops::{jmap_destroy_all, jmap_import_template};
use crate::worker::jmap::wire::{args, call, jmap_call, jmap_session, CAP_CORE, CAP_MAIL};
use crate::worker::test_support::sample_account;
use crate::worker::WorkerEvent;

/// Read mail snoozed into a Snoozed folder the move makes comes back to
/// the Inbox unread, and a second wake finds nothing to do.
#[test]
#[ignore]
fn live_snoozed_mail_comes_back_unread() {
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
    let pid = std::process::id();
    let snoozed = format!("Snoozed-{pid}");
    let mid = format!("hylki-527-{pid}@example.test");
    let raw = format!(
        "From: Ann <ann@example.test>\r\nTo: me@example.test\r\nSubject: hylki-527-{pid}\r\n\
         Message-ID: <{mid}>\r\nDate: Sun, 25 Oct 2026 08:00:00 +0000\r\n\r\nlater\r\n"
    );
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = events.clone();
    let emit = move |ev: WorkerEvent| {
        let line = match ev {
            WorkerEvent::Unsnoozed { message_ids, done } => {
                format!("unsnoozed {message_ids:?} {done}")
            }
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
        refresh_jmap_folders(&s, 1, None, &mut w.state, &w.emit).await;
        let (inbox, (inbox_id, inbox_mailbox)) = w
            .state
            .folders
            .iter()
            .find(|(p, _)| p.eq_ignore_ascii_case("inbox"))
            .map(|(p, v)| (p.clone(), v.clone()))
            .expect("inbox");
        assert!(!w.state.folders.contains_key(&snoozed), "start without one");
        let email = jmap_import_template(&s, raw.as_bytes(), &inbox_mailbox).expect("import");
        let uid = jmap_list_messages(&s, &inbox_mailbox, 1, inbox_id)
            .expect("list")
            .into_iter()
            .find(|(m, _, _)| m.message_id == mid)
            .map(|(m, id, blob)| {
                w.state.uids.insert(m.uid, (id, blob));
                m.uid
            })
            .expect("listed");

        // Snooze: the move makes the folder.
        w.on_move_messages(inbox.clone(), vec![uid], snoozed.clone())
            .await;
        assert!(w.state.folders.contains_key(&snoozed), "made by the move");

        w.on_unsnooze(snoozed.clone(), inbox.clone(), inbox_id, vec![mid.clone()])
            .await;
        w.on_unsnooze(snoozed.clone(), inbox.clone(), inbox_id, vec![mid.clone()])
            .await;
        let seen = events.lock().unwrap().clone();
        println!("events {seen:?}");
        assert_eq!(
            seen,
            vec![
                format!("unsnoozed [{mid:?}] true"),
                format!("unsnoozed [{mid:?}] true"),
            ]
        );

        let got = jmap_call(
            &s,
            &[CAP_CORE, CAP_MAIL],
            vec![call(
                0,
                "Email/get",
                serde_json::json!({
                    "accountId": s.account,
                    "ids": [email],
                    "properties": ["keywords", "mailboxIds"],
                }),
            )],
        )
        .expect("get");
        let found = args(&got, 0)["list"][0].clone();
        println!("back {found}");
        assert_eq!(found["mailboxIds"][&inbox_mailbox], serde_json::json!(true));
        assert!(found["keywords"].get("$seen").is_none(), "unread");

        jmap_destroy_all(&s, &[email]).expect("drop message");
        let (_, made) = w.state.folders.get(&snoozed).cloned().expect("listed");
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
    });
}
