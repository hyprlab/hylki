//! Tests of the folder tree, drag-and-drop payloads and the sidebar's
//! small pure helpers.

use super::dnd::parse_move_payload;
use super::dnd::{folder_drop_zone, FolderDrop};
use super::tree::folder_depth;
use super::tree::hidden_by_collapse;
use super::tree::{is_main_row, nests_in_inbox, order_folders, placed_run, under_inbox};
use crate::config::FolderSort;
use crate::models::{Folder, FolderHierarchy, FolderKind};

fn slash() -> FolderHierarchy {
    FolderHierarchy::imap(Some('/'), Some(String::new()))
}

fn dot(prefix: &str) -> FolderHierarchy {
    FolderHierarchy::imap(Some('.'), Some(prefix.to_string()))
}

fn custom(id: u32, path: &str) -> Folder {
    Folder {
        id,
        account_id: 1,
        name: path.rsplit(['/', '.']).next().unwrap_or(path).to_string(),
        path: path.to_string(),
        kind: FolderKind::Custom,
        unread: 0,
        childless: false,
    }
}

#[test]
fn folder_depth_follows_listed_ancestors() {
    // A Dovecot-style namespace: everything lives under "INBOX.", which is
    // not itself a custom folder — so "INBOX.Clients" is top-level and only
    // real sub-folders are indented.
    let folders = [
        custom(1, "INBOX.Clients"),
        custom(2, "INBOX.Clients.Acme"),
        custom(3, "INBOX.Clients.Acme.Invoices"),
        custom(4, "INBOX.Travel"),
    ];
    let refs: Vec<&Folder> = folders.iter().collect();
    assert_eq!(folder_depth(&folders[0], &refs, &dot("INBOX.")), 0);
    assert_eq!(folder_depth(&folders[1], &refs, &dot("INBOX.")), 1);
    assert_eq!(folder_depth(&folders[2], &refs, &dot("INBOX.")), 2);
    assert_eq!(folder_depth(&folders[3], &refs, &dot("INBOX.")), 0);
}

#[test]
fn folder_depth_needs_a_delimiter_not_just_a_prefix() {
    // "ClientsB" merely shares a prefix with "Clients" — it is a sibling,
    // not a child.
    let folders = [
        custom(1, "Clients"),
        custom(2, "ClientsB"),
        custom(3, "Clients/X"),
    ];
    let refs: Vec<&Folder> = folders.iter().collect();
    assert_eq!(folder_depth(&folders[1], &refs, &slash()), 0);
    assert_eq!(folder_depth(&folders[2], &refs, &slash()), 1);
}

#[test]
fn a_collapsed_node_hides_descendants_and_nothing_else() {
    let mut collapsed = std::collections::HashSet::new();
    collapsed.insert("Clients".to_string());
    // Direct child and grandchild hide; a sibling sharing the prefix does
    // not, and neither does the collapsed node itself.
    assert!(hidden_by_collapse("Clients/Acme", &collapsed, &slash()));
    assert!(hidden_by_collapse(
        "Clients/Acme/Invoices",
        &collapsed,
        &slash()
    ));
    assert!(!hidden_by_collapse("ClientsB", &collapsed, &slash()));
    assert!(!hidden_by_collapse("Clients", &collapsed, &slash()));
    // Dotted hierarchies collapse the same way.
    collapsed.clear();
    collapsed.insert("INBOX.2025".to_string());
    assert!(hidden_by_collapse(
        "INBOX.2025.Archive",
        &collapsed,
        &dot("")
    ));
    assert!(!hidden_by_collapse(
        "INBOX.2026.Archive",
        &collapsed,
        &dot("")
    ));
}

#[test]
fn a_drop_payload_carries_every_dragged_message() {
    // One message (the single-selection case).
    assert_eq!(
        parse_move_payload("vireo-move\t1\t2\t3\t4"),
        vec![(1, 2, 3, 4)]
    );
    // Three, as a multi-selection drag sends them — including a second
    // account, which the app filters out (mail can't cross accounts).
    assert_eq!(
        parse_move_payload("vireo-move\t1\t2\t3\t4\t1\t2\t5\t6\t7\t8\t9\t10"),
        vec![(1, 2, 3, 4), (1, 2, 5, 6), (7, 8, 9, 10)]
    );
}

#[test]
fn a_payload_that_isnt_ours_moves_nothing() {
    for bad in [
        "",
        "some dragged text",
        "vireo-move",
        "vireo-move\t1\t2\t3",       // short group
        "vireo-move\t1\t2\t3\t4\t5", // trailing partial group
        "vireo-move\t1\t2\tx\t4",    // unparsable field
    ] {
        assert!(
            parse_move_payload(bad).is_empty(),
            "{bad:?} should parse to nothing"
        );
    }
}

fn main(id: u32, path: &str, kind: FolderKind) -> Folder {
    Folder {
        kind,
        ..custom(id, path)
    }
}

fn paths(folders: &[Folder]) -> Vec<&str> {
    folders.iter().map(|f| f.path.as_str()).collect()
}

fn server_order() -> Vec<Folder> {
    vec![
        main(1, "INBOX", FolderKind::Inbox),
        main(2, "Sent", FolderKind::Sent),
        main(3, "Archive", FolderKind::Archive),
        custom(4, "Clients"),
        custom(5, "Clients/Acme"),
        custom(6, "Clients/Beta"),
        custom(7, "Travel"),
    ]
}

#[test]
fn no_order_keeps_the_default_order() {
    let folders = order_folders(server_order(), &[], FolderSort::Custom, &slash());
    assert_eq!(
        paths(&folders),
        [
            "INBOX",
            "Sent",
            "Archive",
            "Clients",
            "Clients/Acme",
            "Clients/Beta",
            "Travel"
        ]
    );
}

#[test]
fn a_set_order_sorts_siblings_and_keeps_the_tree() {
    let order: Vec<String> = ["Archive", "INBOX", "Travel", "Clients/Beta"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let folders = order_folders(server_order(), &order, FolderSort::Custom, &slash());
    // Archive leads the main folders; Sent (never placed) follows INBOX,
    // the folder before it by default. Clients (never placed) comes
    // before Travel by name, and keeps its sub-folders under it.
    assert_eq!(
        paths(&folders),
        [
            "Archive",
            "INBOX",
            "Sent",
            "Clients",
            "Clients/Acme",
            "Clients/Beta",
            "Travel"
        ]
    );
}

#[test]
fn a_chosen_sort_ignores_the_dragged_order_but_not_the_main_folders() {
    let folders: Vec<Folder> = [
        main(1, "INBOX", FolderKind::Inbox),
        main(2, "Sent", FolderKind::Sent),
    ]
    .into_iter()
    .chain(
        ["Beta", "Beta/Two", "Beta/One", "[Gmail]/Alpha", "Zulu"]
            .iter()
            .enumerate()
            .map(|(i, p)| custom(i as u32 + 3, p)),
    )
    .collect();
    let order: Vec<String> = ["Sent", "INBOX", "Zulu", "Beta"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let sorted = |sort| {
        paths(&order_folders(folders.clone(), &order, sort, &slash()))
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        sorted(FolderSort::Custom),
        [
            "Sent",
            "INBOX",
            "[Gmail]/Alpha",
            "Zulu",
            "Beta",
            "Beta/One",
            "Beta/Two"
        ]
    );
    assert_eq!(
        sorted(FolderSort::NameAsc),
        [
            "Sent",
            "INBOX",
            "[Gmail]/Alpha",
            "Beta",
            "Beta/One",
            "Beta/Two",
            "Zulu"
        ]
    );
    assert_eq!(
        sorted(FolderSort::NameDesc),
        [
            "Sent",
            "INBOX",
            "Zulu",
            "Beta",
            "Beta/Two",
            "Beta/One",
            "[Gmail]/Alpha"
        ]
    );
    // As the list came in (by path on the server).
    assert_eq!(
        sorted(FolderSort::Path),
        [
            "Sent",
            "INBOX",
            "Beta",
            "Beta/Two",
            "Beta/One",
            "[Gmail]/Alpha",
            "Zulu"
        ]
    );
}

#[test]
fn folders_sort_by_the_name_shown_and_new_ones_by_name_among_the_placed() {
    // Gmail's folders, as the server sorts them by path.
    let folders: Vec<Folder> = [
        "2e-303Disk",
        "9e-308Disk",
        "Deleted Messages",
        "Folder 1",
        "Folder 2",
        "Test 2",
        "Test 3",
        "Trash",
        "[Gmail]/Important",
        "[Gmail]/Test",
    ]
    .iter()
    .enumerate()
    .map(|(i, p)| custom(i as u32 + 1, p))
    .collect();
    // No order of the user's: by name, "[Gmail]/Test" as "Test".
    assert_eq!(
        paths(&order_folders(
            folders.clone(),
            &[],
            FolderSort::Custom,
            &slash()
        )),
        [
            "2e-303Disk",
            "9e-308Disk",
            "Deleted Messages",
            "Folder 1",
            "Folder 2",
            "[Gmail]/Important",
            "[Gmail]/Test",
            "Test 2",
            "Test 3",
            "Trash",
        ]
    );
    // An order dragged before the Folder ones existed: they land after
    // Deleted Messages, the placed folder before them by name.
    let order: Vec<String> = [
        "[Gmail]/Important",
        "[Gmail]/Test",
        "2e-303Disk",
        "9e-308Disk",
        "Deleted Messages",
        "Test 3",
        "Test 2",
        "Trash",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(
        paths(&order_folders(
            folders,
            &order,
            FolderSort::Custom,
            &slash()
        )),
        [
            "[Gmail]/Important",
            "[Gmail]/Test",
            "2e-303Disk",
            "9e-308Disk",
            "Deleted Messages",
            "Folder 1",
            "Folder 2",
            "Test 3",
            "Test 2",
            "Trash",
        ]
    );
}

/// #345: folders inside the Inbox follow it, in their tree, when the
/// account has folders outside it too; with every folder inside it (an
/// `INBOX.` namespace) nothing moves.
#[test]
fn folders_inside_the_inbox_follow_it() {
    let main = |id: u32, path: &str, kind: FolderKind| Folder {
        kind,
        ..custom(id, path)
    };
    let mixed = vec![
        main(1, "INBOX", FolderKind::Inbox),
        main(2, "Sent", FolderKind::Sent),
        custom(3, "Archiv"),
        custom(4, "INBOX.Ablage"),
        custom(5, "INBOX.Mailinglisten"),
        custom(6, "INBOX.Mailinglisten.KiCad"),
    ];
    assert!(nests_in_inbox(&mixed, &dot("")));
    let shown = order_folders(mixed.clone(), &[], FolderSort::Custom, &dot(""));
    assert_eq!(
        paths(&shown),
        [
            "INBOX",
            "INBOX.Ablage",
            "INBOX.Mailinglisten",
            "INBOX.Mailinglisten.KiCad",
            "Sent",
            "Archiv"
        ]
    );
    assert_eq!(
        shown
            .iter()
            .filter(|f| is_main_row(f, true, &dot("")))
            .count(),
        5
    );

    let namespaced = vec![
        main(1, "INBOX", FolderKind::Inbox),
        custom(2, "INBOX.Ablage"),
        custom(3, "INBOX.Archiv"),
    ];
    assert!(!nests_in_inbox(&namespaced, &dot("INBOX.")));
    assert_eq!(
        paths(&order_folders(
            namespaced,
            &[],
            FolderSort::Custom,
            &dot("INBOX.")
        )),
        ["INBOX", "INBOX.Ablage", "INBOX.Archiv"]
    );
    // "INBOXES" is not inside the Inbox.
    assert!(!under_inbox("INBOXES", &slash()));
    assert!(under_inbox("inbox/x", &slash()) && !under_inbox("INBOX", &slash()));
    // Only by the server's delimiter (#554).
    assert!(!under_inbox("INBOX.x", &slash()) && under_inbox("INBOX.x", &dot("")));
    let flat = FolderHierarchy::imap(None, Some(String::new()));
    assert!(!under_inbox("INBOX/x", &flat));
}

fn gmail() -> FolderHierarchy {
    slash()
}

#[test]
fn the_parent_is_the_servers_not_the_one_drawn() {
    assert_eq!(gmail().parent("[Gmail]/Important"), "[Gmail]");
    assert_eq!(gmail().parent("Test 2/Test"), "Test 2");
    assert_eq!(gmail().parent("Test 3"), "");
    // A Dovecot-style namespace counts as the top level.
    let dovecot = dot("INBOX.");
    assert_eq!(dovecot.parent("INBOX.old-messages"), "");
    assert_eq!(
        dovecot.parent("INBOX.old-messages.test 2"),
        "INBOX.old-messages"
    );
}

#[test]
fn a_main_folder_only_reorders_among_main_folders() {
    let custom: Vec<String> = vec!["Clients".into(), "Clients/Acme".into()];
    let drop = |target: &Folder, frac| {
        folder_drop_zone(
            "vireo-folder-main\t1\tSent",
            1,
            target,
            &custom,
            &gmail(),
            frac,
        )
    };
    let inbox = main(1, "INBOX", FolderKind::Inbox);
    assert_eq!(drop(&inbox, 0.4), FolderDrop::Before);
    assert_eq!(drop(&inbox, 0.6), FolderDrop::After);
    assert_eq!(drop(&custom_folder("Clients"), 0.5), FolderDrop::Refuse);
    assert_eq!(
        drop(&main(2, "Sent", FolderKind::Sent), 0.1),
        FolderDrop::Refuse
    );
}

fn custom_folder(path: &str) -> Folder {
    custom(9, path)
}

#[test]
fn a_custom_folder_goes_in_any_gap_or_inside_a_folder() {
    let custom: Vec<String> = [
        "Clients",
        "Clients/Acme",
        "Clients/Beta",
        "Travel",
        "[Gmail]/Important",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let drop = |payload: &str, target: &str, frac| {
        folder_drop_zone(payload, 1, &custom_folder(target), &custom, &gmail(), frac)
    };
    let travel = "vireo-folder\t1\tTravel";
    assert_eq!(drop(travel, "Clients", 0.1), FolderDrop::Before);
    assert_eq!(drop(travel, "Clients", 0.9), FolderDrop::After);
    assert_eq!(drop(travel, "Clients", 0.5), FolderDrop::Into);
    // Another level: a gap there too (the folder moves to that level).
    assert_eq!(drop(travel, "Clients/Acme", 0.1), FolderDrop::Before);
    let acme = "vireo-folder\t1\tClients/Acme";
    assert_eq!(drop(acme, "Travel", 0.9), FolderDrop::After);
    // Not beside a folder whose parent isn't a folder ("[Gmail]"),
    // unless it already lives there; inside it is fine.
    assert_eq!(drop(travel, "[Gmail]/Important", 0.1), FolderDrop::Refuse);
    assert_eq!(drop(travel, "[Gmail]/Important", 0.5), FolderDrop::Into);
    assert_eq!(
        drop("vireo-folder\t1\t[Gmail]/Test", "[Gmail]/Important", 0.1),
        FolderDrop::Before
    );
    // Never inside itself, nor among the main folders or another account.
    let clients = "vireo-folder\t1\tClients";
    assert_eq!(drop(clients, "Clients/Acme", 0.5), FolderDrop::Refuse);
    assert_eq!(drop(clients, "Clients/Acme", 0.1), FolderDrop::Refuse);
    let inbox = main(1, "INBOX", FolderKind::Inbox);
    assert_eq!(
        folder_drop_zone(travel, 1, &inbox, &custom, &gmail(), 0.5),
        FolderDrop::Refuse
    );
    assert_eq!(
        drop("vireo-folder\t2\tTravel", "Clients", 0.1),
        FolderDrop::Refuse
    );
    // Mail always goes in.
    assert_eq!(
        drop("vireo-move\t1\t2\t3\t4", "Clients", 0.1),
        FolderDrop::Into
    );
}

#[test]
fn a_placed_folder_joins_the_targets_run_only() {
    let shown = order_folders(server_order(), &[], FolderSort::Custom, &slash());
    // Travel dropped after Clients: the top-level run, sub-folders apart.
    assert_eq!(
        placed_run(&shown, &gmail(), "Travel", "Clients", false).unwrap(),
        ["Travel", "Clients"]
    );
    // A folder just moved up from a sub-folder, placed before Travel.
    let mut moved = server_order();
    moved[5].path = "Beta".into();
    let shown = order_folders(moved, &[], FolderSort::Custom, &slash());
    assert_eq!(
        placed_run(&shown, &gmail(), "Beta", "Travel", false).unwrap(),
        ["Clients", "Beta", "Travel"]
    );
    // The main folders are a run of their own.
    assert_eq!(
        placed_run(&shown, &gmail(), "Archive", "INBOX", false).unwrap(),
        ["Archive", "INBOX", "Sent"]
    );
}

#[test]
fn a_selection_knows_its_account_and_moves_to_a_renumbered_one() {
    use super::types::Sel;
    let k = FolderKind::Sent;
    let cases = [
        (
            Sel::Folder(3, "a".into()),
            Some(3),
            Sel::Folder(9, "a".into()),
        ),
        (Sel::UnifiedInbox(3), Some(3), Sel::UnifiedInbox(9)),
        (
            Sel::UnifiedFolder(3, "a".into()),
            Some(3),
            Sel::UnifiedFolder(9, "a".into()),
        ),
        (
            Sel::AccountFiltered(3, "a".into()),
            Some(3),
            Sel::AccountFiltered(9, "a".into()),
        ),
        (
            Sel::Tag(Some(3), "t".into()),
            Some(3),
            Sel::Tag(Some(9), "t".into()),
        ),
        (
            Sel::Favorite(3, "a".into()),
            Some(3),
            Sel::Favorite(9, "a".into()),
        ),
        (
            Sel::UnifiedKindRow(k, 3),
            Some(3),
            Sel::UnifiedKindRow(k, 9),
        ),
        (Sel::Tag(None, "t".into()), None, Sel::Tag(None, "t".into())),
        (Sel::None, None, Sel::None),
        (Sel::Unified, None, Sel::Unified),
        (Sel::Outbox, None, Sel::Outbox),
        (Sel::UnifiedKind(k), None, Sel::UnifiedKind(k)),
        (Sel::Smart("s".into()), None, Sel::Smart("s".into())),
    ];
    for (sel, account, moved) in cases {
        assert_eq!(sel.account(), account, "{sel:?}");
        assert_eq!(sel.with_account(9), moved);
    }
}

#[test]
fn unified_rows_have_titles_icons_and_messages() {
    use super::types::{
        chevron_icon, kind_label, row_icon, row_title, select_msg, toggle_msg, Slot, UnifiedRow,
    };
    let dbg = |m: super::SidebarInput| format!("{m:?}");
    let inbox = UnifiedRow::Kind(FolderKind::Inbox);
    let sent = UnifiedRow::Kind(FolderKind::Sent);
    assert_eq!(row_title(inbox), "Inboxes");
    assert_eq!(row_title(sent), "Sent");
    assert_eq!(row_title(UnifiedRow::Filtered), "Filters");
    assert_eq!(row_title(UnifiedRow::Smart), "Smart Folders");
    assert_eq!(row_icon(inbox), "mail-inbox-symbolic");
    assert_eq!(row_icon(UnifiedRow::Favorites), "user-bookmarks-symbolic");
    assert_eq!(
        row_icon(UnifiedRow::Contacts),
        "x-office-address-book-symbolic"
    );
    assert_eq!(dbg(select_msg(inbox)), "UnifiedRowSelected");
    assert_eq!(dbg(select_msg(sent)), "UnifiedKindRowSelected(Sent)");
    assert_eq!(dbg(select_msg(UnifiedRow::Smart)), "ToggleSmartExpand");
    assert_eq!(dbg(select_msg(UnifiedRow::Contacts)), "ContactsRowClicked");
    assert_eq!(dbg(toggle_msg(inbox)), "ToggleUnifiedExpand");
    assert_eq!(dbg(toggle_msg(sent)), "ToggleKindExpand(Sent)");
    assert_eq!(
        dbg(toggle_msg(UnifiedRow::Filtered)),
        "ToggleFilteredExpand(Unified)"
    );
    assert_eq!(
        dbg(toggle_msg(UnifiedRow::Tags)),
        "ToggleTagsExpand(Unified)"
    );
    assert_eq!(chevron_icon(true), "pan-down-symbolic");
    assert_eq!(chevron_icon(false), "pan-end-symbolic");
    assert_eq!(kind_label(FolderKind::Custom), "Folder");
    assert_eq!(kind_label(FolderKind::Templates), "Folder");
    assert_eq!(Slot::Unified.account(), None);
    assert_eq!(Slot::Account(4).account(), Some(4));
}

#[test]
fn account_initials_take_two_words_then_the_address() {
    use super::widgets::account_initials;
    assert_eq!(account_initials("jane doe smith", "x@y"), "JD");
    assert_eq!(account_initials("  émile  ", "x@y"), "É");
    assert_eq!(account_initials("", "work@example.org"), "W");
    assert_eq!(account_initials("   ", ""), "?");
}

#[test]
fn a_namespace_prefix_counts_as_the_top_level() {
    let dovecot = dot("INBOX.");
    assert_eq!(dovecot.parent("INBOX.Foo"), "");
    assert_eq!(dovecot.parent("INBOX.Foo.Bar"), "INBOX.Foo");
    assert_eq!(dovecot.parent("Top"), "");
}

#[test]
fn custom_parents_and_dragged_folder_payloads() {
    use super::dnd::dragged_folder;
    use super::tree::custom_parent;
    let custom = ["A", "A/B", "AB", "C.D"];
    let s = slash();
    assert_eq!(custom_parent("A/B/C", custom, &s), Some("A/B".to_string()));
    assert_eq!(custom_parent("AB/C", custom, &s), Some("AB".to_string()));
    assert_eq!(custom_parent("C.D.E", custom, &s), None);
    assert_eq!(
        custom_parent("C.D.E", custom, &dot("")),
        Some("C.D".to_string())
    );
    assert_eq!(custom_parent("A", custom, &s), None);
    assert_eq!(
        dragged_folder("vireo-folder\t3\tA/B"),
        Some((3, "A/B", false))
    );
    assert_eq!(
        dragged_folder("vireo-folder-main\t2\tINBOX"),
        Some((2, "INBOX", true))
    );
    assert_eq!(dragged_folder("vireo-folder\tx\tA"), None);
    assert_eq!(dragged_folder("vireo-folder\t3"), None);
    assert_eq!(dragged_folder("other"), None);
}

#[test]
fn arrange_keeps_ranked_items_and_slots_the_rest_after_their_predecessor() {
    use super::tree::arrange;
    // 3 and 1 were placed, in that order; each other item follows the
    // placed item with the nearest smaller key (4 after 3).
    let rank = |i: usize| match i {
        3 => Some(0),
        1 => Some(1),
        _ => None,
    };
    assert_eq!(arrange(vec![0, 1, 2, 3, 4], rank, |i| i), [0, 3, 4, 1, 2]);
    assert_eq!(arrange(vec![2, 0], |_| None, |i| i), [0, 2]);
}
