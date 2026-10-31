//! One account's section of the sidebar: its header, its main folders, its
//! custom folders under a Folders heading, and Add Folder.

use super::dnd::{attach_folder_list_drop, folder_drag_source, folder_drop_target, with_drop_line};
use super::menus::{attach_folder_context_menu, folder_sort_menu, show_sidebar_menu};
use super::tree::{hidden_by_collapse, is_main_row, nests_in_inbox, path_is_under};
use super::types::chevron_icon;
use super::widgets::{
    account_gravatar, build_folder_row, filter_icon, heading_chevron, section_heading,
    with_unread_overlay, wrap_row_in_revealer, ROW_LEFT_INSET,
};
use super::*;

impl Sidebar {
    /// Build one account's section into `accounts_box`: the header, then
    /// the folders in a revealer the header folds.
    pub(super) fn build_account_section(
        &mut self,
        section_idx: usize,
        section: &SectionData,
        accounts_box: &gtk::Box,
        container: &gtk::Box,
        sender: &ComponentSender<Self>,
    ) {
        let id = section.account.id;
        let (header, chevron, folded) =
            self.build_account_header(section_idx, section, container, sender);
        accounts_box.append(&header);

        // Animated folder list.
        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        revealer.set_transition_duration(0);
        revealer.set_reveal_child(!folded);

        // Split folders: essential (Inbox/Sent/Trash/Archive/…) are always
        // shown; user-created "custom" folders are tucked under a collapsible
        // "Folders" section. `section.folders` is already essential-first, so
        // the essential list holds row indices 0..E and the custom list E..
        // The folders inside the Inbox go with it, under its row,
        // when the account nests them (#345).
        let h = &section.hierarchy;
        let nest = nests_in_inbox(&section.folders, h);
        let essential: Vec<&Folder> = section
            .folders
            .iter()
            .filter(|f| is_main_row(f, nest, h))
            .collect();
        let custom: Vec<&Folder> = section
            .folders
            .iter()
            .filter(|f| !is_main_row(f, nest, h))
            .collect();
        let e = essential.len() as i32;
        let inside: Vec<&Folder> = essential
            .iter()
            .copied()
            .filter(|f| f.kind == FolderKind::Custom)
            .collect();
        let collapsed_nodes = self.tree_collapsed.get(&id).cloned().unwrap_or_default();

        let (list, list_box) = self.build_main_folder_list(
            section,
            &essential,
            &inside,
            nest,
            &collapsed_nodes,
            sender,
        );
        let (custom_list, custom_revealer, custom_chevron, folders_heading) =
            self.build_custom_folders(section, &custom, e, &collapsed_nodes, sender);
        // Only where the account can have folders at the top (not POP3).
        let add_btn = section
            .hierarchy
            .top_level
            .then(|| self.add_folder_button(id, sender));

        let wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrap.append(&list_box);
        // The account's own Tags section, above its folder hierarchy:
        // every tag scoped to this account, there whatever the unified
        // section shows. (Its filtered folders are marked in place in
        // the hierarchy instead — see `filter_icon`.)
        if !self.tags.is_empty() {
            self.build_tags_section(&wrap, Slot::Account(id), sender);
        }
        if !custom.is_empty() {
            if let Some(h) = &folders_heading {
                wrap.append(h);
            }
            wrap.append(&custom_revealer);
        }
        if let Some(add_btn) = &add_btn {
            wrap.append(add_btn);
        }
        revealer.set_child(Some(&wrap));
        accounts_box.append(&revealer);

        self.revealers.insert(id, revealer);
        self.chevrons.insert(id, chevron);
        self.folder_lists.insert(id, list);
        self.custom_folder_lists.insert(id, custom_list);
        self.custom_revealers.insert(id, custom_revealer);
        self.custom_chevrons.insert(id, custom_chevron);
    }

    /// The account's header: avatar circle, name and chevron, with its
    /// clicks, menu and drag hover. Returns the header, its chevron and
    /// whether the section shows folded.
    fn build_account_header(
        &mut self,
        section_idx: usize,
        section: &SectionData,
        container: &gtk::Box,
        sender: &ComponentSender<Self>,
    ) -> (gtk::Button, gtk::Image, bool) {
        let id = section.account.id;
        // Header: avatar circle + name/email on the left, chevron on the right.
        let header = gtk::Button::new();
        header.add_css_class("flat");
        header.add_css_class("account-header");
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 10);

        // A configured label wins (same as the All Inboxes sub-rows);
        // otherwise the account's name, then its address.
        let name_str = if !section.account.label.trim().is_empty()
            && section.account.label != section.account.email
        {
            section.account.label.clone()
        } else if section.account.name.trim().is_empty() {
            section.account.email.clone()
        } else {
            section.account.name.clone()
        };

        let circle = account_circle(section, &name_str);
        // While this account's section is collapsed its Inbox row (and the
        // chip on it) is hidden inside the revealer, so surface the inbox
        // unread count on the avatar instead — mirroring how the collapsed
        // "All Inboxes" rail badges its icon.
        let inbox_unread = section
            .folders
            .iter()
            .find(|f| f.kind == FolderKind::Inbox)
            .map(|f| f.unread)
            .unwrap_or(0);
        let (circle_overlay, circle_badge) = with_unread_overlay(&circle, inbox_unread);
        circle_overlay.set_halign(gtk::Align::Center);
        circle_overlay.set_hexpand(false);
        // Folded for the rail (Settings → Sidebar → Icon rail) reads as
        // collapsed here, without touching the saved state.
        let folded = self.account_shown_folded(section);
        circle_badge.set_visible(folded && inbox_unread > 0);
        self.account_circle_badges.insert(id, circle_badge);
        if !self.collapsed && self.chevrons_left {
            // Leaves room for the overlaid disclosure chevron, which
            // would otherwise sit right on top of the avatar — a bit
            // more than the minimum, so the two aren't touching.
            circle_overlay.set_margin_start(ROW_LEFT_INSET + 8);
        }

        // Chevron is tracked even when collapsed so per-account toggles
        // still update an icon; it's only shown in the expanded layout —
        // leading (overlaid on the header's left edge, reserving no
        // layout space, like the All Inboxes row's) or classic trailing,
        // per Settings → Chevron placement.
        let chevron = gtk::Image::from_icon_name(chevron_icon(!folded));
        chevron.set_valign(gtk::Align::Center);
        // A button of its own, as the unified rows' chevrons are: it
        // takes its click before the header does.
        let chev_btn = gtk::Button::new();
        chev_btn.set_child(Some(&chevron));
        chev_btn.add_css_class("flat");
        chev_btn.add_css_class("chevron-btn");
        chev_btn.set_valign(gtk::Align::Center);
        chev_btn.set_tooltip_text(Some(i18n("Show or hide folders").as_str()));
        {
            let s = sender.input_sender().clone();
            chev_btn.connect_clicked(move |_| {
                let _ = s.send(SidebarInput::ToggleCollapseLocal(id));
            });
        }

        if self.collapsed {
            hbox.append(&circle_overlay);
            hbox.set_halign(gtk::Align::Center);
            header.set_tooltip_text(Some(&name_str));
        } else {
            hbox.append(&circle_overlay);
            let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
            vbox.set_hexpand(true);
            vbox.set_valign(gtk::Align::Center);
            // Clearance for the unread chip that overlays the circle's
            // corner while the section is collapsed.
            vbox.set_margin_start(6);
            let name = gtk::Label::new(Some(&name_str));
            name.set_halign(gtk::Align::Start);
            name.set_ellipsize(gtk::pango::EllipsizeMode::End);
            name.add_css_class("account-name");
            let email = gtk::Label::new(Some(&section.account.email));
            email.set_halign(gtk::Align::Start);
            email.set_ellipsize(gtk::pango::EllipsizeMode::End);
            email.add_css_class("account-email");
            vbox.append(&name);
            if !section.account.is_local() {
                vbox.append(&email);
            }
            hbox.append(&vbox);
            if self.chevrons_left {
                chevron.set_pixel_size(15);
                chev_btn.add_css_class("row-disclosure-chevron");
                chev_btn.set_halign(gtk::Align::Start);
                let overlay = gtk::Overlay::new();
                overlay.set_child(Some(&hbox));
                overlay.add_overlay(&chev_btn);
                header.set_child(Some(&overlay));
            } else {
                chev_btn.add_css_class("account-chevron-end");
                hbox.append(&chev_btn);
            }
        }

        if header.child().is_none() {
            header.set_child(Some(&hbox));
        }
        if self.collapsed || self.account_click_folds {
            // The rail's avatar, or every header by choice: a click
            // anywhere opens or folds the account.
            let s = sender.input_sender().clone();
            header.connect_clicked(move |_| {
                let _ = s.send(SidebarInput::ToggleCollapseLocal(id));
            });
        } else {
            // As the unified rows: the chevron opens or folds the account,
            // and so does a double click anywhere else on the header; a
            // plain click there does nothing. The chevron's button sits
            // inside the header's, and a GtkButton takes its click in the
            // capture phase, so the header would claim the release before
            // the chevron saw it: the chevron's press is claimed here
            // instead, and its release folds the account.
            let press = gtk::GestureClick::new();
            press.set_button(gtk::gdk::BUTTON_PRIMARY);
            press.set_propagation_phase(gtk::PropagationPhase::Capture);
            let on_chevron = move |head: &gtk::Button, chev: &gtk::Button, x, y| {
                head.pick(x, y, gtk::PickFlags::DEFAULT)
                    .is_some_and(|w| w == *chev || w.is_ancestor(chev))
            };
            // Whether the press now held down began on the chevron.
            let armed = std::rc::Rc::new(std::cell::Cell::new(false));
            let (head, chev) = (header.downgrade(), chev_btn.downgrade());
            let s = sender.input_sender().clone();
            let a = armed.clone();
            press.connect_pressed(move |g, presses, x, y| {
                let (Some(head), Some(chev)) = (head.upgrade(), chev.upgrade()) else {
                    return;
                };
                a.set(on_chevron(&head, &chev, x, y));
                if a.get() {
                    g.set_state(gtk::EventSequenceState::Claimed);
                } else if presses == 2 {
                    let _ = s.send(SidebarInput::ToggleCollapseLocal(id));
                }
            });
            let (head, chev) = (header.downgrade(), chev_btn.downgrade());
            let s = sender.input_sender().clone();
            let a = armed.clone();
            press.connect_released(move |_, _, x, y| {
                let (Some(head), Some(chev)) = (head.upgrade(), chev.upgrade()) else {
                    return;
                };
                // Pressed and released on the chevron: a click, as the
                // button itself would count it.
                if a.replace(false) && on_chevron(&head, &chev, x, y) {
                    let _ = s.send(SidebarInput::ToggleCollapseLocal(id));
                }
            });
            press.connect_cancel(move |_, _| armed.set(false));
            header.add_controller(press);
        }
        attach_account_menu(&header, section, sender);
        attach_drop_hover(&header, id, sender);
        // The first account sits 10px further below the unified block
        // above it (when there is one), so the two read as separate
        // groups (Jason, 2026-09-13).
        if section_idx == 0 && container.first_child().is_some() {
            header.set_margin_top(10);
        }
        (header, chevron, folded)
    }

    /// The main folders (Inbox, Sent, …, and the folders nested in the
    /// Inbox): the list, and the box that holds it with its drop line.
    fn build_main_folder_list(
        &mut self,
        section: &SectionData,
        essential: &[&Folder],
        inside: &[&Folder],
        nest: bool,
        collapsed_nodes: &HashSet<String>,
        sender: &ComponentSender<Self>,
    ) -> (gtk::ListBox, gtk::Overlay) {
        let id = section.account.id;
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("navigation-sidebar");
        for (index, folder) in essential.iter().enumerate() {
            let icon = filter_icon(section, folder);
            let nested = folder.kind == FolderKind::Custom;
            // With folders under the Inbox, every main row has the
            // expander slot the tree rows have, so their names align:
            // a chevron on the Inbox and on a nested folder with
            // sub-folders, a spacer on the rest.
            let has_children = nest
                && !self.collapsed
                && match folder.kind {
                    FolderKind::Inbox => !inside.is_empty(),
                    FolderKind::Custom => inside
                        .iter()
                        .any(|g| path_is_under(&g.path, &folder.path, &section.hierarchy)),
                    _ => false,
                };
            let lead: Option<gtk::Widget> = if !nest || self.collapsed {
                None
            } else {
                Some(self.tree_lead(id, folder, has_children, collapsed_nodes, sender))
            };
            let depth = if nested {
                folder_depth(folder, inside, &section.hierarchy) + 1
            } else {
                0
            };
            let (row, badge) = build_folder_row(
                folder,
                self.collapsed,
                depth,
                lead.as_ref(),
                self.chevrons_left,
                icon,
                &section.hierarchy,
            );
            if nested {
                // Hidden while the Inbox or a parent is folded, as in
                // the Folders section.
                let hidden = hidden_by_collapse(&folder.path, collapsed_nodes, &section.hierarchy);
                if let Some(rev) = wrap_row_in_revealer(&row, hidden) {
                    self.main_row_revealers
                        .entry(id)
                        .or_default()
                        .insert(index, rev);
                }
                row.set_visible(!hidden);
                if !self.collapsed {
                    let payload = format!("vireo-folder\t{id}\t{}", folder.path);
                    row.add_controller(folder_drag_source(&row, payload));
                }
            } else if !self.collapsed && essential.len() > 1 {
                // The main folders can be put in another order among
                // themselves; where they sit on the server is not
                // theirs to change.
                let payload = format!("vireo-folder-main\t{id}\t{}", folder.path);
                row.add_controller(folder_drag_source(&row, payload));
            }
            list.append(&row);
            if let Some(badge) = badge {
                self.folder_badges.insert((id, folder.id), badge);
            }
        }
        self.main_folders
            .insert(id, essential.iter().map(|f| (*f).clone()).collect());
        // The folders inside the Inbox take folders dropped on them.
        let list_box = self.wire_folder_list(&list, section, essential, inside, 0, sender);
        (list, list_box)
    }

    /// The custom folders as a tree in a revealer, with their Folders (or
    /// Labels) heading when there are any. `e` is how many main folders come
    /// first in `section.folders`.
    fn build_custom_folders(
        &mut self,
        section: &SectionData,
        custom: &[&Folder],
        e: i32,
        collapsed_nodes: &HashSet<String>,
        sender: &ComponentSender<Self>,
    ) -> (gtk::ListBox, gtk::Revealer, gtk::Image, Option<gtk::Widget>) {
        let id = section.account.id;
        // The collapsible custom-folders list + its "Folders" toggle header.
        let custom_list = gtk::ListBox::new();
        custom_list.set_selection_mode(gtk::SelectionMode::Single);
        custom_list.add_css_class("navigation-sidebar");
        let custom_revealer = gtk::Revealer::new();
        let custom_chevron = gtk::Image::from_icon_name(chevron_icon(section.custom_expanded));
        let mut folders_heading: Option<gtk::Widget> = None;
        if !custom.is_empty() {
            for folder in custom {
                let depth = folder_depth(folder, custom, &section.hierarchy);
                // The expander slot (#51): a chevron for folders with
                // sub-folders, an equal-width spacer for leaves so names
                // at one depth stay aligned. Rail mode has no room for
                // either.
                let has_children = custom
                    .iter()
                    .any(|g| path_is_under(&g.path, &folder.path, &section.hierarchy));
                let lead: Option<gtk::Widget> = if self.collapsed {
                    None
                } else {
                    Some(self.tree_lead(id, folder, has_children, collapsed_nodes, sender))
                };
                let (row, badge) = build_folder_row(
                    folder,
                    self.collapsed,
                    depth,
                    lead.as_ref(),
                    self.chevrons_left,
                    filter_icon(section, folder),
                    &section.hierarchy,
                );
                // Hidden while any ancestor is collapsed; the row still
                // exists, so selection indices stay stable. Its content
                // sits in a revealer so user toggles slide open/closed —
                // built with no transition (rebuilds must reach full
                // height in one pass; see the scroll-jump saga), armed to
                // 200ms alongside the section revealers below.
                let hidden = hidden_by_collapse(&folder.path, collapsed_nodes, &section.hierarchy);
                if let Some(rev) = wrap_row_in_revealer(&row, hidden) {
                    self.tree_row_revealers.entry(id).or_default().push(rev);
                }
                row.set_visible(!hidden);
                // Custom folders can be picked up and dropped on a new
                // parent (#51), or on a sibling's edge to reorder them.
                if !self.collapsed {
                    let payload = format!("vireo-folder\t{id}\t{}", folder.path);
                    row.add_controller(folder_drag_source(&row, payload));
                }
                custom_list.append(&row);
                if let Some(badge) = badge {
                    self.folder_badges.insert((id, folder.id), badge);
                }
            }
            self.custom_folders
                .insert(id, custom.iter().map(|f| (*f).clone()).collect());
            let custom_box =
                self.wire_folder_list(&custom_list, section, custom, custom, e, sender);
            custom_revealer.set_child(Some(&custom_box));

            custom_revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
            custom_revealer.set_transition_duration(0);
            custom_revealer.set_reveal_child(section.custom_expanded);

            let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            hb.add_css_class("folder-row");
            let mut folders_chevron: Option<gtk::Button> = None;
            if self.collapsed {
                hb.set_halign(gtk::Align::Center);
                hb.append(&gtk::Image::from_icon_name("folder-symbolic"));
            } else {
                if self.chevrons_left {
                    // A chevron glyph's ink sits further into its canvas
                    // than a regular icon's, so the folder rows' full
                    // inset would land it visually right of the icons
                    // above — a smaller value keeps the same column.
                    custom_chevron.set_margin_start(2);
                }
                let btn = heading_chevron(&custom_chevron);
                btn.set_tooltip_text(Some(
                    if section.gmail {
                        i18n("Show or hide labels")
                    } else {
                        i18n("Show or hide folders")
                    }
                    .as_str(),
                ));
                hb.append(&btn);
                folders_chevron = Some(btn);
                // Gmail's own word for them: its folders are labels.
                let len = custom.len().to_string();
                let heading = if section.gmail {
                    i18n_f("Labels ({len})", &[("len", &len)])
                } else {
                    i18n_f("Folders ({len})", &[("len", &len)])
                };
                let lbl = gtk::Label::new(Some(&heading));
                lbl.set_halign(gtk::Align::Start);
                lbl.set_hexpand(true);
                hb.append(&lbl);
            }
            let folders_toggle = section_heading(
                &hb,
                folders_chevron.as_ref(),
                &[],
                SidebarInput::ToggleCustomFoldersLocal(id),
                sender,
            );
            if self.collapsed {
                folders_toggle.set_tooltip_text(Some(
                    if section.gmail {
                        i18n("Labels")
                    } else {
                        i18n("Folders")
                    }
                    .as_str(),
                ));
            }
            // Right-click: how this account's folders are sorted.
            let click = gtk::GestureClick::new();
            click.set_button(gtk::gdk::BUTTON_SECONDARY);
            let cs = sender.clone();
            let section_c = section.clone();
            let toggle_w = folders_toggle.clone();
            click.connect_pressed(move |_, _, x, y| {
                show_context_menu_with_header(
                    &toggle_w,
                    x,
                    y,
                    Some(i18n("Sort Folders").as_str()),
                    folder_sort_menu(&section_c, &cs),
                );
            });
            folders_toggle.add_controller(click);
            // Dropping a folder on the section header moves it to the
            // account's top level ("" — resolved to the namespace root).
            folders_toggle.add_controller(folder_drop_target(id, String::new(), sender));
            folders_heading = Some(folders_toggle);
        }
        (
            custom_list,
            custom_revealer,
            custom_chevron,
            folders_heading,
        )
    }

    /// "+ Add Folder" at the bottom of the list, for quick creation.
    fn add_folder_button(&self, id: u32, sender: &ComponentSender<Self>) -> gtk::Button {
        let add_btn = gtk::Button::new();
        add_btn.add_css_class("flat");
        add_btn.add_css_class("add-folder-btn");
        let add_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        add_box.add_css_class("folder-row");
        let add_img = gtk::Image::from_icon_name("list-add-symbolic");
        pin_icon_size(&add_img);
        add_box.append(&add_img);
        if self.collapsed {
            add_box.set_halign(gtk::Align::Center);
            add_btn.set_tooltip_text(Some(i18n("Add Folder").as_str()));
        } else {
            if self.chevrons_left {
                add_img.set_margin_start(4);
            }
            let lbl = gtk::Label::new(Some(i18n("Add Folder").as_str()));
            lbl.set_halign(gtk::Align::Start);
            lbl.set_hexpand(true);
            add_box.append(&lbl);
        }
        add_btn.set_child(Some(&add_box));
        let cs = sender.clone();
        add_btn.connect_clicked(move |_| {
            let _ = cs.output(SidebarOutput::Context(CtxAction::NewFolder {
                account_id: id,
                parent: String::new(),
            }));
        });
        add_btn
    }

    /// A tree row's expander slot (#51): for a folder with sub-folders, one
    /// right-pointing caret whose "open" class rotates it 90° via a CSS
    /// transition, so toggling spins smoothly instead of swapping glyphs;
    /// for a leaf, a spacer of the same width so names at one depth line up.
    fn tree_lead(
        &mut self,
        id: u32,
        folder: &Folder,
        has_children: bool,
        collapsed_nodes: &HashSet<String>,
        sender: &ComponentSender<Self>,
    ) -> gtk::Widget {
        if has_children {
            let img = gtk::Image::from_icon_name("pan-end-symbolic");
            img.add_css_class("tree-expander-icon");
            if !collapsed_nodes.contains(&folder.path) {
                img.add_css_class("open");
            }
            img.set_pixel_size(12);
            let btn = gtk::Button::new();
            btn.set_child(Some(&img));
            btn.add_css_class("flat");
            btn.add_css_class("tree-expander");
            btn.set_valign(gtk::Align::Center);
            btn.set_tooltip_text(Some(i18n("Show or hide sub-folders").as_str()));
            let st = sender.input_sender().clone();
            let path = folder.path.clone();
            btn.connect_clicked(move |_| {
                let _ = st.send(SidebarInput::ToggleFolderNode {
                    account_id: id,
                    path: path.clone(),
                });
            });
            self.tree_chevrons.insert((id, folder.path.clone()), img);
            btn.upcast()
        } else {
            let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            spacer.set_width_request(TREE_EXPANDER_WIDTH);
            spacer.upcast()
        }
    }

    /// Wire a folder list: its selection (row `i` stands for the folder at
    /// `offset + i` in `section.folders`), folders and messages dropped on
    /// it (`parents` are the rows a folder can be dropped into), and its
    /// right-click menu. Returns the list in the box that draws its drop
    /// line.
    fn wire_folder_list(
        &self,
        list: &gtk::ListBox,
        section: &SectionData,
        folders: &[&Folder],
        parents: &[&Folder],
        offset: i32,
        sender: &ComponentSender<Self>,
    ) -> gtk::Overlay {
        let id = section.account.id;
        let s = sender.input_sender().clone();
        let quiet = self.quiet.clone();
        list.connect_row_selected(move |_, row| {
            if quiet.get() {
                return;
            }
            if let Some(row) = row {
                let _ = s.send(SidebarInput::FolderRowSelected {
                    account_id: id,
                    index: offset + row.index(),
                });
            }
        });
        let (list_box, list_line) = with_drop_line(list);
        attach_folder_list_drop(
            list,
            &list_line,
            id,
            folders.iter().map(|f| (*f).clone()).collect(),
            parents.iter().map(|f| f.path.clone()).collect(),
            section.hierarchy.clone(),
            sender,
        );
        attach_folder_context_menu(
            list,
            id,
            folders.iter().map(|f| (*f).clone()).collect(),
            section.filtered.iter().map(|f| f.path.clone()).collect(),
            self.favorite_paths(id),
            section.has_filters,
            section.hierarchy.nests(),
            sender,
        );
        list_box
    }
}

/// The account's avatar disc (`name_str` gives its initials): its color,
/// ringed, a fixed 30 px square, with the glyph drawn ink-centred (see `ui::initials`) rather than as a label.
/// The account's own Gravatar leads when it asked for one and the address
/// has one (#189); the picture and the emoji are what it falls back to.
fn account_circle(section: &SectionData, name_str: &str) -> gtk::Box {
    let id = section.account.id;
    let circle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    circle.add_css_class("account-circle");
    circle.add_css_class(&format!("acct-color-{id}"));
    // Ringed in the account's color, as the list's avatars are in
    // All Inboxes: a picture covers the color the disc would show.
    circle.add_css_class(&format!("acct-ring-{id}"));
    circle.set_valign(gtk::Align::Center);
    // Keep it a perfect circle: a fixed square that never stretches with
    // the row. (Without this the glyph's hexpand propagates up and the
    // circle widens into an oval whenever a name column sits beside it.)
    circle.set_halign(gtk::Align::Center);
    circle.set_hexpand(false);
    circle.set_size_request(30, 30);
    // Drawn ink-centred (see `ui::initials`), not a label: a lone
    // letter or an emoji sits exactly in the middle of the disc.
    // The account's own Gravatar leads when it asked for one and the
    // address has one (#189); the picture and the emoji are what it
    // falls back to.
    let gravatar = account_gravatar(&section.account.email);
    let glyph: gtk::Widget = match (&gravatar, &section.avatar, &section.emoji) {
        (Some(texture), ..) => {
            circle.set_overflow(gtk::Overflow::Hidden);
            crate::ui::initials::picture_from_texture(texture, 30).upcast()
        }
        (None, Some(path), _) => {
            // A picture fills the disc; the disc's rounded corners
            // clip it into a circle.
            circle.set_overflow(gtk::Overflow::Hidden);
            crate::ui::initials::avatar_picture(path, 30).upcast()
        }
        (None, None, Some(em)) if !em.is_empty() => {
            crate::ui::initials::glyph_picture(em, &section.color, 0.55, 30).upcast()
        }
        _ => crate::ui::initials::glyph_picture(
            &account_initials(name_str, &section.account.email),
            &section.color,
            0.47,
            30,
        )
        .upcast(),
    };
    circle.append(&glyph);
    circle
}

/// Right-click an account: act on the account / its inbox.
fn attach_account_menu(
    header: &gtk::Button,
    section: &SectionData,
    sender: &ComponentSender<Sidebar>,
) {
    let id = section.account.id;
    let inbox_id = section
        .folders
        .iter()
        .find(|f| f.kind == FolderKind::Inbox)
        .map(|f| f.id);
    let own_order = !section.folder_order.is_empty();
    let top_level = section.hierarchy.top_level;
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_SECONDARY);
    let cs = sender.clone();
    let header_w = header.clone();
    click.connect_pressed(move |_, _, x, y| {
        let mut items: Vec<(&str, CtxAction)> = Vec::new();
        if let Some(fid) = inbox_id {
            items.push((
                i18n_noop("Mark Inbox as Read"),
                CtxAction::MarkFolderRead {
                    account_id: id,
                    folder_id: fid,
                },
            ));
            items.push((
                i18n_noop("Refresh"),
                CtxAction::RefreshFolder {
                    account_id: id,
                    folder_id: fid,
                },
            ));
        }
        if top_level {
            items.push((
                i18n_noop("New Folder…"),
                CtxAction::NewFolder {
                    account_id: id,
                    parent: String::new(),
                },
            ));
        }
        if own_order {
            items.push((
                i18n_noop("Reset Folder Order"),
                CtxAction::ResetFolderOrder(id),
            ));
        }
        items.push((
            i18n_noop("Account Settings…"),
            CtxAction::OpenAccountSettings(id),
        ));
        items.push((i18n_noop("Remove Account…"), CtxAction::RemoveAccount(id)));
        show_sidebar_menu(&header_w, x, y, items, &cs);
    });
    header.add_controller(click);
}

/// While dragging a message, hovering the account header for 500ms
/// expands a collapsed account so its folders become drop targets.
fn attach_drop_hover(header: &gtk::Button, id: u32, sender: &ComponentSender<Sidebar>) {
    let motion = gtk::DropControllerMotion::new();
    let ms = sender.input_sender().clone();
    let timer: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let t_enter = timer.clone();
    motion.connect_enter(move |_, _, _| {
        if t_enter.borrow().is_some() {
            return;
        }
        let ms = ms.clone();
        let t = t_enter.clone();
        let src =
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                *t.borrow_mut() = None;
                let _ = ms.send(SidebarInput::ExpandForDrop(id));
            });
        *t_enter.borrow_mut() = Some(src);
    });
    let t_leave = timer.clone();
    motion.connect_leave(move |_| {
        if let Some(src) = t_leave.borrow_mut().take() {
            src.remove();
        }
    });
    header.add_controller(motion);
}
