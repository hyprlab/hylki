//! Characterization tests for the list's pure helpers: they pin today's
//! behaviour, quirks included, before the module is split.

use super::filters::{bare_address, CheckFilter};
use super::sort::{normalize_subject, tag_sort_key, SortOrder};
use crate::models::Message;

fn blank() -> Message {
    Message {
        id: 1,
        account_id: 1,
        folder_id: 1,
        uid: 1,
        from_name: String::new(),
        from_addr: String::new(),
        reply_to: String::new(),
        reply_to_name: String::new(),
        to: String::new(),
        cc: String::new(),
        bcc: String::new(),
        subject: String::new(),
        preview: String::new(),
        body: String::new(),
        date: String::new(),
        timestamp: 0,
        unread: false,
        starred: false,
        answered: false,
        forwarded: false,
        keywords: Vec::new(),
        has_attachment: false,
        message_id: String::new(),
        references: String::new(),
        importance: Default::default(),
        due: 0,
        labels: Vec::new(),
    }
}

fn msg(unread: bool, starred: bool) -> Message {
    Message {
        unread,
        starred,
        ..blank()
    }
}

#[test]
fn subjects_lose_any_run_of_reply_prefixes() {
    assert_eq!(normalize_subject("  Re: FWD: aw:Hello  "), "hello");
    assert_eq!(normalize_subject("Re:Re:Re: x"), "x");
    assert_eq!(normalize_subject("SV: Antw: WG: Fw: x"), "x");
    // Only a leading run: a prefix after other text stays.
    assert_eq!(normalize_subject("Hello Re: x"), "hello re: x");
    // `Re :` with a space is not a prefix.
    assert_eq!(normalize_subject("Re : x"), "re : x");
    assert_eq!(normalize_subject("Re:"), "");
    assert_eq!(normalize_subject(""), "");
    // Only ASCII is lowered.
    assert_eq!(normalize_subject("ÄBC"), "Äbc");
}

#[test]
fn check_filters_select_by_any_member() {
    let read = [msg(false, false), msg(false, false)];
    let one_unread = [msg(false, false), msg(true, false)];
    let one_starred = [msg(false, true), msg(false, false)];
    assert!(CheckFilter::Read.matches(&read));
    assert!(!CheckFilter::Read.matches(&one_unread));
    assert!(CheckFilter::Unread.matches(&one_unread));
    assert!(!CheckFilter::Unread.matches(&read));
    assert!(CheckFilter::Starred.matches(&one_starred));
    assert!(!CheckFilter::Unstarred.matches(&one_starred));
    assert!(CheckFilter::Unstarred.matches(&read));
    // An empty conversation reads as read and unstarred.
    assert!(CheckFilter::Read.matches(&[]));
    assert!(CheckFilter::Unstarred.matches(&[]));
}

#[test]
fn sort_orders_round_trip_through_their_keys() {
    for (order, key) in SortOrder::KEYS {
        assert_eq!(order.key(), key);
        assert_eq!(SortOrder::from_key(key), order);
    }
    assert_eq!(SortOrder::from_key("nonsense"), SortOrder::DateNewest);
    assert_eq!(SortOrder::from_key("DATE_OLDEST"), SortOrder::DateNewest);
    let downwards: Vec<_> = SortOrder::KEYS
        .iter()
        .filter(|(o, _)| o.downwards())
        .map(|(_, k)| *k)
        .collect();
    assert_eq!(
        downwards,
        [
            "date_newest",
            "unread",
            "flagged",
            "attachment",
            "importance",
            "replied"
        ]
    );
}

#[test]
fn bare_addresses_take_the_last_angle_bracket() {
    assert_eq!(bare_address(" Ann <Ann@X.org> "), "ann@x.org");
    assert_eq!(bare_address("B@Y"), "b@y");
    assert_eq!(bare_address("\"a <b>\" <c@d>"), "c@d");
    // No closing bracket: the rest is taken as it is.
    assert_eq!(bare_address("Ann <a@x"), "a@x");
    // Quotes are not trimmed.
    assert_eq!(bare_address("\"q@x\""), "\"q@x\"");
    assert_eq!(bare_address(""), "");
}

#[test]
fn tag_sort_keys_name_the_tags_in_row_order() {
    let tags = vec![
        crate::config::Tag {
            name: "Zeta".into(),
            keyword: "z".into(),
            color: "#000000".into(),
        },
        crate::config::Tag {
            name: "Alpha".into(),
            keyword: "a".into(),
            color: "#000000".into(),
        },
    ];
    let mut m = blank();
    assert_eq!(tag_sort_key(&m, &tags), "");
    m.keywords = vec!["a".into(), "z".into(), "unknown".into()];
    m.labels = vec!["Label".into()];
    // In the order of the tag list, then the labels; unknown keywords drop.
    assert_eq!(tag_sort_key(&m, &tags), "zeta, alpha, label");
}
