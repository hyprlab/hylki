//! Drag and drop in the sidebar: messages dropped on folders, folders
//! moved and reordered. The payload prefixes are shared with message_row.rs.

use super::tree::path_is_under;
use super::*;

/// The dragged messages in a drop payload: the marker "vireo-move" followed by
/// one tab-separated (account, folder, uid, id) group per message. A drag from a
/// multi-selection carries every selected message (#23); anything malformed
/// yields nothing rather than a partial move.
pub(super) fn parse_move_payload(payload: &str) -> Vec<(u32, u32, u32, u32)> {
    let parts: Vec<&str> = payload.split('\t').collect();
    if parts.first() != Some(&"vireo-move") || parts.len() < 5 || parts.len() % 4 != 1 {
        return Vec::new();
    }
    let items: Vec<(u32, u32, u32, u32)> = parts[1..]
        .chunks(4)
        .filter_map(|c| {
            Some((
                c[0].parse().ok()?,
                c[1].parse().ok()?,
                c[2].parse().ok()?,
                c[3].parse().ok()?,
            ))
        })
        .collect();
    // All or nothing: a group that won't parse means the payload isn't ours.
    if items.len() * 4 + 1 == parts.len() {
        items
    } else {
        Vec::new()
    }
}

/// A drop target that moves a dragged message into `dest_path` on account `id`.
pub(super) fn folder_drop_target(
    id: u32,
    dest_path: String,
    sender: &ComponentSender<Sidebar>,
) -> gtk::DropTarget {
    let drop = gtk::DropTarget::new(gtk::glib::types::Type::STRING, gtk::gdk::DragAction::MOVE);
    let ds = sender.input_sender().clone();
    drop.connect_drop(move |_, value, _, _| {
        if let Ok(payload) = value.get::<String>() {
            let _ = ds.send(SidebarInput::DropOnFolder {
                account_id: id,
                path: dest_path.clone(),
                payload,
            });
            return true;
        }
        false
    });
    drop
}

/// Where a drag over one of an account's own folder rows would land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FolderDrop {
    /// Just before or after the row, among its siblings.
    Before,
    After,
    /// Into the row: messages move there, a folder moves under it.
    Into,
    Refuse,
}

/// A dragged folder, read from its payload: account, path, and whether it
/// is a main folder (Inbox, Sent…), which can be reordered but not moved.
pub(super) fn dragged_folder(payload: &str) -> Option<(u32, &str, bool)> {
    let (main, rest) = match payload.strip_prefix("vireo-folder-main\t") {
        Some(rest) => (true, rest),
        None => (false, payload.strip_prefix("vireo-folder\t")?),
    };
    let (account, path) = rest.split_once('\t')?;
    Some((account.parse().ok()?, path, main))
}

/// What a drop on a folder row does, from the payload and how far down the
/// row (0 to 1) the pointer is. A main folder only reorders among the main
/// folders, so it splits a row at half. A custom folder dropped on the top
/// or bottom third of a custom folder goes in the gap there, at that
/// folder's level: among its siblings if they are its own, moved on the
/// server to that level if not. The middle third moves it inside. A gap
/// under a parent the account doesn't list (Gmail's "[Gmail]") takes only
/// the folders already there. `custom` is the account's custom folders.
pub(super) fn folder_drop_zone(
    payload: &str,
    account_id: u32,
    target: &Folder,
    custom: &[String],
    hierarchy: &FolderHierarchy,
    frac: f64,
) -> FolderDrop {
    let Some((src_account, src, main)) = dragged_folder(payload) else {
        return FolderDrop::Into;
    };
    if src_account != account_id || src == target.path {
        return FolderDrop::Refuse;
    }
    let target_custom = target.kind == FolderKind::Custom;
    if main {
        return match (target_custom, frac < 0.5) {
            (true, _) => FolderDrop::Refuse,
            (false, true) => FolderDrop::Before,
            (false, false) => FolderDrop::After,
        };
    }
    // Not among the main folders, and never inside itself.
    if !target_custom || path_is_under(&target.path, src, hierarchy) {
        return FolderDrop::Refuse;
    }
    // Into it only where the server lets the folder hold others (#554);
    // elsewhere the whole row is its two edges.
    let into = hierarchy.nests() && !target.childless;
    if into && (1.0 / 3.0..=2.0 / 3.0).contains(&frac) {
        return FolderDrop::Into;
    }
    let level = hierarchy.parent(&target.path);
    if level != hierarchy.parent(src) {
        let allowed = if level.is_empty() {
            hierarchy.top_level
        } else {
            custom.contains(&level)
        };
        if !allowed {
            return FolderDrop::Refuse;
        }
    }
    if frac < if into { 1.0 / 3.0 } else { 0.5 } {
        FolderDrop::Before
    } else {
        FolderDrop::After
    }
}

/// The row a drop at `y` (in the list's coordinates) is aimed at: the
/// shown row under the pointer, or the nearest one when the pointer is in
/// the gap between two rows.
fn row_near(list: &gtk::ListBox, y: f64) -> Option<(i32, gtk::graphene::Rect)> {
    let mut best: Option<(f64, i32, gtk::graphene::Rect)> = None;
    let mut i = 0;
    while let Some(row) = list.row_at_index(i) {
        if row.is_visible() {
            if let Some(b) = row.compute_bounds(list) {
                let (top, bottom) = (b.y() as f64, (b.y() + b.height()) as f64);
                let off = if y < top {
                    top - y
                } else if y > bottom {
                    y - bottom
                } else {
                    0.0
                };
                if best.as_ref().is_none_or(|(d, ..)| off < *d) {
                    best = Some((off, i, b));
                }
            }
        }
        i += 1;
    }
    best.filter(|(d, ..)| *d <= 8.0).map(|(_, i, b)| (i, b))
}

/// Where the line for a drop before (`after` false) or after row `i` goes:
/// midway through the gap on that side. After a folder with sub-folders
/// shown, that is past the last of them, which is where the folder lands.
fn drop_line_y(
    list: &gtk::ListBox,
    folders: &[Folder],
    hierarchy: &FolderHierarchy,
    i: i32,
    after: bool,
) -> Option<f64> {
    let shown = |j: i32| {
        list.row_at_index(j)
            .filter(|r| r.is_visible())
            .and_then(|r| r.compute_bounds(list))
    };
    let edge_between =
        |upper: Option<gtk::graphene::Rect>, lower: Option<gtk::graphene::Rect>| match (
            upper, lower,
        ) {
            (Some(u), Some(l)) => Some(((u.y() + u.height() + l.y()) / 2.0) as f64),
            (Some(u), None) => Some((u.y() + u.height()) as f64),
            (None, Some(l)) => Some(l.y() as f64),
            (None, None) => None,
        };
    let n = folders.len() as i32;
    if !after {
        let upper = (0..i).rev().find_map(&shown);
        return edge_between(upper, shown(i));
    }
    let path = &folders.get(i as usize)?.path;
    let mut last = i;
    while last + 1 < n && path_is_under(&folders[(last + 1) as usize].path, path, hierarchy) {
        last += 1;
    }
    let upper = (i..=last).rev().find_map(&shown);
    let lower = (last + 1..n).find_map(&shown);
    edge_between(upper, lower)
}

/// Drag-and-drop on one of an account's folder lists (its main folders, or
/// its custom ones), with ONE target for the whole list that finds the row
/// from the pointer. Messages, and a folder dropped on a folder it is not a
/// sibling of (or on the middle third of a sibling), go
/// into that folder as before, the row outlined. Anywhere else a folder
/// is reordered among its siblings, and `line` shows the gap it will land
/// in. `folders` is the list's rows in order.
pub(super) fn attach_folder_list_drop(
    list: &gtk::ListBox,
    line: &gtk::Box,
    id: u32,
    folders: Vec<Folder>,
    custom: Vec<String>,
    hierarchy: FolderHierarchy,
    sender: &ComponentSender<Sidebar>,
) {
    let drop = gtk::DropTarget::new(gtk::glib::types::Type::STRING, gtk::gdk::DragAction::MOVE);
    // Read as the drag comes in, so the hover can tell a folder from mail.
    drop.set_preload(true);
    list.add_css_class("folder-drop-list");
    let depths: Vec<usize> = {
        let refs: Vec<&Folder> = folders
            .iter()
            .filter(|f| f.kind == FolderKind::Custom)
            .collect();
        folders
            .iter()
            .map(|f| {
                if f.kind == FolderKind::Custom {
                    folder_depth(f, &refs, &hierarchy)
                } else {
                    0
                }
            })
            .collect()
    };
    let folders = std::rc::Rc::new(folders);
    // What a drop at `y` does, and on which row.
    let aim = {
        let list = list.clone();
        let folders = folders.clone();
        let hierarchy = hierarchy.clone();
        std::rc::Rc::new(move |payload: &str, y: f64| -> Option<(i32, FolderDrop)> {
            let (i, b) = row_near(&list, y)?;
            let f = folders.get(i as usize)?;
            let frac = ((y - b.y() as f64) / b.height().max(1.0) as f64).clamp(0.0, 1.0);
            Some((
                i,
                folder_drop_zone(payload, id, f, &custom, &hierarchy, frac),
            ))
        })
    };
    // Clear the last hover's marks.
    let clear = {
        let list = list.clone();
        let line = line.clone();
        move || {
            line.set_visible(false);
            let mut i = 0;
            while let Some(row) = list.row_at_index(i) {
                row.remove_css_class("drop-into");
                i += 1;
            }
        }
    };
    {
        let aim = aim.clone();
        let clear = clear.clone();
        let list = list.clone();
        let line = line.clone();
        let folders = folders.clone();
        drop.connect_motion(move |t, _, y| {
            clear();
            let payload = t
                .value()
                .and_then(|v| v.get::<String>().ok())
                .unwrap_or_default();
            let Some((i, at)) = aim(&payload, y) else {
                return gtk::gdk::DragAction::empty();
            };
            match at {
                FolderDrop::Refuse => return gtk::gdk::DragAction::empty(),
                FolderDrop::Into => {
                    if let Some(row) = list.row_at_index(i) {
                        row.add_css_class("drop-into");
                    }
                }
                FolderDrop::Before | FolderDrop::After => {
                    if let Some(ly) =
                        drop_line_y(&list, &folders, &hierarchy, i, at == FolderDrop::After)
                    {
                        // Indented to the level the folder lands at.
                        line.set_margin_start(20 + 14 * depths[i as usize].min(4) as i32);
                        line.set_margin_top((ly - 1.0).max(0.0) as i32);
                        line.set_visible(true);
                    }
                }
            }
            gtk::gdk::DragAction::MOVE
        });
    }
    {
        let clear = clear.clone();
        drop.connect_leave(move |_| clear());
    }
    let ds = sender.input_sender().clone();
    drop.connect_drop(move |_, value, _, y| {
        clear();
        let Ok(payload) = value.get::<String>() else {
            return false;
        };
        let Some((i, at)) = aim(&payload, y) else {
            return false;
        };
        let Some(target) = folders.get(i as usize).map(|f| f.path.clone()) else {
            return false;
        };
        match at {
            FolderDrop::Refuse => false,
            FolderDrop::Into => {
                let _ = ds.send(SidebarInput::DropOnFolder {
                    account_id: id,
                    path: target,
                    payload,
                });
                true
            }
            FolderDrop::Before | FolderDrop::After => {
                let Some((_, src, _)) = dragged_folder(&payload) else {
                    return false;
                };
                let _ = ds.send(SidebarInput::ReorderFolder {
                    account_id: id,
                    path: src.to_string(),
                    target,
                    after: at == FolderDrop::After,
                });
                true
            }
        }
    });
    list.add_controller(drop);
}

/// A folder list with room for the insertion line over it: the line is
/// drawn on top, never taking space, and moved to the gap a drag aims at.
pub(super) fn with_drop_line(list: &gtk::ListBox) -> (gtk::Overlay, gtk::Box) {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    line.add_css_class("folder-drop-line");
    line.set_valign(gtk::Align::Start);
    line.set_height_request(2);
    line.set_margin_end(14);
    line.set_can_target(false);
    line.set_visible(false);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(list));
    overlay.add_overlay(&line);
    (overlay, line)
}

/// Let a folder row be picked up, carrying `payload` and a picture of the
/// row under the pointer.
pub(super) fn folder_drag_source(row: &gtk::ListBoxRow, payload: String) -> gtk::DragSource {
    let drag = gtk::DragSource::new();
    drag.set_actions(gtk::gdk::DragAction::MOVE);
    let r = row.downgrade();
    drag.connect_prepare(move |src, x, y| {
        if let Some(row) = r.upgrade() {
            src.set_icon(
                Some(&gtk::WidgetPaintable::new(Some(&row))),
                x as i32,
                y as i32,
            );
        }
        Some(gtk::gdk::ContentProvider::for_value(&payload.to_value()))
    });
    drag
}

impl Sidebar {
    pub(super) fn on_reorder_folder(
        &mut self,
        account_id: u32,
        path: String,
        target: String,
        after: bool,
        sender: ComponentSender<Self>,
    ) {
        let Some(section) = self.sections.iter().find(|s| s.account.id == account_id) else {
            return;
        };
        let h = &section.hierarchy;
        let level = h.parent(&target);
        let custom = section
            .folders
            .iter()
            .any(|f| f.path == path && f.kind == FolderKind::Custom);
        if !custom || level == h.parent(&path) {
            // Already one of the target's siblings: only the order changes.
            if let Some(paths) = placed_run(&section.folders, h, &path, &target, after) {
                let _ = sender.output(SidebarOutput::ReorderFolders { account_id, paths });
            }
        } else {
            // From another level: the server moves it there first.
            let _ = sender.output(SidebarOutput::PlaceFolder {
                account_id,
                path,
                dest: level,
                target,
                after,
            });
        }
    }

    pub(super) fn on_drop_on_folder(
        &mut self,
        dest_account: u32,
        dest: String,
        payload: String,
        sender: ComponentSender<Self>,
    ) {
        // A dragged folder, not messages: reparent it (#51). Folders
        // never cross accounts — mailboxes belong to one server.
        if let Some(rest) = payload.strip_prefix("vireo-folder\t") {
            let mut it = rest.splitn(2, '\t');
            let src_account = it.next().and_then(|s| s.parse::<u32>().ok());
            let src_path = it.next().map(String::from);
            if let (Some(src_account), Some(src_path)) = (src_account, src_path) {
                if src_account == dest_account && src_path != dest {
                    let _ = sender.output(SidebarOutput::MoveFolder {
                        account_id: dest_account,
                        path: src_path,
                        dest,
                    });
                }
            }
            return;
        }
        let items = parse_move_payload(&payload);
        // "" is the Folders header (a folder-move destination only);
        // messages need a real mailbox.
        if !items.is_empty() && !dest.is_empty() {
            let _ = sender.output(SidebarOutput::MoveMessages {
                dest_account,
                dest,
                items,
            });
        }
    }
}
