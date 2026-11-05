//! What is on screen and being written: the tools that see what the user is
//! doing right now, and what must stay out of their reach.

use super::*;
use crate::mcp::{OpenDraft, OpenNow, OpenThread};

/// Ada's account shared for these alone.
fn ada_for(caps: &[AssistantCap]) -> AssistantPolicy {
    let mut policy = ada_may(&[]);
    let grant = policy.grant_mut(ADA);
    for cap in caps {
        grant.set_switch(*cap, true);
    }
    policy
}

fn composer(id: u32, account_id: u32, by_assistant: bool) -> OpenDraft {
    OpenDraft {
        compose_id: id,
        account_id,
        from: "me@example.org".into(),
        to: "friend@example.org".into(),
        cc: String::new(),
        bcc: String::new(),
        subject: format!("Subject of {id}"),
        kind: "new",
        by_assistant,
        attachments: vec!["plan.pdf".into()],
        encrypted: false,
        format: "plain",
        text: Some(format!("Words of {id}")),
        pending: None,
        last_edit: None,
    }
}

fn thread(selected: (&str, u32), members: &[(&str, u32)]) -> OpenThread {
    let shown = |(p, u): (&str, u32)| (1, p.to_string(), u);
    OpenThread {
        selected: shown(selected),
        members: members.iter().copied().map(shown).collect(),
    }
}

const DRAFTS: &[AssistantCap] = &[AssistantCap::OpenDrafts];
const OWN_TOO: &[AssistantCap] = &[AssistantCap::OpenDrafts, AssistantCap::OwnDrafts];
const OPEN: &[AssistantCap] = &[
    AssistantCap::List,
    AssistantCap::Read,
    AssistantCap::OpenConversation,
];

#[tokio::test]
async fn open_drafts_need_their_own_switch_and_full_access_is_not_it() {
    let mut policy = ada_may(&[]);
    policy.grant_mut(ADA).full = true;
    let mut s = Session::start(policy, Ok(plain_raw("text"))).await;
    s.saw.lock().unwrap().composers = vec![composer(4, 1, true)];
    for (tool, args) in [
        ("list_drafts", json!({})),
        ("read_draft", json!({"id":"composer-4"})),
        ("update_draft", json!({"id":"composer-4","subject":"New"})),
    ] {
        let refused_here = s.call(tool, args).await;
        assert!(refused(&refused_here), "{tool}: {refused_here}");
    }
    let saw = s.saw.lock().unwrap();
    assert_eq!(saw.texts_read, 0);
    assert!(saw.edits.is_empty());
}

#[tokio::test]
async fn the_users_own_drafts_look_like_none_without_their_switch() {
    let mut s = Session::start(ada_for(DRAFTS), Ok(plain_raw("text"))).await;
    s.saw.lock().unwrap().composers = vec![composer(3, 1, false), composer(4, 1, true)];

    let listed = s.call("list_drafts", json!({})).await;
    let drafts = data(&listed);
    let ids: Vec<&str> = drafts
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["id"].as_str())
        .collect();
    assert_eq!(ids, ["composer-4"], "{listed}");
    assert!(!listed.to_string().contains("Subject of 3"), "{listed}");

    let theirs = s.call("read_draft", json!({"id":"composer-3"})).await;
    assert!(refused(&theirs), "{theirs}");
    let edit = s
        .call("update_draft", json!({"id":"composer-3","subject":"New"}))
        .await;
    assert!(refused(&edit), "{edit}");
    // No different from one that is not open at all.
    let absent = s.call("read_draft", json!({"id":"composer-99"})).await;
    assert!(refused(&absent), "{absent}");
    assert_eq!(
        theirs.to_string().replace("composer-3", "X"),
        absent.to_string().replace("composer-99", "X")
    );
    assert_eq!(s.saw.lock().unwrap().texts_read, 0);
    assert!(s.saw.lock().unwrap().edits.is_empty());

    let ours = s.call("read_draft", json!({"id":"composer-4"})).await;
    assert_eq!(data(&ours)["text"], "Words of 4", "{ours}");
}

#[tokio::test]
async fn own_drafts_open_with_their_switch() {
    let mut s = Session::start(ada_for(OWN_TOO), Ok(plain_raw("text"))).await;
    s.saw.lock().unwrap().composers = vec![composer(3, 1, false)];
    let read = s.call("read_draft", json!({"id":"composer-3"})).await;
    assert_eq!(data(&read)["text"], "Words of 3", "{read}");
    let edit = s
        .call(
            "update_draft",
            json!({"id":"composer-3","subject":"New","bcc":["x@example.org"]}),
        )
        .await;
    assert_eq!(data(&edit)["pending"], true, "{edit}");
    let saw = s.saw.lock().unwrap();
    assert_eq!(saw.edits[0].0, 3);
    assert_eq!(saw.edits[0].1.subject.as_deref(), Some("New"));
}

#[tokio::test]
async fn a_composer_goes_by_the_account_it_sends_from() {
    let mut s = Session::start(ada_for(OWN_TOO), Ok(plain_raw("text"))).await;
    // From an account not shared, an assistant's own included.
    s.saw.lock().unwrap().composers = vec![composer(3, 2, false), composer(4, 2, true)];
    let listed = s.call("list_drafts", json!({})).await;
    assert_eq!(data(&listed), json!([]), "{listed}");
    let read = s.call("read_draft", json!({"id":"composer-4"})).await;
    assert!(refused(&read), "{read}");
    let edit = s
        .call("update_draft", json!({"id":"composer-4","subject":"New"}))
        .await;
    assert!(refused(&edit), "{edit}");
    assert!(s.saw.lock().unwrap().edits.is_empty());
}

#[tokio::test]
async fn an_encrypted_composer_says_it_is_there_and_nothing_more() {
    let mut s = Session::start(ada_for(OWN_TOO), Ok(plain_raw("text"))).await;
    s.saw.lock().unwrap().composers = vec![OpenDraft::sealed(5, 1, false)];
    for answer in [
        s.call("list_drafts", json!({})).await,
        s.call("read_draft", json!({"id":"composer-5"})).await,
    ] {
        let shown = answer.to_string();
        assert!(shown.contains("\\\"encrypted\\\":true"), "{shown}");
        assert!(shown.contains("composer-5"), "{shown}");
        for field in ["to", "from", "subject", "attachments", "text"] {
            assert!(!shown.contains(&format!("\\\"{field}\\\":")), "{shown}");
        }
    }
    let edit = s
        .call(
            "update_draft",
            json!({"id":"composer-5","to":["x@example.org"]}),
        )
        .await;
    assert!(refused(&edit), "{edit}");
    assert!(edit.to_string().contains("encrypted"), "{edit}");
    assert!(s.saw.lock().unwrap().edits.is_empty());
}

#[tokio::test]
async fn an_edit_is_checked_before_it_reaches_the_composer() {
    let mut s = Session::start(ada_for(DRAFTS), Ok(plain_raw("text"))).await;
    s.saw.lock().unwrap().composers = vec![composer(4, 1, true)];
    let name = s
        .call("update_draft", json!({"id":"composer-4","to":["Ada"]}))
        .await;
    assert!(refused(&name), "{name}");
    let nothing = s.call("update_draft", json!({"id":"composer-4"})).await;
    assert!(refused(&nothing), "{nothing}");
    let message_id = s
        .call("read_draft", json!({"id":id_of(ADA, "INBOX", 5)}))
        .await;
    assert!(refused(&message_id), "{message_id}");
    assert!(s.saw.lock().unwrap().edits.is_empty());
}

#[tokio::test]
async fn a_prepared_message_says_which_composer_it_is() {
    let mut s = Session::start(ada_may(&[AssistantCap::Draft]), Ok(plain_raw("text"))).await;
    let opened = s
        .call("compose_message", json!({"to":["x@example.org"]}))
        .await;
    assert_eq!(data(&opened)["id"], "composer-7", "{opened}");
}

#[tokio::test]
async fn the_open_conversation_needs_its_own_switch_and_full_access_is_not_it() {
    let mut policy = ada_may(LOOKING);
    policy.grant_mut(ADA).full = true;
    let mut s = Session::start(policy, Ok(plain_raw("text"))).await;
    s.saw.lock().unwrap().on_screen = OpenNow {
        main: Some(thread(("INBOX", 5), &[("INBOX", 5)])),
        ..Default::default()
    };
    let answer = s.call("read_open_thread", json!({})).await;
    assert!(refused(&answer), "{answer}");
    assert!(!answer.to_string().contains("Lunch"), "{answer}");
}

#[tokio::test]
async fn the_open_conversation_reads_as_any_message_does() {
    let mut s = Session::start(ada_for(OPEN), Ok(plain_raw("text"))).await;
    let nothing = s.call("read_open_thread", json!({})).await;
    assert_eq!(data(&nothing)["open"], false, "{nothing}");

    s.saw.lock().unwrap().on_screen = OpenNow {
        main: Some(thread(("INBOX", 5), &[("INBOX", 5), ("INBOX", 6)])),
        ..Default::default()
    };
    let open = s.call("read_open_thread", json!({})).await;
    assert_eq!(data(&open)["open"], true, "{open}");
    assert_eq!(data(&open)["shown_in"], "main_window");
    assert_eq!(data(&open)["selected_id"], id_of(ADA, "INBOX", 5));
    assert_eq!(data(&open)["message_count"], 2);
    assert_eq!(
        data(&open)["messages"][0]["body"],
        "Friday at noon?",
        "{open}"
    );
}

#[tokio::test]
async fn what_the_user_is_on_stands_for_this_message_or_nothing_does() {
    let mut s = Session::start(ada_for(OPEN), Ok(plain_raw("text"))).await;
    // On a message in Junk, in a window of its own, with a shared one behind.
    s.saw.lock().unwrap().on_screen = OpenNow {
        main: Some(thread(("INBOX", 5), &[("INBOX", 5)])),
        windows: vec![thread(("Spam", 7), &[("Spam", 7)])],
        focused: Some(0),
    };
    let answer = s.call("read_open_thread", json!({})).await;
    assert_eq!(data(&answer)["open"], false, "{answer}");
    assert!(data(&answer)["messages"].is_null(), "{answer}");
    assert!(!answer.to_string().contains("Prize"), "{answer}");
    assert_eq!(
        data(&answer)["also_open"][0]["subject"],
        "Lunch",
        "{answer}"
    );

    // Now in the main window, with Junk's still open: it goes unnamed.
    s.saw.lock().unwrap().on_screen.focused = None;
    let answer = s.call("read_open_thread", json!({})).await;
    assert_eq!(data(&answer)["open"], true, "{answer}");
    assert!(data(&answer)["also_open"].is_null(), "{answer}");
    assert!(!answer.to_string().contains("Prize"), "{answer}");
}

#[tokio::test]
async fn nothing_out_of_reach_is_named_in_an_open_conversation() {
    let mut s = Session::start(ada_for(OPEN), Ok(plain_raw("text"))).await;
    // A member kept back, and another account's conversation beside it.
    let mut main = thread(("INBOX", 5), &[("INBOX", 5), ("Spam", 7)]);
    main.members.push((2, "INBOX".into(), 9));
    s.saw.lock().unwrap().on_screen = OpenNow {
        main: Some(main),
        windows: vec![OpenThread {
            selected: (2, "INBOX".into(), 9),
            members: vec![(2, "INBOX".into(), 9)],
        }],
        focused: None,
    };
    let answer = s.call("read_open_thread", json!({})).await;
    assert_eq!(data(&answer)["message_count"], 1, "{answer}");
    let shown = answer.to_string();
    assert!(!shown.contains("Prize"), "{shown}");
    assert!(!shown.contains("Bo's mail"), "{shown}");
    assert!(!shown.contains(BO), "{shown}");
}

#[tokio::test]
async fn an_encrypted_message_on_screen_keeps_its_text() {
    let mut s = Session::start(ada_for(OPEN), Ok(plain_raw("text"))).await;
    s.saw.lock().unwrap().on_screen = OpenNow {
        main: Some(thread(("Vault", 11), &[("Vault", 11)])),
        ..Default::default()
    };
    let answer = s.call("read_open_thread", json!({})).await;
    assert_eq!(data(&answer)["open"], true, "{answer}");
    assert!(data(&answer)["messages"][0]["body"].is_null(), "{answer}");
    assert_eq!(data(&answer)["messages"][0]["encrypted"], true, "{answer}");
    assert_eq!(s.saw.lock().unwrap().fetched, 0);
}
