//! The folder tree, without GTK: hierarchy, sibling runs, the order the
//! sidebar lists an account's folders in, and the Inbox nesting rule.

use super::*;

/// Width of the tree-expander slot: chevron button and leaf spacer alike, so
/// folder names at one depth line up whether or not they have children (#51).
pub(crate) const TREE_EXPANDER_WIDTH: i32 = 18;

/// Whether `child` sits under `parent` in the mailbox hierarchy: a strict
/// descendant by the server's own delimiter (#554), so "v1.2" is no child
/// of "v1" on a '/' server.
pub(super) fn path_is_under(child: &str, parent: &str, h: &FolderHierarchy) -> bool {
    h.is_under(child, parent)
}

/// Which run of siblings a folder is ordered within: the main folders are
/// one run, and each custom folder shares its run with the folders under the
/// same parent on the server ("" for the top level).
#[derive(Clone, Debug, PartialEq, Eq)]
enum SiblingRun {
    Main,
    Custom(String),
}

fn run(h: &FolderHierarchy, folder: &Folder) -> SiblingRun {
    if folder.kind == FolderKind::Custom {
        SiblingRun::Custom(h.parent(&folder.path))
    } else {
        SiblingRun::Main
    }
}

/// The run `target` belongs to, in the order `shown` lists it, with `moving`
/// put just before or after `target`: the new order to keep once a folder
/// has been dropped beside it. `moving` must already be in that run's
/// place on the server (a sibling, or moved there).
pub(crate) fn placed_run(
    shown: &[Folder],
    hierarchy: &FolderHierarchy,
    moving: &str,
    target: &str,
    after: bool,
) -> Option<Vec<String>> {
    let target_run = run(hierarchy, shown.iter().find(|f| f.path == target)?);
    let mut paths: Vec<String> = shown
        .iter()
        .filter(|f| f.path != moving && run(hierarchy, f) == target_run)
        .map(|f| f.path.clone())
        .collect();
    let at = paths.iter().position(|p| p == target)?;
    paths.insert(if after { at + 1 } else { at }, moving.to_string());
    Some(paths)
}

/// A custom folder's nearest listed ancestor, the parent it is drawn under,
/// among the account's custom folder paths.
pub(super) fn custom_parent<'a>(
    path: &str,
    custom: impl IntoIterator<Item = &'a str>,
    h: &FolderHierarchy,
) -> Option<String> {
    custom
        .into_iter()
        .filter(|g| path_is_under(path, g, h))
        .max_by_key(|g| g.len())
        .map(String::from)
}

/// An account's folders as the sidebar lists them: the main folders first,
/// in the server's order by kind, then the custom folders as a tree, each
/// parent's children sorted by `sort`. By name means the name shown (so
/// Gmail's "[Gmail]/Important" sorts as "Important", as Thunderbird has it).
/// Custom Order keeps the folders named in `order` in the order the user
/// dragged them into (see [`arrange`] for where the others go); the main
/// folders keep theirs whatever the sort.
pub(crate) fn order_folders(
    folders: Vec<Folder>,
    order: &[String],
    sort: crate::config::FolderSort,
    h: &FolderHierarchy,
) -> Vec<Folder> {
    use crate::config::FolderSort;
    let nest = nests_in_inbox(&folders, h);
    let rank = |f: &Folder| order.iter().position(|p| *p == f.path);
    let (main, custom): (Vec<Folder>, Vec<Folder>) = folders
        .into_iter()
        .partition(|f| f.kind != FolderKind::Custom);
    let main_order = arrange((0..main.len()).collect(), |i| rank(&main[i]), |i| i);

    // The tree, flattened depth-first so every folder follows its parent,
    // each parent's children in their arranged order.
    let parents: Vec<Option<String>> = custom
        .iter()
        .map(|f| custom_parent(&f.path, custom.iter().map(|g| g.path.as_str()), h))
        .collect();
    let by_name = |i: usize| (custom[i].name.to_lowercase(), custom[i].path.clone());
    fn walk(
        parent: Option<&str>,
        custom: &[Folder],
        parents: &[Option<String>],
        sort: &dyn Fn(Vec<usize>) -> Vec<usize>,
        out: &mut Vec<usize>,
    ) {
        let children: Vec<usize> = (0..custom.len())
            .filter(|&i| parents[i].as_deref() == parent)
            .collect();
        for i in sort(children) {
            out.push(i);
            walk(Some(&custom[i].path), custom, parents, sort, out);
        }
    }
    let sort = |mut children: Vec<usize>| match sort {
        FolderSort::Custom => arrange(children, |i| rank(&custom[i]), by_name),
        FolderSort::NameAsc => {
            children.sort_by_key(|&i| by_name(i));
            children
        }
        FolderSort::NameDesc => {
            children.sort_by_key(|&i| std::cmp::Reverse(by_name(i)));
            children
        }
        // As they came in: sorted by path, like the server's listing.
        FolderSort::Path => children,
    };
    let mut flat = Vec::with_capacity(custom.len());
    walk(None, &custom, &parents, &sort, &mut flat);

    // The folders inside the Inbox follow it (#345), in their tree order;
    // the rest of the tree comes after the main folders.
    let (inside, outside): (Vec<usize>, Vec<usize>) = flat
        .into_iter()
        .partition(|&i| nest && under_inbox(&custom[i].path, h));
    let mut main_slots: Vec<Option<Folder>> = main.into_iter().map(Some).collect();
    let mut custom_slots: Vec<Option<Folder>> = custom.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(main_slots.len() + custom_slots.len());
    let mut placed = inside.is_empty();
    for i in main_order {
        let Some(f) = main_slots[i].take() else {
            continue;
        };
        let inbox = f.kind == FolderKind::Inbox;
        out.push(f);
        if inbox && !placed {
            out.extend(inside.iter().filter_map(|&j| custom_slots[j].take()));
            placed = true;
        }
    }
    out.extend(
        inside
            .iter()
            .chain(&outside)
            .filter_map(|&j| custom_slots[j].take()),
    );
    out
}

/// One run of siblings in order: those with a `rank` (their place in the
/// user's saved order) by it, and each of the rest just after the sibling
/// that comes before it by `key`, the default order. So a folder the user
/// never placed, such as one created since, lands where it would have by
/// name among its neighbours rather than at the end.
pub(super) fn arrange<K: Ord>(
    items: Vec<usize>,
    rank: impl Fn(usize) -> Option<usize>,
    key: impl Fn(usize) -> K,
) -> Vec<usize> {
    let (mut out, mut rest): (Vec<usize>, Vec<usize>) =
        items.into_iter().partition(|&i| rank(i).is_some());
    out.sort_by_key(|&i| rank(i));
    // In default order, so each one can follow the one placed before it.
    rest.sort_by_key(|&i| key(i));
    for i in rest {
        let k = key(i);
        let at = out
            .iter()
            .enumerate()
            .filter(|(_, &o)| key(o) < k)
            .max_by(|(_, &a), (_, &b)| key(a).cmp(&key(b)))
            .map_or(0, |(pos, _)| pos + 1);
        out.insert(at, i);
    }
    out
}

/// Whether `path` is inside the Inbox: `INBOX`, then the delimiter (#345).
pub(super) fn under_inbox(path: &str, h: &FolderHierarchy) -> bool {
    let Some(d) = h.delimiter else {
        return false;
    };
    path.get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case("INBOX"))
        && path[5..].starts_with(d)
}

/// Whether an account lists the folders inside its Inbox under the Inbox
/// row (#345), as Roundcube and Apple Mail do. Only when it has folders
/// outside the Inbox too: a server that puts every folder inside it (an
/// `INBOX.` namespace, as on Courier and older Dovecot) keeps them at the
/// top of the Folders section, where they always were.
pub(super) fn nests_in_inbox(folders: &[Folder], h: &FolderHierarchy) -> bool {
    let mut custom = folders.iter().filter(|f| f.kind == FolderKind::Custom);
    let (mut inside, mut outside) = (false, false);
    for f in custom.by_ref() {
        if under_inbox(&f.path, h) {
            inside = true;
        } else {
            outside = true;
        }
    }
    inside && outside
}

/// A folder listed with the main folders: a main folder, or, when the
/// account nests them, one inside the Inbox.
pub(super) fn is_main_row(f: &Folder, nest: bool, h: &FolderHierarchy) -> bool {
    f.kind != FolderKind::Custom || (nest && under_inbox(&f.path, h))
}

/// Whether a folder row is hidden because some ancestor node is collapsed.
pub(super) fn hidden_by_collapse(
    path: &str,
    collapsed: &std::collections::HashSet<String>,
    h: &FolderHierarchy,
) -> bool {
    collapsed.iter().any(|p| path_is_under(path, p, h))
}

/// Nesting depth of a custom folder: how many of the *other listed* folders are
/// ancestors of its IMAP path. Working from listed ancestors (rather than
/// counting delimiters) keeps namespace prefixes honest — "INBOX.Clients" is
/// top-level on a Dovecot-style server because "INBOX" isn't in the custom
/// list, while "INBOX.Clients.Acme" is one level down because "INBOX.Clients"
/// is.
pub(crate) fn folder_depth(folder: &Folder, all: &[&Folder], h: &FolderHierarchy) -> usize {
    all.iter()
        .filter(|g| g.id != folder.id && h.is_under(&folder.path, &g.path))
        .count()
}
