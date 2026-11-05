//! The unified section's rows (All Inboxes, Starred, Sent, Drafts, Archive,
//! Junk, Trash, Favorites, Filters, Tags, Smart Folders, Contacts) and the
//! Tags sections.

use super::dnd::folder_drop_target;
use super::menus::{attach_tag_context_menu, folder_menu_items, show_sidebar_menu};
use super::types::{
    chevron_icon, row_icon, row_title, select_msg, toggle_msg, InboxRef, KindWidgets,
    SectionWidgets, UnifiedRow,
};
use super::widgets::{
    build_unified_inbox_row, filtered_folder_icon, heading_chevron, section_heading, style_badge,
    tree_indent, with_unread_overlay, ROW_LEFT_INSET,
};
use super::*;

impl Sidebar {
    /// One row of the unified section — All Inboxes, Starred / Sent /
    /// Drafts, Filtered Folders or Tags — with its expandable list beneath:
    /// the header row selects the merged view, the caret opens the list
    /// (each account's folder of that kind, each filtered folder, each
    /// tag). In the icon-only rail a small toggle button under the icon
    /// stands in for the in-row chevron.
    fn build_unified_row(
        &mut self,
        parent: &gtk::Box,
        row_kind: UnifiedRow,
        sections: &[SectionData],
        sender: &ComponentSender<Self>,
    ) -> KindWidgets {
        let is_inbox = row_kind == UnifiedRow::Kind(FolderKind::Inbox);
        let title = row_title(row_kind);
        let unread = self.row_unread(row_kind);
        let expanded = self.row_shown_open(row_kind);
        let show_chip = unread > 0 && !expanded && self.chip_shown(row_kind);
        let toggle_tip = match row_kind {
            UnifiedRow::Kind(FolderKind::Inbox) => i18n("Show each inbox"),
            UnifiedRow::Kind(_) => i18n("Show each account"),
            UnifiedRow::Favorites | UnifiedRow::Filtered => i18n("Show each folder"),
            UnifiedRow::Tags => i18n("Show each tag"),
            UnifiedRow::Smart => i18n("Show each smart folder"),
            UnifiedRow::Contacts => i18n("Show each address book"),
        };

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("navigation-sidebar");
        // The unified rows stack with no gap between their pills, as
        // folder rows in one list do (see styles.css).
        list.add_css_class("unified-item");

        let row = gtk::ListBoxRow::new();
        // Tagged so the disclosure chevron can be lined up with the
        // account headers' (see styles.css).
        row.add_css_class("unified-row");
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        hbox.add_css_class("folder-row");
        let img = gtk::Image::from_icon_name(row_icon(row_kind));
        img.add_css_class("folder-icon");
        let mut chevron_img: Option<gtk::Image> = None;
        let badge: gtk::Label;
        if self.collapsed {
            hbox.set_halign(gtk::Align::Center);
            let mut tip = if unread > 0 {
                format!("{title} ({unread})")
            } else {
                title.clone()
            };
            // The rail has no room for a chevron: a long press on the icon
            // opens or folds the list instead.
            tip.push('\n');
            tip.push_str(&i18n("Long-press to expand or collapse"));
            row.set_tooltip_text(Some(&tip));
            // Total-unread chip overlaid on the icon so the count stays
            // visible in the icon-only rail.
            let (overlay, b) = with_unread_overlay(&img, unread);
            b.set_visible(show_chip);
            hbox.append(&overlay);
            badge = b;
        } else {
            // The disclosure chevron toggling the list — its own button
            // either way (selecting the merged view and expanding it stay
            // separate actions). Placement follows Settings → Chevron
            // placement: leading (overlaid on the row's left edge — see
            // below), or classic trailing.
            let chevron = gtk::Image::from_icon_name(chevron_icon(expanded));
            let chev_btn = gtk::Button::new();
            chev_btn.set_child(Some(&chevron));
            chev_btn.add_css_class("flat");
            chev_btn.add_css_class("chevron-btn");
            chev_btn.set_valign(gtk::Align::Center);
            chev_btn.set_tooltip_text(Some(toggle_tip.as_str()));
            let cs = sender.input_sender().clone();
            chev_btn.connect_clicked(move |_| {
                let _ = cs.send(toggle_msg(row_kind));
            });
            // The tree style (Settings → Sidebar → Sections): the caret
            // takes the icon's place, centred on the icon column above,
            // and the row has no chevron at its end.
            let tree = self.as_heading(row_kind);
            row.add_css_class(match (self.chevrons_left, tree) {
                (true, _) => "chev-left",
                (false, false) => "chev-right",
                (false, true) => "tree-head",
            });
            pin_icon_size(&img);
            // Centers this 16px icon on the avatar circles below it (rather
            // than matching left edges): a small icon flush with a much
            // wider circle's left edge reads as off-centre next to it
            // (PR #95).
            let icon_inset = if self.chevrons_left {
                ROW_LEFT_INSET + 9
            } else {
                0
            };
            if tree {
                // The 24px button around the 16px glyph overhangs it 4px a
                // side, taken back in styles.css (`.tree-caret`).
                chev_btn.add_css_class("tree-caret");
                chev_btn.set_margin_start(icon_inset);
                hbox.append(&chev_btn);
            } else {
                img.set_margin_start(icon_inset);
                hbox.append(&img);
            }
            let label = gtk::Label::new(Some(&title));
            label.set_halign(gtk::Align::Start);
            label.set_hexpand(true);
            // Squeezed by a wide chip, the title shortens; the chip and the
            // chevron never leave the row.
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.add_css_class("account-name");
            if self.chevrons_left {
                label.set_margin_start(-2);
            }
            hbox.append(&label);
            chevron_img = Some(chevron.clone());
            // The total-unread chip right-aligns like every folder row's,
            // one shared column down the sidebar. While the list is
            // expanded its rows carry the counts, so the total is
            // redundant and hidden.
            let b = gtk::Label::new(Some(&unread.to_string()));
            style_badge(&b, 5);
            b.set_visible(show_chip);
            hbox.append(&b);
            badge = b;
            if tree {
                // The caret leads already.
            } else if self.chevrons_left {
                // Overlaid on the row's left edge rather than packed into
                // the layout — the same trick as the rail's unread badges
                // — so it reserves no space of its own: icon and label
                // keep their normal position and the chip keeps the
                // shared flush-right column (Isaac's PR #95 mechanism).
                chevron.set_pixel_size(15);
                chev_btn.add_css_class("row-disclosure-chevron");
                chev_btn.set_halign(gtk::Align::Start);
                let overlay = gtk::Overlay::new();
                overlay.set_child(Some(&hbox));
                overlay.add_overlay(&chev_btn);
                row.set_child(Some(&overlay));
            } else {
                hbox.append(&chev_btn);
            }
        }
        if row.child().is_none() {
            row.set_child(Some(&hbox));
        }
        list.append(&row);

        let s = sender.input_sender().clone();
        let quiet = self.quiet.clone();
        list.connect_row_selected(move |_, row| {
            if quiet.get() {
                return;
            }
            if row.is_some() {
                let _ = s.send(select_msg(row_kind));
            }
        });
        if is_inbox {
            // Right-click "All Inboxes": act on every inbox at once.
            let click = gtk::GestureClick::new();
            click.set_button(gtk::gdk::BUTTON_SECONDARY);
            let cs = sender.clone();
            let list_w = list.clone();
            click.connect_pressed(move |_, _, x, y| {
                show_sidebar_menu(
                    &list_w,
                    x,
                    y,
                    vec![
                        (i18n_noop("Mark All as Read"), CtxAction::MarkAllInboxesRead),
                        (i18n_noop("Refresh"), CtxAction::RefreshAllInboxes),
                    ],
                    &cs,
                );
            });
            list.add_controller(click);
        } else if let UnifiedRow::Kind(kind @ (FolderKind::Trash | FolderKind::Junk)) = row_kind {
            // Right-click the merged Trash or Junk: empty every account's
            // (#413), each one still a row of its own below for one at a time.
            let label = if kind == FolderKind::Trash {
                i18n_noop("Empty Trash…")
            } else {
                i18n_noop("Empty Junk…")
            };
            let click = gtk::GestureClick::new();
            click.set_button(gtk::gdk::BUTTON_SECONDARY);
            let cs = sender.clone();
            let list_w = list.clone();
            click.connect_pressed(move |_, _, x, y| {
                show_sidebar_menu(&list_w, x, y, vec![(label, CtxAction::EmptyAll(kind))], &cs);
            });
            list.add_controller(click);
        } else if row_kind == UnifiedRow::Smart {
            let click = gtk::GestureClick::new();
            click.set_button(gtk::gdk::BUTTON_SECONDARY);
            let cs = sender.clone();
            let list_w = list.clone();
            click.connect_pressed(move |_, _, x, y| {
                show_sidebar_menu(
                    &list_w,
                    x,
                    y,
                    vec![(i18n_noop("New Smart Folder…"), CtxAction::NewSmartFolder)],
                    &cs,
                );
            });
            list.add_controller(click);
        }
        parent.append(&list);

        // A double click on the row opens or folds the list too, as the
        // chevron does; its first click selects the merged view as any
        // click does. Smart Folders fold on one click already, which a
        // second would undo.
        if row_kind != UnifiedRow::Smart {
            let double = gtk::GestureClick::new();
            double.set_button(gtk::gdk::BUTTON_PRIMARY);
            let cs = sender.input_sender().clone();
            double.connect_pressed(move |_, presses, _, _| {
                if presses == 2 {
                    let _ = cs.send(toggle_msg(row_kind));
                }
            });
            list.add_controller(double);
        }

        // In the rail, with no room for a chevron, a long press on the row
        // opens or folds the list too (a plain click still selects the
        // merged view).
        if self.collapsed {
            let press = gtk::GestureLongPress::new();
            press.set_touch_only(false);
            let cs = sender.input_sender().clone();
            press.connect_pressed(move |g, _, _| {
                // Claimed, so the release doesn't also count as a click.
                g.set_state(gtk::EventSequenceState::Claimed);
                let _ = cs.send(toggle_msg(row_kind));
            });
            list.add_controller(press);
        }

        // The list beneath: the same row shape for all — a lead (account
        // pill, tinted folder glyph, tag disc), a name, an unread chip.
        let sub = gtk::ListBox::new();
        sub.set_selection_mode(gtk::SelectionMode::Single);
        sub.add_css_class("navigation-sidebar");
        sub.add_css_class("unified-item");
        let mut rows: Vec<InboxRef> = Vec::new();
        let mut row_badges: HashMap<(u32, u32), gtk::Label> = HashMap::new();
        match row_kind {
            UnifiedRow::Kind(kind) => {
                self.fill_kind_list(kind, &sub, sections, &mut rows, &mut row_badges, sender)
            }
            UnifiedRow::Favorites => self.fill_favorites(&sub, &mut row_badges, sender),
            UnifiedRow::Smart => self.fill_smart(&sub, sender),
            UnifiedRow::Contacts => self.fill_contacts(&sub, sender),
            UnifiedRow::Filtered => self.fill_filtered(&sub, sections, &mut row_badges, sender),
            UnifiedRow::Tags => self.fill_tags(&sub, sender),
        }

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        // 0 during the rebuild: an animated reveal grows the content's
        // height over 200ms, which drags the scroll toward the top
        // mid-rebuild. Real duration restored one frame later.
        revealer.set_transition_duration(0);
        revealer.set_reveal_child(expanded);
        revealer.set_child(Some(&sub));
        parent.append(&revealer);

        KindWidgets {
            header: list,
            revealer,
            chevron: chevron_img,
            badge: Some(badge),
            list: sub,
            rows,
            row_badges,
        }
    }

    /// Build a run of unified rows into `parent`, the first row keeping the
    /// list's top padding as the gap above the run and the last row's list
    /// carrying the gap to whatever follows.
    pub(super) fn build_unified_run(
        &mut self,
        parent: &gtk::Box,
        rows: &[UnifiedRow],
        sections: &[SectionData],
        sender: &ComponentSender<Self>,
    ) {
        let n = rows.len();
        for (i, row) in rows.iter().copied().enumerate() {
            let built = self.build_unified_row(parent, row, sections, sender);
            if i == 0 {
                built.header.add_css_class("unified-first");
            }
            if i + 1 == n {
                // 14 plus the 6 the list's own padding used to give.
                built.list.set_margin_bottom(20);
            }
            if row == UnifiedRow::Kind(FolderKind::Inbox) {
                // All Inboxes keeps its own fields: the in-place updates and
                // the tray's "View all unread" address it directly.
                self.unified_list = Some(built.header);
                self.unified_revealer = Some(built.revealer);
                self.unified_chevron = built.chevron;
                self.unified_badge = built.badge;
                self.unified_inbox_list = Some(built.list);
                self.unified_inboxes = built.rows;
                self.unified_inbox_badges = built.row_badges;
            } else {
                self.kind_widgets.insert(row, built);
            }
        }
    }

    /// An account's Tags section (#71): a toggle header and, under an
    /// animated revealer, one row per tag (color disc, its name) showing
    /// that account's mail with it. In the rail the header is a tag glyph
    /// and the rows are discs.
    pub(super) fn build_tags_section(
        &mut self,
        parent: &gtk::Box,
        slot: Slot,
        sender: &ComponentSender<Self>,
    ) {
        let open = self.tags_open(slot);
        let chevron = gtk::Image::from_icon_name(chevron_icon(open));
        let hb = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        hb.add_css_class("folder-row");
        let mut chevron_btn: Option<gtk::Button> = None;
        if self.collapsed {
            let icon = gtk::Image::from_icon_name("tag-outline-symbolic");
            pin_icon_size(&icon);
            hb.set_halign(gtk::Align::Center);
            hb.append(&icon);
        } else {
            if self.chevrons_left {
                chevron.set_margin_start(2);
            }
            let btn = heading_chevron(&chevron);
            btn.set_tooltip_text(Some(i18n("Show or hide tags").as_str()));
            hb.append(&btn);
            chevron_btn = Some(btn);
            let lbl = gtk::Label::new(Some(i18n("Tags").as_str()));
            lbl.set_halign(gtk::Align::Start);
            lbl.set_hexpand(true);
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            hb.append(&lbl);
        }
        let toggle = section_heading(
            &hb,
            chevron_btn.as_ref(),
            &[],
            SidebarInput::ToggleTagsExpand(slot),
            sender,
        );
        if self.collapsed {
            toggle.set_tooltip_text(Some(i18n("Tags").as_str()));
        }
        parent.append(&toggle);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("navigation-sidebar");
        list.add_css_class("section-child-list");
        for t in &self.tags {
            let row = gtk::ListBoxRow::new();
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            hbox.add_css_class("folder-row");
            let disc = crate::ui::context_menu::swatch_widget(&t.color, true);
            if self.collapsed {
                hbox.set_halign(gtk::Align::Center);
                hbox.append(&disc);
                row.set_tooltip_text(Some(&t.name));
            } else {
                // Laid out like the Filtered Folders rows: the same leaf
                // expander slot, then the disc exactly where their glyph
                // sits (both 16px), so the two sections' rows line up.
                let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                spacer.set_width_request(TREE_EXPANDER_WIDTH);
                hbox.append(&spacer);
                if self.chevrons_left {
                    disc.set_margin_start(ROW_LEFT_INSET);
                }
                hbox.append(&disc);
                // Regular weight, like the folder names above (the
                // heading's bold is the section's, not its rows').
                let label = gtk::Label::new(Some(&t.name));
                label.set_hexpand(true);
                label.set_halign(gtk::Align::Start);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                hbox.append(&label);
            }
            row.set_child(Some(&hbox));
            list.append(&row);
        }
        let ss = sender.input_sender().clone();
        let quiet = self.quiet.clone();
        list.connect_row_selected(move |_, row| {
            if quiet.get() {
                return;
            }
            if let Some(row) = row {
                let _ = ss.send(SidebarInput::TagRowSelected {
                    slot,
                    index: row.index(),
                });
            }
        });
        attach_tag_context_menu(
            &list,
            self.tags.iter().map(|t| t.keyword.clone()).collect(),
            sender,
        );

        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        revealer.set_transition_duration(0);
        revealer.set_reveal_child(open);
        revealer.set_child(Some(&list));
        parent.append(&revealer);
        self.tag_sections.insert(
            slot,
            SectionWidgets {
                revealer,
                chevron,
                list,
            },
        );
    }
}

impl Sidebar {
    /// Each account's folder of `kind` (a Starred row with no path for an
    /// account that keeps stars as flags, #482).
    fn fill_kind_list(
        &mut self,
        kind: FolderKind,
        sub: &gtk::ListBox,
        sections: &[SectionData],
        rows: &mut Vec<InboxRef>,
        row_badges: &mut HashMap<(u32, u32), gtk::Label>,
        sender: &ComponentSender<Self>,
    ) {
        let is_inbox = kind == FolderKind::Inbox;
        // Sent wears no unread chip anywhere: what you sent is not new mail.
        let counted = kind != FolderKind::Sent;
        for section in sections {
            // Most accounts keep a star as a flag, with no Starred
            // folder (#482): theirs is a row with no path, which
            // opens the account's starred mail wherever it is filed.
            let flagged;
            let folder = match section.folders.iter().find(|f| f.kind == kind) {
                Some(f) => f,
                None if kind == FolderKind::Starred && !section.folders.is_empty() => {
                    flagged = Folder {
                        id: u32::MAX,
                        account_id: section.account.id,
                        name: kind_label(kind),
                        path: String::new(),
                        kind,
                        unread: 0,
                        childless: false,
                    };
                    &flagged
                }
                None => continue,
            };
            let aid = section.account.id;
            let (row, badge) =
                build_unified_inbox_row(section, folder, self.collapsed, self.chevrons_left);
            // A message dropped here moves to that account's folder
            // (the app declines mail from other accounts, #23).
            if !folder.path.is_empty() {
                row.add_controller(folder_drop_target(aid, folder.path.clone(), sender));
            }
            sub.append(&row);
            if counted && !folder.path.is_empty() {
                row_badges.insert((aid, folder.id), badge);
            } else {
                badge.set_visible(false);
                row.set_tooltip_text(None);
            }
            rows.push(InboxRef {
                account_id: aid,
                folder_id: folder.id,
                name: folder.name.clone(),
                path: folder.path.clone(),
            });
        }
        let ss = sender.input_sender().clone();
        let quiet = self.quiet.clone();
        sub.connect_row_selected(move |_, row| {
            if quiet.get() {
                return;
            }
            if let Some(row) = row {
                let _ = ss.send(if is_inbox {
                    SidebarInput::UnifiedInboxRowSelected(row.index())
                } else {
                    SidebarInput::KindSubRowSelected {
                        kind,
                        index: row.index(),
                    }
                });
            }
        });
        // Right-click a row: act on that account's folder.
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        let cs = sender.clone();
        let sub_w = sub.clone();
        let refs = rows.clone();
        click.connect_pressed(move |_, _, x, y| {
            if let Some(r) = sub_w
                .row_at_y(y as i32)
                .and_then(|row| refs.get(row.index() as usize))
            {
                if r.path.is_empty() {
                    let items = vec![(
                        i18n_noop("Account Settings…"),
                        CtxAction::OpenAccountSettings(r.account_id),
                    )];
                    show_sidebar_menu(&sub_w, x, y, items, &cs);
                    return;
                }
                let mut items = vec![
                    (
                        i18n_noop("Mark as Read"),
                        CtxAction::MarkFolderRead {
                            account_id: r.account_id,
                            folder_id: r.folder_id,
                        },
                    ),
                    (
                        i18n_noop("Refresh"),
                        CtxAction::RefreshFolder {
                            account_id: r.account_id,
                            folder_id: r.folder_id,
                        },
                    ),
                ];
                // Each account's Trash and Junk empties from here
                // too (#369), as from its own section.
                let empty_label = match kind {
                    FolderKind::Trash => Some(i18n_noop("Empty Trash…")),
                    FolderKind::Junk => Some(i18n_noop("Empty Junk…")),
                    _ => None,
                };
                if let Some(label) = empty_label {
                    items.push((
                        label,
                        CtxAction::EmptyFolder {
                            account_id: r.account_id,
                            folder_id: r.folder_id,
                            name: r.name.clone(),
                            path: r.path.clone(),
                        },
                    ));
                }
                items.push((
                    i18n_noop("Account Settings…"),
                    CtxAction::OpenAccountSettings(r.account_id),
                ));
                show_sidebar_menu(&sub_w, x, y, items, &cs);
            }
        });
        sub.add_controller(click);
    }

    /// Every favorite folder (#367), from any account.
    fn fill_favorites(
        &mut self,
        sub: &gtk::ListBox,
        row_badges: &mut HashMap<(u32, u32), gtk::Label>,
        sender: &ComponentSender<Self>,
    ) {
        // Every account's favorites, whether or not the account is
        // in the unified section: picking one was choice enough.
        let refs = self.favorite_folders.clone();
        let all = self.sections.clone();
        for r in &refs {
            let Some(section) = all.iter().find(|s| s.account.id == r.account_id) else {
                continue;
            };
            // The folder's own glyph, in the account's color.
            let icon = gtk::Image::from_icon_name(r.folder.kind.icon());
            icon.add_css_class(&format!("acct-tint-{}", section.account.id));
            pin_icon_size(&icon);
            let tip = format!("{} \u{2014} {}", r.folder.name, section.account.label);
            let counted = r.folder.kind != FolderKind::Sent;
            let (row, badge) = build_unified_sub_row(
                &tree_indent(&icon, self.collapsed),
                &r.folder.name,
                &tip,
                if counted { r.folder.unread } else { 0 },
                self.collapsed,
                self.chevrons_left,
                false,
            );
            row.add_controller(folder_drop_target(
                r.account_id,
                r.folder.path.clone(),
                sender,
            ));
            sub.append(&row);
            if counted {
                row_badges.insert((r.account_id, r.folder.id), badge);
            } else {
                badge.set_visible(false);
            }
        }
        let ss = sender.input_sender().clone();
        let quiet = self.quiet.clone();
        sub.connect_row_selected(move |_, row| {
            if quiet.get() {
                return;
            }
            if let Some(row) = row {
                let _ = ss.send(SidebarInput::FavoriteRowSelected(row.index()));
            }
        });
        // Right-click a favorite: the folder's own menu, which
        // takes it out of Favorites too.
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        let cs = sender.clone();
        let sub_w = sub.clone();
        let nests = self.nesting_accounts();
        let filtered: Vec<(u32, String)> = self
            .unified_folders
            .iter()
            .map(|r| (r.account_id, r.folder.path.clone()))
            .collect();
        click.connect_pressed(move |_, _, x, y| {
            if let Some(r) = sub_w
                .row_at_y(y as i32)
                .and_then(|row| refs.get(row.index() as usize))
            {
                let is_filtered = filtered
                    .iter()
                    .any(|(a, p)| *a == r.account_id && *p == r.folder.path);
                let items = folder_menu_items(
                    r.account_id,
                    &r.folder,
                    is_filtered,
                    true,
                    true,
                    nests.contains(&r.account_id),
                );
                show_sidebar_menu(&sub_w, x, y, items, &cs);
            }
        });
        sub.add_controller(click);
    }

    /// The accounts whose folders can hold others (#554).
    fn nesting_accounts(&self) -> Vec<u32> {
        self.sections
            .iter()
            .filter(|s| s.hierarchy.nests())
            .map(|s| s.account.id)
            .collect()
    }

    /// Every smart folder (#417).
    fn fill_smart(&mut self, sub: &gtk::ListBox, sender: &ComponentSender<Self>) {
        self.smart_badges.clear();
        for (id, name) in &self.smart_folders {
            let icon = gtk::Image::from_icon_name("folder-saved-search-symbolic");
            pin_icon_size(&icon);
            let unread = self.smart_unread.get(id).copied().unwrap_or(0);
            let (row, badge) = build_unified_sub_row(
                &icon,
                name,
                name,
                unread,
                self.collapsed,
                self.chevrons_left,
                false,
            );
            sub.append(&row);
            self.smart_badges.insert(id.clone(), badge);
        }
        let ss = sender.input_sender().clone();
        let quiet = self.quiet.clone();
        sub.connect_row_selected(move |_, row| {
            if quiet.get() {
                return;
            }
            if let Some(row) = row {
                let _ = ss.send(SidebarInput::SmartRowSelected(row.index()));
            }
        });
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        let cs = sender.clone();
        let sub_w = sub.clone();
        let ids: Vec<String> = self
            .smart_folders
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        click.connect_pressed(move |_, _, x, y| {
            if let Some(id) = sub_w
                .row_at_y(y as i32)
                .and_then(|row| ids.get(row.index() as usize))
            {
                let items = vec![
                    (
                        i18n_noop("Edit Smart Folder…"),
                        CtxAction::EditSmartFolder(id.clone()),
                    ),
                    (
                        i18n_noop("Delete Smart Folder…"),
                        CtxAction::DeleteSmartFolder(id.clone()),
                    ),
                    (i18n_noop("New Smart Folder…"), CtxAction::NewSmartFolder),
                ];
                show_sidebar_menu(&sub_w, x, y, items, &cs);
            }
        });
        sub.add_controller(click);
    }

    /// The address books and groups under Contacts.
    fn fill_contacts(&mut self, sub: &gtk::ListBox, sender: &ComponentSender<Self>) {
        // The tree the contact page handed over; filled again as it
        // changes (see `fill_contact_tree`).
        if let Some(tree) = &self.contact_tree {
            let filled = crate::ui::contact_tree::fill(
                sub,
                tree,
                self.collapsed,
                self.chevrons_left,
                &self.contacts_folded,
                sender.input_sender(),
            );
            *self.tree_rows.borrow_mut() = filled.rows;
            self.contact_layout = filled.layout;
            self.contact_chevrons = filled.chevrons;
        }
        let ss = sender.input_sender().clone();
        let quiet = self.quiet.clone();
        let rows = self.tree_rows.clone();
        sub.connect_row_selected(move |_, row| {
            if quiet.get() {
                return;
            }
            let Some(row) = row else { return };
            if let Some((scope, _)) = rows.borrow().iter().find(|(_, r)| r == row) {
                let _ = ss.send(SidebarInput::ContactTreeSelected(scope.clone()));
            }
        });
    }

    /// Every folder a filter rule files into.
    fn fill_filtered(
        &mut self,
        sub: &gtk::ListBox,
        sections: &[SectionData],
        row_badges: &mut HashMap<(u32, u32), gtk::Label>,
        sender: &ComponentSender<Self>,
    ) {
        let refs = self.unified_folders.clone();
        for r in &refs {
            let Some(section) = sections.iter().find(|s| s.account.id == r.account_id) else {
                continue;
            };
            // The glyph in the account's color says whose folder
            // this is; the tooltip names the account.
            let icon = filtered_folder_icon(&r.folder, section.account.id);
            pin_icon_size(&icon);
            let tip = format!("{} \u{2014} {}", r.folder.name, section.account.label);
            let (row, badge) = build_unified_sub_row(
                &tree_indent(&icon, self.collapsed),
                &r.folder.name,
                &tip,
                r.folder.unread,
                self.collapsed,
                self.chevrons_left,
                false,
            );
            // Filtered folders take drops like any folder of their account.
            row.add_controller(folder_drop_target(
                r.account_id,
                r.folder.path.clone(),
                sender,
            ));
            sub.append(&row);
            row_badges.insert((r.account_id, r.folder.id), badge);
        }
        self.filtered_badges
            .insert(Slot::Unified, row_badges.clone());
        let ss = sender.input_sender().clone();
        let quiet = self.quiet.clone();
        sub.connect_row_selected(move |_, row| {
            if quiet.get() {
                return;
            }
            if let Some(row) = row {
                let _ = ss.send(SidebarInput::FilteredRowSelected {
                    slot: Slot::Unified,
                    index: row.index(),
                });
            }
        });
        // Right-click a filtered folder: act on that folder alone.
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        let cs = sender.clone();
        let sub_w = sub.clone();
        let favorites = self.favorite_keys();
        let nests = self.nesting_accounts();
        click.connect_pressed(move |_, _, x, y| {
            if let Some(r) = sub_w
                .row_at_y(y as i32)
                .and_then(|row| refs.get(row.index() as usize))
            {
                // The same menu the folder has under its account.
                let favorite = favorites
                    .iter()
                    .any(|(a, p)| *a == r.account_id && *p == r.folder.path);
                let items = folder_menu_items(
                    r.account_id,
                    &r.folder,
                    true,
                    true,
                    favorite,
                    nests.contains(&r.account_id),
                );
                show_sidebar_menu(&sub_w, x, y, items, &cs);
            }
        });
        sub.add_controller(click);
    }

    /// Every tag (#71).
    fn fill_tags(&mut self, sub: &gtk::ListBox, sender: &ComponentSender<Self>) {
        for t in &self.tags {
            let disc = crate::ui::context_menu::swatch_widget(&t.color, true);
            let (row, badge) = build_unified_sub_row(
                &tree_indent(&disc, self.collapsed),
                &t.name,
                &t.name,
                0,
                self.collapsed,
                self.chevrons_left,
                false,
            );
            badge.set_visible(false);
            sub.append(&row);
        }
        let ss = sender.input_sender().clone();
        let quiet = self.quiet.clone();
        sub.connect_row_selected(move |_, row| {
            if quiet.get() {
                return;
            }
            if let Some(row) = row {
                let _ = ss.send(SidebarInput::TagRowSelected {
                    slot: Slot::Unified,
                    index: row.index(),
                });
            }
        });
        attach_tag_context_menu(
            sub,
            self.tags.iter().map(|t| t.keyword.clone()).collect(),
            sender,
        );
    }
}
