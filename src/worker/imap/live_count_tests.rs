//! Counting by the server (#296) against a real one, ignored by default.

use std::time::Duration;

use crate::worker::imap::connect::connect;
use crate::worker::imap::mutate::{move_message, store_flag};
use crate::worker::imap::wire::{search_count, search_uids, sel};
use crate::worker::test_support::sample_account;

/// With `IMAP_LIVE=host,port,user,password`: the count the server gives
/// for unread mail is the length of the UID list it gives for the same
/// search, the session carries on after it, and a flag change and a move
/// on the selection a count left behind still land.
#[test]
#[ignore]
fn live_unseen_count_matches_the_uid_list() {
    let Ok(spec) = std::env::var("IMAP_LIVE") else {
        return;
    };
    let p: Vec<&str> = spec.splitn(4, ',').collect();
    let account = crate::config::AccountConfig {
        email: p[2].into(),
        imap_host: p[0].into(),
        imap_port: p[1].parse().expect("port"),
        username: p[2].into(),
        password: p[3].into(),
        security: Some(crate::config::ServerSecurity {
            imap_starttls: false,
            imap_accept_invalid_certs: true,
            ..Default::default()
        }),
        ..sample_account()
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut sess = connect(&account).await.expect("connect");
        sel(&mut sess, "INBOX").await.expect("select");
        for query in ["UNSEEN", "SEEN", "ALL"] {
            let listed = search_uids(&mut sess, query).await.expect("search").len() as u32;
            let counted = search_count(&mut sess, query).await;
            assert_eq!(counted, Some(listed), "{query}");
        }
        // Nothing matches: the server may leave the count out.
        let none = search_count(&mut sess, "UID 4000000000").await;
        assert_eq!(none, Some(0));
        // The session is still in step: a flag change goes through and
        // shows in the next count.
        let all = search_uids(&mut sess, "ALL").await.expect("all");
        let Some(&uid) = all.iter().max() else {
            return;
        };
        let was_seen = search_uids(&mut sess, format!("UID {uid} SEEN"))
            .await
            .expect("seen")
            .contains(&uid);
        let before = search_count(&mut sess, "UNSEEN").await.expect("count");
        let flipped = |n: u32| if was_seen { n + 1 } else { n - 1 };
        store_flag(&mut sess, "INBOX", uid, "\\Seen", !was_seen)
            .await
            .expect("toggle");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let during = search_count(&mut sess, "UNSEEN").await.expect("count");
        assert_eq!(during, flipped(before));
        store_flag(&mut sess, "INBOX", uid, "\\Seen", was_seen)
            .await
            .expect("toggle back");
        let after = search_count(&mut sess, "UNSEEN").await.expect("count");
        assert_eq!(before, after);
        // A move with MOVE read from the cached capabilities, and back.
        let _ = crate::worker::imap::wire::create_box(&mut sess, "HylkiCount").await;
        move_message(&mut sess, "INBOX", uid, "HylkiCount")
            .await
            .expect("move");
        let moved = sel(&mut sess, "HylkiCount").await.expect("select").exists;
        assert!(moved >= 1);
        let back = search_uids(&mut sess, "ALL").await.expect("all");
        let back = *back.iter().max().expect("moved message");
        move_message(&mut sess, "HylkiCount", back, "INBOX")
            .await
            .expect("move back");
        let _ = sess.logout().await;
    });
}
