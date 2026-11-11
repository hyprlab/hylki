//! Tests of conversation lookups, counts and the label-copy rules.

use rusqlite::params;

use super::test_util::*;
use super::*;
use crate::models::{Attachment, FolderKind};

/// Insert a message carrying threading headers.
/// Like [`add_threaded`], but with a sender — for the checks that a shared
/// Message-ID alone is not enough to call two rows the same mail.
fn add_from(c: &Cache, folder: &str, uid: u32, ts: i64, msgid: &str, from: &str) {
    c.conn
        .execute(
            "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, message_id, references_) \
             VALUES (1, ?1, ?2, 'X', ?5, 'S', '', ?3, 0, 0, 0, ?4, '')",
            params![folder, uid, ts, msgid, from],
        )
        .unwrap();
}

/// Just the sizes, for the tests that predate the newest-message half of
/// the answer.
fn counts(c: &Cache, groups: &[(String, Vec<String>)]) -> Vec<(String, usize)> {
    c.thread_summaries(1, groups)
        .into_iter()
        .map(|(tag, s)| (tag, s.count))
        .collect()
}

/// As [`add_threaded`], with the sender and preview a row would show.
// One argument per column the INSERT fills.
#[allow(clippy::too_many_arguments)]
fn add_shown(
    c: &Cache,
    folder: &str,
    uid: u32,
    ts: i64,
    msgid: &str,
    refs: &str,
    from: &str,
    preview: &str,
) {
    c.conn
        .execute(
            "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, message_id, references_, preview) \
             VALUES (1, ?1, ?2, ?6, ?6, 'S', '', ?3, 0, 0, 0, ?4, ?5, ?7)",
            params![folder, uid, ts, msgid, refs, from, preview],
        )
        .unwrap();
}

fn add_threaded(c: &Cache, folder: &str, uid: u32, ts: i64, msgid: &str, refs: &str) {
    c.conn
        .execute(
            "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, message_id, references_) \
             VALUES (1, ?1, ?2, 'X', '', 'S', '', ?3, 0, 0, 0, ?4, ?5)",
            params![folder, uid, ts, msgid, refs],
        )
        .unwrap();
}

/// A conversation lookup must return the messages that actually reference the
/// one being opened — nothing else.
///
/// The bind indices for the `references_` comparisons were off by a slot, so
/// the last one ran against the *account id*. Coerced to text, `instr` then
/// matched every message whose References merely contained that digit, and
/// one click dragged thousands of messages into the reader — each of them a
/// body to fetch and re-render. That is worth a test.
/// The repair asks the server only about replies. A message that opens a
/// conversation has no References and never will, so fetching its header
/// buys nothing — and on a real cache that distinction is the difference
/// between 68k messages and 4k.
#[test]
fn only_replies_are_worth_repairing() {
    let c = Cache::in_memory().unwrap();
    // A reply whose References is the single id an ENVELOPE gives.
    c.conn.execute(
        "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, message_id, references_) \
         VALUES (1,'INBOX',10,'X','','Re: hello','',1,0,0,0,'a@x','parent@x')",
        [],
    ).unwrap();
    // A thread opener: no prefix, no references. Nothing to find.
    c.conn.execute(
        "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, message_id, references_) \
         VALUES (1,'INBOX',11,'X','','hello','',1,0,0,0,'b@x','')",
        [],
    ).unwrap();
    // Already complete: two ids, from a full header parse.
    c.conn.execute(
        "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, message_id, references_) \
         VALUES (1,'INBOX',12,'X','','Re: hello','',1,0,0,0,'c@x','root@x parent@x')",
        [],
    ).unwrap();

    let want = c.uids_needing_references(1, "INBOX", u32::MAX, 100);
    let uids: Vec<u32> = want.iter().map(|(u, _)| *u).collect();
    assert_eq!(uids, vec![10], "only the thin reply");
}

/// The watermark walks strictly down, so a message with nothing to find is
/// asked about once rather than on every pass forever.
#[test]
fn the_repair_walks_downwards_and_finishes() {
    let c = Cache::in_memory().unwrap();
    for uid in [10u32, 20, 30] {
        c.conn.execute(
            "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, message_id, references_) \
             VALUES (1,'INBOX',?1,'X','','Re: t','',1,0,0,0,'m@x','p@x')",
            params![uid],
        ).unwrap();
    }
    assert_eq!(
        c.refs_repair_state(1, "INBOX"),
        (u32::MAX, false),
        "starts at the top"
    );
    let first = c.uids_needing_references(1, "INBOX", u32::MAX, 2);
    assert_eq!(
        first.iter().map(|(u, _)| *u).collect::<Vec<_>>(),
        vec![30, 20],
        "newest first"
    );

    c.set_refs_repair_state(1, "INBOX", 20, false);
    let next = c.uids_needing_references(1, "INBOX", 20, 2);
    assert_eq!(
        next.iter().map(|(u, _)| *u).collect::<Vec<_>>(),
        vec![10],
        "strictly below"
    );

    c.set_refs_repair_state(1, "INBOX", 0, true);
    assert!(c.refs_repair_state(1, "INBOX").1, "and it finishes");
}

#[test]
fn a_conversation_holds_only_messages_that_reference_it() {
    let c = Cache::in_memory().unwrap();
    add_threaded(&c, "INBOX", 1, 500, "root@x", "");
    add_threaded(&c, "Sent", 2, 600, "reply@x", "root@x");
    // Unrelated, but its References contain the digit "1" — the account id.
    add_threaded(&c, "Archive", 3, 550, "other@x", "31337@elsewhere");

    let found = c.messages_by_thread_ids(1, &["root@x".to_string()]);
    let ids: Vec<&str> = found.iter().map(|(_, m)| m.message_id.as_str()).collect();
    assert_eq!(ids, vec!["reply@x", "root@x"], "only the real conversation");
}

/// The badge on a thread row said how much of the conversation this folder
/// holds, which for anything you have answered is the wrong number: the
/// replies are in Sent (#222).
#[test]
fn a_conversation_is_counted_across_folders() {
    let c = Cache::in_memory().unwrap();
    add_folder(&c, "INBOX", FolderKind::Inbox);
    add_folder(&c, "Sent", FolderKind::Sent);
    add_threaded(&c, "INBOX", 1, 500, "root@x", "");
    add_threaded(&c, "Sent", 2, 600, "mine@x", "root@x");
    add_threaded(&c, "Sent", 3, 700, "mine2@x", "root@x mine@x");

    let groups = vec![("t".to_string(), vec!["root@x".to_string()])];
    assert_eq!(counts(&c, &groups), vec![("t".to_string(), 3)]);
}

/// Each thread gets its own number, and threads share the one scan.
#[test]
fn batched_counts_stay_with_their_own_thread() {
    let c = Cache::in_memory().unwrap();
    add_folder(&c, "INBOX", FolderKind::Inbox);
    add_folder(&c, "Sent", FolderKind::Sent);
    add_threaded(&c, "INBOX", 1, 500, "a@x", "");
    add_threaded(&c, "Sent", 2, 600, "a-reply@x", "a@x");
    add_threaded(&c, "INBOX", 3, 550, "b@x", "");

    let groups = vec![
        ("a".to_string(), vec!["a@x".to_string()]),
        ("b".to_string(), vec!["b@x".to_string()]),
    ];
    let mut got = counts(&c, &groups);
    got.sort();
    assert_eq!(got, vec![("a".to_string(), 2), ("b".to_string(), 1)]);
}

/// The reader leaves deleted and spam copies out of a conversation, so a
/// badge that counted them would promise cards that never appear.
#[test]
fn trash_and_junk_are_left_out_of_the_count() {
    let c = Cache::in_memory().unwrap();
    add_folder(&c, "INBOX", FolderKind::Inbox);
    add_folder(&c, "Trash", FolderKind::Trash);
    add_folder(&c, "Junk", FolderKind::Junk);
    add_threaded(&c, "INBOX", 1, 500, "root@x", "");
    add_threaded(&c, "Trash", 2, 600, "binned@x", "root@x");
    add_threaded(&c, "Junk", 3, 700, "spam@x", "root@x");

    let groups = vec![("t".to_string(), vec!["root@x".to_string()])];
    assert_eq!(counts(&c, &groups), vec![("t".to_string(), 1)]);
}

/// Gmail files one message under every label it carries, so counting rows
/// would say a two-message conversation has five. It is one mail per
/// Message-ID, which is how the reader merges them too.
#[test]
fn gmail_label_copies_count_once() {
    let c = Cache::in_memory().unwrap();
    add_folder(&c, "INBOX", FolderKind::Inbox);
    add_folder(&c, "[Gmail]/All Mail", FolderKind::Archive);
    add_folder(&c, "Work", FolderKind::Custom);
    add_threaded(&c, "INBOX", 1, 500, "root@x", "");
    add_threaded(&c, "[Gmail]/All Mail", 10, 500, "root@x", "");
    add_threaded(&c, "Work", 20, 500, "root@x", "");
    add_threaded(&c, "[Gmail]/All Mail", 11, 600, "reply@x", "root@x");
    add_threaded(&c, "Work", 21, 600, "reply@x", "root@x");

    let groups = vec![("t".to_string(), vec!["root@x".to_string()])];
    assert_eq!(counts(&c, &groups), vec![("t".to_string(), 2)]);
}

/// Moving mail to Trash on Gmail takes it out of every label, so its
/// copies go from the cache too, but not the one in Trash and not an
/// unrelated message (#257).
#[test]
fn label_copies_go_with_a_move_to_trash() {
    let c = Cache::in_memory().unwrap();
    add_folder(&c, "INBOX", FolderKind::Inbox);
    add_folder(&c, "[Gmail]/All Mail", FolderKind::Archive);
    add_folder(&c, "[Gmail]/Trash", FolderKind::Trash);
    add_threaded(&c, "INBOX", 1, 500, "root@x", "");
    add_threaded(&c, "[Gmail]/All Mail", 10, 500, "root@x", "");
    add_threaded(&c, "[Gmail]/Trash", 30, 500, "root@x", "");
    add_threaded(&c, "INBOX", 2, 600, "other@x", "");
    add_threaded(&c, "[Gmail]/All Mail", 11, 600, "other@x", "");

    assert_eq!(c.drop_label_copies(1, "INBOX", &[1], "[Gmail]/Trash"), 1);
    let left = c.locate_by_message_id("root@x");
    assert_eq!(left.len(), 2);
    assert!(left.iter().any(|(_, p, u)| p == "INBOX" && *u == 1));
    assert!(left
        .iter()
        .any(|(_, p, u)| p == "[Gmail]/Trash" && *u == 30));
    assert_eq!(c.locate_by_message_id("other@x").len(), 2);
}

/// An id that matches nothing gets no entry at all, so the row keeps the
/// count the list worked out for itself rather than being told "0".
#[test]
fn a_thread_the_cache_does_not_know_gets_no_answer() {
    let c = Cache::in_memory().unwrap();
    add_folder(&c, "INBOX", FolderKind::Inbox);
    add_threaded(&c, "INBOX", 1, 500, "root@x", "");

    let groups = vec![("gone".to_string(), vec!["nothing@x".to_string()])];
    assert!(counts(&c, &groups).is_empty());
}

/// The row said the other side spoke last however recently you had
/// answered: the reply is in Sent and the Inbox list cannot see it (#236).
#[test]
fn the_newest_message_of_a_conversation_is_found_in_sent() {
    let c = Cache::in_memory().unwrap();
    add_folder(&c, "INBOX", FolderKind::Inbox);
    add_folder(&c, "Sent", FolderKind::Sent);
    add_shown(
        &c,
        "INBOX",
        1,
        500,
        "root@x",
        "",
        "Ada",
        "Can you take a look?",
    );
    add_shown(
        &c,
        "Sent",
        2,
        600,
        "mine@x",
        "root@x",
        "Me",
        "Looked, all good",
    );

    let groups = vec![("t".to_string(), vec!["root@x".to_string()])];
    let got = c.thread_summaries(1, &groups);
    let latest = got[0].1.latest.as_ref().expect("the reply is the newest");
    assert_eq!(latest.from_name, "Me");
    assert_eq!(latest.preview, "Looked, all good");
    assert_eq!(latest.timestamp, 600);
}

/// A reply still being written is not one the row should speak for, so
/// drafts are counted but never shown.
#[test]
fn a_draft_is_not_the_newest_message() {
    let c = Cache::in_memory().unwrap();
    add_folder(&c, "INBOX", FolderKind::Inbox);
    add_folder(&c, "Drafts", FolderKind::Drafts);
    add_shown(
        &c,
        "INBOX",
        1,
        500,
        "root@x",
        "",
        "Ada",
        "Can you take a look?",
    );
    add_shown(
        &c,
        "Drafts",
        2,
        900,
        "draft@x",
        "root@x",
        "Me",
        "Half an answ",
    );

    let groups = vec![("t".to_string(), vec!["root@x".to_string()])];
    let got = c.thread_summaries(1, &groups);
    let latest = got[0].1.latest.as_ref().expect("the received mail is left");
    assert_eq!(latest.from_name, "Ada");
    assert_eq!(latest.timestamp, 500);
}

/// What the reader leaves out of a conversation, the row must not quote.
#[test]
fn a_binned_reply_is_not_the_newest_message() {
    let c = Cache::in_memory().unwrap();
    add_folder(&c, "INBOX", FolderKind::Inbox);
    add_folder(&c, "Trash", FolderKind::Trash);
    add_shown(
        &c,
        "INBOX",
        1,
        500,
        "root@x",
        "",
        "Ada",
        "Can you take a look?",
    );
    add_shown(
        &c,
        "Trash",
        2,
        900,
        "binned@x",
        "root@x",
        "Ada",
        "Never mind",
    );

    let groups = vec![("t".to_string(), vec!["root@x".to_string()])];
    let got = c.thread_summaries(1, &groups);
    let latest = got[0].1.latest.as_ref().expect("the received mail is left");
    assert_eq!(latest.preview, "Can you take a look?");
}

/// The same off-by-a-slot bind bug `a_conversation_holds_only_messages_that_reference_it`
/// guards, in the batched query this time: an unrelated message whose
/// References merely contain the account id must not be counted.
#[test]
fn counting_does_not_sweep_in_unrelated_mail() {
    let c = Cache::in_memory().unwrap();
    add_folder(&c, "INBOX", FolderKind::Inbox);
    add_folder(&c, "Archive", FolderKind::Archive);
    add_threaded(&c, "INBOX", 1, 500, "root@x", "");
    add_threaded(&c, "Archive", 3, 550, "other@x", "31337@elsewhere");

    let groups = vec![("t".to_string(), vec!["root@x".to_string()])];
    assert_eq!(counts(&c, &groups), vec![("t".to_string(), 1)]);
}

/// Gmail stores one message under every label it carries, so INBOX, All Mail
/// and Important hold three copies with three UIDs. A body read under one
/// label has to answer for the others, or opening the conversation from the
/// Inbox re-downloads mail already in hand — and its attachments come back
/// as a "Load attachments" button rather than the files themselves.
#[test]
fn a_body_cached_under_one_gmail_label_answers_for_the_others() {
    let c = Cache::in_memory().unwrap();
    add_threaded(&c, "INBOX", 42, 500, "same@x", "");
    add_threaded(&c, "[Gmail]/All Mail", 900, 500, "same@x", "");
    c.save_body(1, "[Gmail]/All Mail", 900, "the real body");

    assert_eq!(
        c.load_body(1, "INBOX", 42).as_deref(),
        Some("the real body")
    );
    // The copy that actually holds it still answers directly.
    assert_eq!(
        c.load_body(1, "[Gmail]/All Mail", 900).as_deref(),
        Some("the real body")
    );
}

#[test]
fn attachments_downloaded_under_one_label_answer_for_the_others() {
    let c = Cache::in_memory().unwrap();
    add_threaded(&c, "INBOX", 42, 500, "same@x", "");
    add_threaded(&c, "Paratype", 7, 500, "same@x", "");
    c.save_attachments(
        1,
        "Paratype",
        7,
        &[Attachment {
            name: "spec.pdf".into(),
            data: vec![1, 2, 3],
        }],
    );

    let found = c.load_attachments(1, "INBOX", 42);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "spec.pdf");
}

/// One message under two labels is one download, not two. `load_attachments`
/// already answers for every label from whichever copy was fetched, so
/// fetching the others only stores the same blobs again.
#[test]
fn attachments_fetched_under_one_label_count_as_fetched_for_the_others() {
    let c = Cache::in_memory().unwrap();
    add_threaded(&c, "INBOX", 42, 500, "same@x", "");
    add_threaded(&c, "Work", 7, 500, "same@x", "");
    c.mark_attachments_checked(1, "Work", 7);

    assert!(c.attachments_checked(1, "INBOX", 42));
}

/// The reach stops at the message: an unrelated mail is still unfetched.
#[test]
fn another_message_being_fetched_does_not_count_as_this_one() {
    let c = Cache::in_memory().unwrap();
    add_threaded(&c, "INBOX", 42, 500, "mine@x", "");
    add_threaded(&c, "Work", 7, 500, "someone-else@x", "");
    c.mark_attachments_checked(1, "Work", 7);

    assert!(!c.attachments_checked(1, "INBOX", 42));
}

/// Two different messages must never answer for each other — the fallback
/// keys on Message-ID, and a message that has none has nothing to match.
#[test]
fn a_different_message_never_answers_for_this_one() {
    let c = Cache::in_memory().unwrap();
    add_threaded(&c, "INBOX", 42, 500, "mine@x", "");
    add_threaded(&c, "[Gmail]/All Mail", 900, 500, "someone-else@x", "");
    c.save_body(1, "[Gmail]/All Mail", 900, "not yours");
    assert_eq!(c.load_body(1, "INBOX", 42), None);

    add_threaded(&c, "INBOX", 43, 500, "", "");
    add_threaded(&c, "Archive", 44, 500, "", "");
    c.save_body(1, "Archive", 44, "unrelated");
    assert_eq!(
        c.load_body(1, "INBOX", 43),
        None,
        "an empty Message-ID matches nothing, not everything"
    );
}

/// Message-ID is written by the sender, and spam reuses one across unrelated
/// mail. Serving one message's body for another would be worse than the
/// cache miss the fallback exists to avoid, so sender and timestamp have to
/// agree too.
#[test]
fn a_reused_message_id_from_a_different_sender_answers_for_nothing() {
    let c = Cache::in_memory().unwrap();
    add_from(&c, "INBOX", 42, 500, "dup@x", "me@real.example");
    add_from(&c, "Archive", 900, 500, "dup@x", "spammer@fake.example");
    c.save_body(1, "Archive", 900, "not the same mail");
    assert_eq!(c.load_body(1, "INBOX", 42), None);

    // Same sender, but a different message that happens to reuse the id.
    add_from(&c, "INBOX", 43, 500, "dup2@x", "me@real.example");
    add_from(&c, "Archive", 901, 900, "dup2@x", "me@real.example");
    c.save_body(1, "Archive", 901, "a different day's mail");
    assert_eq!(c.load_body(1, "INBOX", 43), None);
}

/// Re-adding an account re-downloads mail that is all older than the moment
/// it was added. Threading reads what a message answers, not when it was
/// sent, so every member belongs to the conversation regardless of age.
#[test]
fn a_conversation_holds_its_members_however_old_they_are() {
    let c = Cache::in_memory().unwrap();
    add_threaded(&c, "INBOX", 1, 500, "root@x", "");
    add_threaded(&c, "Sent", 2, 400, "old-reply@x", "root@x");
    add_threaded(&c, "Sent", 3, 600, "new-reply@x", "root@x");

    let found = c.messages_by_thread_ids(1, &["root@x".to_string()]);
    let mut ids: Vec<&str> = found.iter().map(|(_, m)| m.message_id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        vec!["new-reply@x", "old-reply@x", "root@x"],
        "all three, any age"
    );
}

/// A page of conversations looked up at once (#296) holds, for each one,
/// what looking it up alone holds: shared members in both, unrelated mail
/// in neither, newest first.
#[test]
fn a_page_of_conversations_matches_each_one_looked_up_alone() {
    let c = Cache::in_memory().unwrap();
    add_threaded(&c, "INBOX", 1, 500, "a@x", "");
    add_threaded(&c, "Sent", 2, 600, "a-reply@x", "a@x");
    add_threaded(&c, "INBOX", 3, 700, "b@x", "");
    add_threaded(&c, "Archive", 4, 800, "b-reply@x", "b@x a@x");
    add_threaded(&c, "Archive", 5, 900, "other@x", "nothing@x");
    let groups = [
        vec!["a@x".to_string()],
        vec!["b@x".to_string(), "b-reply@x".to_string()],
        vec!["unknown@x".to_string()],
        vec![String::new()],
    ];
    let lists: Vec<&[String]> = groups.iter().map(Vec::as_slice).collect();
    let page = c.messages_by_thread_groups(1, &lists);
    assert_eq!(page.len(), groups.len());
    for (ids, found) in groups.iter().zip(&page) {
        let alone: Vec<(String, u32)> = c
            .messages_by_thread_ids(1, ids)
            .into_iter()
            .map(|(p, m)| (p, m.uid))
            .collect();
        let together: Vec<(String, u32)> = found.iter().map(|(p, m)| (p.clone(), m.uid)).collect();
        assert_eq!(together, alone, "{ids:?}");
    }
    let uids = |i: usize| page[i].iter().map(|(_, m)| m.uid).collect::<Vec<_>>();
    assert_eq!(uids(0), [4, 2, 1]);
    assert_eq!(uids(1), [4, 3]);
    assert!(page[2].is_empty() && page[3].is_empty());
}
