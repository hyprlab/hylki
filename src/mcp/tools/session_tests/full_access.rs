//! What full access opens, and what it leaves to its own switches.

use super::*;

#[tokio::test]
async fn full_access_opens_all_but_junk_and_trash_and_still_not_what_no_setting_allows() {
    let mut policy = ada_may(&[]);
    let grant = policy.grant_mut(ADA);
    grant.full = true;
    grant.folders.insert("Private".into(), false);
    let mut s = Session::start(policy, Ok(plain_raw("text"))).await;

    let status = data(&s.call("hylki_status", json!({})).await);
    assert_eq!(
        status["accounts"][0]["allowed"].as_array().unwrap().len(),
        AssistantCap::ALL
            .iter()
            .filter(|c| c.in_full_access())
            .count()
    );
    let listed = s.call("list_messages", json!({"folder":"Private"})).await;
    assert_eq!(subjects(&listed), ["Diagnosis"], "{listed}");
    let found = s.call("search", json!({"query":"Diagnosis"})).await;
    assert_eq!(subjects(&found), ["Diagnosis"], "{found}");

    // Junk is its own choice, by name or by a message's id.
    let junk = s.call("list_messages", json!({"folder":"Spam"})).await;
    assert!(refused(&junk), "{junk}");
    let by_id = s
        .call("read_message", json!({"id":id_of(ADA, "Spam", 7)}))
        .await;
    assert!(refused(&by_id), "{by_id}");
    let found = s.call("search", json!({"query":"Prize"})).await;
    assert!(subjects(&found).is_empty(), "{found}");

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

#[tokio::test]
async fn junk_on_its_own_switch_opens_under_full_access() {
    let mut policy = ada_may(&[]);
    let grant = policy.grant_mut(ADA);
    grant.full = true;
    grant.folders.insert("Spam".into(), true);
    let mut s = Session::start(policy, Ok(plain_raw("text"))).await;

    let listed = s.call("list_messages", json!({"folder":"Spam"})).await;
    assert_eq!(subjects(&listed), ["Prize"], "{listed}");
    // Trash has a switch of its own, still off.
    let bin = s.call("list_messages", json!({"folder":"Bin"})).await;
    assert!(refused(&bin), "{bin}");
}
