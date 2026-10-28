//! The list's tests: row diffs, threading, conversation heads, swipes,
//! day sections, quick filters and search text.

use super::filters::{QuickFilter, QuickFilterContext, QuickFilters};
use super::sort::{column_sort, correspondents, message_cmp, text_key_cmp, SortOrder};
use super::threading::{
    compute_thread_keys, heads_its_row, latest_elsewhere, nested_members, reader_conversation,
    row_for_reader_key, thread_slot, unasked_threads,
};
use crate::models::Message;
use crate::ui::message_row::{
    row_edits, swap_wanted, swipe_progress_px, DaySection, RowData, RowEdit, SWIPE_ARM, SWIPE_MAX,
};
use std::rc::Rc;

/// Apply `row_edits` to `old` the way the model applies them, then add
/// the rest of `new`, and return the result alongside the number of rows
/// put in or taken out.
fn apply_edits(old: &[u32], new: &[u32]) -> (Vec<u32>, usize) {
    let (edits, tail) = row_edits(old, new);
    let mut rows = old.to_vec();
    let mut pos = 0;
    let mut cost = 0;
    for e in edits {
        match e {
            RowEdit::Keep => pos += 1,
            RowEdit::Remove => {
                rows.remove(pos);
                cost += 1;
            }
            RowEdit::Insert(i) => {
                rows.insert(pos, new[i]);
                pos += 1;
                cost += 1;
            }
        }
    }
    cost += rows.len() - pos;
    rows.truncate(pos);
    rows.extend(&new[tail..]);
    (rows, cost)
}

/// #323: a rebuild keeps the rows that still show the same message and
/// touches only what changed, so a long list is not built again for new
/// mail, a lifted conversation or more of the folder.
#[test]
fn rebuilds_touch_only_the_rows_that_changed() {
    let old: Vec<u32> = (0..1000).collect();
    // More of the folder: nothing to touch, the rest joins after.
    let grown: Vec<u32> = (0..1500).collect();
    assert_eq!(apply_edits(&old, &grown), (grown.clone(), 0));
    // Two new messages at the top.
    let mut fresh = vec![5000, 5001];
    fresh.extend(0..1000);
    assert_eq!(apply_edits(&old, &fresh), (fresh.clone(), 2));
    // A reply lifts row 700's conversation to the top.
    let mut lifted = vec![700];
    lifted.extend((0..1000).filter(|&n| n != 700));
    assert_eq!(apply_edits(&old, &lifted), (lifted.clone(), 2));
    // Mail deleted elsewhere, from the middle.
    let gone: Vec<u32> = (0..1000).filter(|n| ![10, 11, 500].contains(n)).collect();
    assert_eq!(apply_edits(&old, &gone), (gone.clone(), 3));
    // The same rows: nothing to put in or take out.
    assert_eq!(apply_edits(&old, &old), (old.clone(), 0));
    // A different folder shares nothing, so everything goes.
    let other: Vec<u32> = (2000..2010).collect();
    let (rows, cost) = apply_edits(&old, &other);
    assert_eq!(rows, other);
    assert!(cost >= 1000);
}

/// #401: a list that is mostly another goes to the view as one swap, so
/// it keeps its row widgets; small changes still go row by row.
#[test]
fn another_list_is_swapped_in_whole() {
    let swaps = |old: &[u32], new: &[u32]| {
        let (edits, tail) = row_edits(old, new);
        swap_wanted(&edits, old.len(), tail, new.len())
    };
    let old: Vec<u32> = (0..1000).collect();
    let mut fresh = vec![5000, 5001];
    fresh.extend(0..1000);
    assert!(!swaps(&old, &fresh));
    let mut lifted = vec![700];
    lifted.extend((0..1000).filter(|&n| n != 700));
    assert!(!swaps(&old, &lifted));
    let gone: Vec<u32> = (0..1000).filter(|n| ![10, 11, 500].contains(n)).collect();
    assert!(!swaps(&old, &gone));
    assert!(!swaps(&old, &old));
    assert!(!swaps(&old, &(0..1500).collect::<Vec<_>>()));
    assert!(!swaps(&[], &old));
    assert!(!swaps(&old, &[]));
    // Another account's folder.
    assert!(swaps(&old, &(2000..3000).collect::<Vec<_>>()));
    // All Inboxes to one of its accounts: every other row goes.
    let one: Vec<u32> = (0..1000).filter(|n| n % 2 == 0).collect();
    assert!(swaps(&old, &one));
}

/// #236: the row a conversation collapses to is judged over the rendered
/// window's grouping, so a conversation whose oldest message lies past
/// the window still opens whole from its row.
#[test]
fn a_row_heads_its_conversation_within_the_rendered_window() {
    use std::collections::HashMap;
    let key = (1u32, "root@x".to_string());
    // The window holds uids 30 and 40 of the conversation; uid 10, its
    // real start, is past the window and so in neither map.
    let mut msg_thread = HashMap::new();
    msg_thread.insert((1, 30), key.clone());
    msg_thread.insert((1, 40), key.clone());
    let mut thread_members = HashMap::new();
    thread_members.insert(key, vec![(1, 30), (1, 40)]);

    assert!(
        heads_its_row((1, 30), &msg_thread, &thread_members),
        "the oldest shown is the row"
    );
    assert!(
        !heads_its_row((1, 40), &msg_thread, &thread_members),
        "a child row is not"
    );
    // A message grouped with nothing in the window is its own row, and the
    // reader is still free to look for the rest of it in the cache.
    assert!(heads_its_row((1, 10), &msg_thread, &thread_members));
    assert!(heads_its_row((2, 7), &msg_thread, &thread_members));
}

/// #236 again, the second half: the row for a mail you have answered says
/// so, because the cache hands down a reply this folder does not hold.
#[test]
fn a_row_speaks_for_the_reply_filed_in_sent() {
    use crate::models::{ThreadLatest, ThreadSummary};
    let reply = ThreadLatest {
        from_name: "Me".into(),
        from_addr: "me@example.com".into(),
        preview: "Looked, all good".into(),
        timestamp: 600,
        date: String::new(),
    };
    let summary = ThreadSummary {
        count: 2,
        latest: Some(reply.clone()),
        ..Default::default()
    };

    // The Inbox holds the message that came in at 500; the answer is later.
    assert_eq!(latest_elsewhere(Some(&summary), 500), Some(reply));
    // Once the folder itself holds something at least as new — the reply
    // is in this folder too, or a newer mail has arrived — the row keeps
    // describing what it can see.
    assert_eq!(latest_elsewhere(Some(&summary), 600), None);
    assert_eq!(latest_elsewhere(Some(&summary), 900), None);
    // A conversation the cache has said nothing about, or nothing but a
    // size, leaves the row alone.
    assert_eq!(latest_elsewhere(None, 500), None);
    assert_eq!(
        latest_elsewhere(
            Some(&ThreadSummary {
                count: 3,
                ..Default::default()
            }),
            500
        ),
        None
    );
}

fn msg(id: u32, message_id: &str, references: &str) -> Message {
    Message {
        id,
        account_id: 1,
        folder_id: 1,
        uid: id,
        from_name: "X".into(),
        from_addr: "x@example.com".into(),
        reply_to: String::new(),
        to: String::new(),
        cc: String::new(),
        bcc: String::new(),
        subject: "S".into(),
        preview: String::new(),
        body: String::new(),
        date: String::new(),
        timestamp: 1000,
        unread: false,
        starred: false,
        answered: false,
        forwarded: false,
        keywords: Vec::new(),
        has_attachment: false,
        message_id: message_id.into(),
        references: references.into(),
        importance: Default::default(),
        due: 0,
        labels: Vec::new(),
    }
}

/// #334: every sort keeps its key, a heading sorts by its column (the
/// recipients in Sent), and due dates sort soonest first, undated last.
#[test]
fn headings_sort_by_their_columns() {
    use crate::config::ListColumn;
    for c in ListColumn::ALL {
        let order = column_sort(c, false);
        assert_eq!(SortOrder::from_key(order.key()), order);
    }
    assert_eq!(column_sort(ListColumn::Sender, true), SortOrder::Recipients);
    assert_eq!(column_sort(ListColumn::Tags, false), SortOrder::Tags);
    assert_eq!(text_key_cmp("", "a"), std::cmp::Ordering::Greater);
    assert_eq!(text_key_cmp("ann", "bob"), std::cmp::Ordering::Less);
    let due = |id, due| Message {
        due,
        ..msg(id, "", "")
    };
    let mut list = [due(1, 0), due(2, 300), due(3, 100)];
    list.sort_by(|a, b| message_cmp(a, b, SortOrder::Due));
    assert_eq!(list.iter().map(|m| m.id).collect::<Vec<_>>(), vec![3, 2, 1]);
}

/// #334: who wrote, each once; who it went to when nobody else has
/// written. #443: the newest writer first.
#[test]
fn correspondents_name_each_writer_once() {
    let from = |id, name: &str, addr: &str, ts| Message {
        from_name: name.into(),
        from_addr: addr.into(),
        timestamp: ts,
        to: "Zoe <z@example.com>".into(),
        ..msg(id, "", "")
    };
    let (a, b, a2) = (
        from(1, "Ann", "a@example.com", 10),
        from(2, "", "b@example.com", 20),
        from(3, "Ann", "A@example.com", 30),
    );
    assert_eq!(correspondents(&[&a, &b, &a2]), "Ann, b@example.com");
    assert_eq!(correspondents(&[&a, &b]), "b@example.com, Ann");
    assert_eq!(correspondents(&[&a]), "Ann");
    assert_eq!(correspondents(&[]), "");
}

/// #309: a conversation opens out into its parts in other folders, but
/// not into what the list already shows, a second label's copy of the
/// same mail, or what the list's filters leave out.
#[test]
fn a_row_nests_only_the_conversation_filed_elsewhere() {
    let in_folder = |id, mid: &str, folder| Message {
        folder_id: folder,
        ..msg(id, mid, "")
    };
    let own = vec![in_folder(1, "a@x", 1)];
    let found = vec![
        in_folder(1, "a@x", 1),  // the inbox message itself
        in_folder(90, "a@x", 9), // its copy under All Mail
        in_folder(91, "b@x", 3), // your reply in Sent
        in_folder(92, "b@x", 9), // the reply's All Mail copy
        in_folder(93, "c@x", 2), // in another listed folder
        Message {
            unread: true,
            ..in_folder(94, "d@x", 5)
        },
        in_folder(95, "", 5), // no Message-ID to tell copies by
    ];
    let listed = [(1u32, 1u32), (1, 2)].into_iter().collect();
    let ids = |v: Vec<Message>| v.iter().map(|m| m.id).collect::<Vec<_>>();
    assert_eq!(
        ids(nested_members(&found, &own, &listed, &|_| true)),
        vec![91, 94]
    );
    assert_eq!(
        ids(nested_members(&found, &own, &listed, &|m| m.unread)),
        vec![94]
    );
}

/// Two replies in an Inbox each answer a different message in Sent, and
/// reference nothing else — so within the Inbox they share no id at all.
/// They are one conversation, and the messages that say so are the ones in
/// Sent, which the folder on screen never shows.
#[test]
fn a_conversation_joined_through_another_folder_still_groups() {
    let shown = [
        msg(1, "reply-a@them", "sent-1@us"),
        msg(2, "reply-b@them", "sent-2@us"),
    ];
    // Without the Sent messages there is nothing to join them.
    let alone = compute_thread_keys(&shown, &[]);
    assert_ne!(
        alone.get(&(1, 1, 1)),
        alone.get(&(1, 1, 2)),
        "nothing on screen links these two"
    );

    // Sent 2 replied to reply-a, which replied to Sent 1: one conversation.
    let links = vec![
        (1u32, "sent-1@us".to_string(), String::new()),
        (
            1u32,
            "sent-2@us".to_string(),
            "sent-1@us reply-a@them".to_string(),
        ),
    ];
    let joined = compute_thread_keys(&shown, &links);
    assert_eq!(
        joined.get(&(1, 1, 1)),
        joined.get(&(1, 1, 2)),
        "the messages in Sent say they belong together"
    );
}

/// A re-added account re-downloads its whole mailbox, so every message in it
/// is older than the moment the account was added. Threading reads the reply
/// headers, which say the same thing whenever the mail was sent — the three
/// messages here are the shape iCloud delivered: a root with no References,
/// and two replies naming it.
#[test]
fn mail_older_than_the_account_still_threads() {
    let old = 1_787_565_140i64; // long before this list was ever built
    let mut root = msg(1, "root@dccma.com", "");
    root.timestamp = old;
    let mut first = msg(2, "r1@dccma.com", "root@dccma.com sent-1@me.com");
    first.timestamp = old + 48;
    let mut second = msg(3, "r2@dccma.com", "root@dccma.com sent-2@me.com");
    second.timestamp = old + 224;

    let shown = [root, first, second];
    let keys = compute_thread_keys(&shown, &[]);
    let root_key = keys.get(&(1, 1, 1)).cloned().expect("the root is threaded");
    assert_eq!(keys.get(&(1, 1, 2)), Some(&root_key), "first reply joins");
    assert_eq!(keys.get(&(1, 1, 3)), Some(&root_key), "second reply joins");
}

fn listed(items: &[(u32, &str)]) -> Vec<(u32, String, Vec<String>)> {
    items
        .iter()
        .map(|(a, r)| (*a, r.to_string(), vec![format!("{r}@x")]))
        .collect()
}

/// The counts come back as a rebuild, and a rebuild is what decides to ask.
/// If asking were driven by the page alone, that would be a loop; it is
/// driven by what has not been asked yet, so the second pass is silent.
#[test]
fn a_page_is_only_asked_about_once() {
    let page = listed(&[(1, "a"), (1, "b")]);
    let mut asked = std::collections::HashSet::new();

    let first = unasked_threads(&page, &asked);
    assert_eq!(first.len(), 2, "nothing counted yet, so ask about both");
    for (aid, root, _) in &first {
        asked.insert((*aid, root.clone()));
    }
    assert!(
        unasked_threads(&page, &asked).is_empty(),
        "the answer must not start another round"
    );
}

/// A search re-filters the page on every keystroke, and each rebuild would
/// otherwise be a fresh scan of the message index.
#[test]
fn narrowing_the_page_asks_nothing_further() {
    let page = listed(&[(1, "a"), (1, "b"), (1, "c")]);
    let asked: std::collections::HashSet<(u32, String)> =
        page.iter().map(|(a, r, _)| (*a, r.clone())).collect();

    let narrowed = listed(&[(1, "b")]);
    assert!(unasked_threads(&narrowed, &asked).is_empty());
}

/// New mail brings threads nobody has counted; only those are asked about.
#[test]
fn only_the_new_conversations_are_asked_about() {
    let asked: std::collections::HashSet<(u32, String)> =
        [(1u32, "a".to_string())].into_iter().collect();
    let page = listed(&[(1, "a"), (1, "new")]);

    let fresh = unasked_threads(&page, &asked);
    assert_eq!(
        fresh.iter().map(|(_, r, _)| r.as_str()).collect::<Vec<_>>(),
        vec!["new"]
    );
}

/// Two accounts can root a thread at the same Message-ID (the unified
/// inbox shows both), and they are different conversations in different
/// caches — asking about one must not silence the other.
#[test]
fn the_same_thread_root_in_two_accounts_is_two_questions() {
    let asked: std::collections::HashSet<(u32, String)> =
        [(1u32, "shared".to_string())].into_iter().collect();
    let page = listed(&[(1, "shared"), (2, "shared")]);

    let fresh = unasked_threads(&page, &asked);
    assert_eq!(
        fresh.iter().map(|(a, _, _)| *a).collect::<Vec<_>>(),
        vec![2]
    );
}

/// A search over every folder holds the same UID once per folder. Those
/// are different messages, and each keeps its own conversation (#317).
#[test]
fn the_same_uid_in_two_folders_stays_two_messages() {
    let root = msg(5, "root@x", "");
    let reply = msg(6, "reply@x", "root@x");
    let mut other = msg(6, "other@y", "");
    other.folder_id = 2;
    let mut bare_here = msg(9, "", "");
    bare_here.folder_id = 1;
    let mut bare_there = msg(9, "", "");
    bare_there.folder_id = 2;
    let pool = [root, reply, other.clone(), bare_here, bare_there];
    let keys = compute_thread_keys(&pool, &[]);
    let conversation = keys.get(&(1, 1, 5)).cloned().expect("threaded");
    let members: Vec<(u32, u32)> = pool
        .iter()
        .filter(|m| keys.get(&thread_slot(m)) == Some(&conversation))
        .map(|m| (m.folder_id, m.uid))
        .collect();
    assert_eq!(members, vec![(1, 5), (1, 6)], "only the root and its reply");
    assert_ne!(keys.get(&thread_slot(&other)), Some(&conversation));
    assert_ne!(
        keys.get(&(1, 1, 9)),
        keys.get(&(1, 2, 9)),
        "no Message-ID and one UID in two folders: still two messages"
    );
}

/// Links are evidence, not glue: unrelated mail must not be pulled in.
#[test]
fn links_do_not_merge_unrelated_conversations() {
    let shown = [msg(1, "a@x", ""), msg(2, "b@x", "")];
    let links = vec![(1u32, "c@x".to_string(), "a@x".to_string())];
    let keys = compute_thread_keys(&shown, &links);
    assert_ne!(
        keys.get(&(1, 1, 1)),
        keys.get(&(1, 1, 2)),
        "still two conversations"
    );
}

/// A long conversation groups whole. What it may drag in from *other*
/// folders is bounded by the cache's own per-thread limit; what is already
/// in the folder on screen is shown in full.
#[test]
fn a_long_conversation_groups_every_message() {
    let n = 60usize;
    let mut members: Vec<Message> = Vec::new();
    for i in 0..n {
        let mut m = msg(i as u32 + 1, &format!("m{i}@x"), "root@x");
        m.timestamp = 1000 + i as i64;
        members.push(m);
    }
    // All one conversation by their shared reference.
    let keys = compute_thread_keys(&members, &[]);
    let root = keys.get(&(1, 1, 1)).cloned().expect("threaded");
    assert!(
        members
            .iter()
            .all(|m| keys.get(&(1, 1, m.id)) == Some(&root)),
        "one conversation"
    );
    assert_eq!(
        members
            .iter()
            .filter(|m| keys.get(&(1, 1, m.id)) == Some(&root))
            .count(),
        n,
        "every message belongs to it, however long the thread runs"
    );
}

/// libadwaita's own scale for a touchpad's two-finger scroll: it spends a
/// fixed 400px of horizontal delta on a full swipe, whatever the widget's
/// `distance` says, which is why the preference exists at all.
const TOUCHPAD_BASE: f64 = 400.0;

#[test]
fn mouse_drag_tracks_the_pointer_at_every_sensitivity() {
    // `AdwSwipeTracker` divides a drag by `SwipeSurface::distance`, which
    // is SWIPE_MAX * sensitivity, so the row must come back out at the
    // pointer's own px however the preference is set.
    for sensitivity in [1.0, 3.5, 10.0] {
        for dragged in [10.0, 72.0, 120.0] {
            let progress = dragged / (SWIPE_MAX * sensitivity);
            assert!(
                (swipe_progress_px(progress, sensitivity) - dragged).abs() < 0.001,
                "{dragged}px drag at {sensitivity}"
            );
        }
    }
}

#[test]
fn sensitivity_shortens_the_trackpad_swipe() {
    // How much two-finger scroll it takes to reach the commit distance.
    let travel = |sensitivity: f64| {
        (1..=2000)
            .map(|px| px as f64)
            .find(|px| swipe_progress_px(px / TOUCHPAD_BASE, sensitivity).abs() >= SWIPE_ARM)
            .expect("armed eventually")
    };
    // Untuned, a trackpad has to travel further than most can in one go —
    // the complaint behind the setting.
    assert_eq!(travel(1.0), 240.0);
    // The default puts it within a comfortable swipe, and raising it
    // further keeps shortening it.
    assert!(travel(3.5) < 70.0, "default is a short swipe");
    assert!(travel(10.0) < travel(3.5), "higher is always shorter");
}

#[test]
fn a_long_swipe_stops_at_the_action_strip() {
    for sensitivity in [1.0, 10.0] {
        assert_eq!(swipe_progress_px(1.0, sensitivity), SWIPE_MAX);
        assert_eq!(swipe_progress_px(-1.0, sensitivity), -SWIPE_MAX);
    }
}

#[test]
fn preview_lines_clamp_but_keep_zero() {
    // 0 is "off"; anything above 3 is a hand-edited file, not a setting.
    for (asked, expected) in [(0u32, 0u32), (1, 1), (3, 3), (9, 3)] {
        assert_eq!(asked.min(3), expected, "for {asked}");
    }
}

/// Clicking a reply's card keeps the conversation's row selected even when
/// the list never shows that reply a row of its own (#211).
#[test]
fn a_hidden_reply_selects_its_conversation_row() {
    use std::collections::HashMap;
    // The list shows the head of a three-message conversation, and one
    // unrelated message below it.
    let shown = [msg(1, "head@them", ""), msg(9, "other@them", "")];
    let thread = (1u32, "head@them".to_string());
    let msg_thread: HashMap<(u32, u32), (u32, String)> = [
        ((1, 1), thread.clone()),
        ((1, 2), thread.clone()),
        ((1, 3), thread.clone()),
    ]
    .into_iter()
    .collect();

    // The conversation as this list handed it over, opened from its head
    // row. A message of the user's own, pulled in from Sent, joins it
    // only later and only on the app's side: this folder has no row for
    // it, no thread entry, and never listed it at all (#220).
    let emitted = [(1, 1), (1, 2), (1, 3)];
    let merged = [(1, 1), (1, 2), (1, 3), (1, 77)];
    let conversation = reader_conversation(&emitted, &merged);
    assert_eq!(conversation, [(1, 1), (1, 2), (1, 3), (1, 77)]);
    let viewed = Some((1, 1));
    let row =
        |key: (u32, u32)| row_for_reader_key(&key, &shown, &msg_thread, &conversation, viewed);

    // The head has a row of its own.
    assert_eq!(row((1, 1)), Some(0));
    // Its replies do not, and land on the head's row rather than nowhere.
    assert_eq!(row((1, 2)), Some(0));
    assert_eq!(row((1, 3)), Some(0));
    // Neither does the copy from Sent, which this folder never lists.
    assert_eq!(row((1, 77)), Some(0));
    // Going by what the list handed over alone, that copy would land
    // nowhere — the bug behind #220.
    assert_eq!(
        row_for_reader_key(&(1, 77), &shown, &msg_thread, &emitted, viewed),
        None
    );
    // A message in no conversation still matches only itself.
    assert_eq!(row((1, 9)), Some(1));
    // And one from neither is no row at all.
    assert_eq!(row((1, 42)), None);
}

/// #435: each day falls under its heading, the boundaries at midnight,
/// and mail dated ahead of the clock under Today.
#[test]
fn mail_falls_under_its_days_heading() {
    let day = 86_400;
    let today = 100 * day;
    let starts = [today, today - day, today - 6 * day, today - 13 * day];
    let of = |ts| DaySection::of(ts, &starts);
    assert_eq!(of(today + 5 * day), DaySection::Today);
    assert_eq!(of(today), DaySection::Today);
    assert_eq!(of(today - 1), DaySection::Yesterday);
    assert_eq!(of(today - day), DaySection::Yesterday);
    assert_eq!(of(today - day - 1), DaySection::LastWeek);
    assert_eq!(of(today - 6 * day), DaySection::LastWeek);
    assert_eq!(of(today - 6 * day - 1), DaySection::LastFortnight);
    assert_eq!(of(today - 13 * day), DaySection::LastFortnight);
    assert_eq!(of(today - 13 * day - 1), DaySection::Older);
    assert_eq!(of(0), DaySection::Older);
}

/// The local midnights run back a day, a week and a fortnight, and the
/// first is the start of the day `now` is in.
#[test]
fn the_days_start_at_local_midnight() {
    let now = crate::datefmt::now();
    let [today, yesterday, week, fortnight] = crate::datefmt::day_starts(now);
    assert!(today <= now && now - today < 25 * 3600);
    assert_eq!(crate::datefmt::day_key(today), crate::datefmt::day_key(now));
    assert_ne!(
        crate::datefmt::day_key(today - 1),
        crate::datefmt::day_key(today)
    );
    for (later, earlier, days) in [
        (today, yesterday, 1),
        (today, week, 6),
        (today, fortnight, 13),
    ] {
        let gap = later - earlier;
        assert!(
            (gap - days * 86_400).abs() <= 3600,
            "{days} days back is {gap}s"
        );
    }
}

/// A section is a run of rows under one heading, so the view's question
/// about any row is a search among a handful of starts.
#[test]
fn sections_are_the_runs_of_one_heading() {
    use crate::ui::message_row::section_starts;
    let row = |id: u32, section| {
        Rc::new(RowData {
            msg: Rc::new(msg(id, "", "")),
            meta: Default::default(),
            section,
        })
    };
    assert_eq!(section_starts(&[]), vec![0]);
    let plain: Vec<_> = (0..5).map(|i| row(i, DaySection::None)).collect();
    assert_eq!(section_starts(&plain), vec![0]);
    let days = [
        DaySection::Today,
        DaySection::Today,
        DaySection::Yesterday,
        DaySection::Older,
        DaySection::Older,
        DaySection::Older,
    ];
    let rows: Vec<_> = days
        .iter()
        .enumerate()
        .map(|(i, s)| row(i as u32, *s))
        .collect();
    assert_eq!(section_starts(&rows), vec![0, 2, 3]);
}

/// #441: the quick filters narrow together, and "Addressed to Me" reads
/// both To and Cc in either address form.
#[test]
fn quick_filters_narrow_together() {
    let ctx = |filters: QuickFilters| QuickFilterContext {
        filters,
        own: ["me@example.com".to_string()].into_iter().collect(),
        contacts: Some(std::sync::Arc::new(
            ["x@example.com".to_string()].into_iter().collect(),
        )),
        tags: Vec::new(),
        today: 900,
        span: filters.span(),
    };
    let mut m = msg(1, "a", "");
    m.cc = "\"Doe, Jo\" <jo@example.com>, Me <ME@example.com>".into();
    let all = QuickFilters::default();
    assert!(ctx(all).passes(&m));
    assert!(ctx(all.with(QuickFilter::ToMe, true)).passes(&m));
    assert!(ctx(all
        .with(QuickFilter::Contacts, true)
        .with(QuickFilter::Today, true))
    .passes(&m));
    assert!(!ctx(all
        .with(QuickFilter::ToMe, true)
        .with(QuickFilter::Unread, true))
    .passes(&m));
    m.unread = true;
    assert!(ctx(all
        .with(QuickFilter::ToMe, true)
        .with(QuickFilter::Unread, true))
    .passes(&m));
    assert!(!ctx(all
        .with(QuickFilter::Unread, true)
        .with(QuickFilter::Unread, false)
        .with(QuickFilter::Attachment, true))
    .passes(&m));
    m.timestamp = 800;
    assert!(!ctx(all.with(QuickFilter::Today, true)).passes(&m));
}

/// The date range takes in the whole of both days it names, on this
/// computer's clock, whichever way round they were picked.
#[test]
fn date_range_spans_whole_days() {
    use super::filters::{start_of_day, DateBound};
    let day = chrono::NaiveDate::from_ymd_opt(2026, 3, 10).unwrap();
    let before = day.pred_opt().unwrap();
    let after = day.succ_opt().unwrap();
    let ctx = |filters: QuickFilters| QuickFilterContext {
        filters,
        own: Default::default(),
        contacts: None,
        tags: Vec::new(),
        today: 0,
        span: filters.span(),
    };
    let range = |from, to| {
        QuickFilters::default()
            .with_date(DateBound::From, from)
            .with_date(DateBound::To, to)
    };
    let mut m = msg(1, "a", "");
    for at in [start_of_day(day), start_of_day(after) - 1] {
        m.timestamp = at;
        assert!(ctx(range(Some(day), Some(day))).passes(&m));
        assert!(ctx(range(Some(after), Some(before))).passes(&m));
        assert!(ctx(range(Some(day), None)).passes(&m));
        assert!(ctx(range(None, Some(day))).passes(&m));
        assert!(!ctx(range(Some(after), None)).passes(&m));
        assert!(!ctx(range(None, Some(before))).passes(&m));
    }
    m.timestamp = start_of_day(after);
    assert!(!ctx(range(Some(day), Some(day))).passes(&m));
    assert!(!range(None, None).has_dates());
    assert!(!range(Some(day), None).is_empty());
}

/// #515: what a search looks in is made once, lowercased, and a query
/// matches inside one field, never across two.
#[test]
fn search_text_keeps_the_fields_apart() {
    let mut m = msg(1, "a", "");
    m.subject = "Quarterly REPORT".into();
    m.from_name = "Ada Lovelace".into();
    m.from_addr = "ada@example.com".into();
    m.preview = "Figures attached.".into();
    let text = super::search_text_of(&m);
    for hit in [
        "report",
        "ada lovelace",
        "ada@example",
        "figures",
        "QUARTERLY".to_lowercase().as_str(),
    ] {
        assert!(text.contains(hit), "{hit}");
    }
    assert!(!text.contains("report ada"));
    assert!(!text.contains("reportada"));
    assert!(!text.contains("REPORT"));
}

/// #509: each word may be found in a different place: the list's own
/// text, the body on disk, or a server that found them all.
#[test]
fn each_word_may_be_found_in_its_own_place() {
    use super::query::Query;
    use super::search::{decide, text_match, HitSet, TextHits};
    let mail = |uid: u32, subject: &str, message_id: &str| {
        let mut m = msg(uid, message_id, "");
        m.subject = subject.into();
        m
    };
    let q = Query::parse("rossi invoice");
    let m = mail(7, "Lunch with Rossi", "a@x");
    let found = text_match(&q, &super::search_text_of(&m));
    assert!(!decide(&q, found, &m, None), "invoice is nowhere yet");

    let mut body = HitSet::default();
    body.insert(1, 1, 7, "a@x");
    let hits = TextHits {
        terms: q.terms.clone(),
        body: vec![Default::default(), std::rc::Rc::new(body)],
        ..Default::default()
    };
    assert!(decide(&q, found, &m, Some(&hits)), "invoice is in its body");

    // A copy in another folder is found by its Message-ID.
    let mut server = HitSet::default();
    server.insert(1, 9, 99, "b@x");
    let other = mail(8, "Hello", "b@x");
    let hits = TextHits {
        terms: q.terms.clone(),
        server,
        ..Default::default()
    };
    let found = text_match(&q, &super::search_text_of(&other));
    assert!(decide(&q, found, &other, Some(&hits)));

    // Answers for other words do not count.
    let stale = TextHits {
        terms: vec!["else".into()],
        ..hits.clone()
    };
    assert!(!decide(&q, found, &other, Some(&stale)));

    // An operator on the text still rules a server's find out.
    let q = Query::parse("from:ann invoice");
    let hits = TextHits {
        terms: q.terms.clone(),
        ..hits
    };
    let found = text_match(&q, &super::search_text_of(&other));
    assert!(!decide(&q, found, &other, Some(&hits)));
}

/// #509: while a word is typed further, what its shorter self found
/// stands in, and a new word starts with nothing of its own.
#[test]
fn hits_carry_over_while_a_word_is_typed_on() {
    use super::search::{HitSet, TextHits};
    let mut body = HitSet::default();
    body.insert(1, 1, 7, "");
    let before = TextHits {
        terms: vec!["zan".into()],
        body: vec![std::rc::Rc::new(body)],
        ..Default::default()
    };
    let on = TextHits::carried(Some(&before), &["zanz".into(), "rossi".into()]);
    assert_eq!(on.body.len(), 1);
    assert_eq!(on.body[0].len(), 1);
    assert!(!on.server_own);
    let other = TextHits::carried(Some(&before), &["za".into()]);
    assert!(other.body.is_empty(), "a word made shorter finds more");
    let other = TextHits::carried(Some(&before), &["bob".into()]);
    assert!(other.body.is_empty());
}
