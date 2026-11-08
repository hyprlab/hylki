//! Rebuilding the whole sidebar from its data (`rebuild_normal`), and
//! restyling the unread counts in place.

use super::types::UnifiedRow;
use super::widgets::{style_badge, ROW_LEFT_INSET};
use super::*;

impl Sidebar {
    /// Rebuild everything and select again what was selected.
    pub(super) fn rebuild(&mut self, widgets: &SidebarWidgets, sender: &ComponentSender<Self>) {
        self.rebuild_normal(
            &widgets.pinned_box,
            &widgets.normal_box,
            &widgets.footer_box,
            sender,
        );
        self.restore_selection();
    }

    /// Rebuild the list: optional unified row, then per-account headers with
    /// animated folder revealers, and refresh the per-account color rules.
    fn rebuild_normal(
        &mut self,
        pinned: &gtk::Box,
        container: &gtk::Box,
        footer: &gtk::Box,
        sender: &ComponentSender<Self>,
    ) {
        use crate::config::SectionPlacement::{AboveAccounts, BelowAccounts};
        release_focus(pinned, container, footer);
        let (scroller, saved_scroll) = self.freeze_over_swap(pinned, container);
        self.clear_boxes(pinned, container, footer);
        self.forget_widgets();

        // No accounts yet: show a prompt to add the first one instead of an empty
        // sidebar (the app is blank in this state).
        if self.sections.is_empty() {
            self.build_empty_state(container, sender);
            return;
        }

        let sections = self.sections.clone();
        self.build_compose_bar(pinned, sender);
        self.build_unified_section(container, &sections, sender);
        self.build_footer(footer, sender);
        self.build_outbox_row(container, sender);

        // Favorites, Filters, Tags and Contacts placed above the accounts,
        // drawn like the unified rows.
        let rows_above = self.placed_rows(AboveAccounts);
        if !rows_above.is_empty() {
            self.build_unified_run(container, &rows_above, &sections, sender);
        }

        // The account sections — unless Settings (or the main menu) has
        // them off, for those who work from the unified section alone.
        let account_sections: Vec<&SectionData> = if self.show_accounts && !self.focus_hide_accounts
        {
            sections.iter().collect()
        } else {
            Vec::new()
        };
        // Every account section goes in one revealer, so Focus Mode can
        // slide them all away together (and back).
        let accounts_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let accounts_revealer = gtk::Revealer::new();
        accounts_revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        accounts_revealer.set_transition_duration(0);
        accounts_revealer.set_reveal_child(!self.reveal_accounts_late);
        accounts_revealer.set_child(Some(&accounts_box));
        let any_accounts = !account_sections.is_empty();
        for (section_idx, section) in account_sections.into_iter().enumerate() {
            self.build_account_section(section_idx, section, &accounts_box, container, sender);
        }

        if any_accounts {
            container.append(&accounts_revealer);
            if std::mem::take(&mut self.reveal_accounts_late) {
                // Slide them in once the rebuild's freeze-frame has lifted
                // (80ms), so the whole of the motion is seen.
                let r = accounts_revealer.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(90),
                    move || {
                        r.set_transition_duration(crate::ui::FOCUS_ANIM_MS);
                        r.set_reveal_child(true);
                    },
                );
            }
        }
        self.accounts_revealer = any_accounts.then_some(accounts_revealer);

        // And those placed after the last account.
        let rows_below = self.placed_rows(BelowAccounts);
        if !rows_below.is_empty() {
            self.build_unified_run(container, &rows_below, &sections, sender);
        }

        self.load_account_css(&sections);
        self.arm_revealers();
        pin_scroll(scroller, saved_scroll);
        self.restyle_seen();
    }

    /// Keep the scroll offset: rebuilding otherwise snaps the sidebar to the
    /// top. Then freeze the sidebar's last-rendered pixels over the swap
    /// (see below). Returns the scroller and the offset to restore.
    fn freeze_over_swap(
        &mut self,
        pinned: &gtk::Box,
        container: &gtk::Box,
    ) -> (Option<gtk::ScrolledWindow>, Option<f64>) {
        // Keep the scroll offset: rebuilding otherwise snaps the sidebar to
        // the top — felt on every folder drag-and-drop, whose optimistic move
        // rebuilds immediately under the pointer.
        let scroller = container
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>();
        let saved_scroll = scroller.as_ref().map(|s| s.vadjustment().value());
        // Freeze the sidebar's last-rendered pixels over the swap: even with
        // the scroll pinned, tearing down and recreating every row can
        // shimmer for a frame. The snapshot covers the rebuild and lifts a
        // few frames later, once the fresh tree has painted beneath it —
        // identical pixels, so the crossover is invisible. The snapshot is of
        // the overlay's whole child (pinned block + scroller + footer),
        // matching the area the freeze-frame Picture stretches over.
        //
        // A toggle between the full sidebar and the icon rail is different:
        // the pane's width animates underneath the snapshot for 200ms (the
        // app's `animate_sidebar`), so the snapshot keeps the width it was
        // taken at (never stretched to the moving pane — that was a visible
        // smear of every icon) and fades out over the same 200ms while the
        // fresh rows, centred in the pane as it moves, come through beneath.
        let rail_toggle = self.built_collapsed != self.collapsed;
        self.built_collapsed = self.collapsed;
        if let (Some(freeze), Some(area)) = (self.freeze_frame.clone(), pinned.parent()) {
            if container.first_child().is_some() || pinned.first_child().is_some() {
                use gtk::gdk::prelude::PaintableExt;
                // Take before skipping: the fade's done handler clears the
                // same slot, so the borrow must be over by then.
                let fading = self.freeze_fade.borrow_mut().take();
                if let Some(prev) = fading {
                    prev.skip();
                }
                let pending = self.freeze_timer.borrow_mut().take();
                if let Some(prev) = pending {
                    prev.remove();
                }
                let live = gtk::WidgetPaintable::new(Some(&area));
                freeze.set_paintable(Some(&live.current_image()));
                freeze.set_size_request(area.width().max(1), -1);
                freeze.set_opacity(1.0);
                freeze.set_visible(true);
                if rail_toggle {
                    let target = adw::CallbackAnimationTarget::new({
                        let freeze = freeze.clone();
                        move |v| freeze.set_opacity(v)
                    });
                    let anim = adw::TimedAnimation::new(&freeze, 1.0, 0.0, 200, target);
                    anim.set_easing(adw::Easing::EaseOutCubic);
                    anim.connect_done({
                        let freeze = freeze.clone();
                        let slot = self.freeze_fade.clone();
                        move |_| {
                            slot.borrow_mut().take();
                            freeze.set_visible(false);
                            freeze.set_opacity(1.0);
                        }
                    });
                    *self.freeze_fade.borrow_mut() = Some(anim.clone());
                    anim.play();
                } else {
                    let timer =
                        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(80), {
                            let freeze = freeze.clone();
                            let slot = self.freeze_timer.clone();
                            move || {
                                slot.borrow_mut().take();
                                freeze.set_visible(false);
                            }
                        });
                    *self.freeze_timer.borrow_mut() = Some(timer);
                }
            }
        }
        (scroller, saved_scroll)
    }

    /// Empty the three boxes, and style them for the icon rail when it is on.
    fn clear_boxes(&self, pinned: &gtk::Box, container: &gtk::Box, footer: &gtk::Box) {
        while let Some(child) = pinned.first_child() {
            pinned.remove(&child);
        }
        while let Some(child) = container.first_child() {
            container.remove(&child);
        }
        while let Some(child) = footer.first_child() {
            footer.remove(&child);
        }
        if self.collapsed {
            pinned.add_css_class("rail-collapsed");
            container.add_css_class("rail-collapsed");
            footer.add_css_class("rail-collapsed");
        } else {
            pinned.remove_css_class("rail-collapsed");
            container.remove_css_class("rail-collapsed");
            footer.remove_css_class("rail-collapsed");
        }
        // Unread dots in place of counts (Settings → Sidebar → Icon rail):
        // a style on the rail's containers, so every mini chip built below
        // shrinks to a dot without each builder knowing.
        for w in [pinned, container, footer] {
            if self.collapsed && self.rail_dots {
                w.add_css_class("rail-dots");
            } else {
                w.remove_css_class("rail-dots");
            }
            // The same way, for every count in the sidebar: their visibility
            // is set in many places as the numbers change, and all of those
            // may stay unaware of this switch.
            if self.unread_counts {
                w.remove_css_class("no-unread-counts");
            } else {
                w.add_css_class("no-unread-counts");
            }
        }
    }

    /// Drop every widget handle of the previous build.
    fn forget_widgets(&mut self) {
        self.revealers.clear();
        self.chevrons.clear();
        self.folder_lists.clear();
        self.custom_folder_lists.clear();
        self.custom_revealers.clear();
        self.custom_chevrons.clear();
        self.unified_list = None;
        self.footer_list = None;
        self.attachments_row = None;
        self.contacts_row = None;
        self.outbox_list = None;
        self.folder_badges.clear();
        self.tree_chevrons.clear();
        self.tree_row_revealers.clear();
        self.main_folders.clear();
        self.main_row_revealers.clear();
        self.unified_badge = None;
        self.unified_revealer = None;
        self.unified_chevron = None;
        self.unified_inbox_list = None;
        self.unified_inboxes.clear();
        self.unified_inbox_badges.clear();
        self.filtered_badges.clear();
        self.account_circle_badges.clear();
        self.tag_sections.clear();
        self.kind_widgets.clear();
    }

    /// The prompt shown instead of an empty sidebar while there are no
    /// accounts.
    fn build_empty_state(&self, container: &gtk::Box, sender: &ComponentSender<Self>) {
        let s = sender.clone();
        let add = gtk::Button::new();
        add.add_css_class("suggested-action");
        add.add_css_class("pill");
        add.set_valign(gtk::Align::Center);
        add.set_halign(gtk::Align::Center);
        add.connect_clicked(move |_| {
            let _ = s.output(SidebarOutput::AddAccount);
        });
        if self.collapsed {
            add.set_icon_name("list-add-symbolic");
            add.set_tooltip_text(Some(i18n("Add account").as_str()));
            add.set_margin_top(12);
            container.append(&add);
        } else {
            let label_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            label_box.append(&gtk::Image::from_icon_name("list-add-symbolic"));

            let add_label = gtk::Label::new(Some(i18n("Add first account").as_str()));
            add_label.set_wrap(true);
            add_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            add_label.set_justify(gtk::Justification::Center);
            add_label.set_xalign(0.5);
            add_label.set_hexpand(true);
            label_box.append(&add_label);

            add.set_halign(gtk::Align::Fill);
            add.set_child(Some(&label_box));
            let empty = gtk::Box::new(gtk::Orientation::Vertical, 12);
            empty.set_valign(gtk::Align::Start);
            empty.set_margin_top(36);
            empty.set_margin_start(16);
            empty.set_margin_end(16);
            let hint = gtk::Label::new(Some(i18n("No accounts yet").as_str()));
            hint.add_css_class("dim-label");
            empty.append(&hint);
            empty.append(&add);
            container.append(&empty);
        }
    }

    fn build_compose_bar(&mut self, pinned: &gtk::Box, sender: &ComponentSender<Self>) {
        // "New message" — the compose action. Expanded, the pill sits alone
        // and centred (Refresh lives in the app's header bar, top-left across
        // from the menu). The collapsed rail's header only has room for the
        // menu button, so Refresh stacks here instead — directly below it.
        {
            let bar = gtk::Box::new(
                if self.collapsed {
                    gtk::Orientation::Vertical
                } else {
                    gtk::Orientation::Horizontal
                },
                0,
            );

            self.sync_stack = None;
            self.sync_spinner = None;
            self.sync_button = None;
            if self.collapsed {
                // Refresh, showing a spinner while any account syncs.
                let refresh = gtk::Button::new();
                refresh
                    .set_tooltip_text(Some(i18n("Refresh or long-press for Status Bar").as_str()));
                refresh.add_css_class("flat");
                refresh.set_valign(gtk::Align::Center);
                refresh.set_halign(gtk::Align::Center);
                let stack = gtk::Stack::new();
                stack.set_transition_type(gtk::StackTransitionType::Crossfade);
                let icon = gtk::Image::from_icon_name("view-refresh-symbolic");
                stack.add_named(&icon, Some("icon"));
                let spinner = gtk::Spinner::new();
                spinner.set_spinning(self.busy);
                stack.add_named(&spinner, Some("spinner"));
                stack.set_visible_child_name(if self.busy { "spinner" } else { "icon" });
                refresh.set_child(Some(&stack));
                let s = sender.clone();
                refresh.connect_clicked(move |_| {
                    let _ = s.output(SidebarOutput::RefreshRequested);
                });
                // Long-press reveals the status bar (same as the header
                // refresh); claiming the sequence suppresses the click.
                let long = gtk::GestureLongPress::new();
                let s = sender.clone();
                long.connect_pressed(move |gesture, _, _| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    let _ = s.output(SidebarOutput::StatusBarRequested);
                });
                refresh.add_controller(long);
                crate::ui::show_refresh_offline(&refresh, self.offline);
                bar.append(&refresh);
                self.sync_button = Some(refresh);
                self.sync_stack = Some(stack);
                self.sync_spinner = Some(spinner);
            }

            // The compose button, drawn like a row so it matches the
            // sidebar's look. Expanded: full width like the rows below, icon
            // and label centred, and a caret at the end, as the composer's
            // Send has for Send Later, opening the menu a right-click opens
            // (#360). The collapsed rail shows the icon alone, its menu on a
            // right-click only.
            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::None);
            list.add_css_class("navigation-sidebar");
            let row = gtk::ListBoxRow::new();
            row.add_css_class("compose-row");
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            hbox.add_css_class("folder-row");
            let menu_sender = sender.clone();
            let compose_menu = move |widget: &gtk::Widget, x: f64, y: f64| {
                let (blank, from_template) = (menu_sender.clone(), menu_sender.clone());
                show_context_menu(
                    widget,
                    x,
                    y,
                    vec![vec![
                        MenuEntry::new(i18n("New Message"), move || {
                            let _ = blank.output(SidebarOutput::ComposeRequested);
                        })
                        .icon("mail-message-new-symbolic"),
                        MenuEntry::new(i18n("New Message from Template…"), move || {
                            let _ =
                                from_template.output(SidebarOutput::ComposeFromTemplateRequested);
                        })
                        .icon("folder-templates-symbolic"),
                    ]],
                );
            };
            if self.collapsed {
                // The rail has no room for a label; the icon carries it there.
                let img = gtk::Image::from_icon_name("mail-message-new-symbolic");
                img.add_css_class("folder-icon");
                pin_icon_size(&img);
                hbox.set_halign(gtk::Align::Center);
                row.set_tooltip_text(Some(i18n("New Message").as_str()));
                hbox.append(&img);
            } else {
                hbox.set_spacing(0);
                hbox.add_css_class("compose-split");
                let main = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                main.set_hexpand(true);
                main.set_halign(gtk::Align::Center);
                let icon = gtk::Image::from_icon_name("mail-message-new-symbolic");
                icon.add_css_class("folder-icon");
                main.append(&icon);
                let label = gtk::Label::new(Some(i18n("New Message").as_str()));
                label.add_css_class("account-name");
                // The pill must be able to shrink with the sidebar (down to its
                // 180px minimum) — otherwise the whole column's minimum width
                // exceeds the pane and every row highlight overflows the edge.
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                main.append(&label);
                hbox.append(&main);
                hbox.append(&gtk::Separator::new(gtk::Orientation::Vertical));
                // Its own click: the row's activation, which composes, never
                // sees it.
                let caret = gtk::Button::from_icon_name("pan-down-symbolic");
                caret.add_css_class("flat");
                caret.add_css_class("compose-caret");
                caret.set_focus_on_click(false);
                caret.set_valign(gtk::Align::Center);
                caret.set_tooltip_text(Some(i18n("New Message from Template…").as_str()));
                let menu = compose_menu.clone();
                let quiet = self.quiet.clone();
                caret.connect_clicked(move |b| {
                    if !quiet.get() {
                        menu(
                            b.upcast_ref(),
                            f64::from(b.width()) / 2.0,
                            f64::from(b.height()),
                        );
                    }
                });
                hbox.append(&caret);
            }
            row.set_child(Some(&hbox));
            // A right-click offers a template to start from (#360).
            let right_click = gtk::GestureClick::new();
            right_click.set_button(gtk::gdk::BUTTON_SECONDARY);
            right_click.connect_pressed(move |gesture, _, x, y| {
                let Some(widget) = gesture.widget() else {
                    return;
                };
                compose_menu(&widget, x, y);
            });
            row.add_controller(right_click);
            list.append(&row);
            let s = sender.clone();
            let quiet = self.quiet.clone();
            list.connect_row_activated(move |_, _| {
                if quiet.get() {
                    return;
                }
                let _ = s.output(SidebarOutput::ComposeRequested);
            });

            if !self.collapsed {
                // Full width, like the rows below it; the centred label
                // carries the action on its own.
                list.set_hexpand(true);
                list.set_halign(gtk::Align::Fill);
            }
            bar.append(&list);
            pinned.append(&bar);
        }
    }

    fn build_unified_section(
        &mut self,
        container: &gtk::Box,
        sections: &[SectionData],
        sender: &ComponentSender<Self>,
    ) {
        // The unified section: the "All Inboxes" row and the Starred / Sent /
        // Drafts rows (Settings → Sidebar → Unified), all built alike: a
        // header row that opens the merged view and a caret that opens the
        // list beneath. It heads the scrolling sidebar rather than the pinned
        // area: with several lists open at once it can stand taller than a
        // short window, and pinned it would have forced the window taller
        // than the screen.
        let mut unified_rows: Vec<UnifiedRow> = Vec::new();
        if self.show_unified {
            unified_rows.push(UnifiedRow::Kind(FolderKind::Inbox));
        }
        unified_rows.extend(
            self.unified_kinds
                .listed()
                .into_iter()
                .map(UnifiedRow::Kind),
        );
        // Smart folders (#417) close the section, and stand alone with a
        // single account.
        if !self.smart_folders.is_empty() {
            unified_rows.push(UnifiedRow::Smart);
        }
        let unified_sections: Vec<SectionData> =
            sections.iter().filter(|s| s.in_unified).cloned().collect();
        self.build_unified_run(container, &unified_rows, &unified_sections, sender);
    }

    fn build_footer(&mut self, footer: &gtk::Box, sender: &ComponentSender<Self>) {
        // Contacts and Attachments live in the pinned footer against the
        // sidebar's bottom edge — they keep out of the way of the account
        // list and never scroll off. A faint rule sets them apart from
        // whatever the scroller above ends on. ONE list box holds both rows,
        // so they read as one gapless section (beta 1.18.0b feedback) and
        // selecting one automatically clears the other.
        if self.show_attachments || self.contacts_in_footer() {
            let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
            sep.add_css_class("footer-separator");
            footer.append(&sep);

            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            list.add_css_class("navigation-sidebar");
            list.add_css_class("sidebar-footer");

            // "Contacts" row: the in-app contacts view, where Settings →
            // Sidebar puts it by default. Right-click offers GNOME Contacts.
            if self.contacts_in_footer() {
                let row = self.footer_row("x-office-address-book-symbolic", &i18n("Contacts"));
                let right_click = gtk::GestureClick::new();
                right_click.set_button(3);
                let s = sender.clone();
                right_click.connect_pressed(move |gesture, _, x, y| {
                    let Some(widget) = gesture.widget() else {
                        return;
                    };
                    let s2 = s.clone();
                    show_context_menu(
                        &widget,
                        x,
                        y,
                        vec![vec![MenuEntry::new(
                            i18n("Open GNOME Contacts"),
                            move || {
                                let _ = s2.output(SidebarOutput::OpenGnomeContacts);
                            },
                        )
                        .icon("adw-external-link-symbolic")]],
                    );
                });
                row.add_controller(right_click);
                list.append(&row);
                self.contacts_row = Some(row);
            }

            // "Attachments" row — a gallery of every inbox attachment. The
            // very last row in the sidebar, below Contacts.
            if self.show_attachments {
                let row = self.footer_row("mail-attachment-symbolic", &i18n("Attachments"));
                list.append(&row);
                self.attachments_row = Some(row);
            }

            let s = sender.clone();
            let attachments_row = self.attachments_row.clone();
            let contacts_row = self.contacts_row.clone();
            let quiet = self.quiet.clone();
            list.connect_row_selected(move |_, row| {
                if quiet.get() {
                    return;
                }
                let Some(row) = row else { return };
                if Some(row) == attachments_row.as_ref() {
                    s.input(SidebarInput::AttachmentsRowSelected);
                } else if Some(row) == contacts_row.as_ref() {
                    s.input(SidebarInput::ContactsRowClicked);
                }
            });
            footer.append(&list);
            self.footer_list = Some(list);
        }
    }

    fn build_outbox_row(&mut self, container: &gtk::Box, sender: &ComponentSender<Self>) {
        // "Outbox" row — only while something is waiting to be sent. It sits
        // directly above the accounts so a stuck message is impossible to miss.
        if self.outbox_count > 0 {
            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            list.add_css_class("navigation-sidebar");

            let row = gtk::ListBoxRow::new();
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            hbox.add_css_class("folder-row");
            let img = gtk::Image::from_icon_name("mail-send-symbolic");
            img.add_css_class("folder-icon");
            pin_icon_size(&img);
            let badge = gtk::Label::new(Some(&self.outbox_count.to_string()));
            style_badge(&badge, 5);
            badge.add_css_class("outbox-count");
            if self.collapsed {
                hbox.set_halign(gtk::Align::Center);
                row.set_tooltip_text(Some(&format!(
                    "Outbox — {} waiting to be sent",
                    self.outbox_count
                )));
                hbox.append(&img);
            } else {
                if self.chevrons_left {
                    img.set_margin_start(ROW_LEFT_INSET);
                }
                hbox.append(&img);
                let label = gtk::Label::new(Some(i18n("Outbox").as_str()));
                label.set_hexpand(true);
                label.set_halign(gtk::Align::Start);
                label.add_css_class("account-name");
                hbox.append(&label);
                hbox.append(&badge);
            }
            row.set_child(Some(&hbox));
            list.append(&row);

            let s = sender.clone();
            let quiet = self.quiet.clone();
            list.connect_row_selected(move |_, row| {
                if quiet.get() {
                    return;
                }
                if row.is_some() {
                    s.input(SidebarInput::OutboxRowSelected);
                }
            });
            container.append(&list);
            self.outbox_list = Some(list);
        }
    }

    fn load_account_css(&self, sections: &[SectionData]) {
        // Per-account avatar colors (background + readable text), and the
        // ring every account circle wears. The ring stands a pixel off the
        // disc: flush against a disc of its own color it would only make
        // the disc bigger.
        let mut css = String::new();
        for s in sections {
            let text = crate::color::readable_text(&s.color);
            css.push_str(&format!(
                ".acct-color-{0} {{ background-color: {1}; }} \
                 .acct-color-{0} label {{ color: {2}; }} \
                 .acct-tint-{0} {{ color: {1}; }} \
                 .acct-ring-{0} {{ outline: 2px solid {1}; outline-offset: 1px; }}\n",
                s.account.id, s.color, text
            ));
        }
        self.color_provider.load(css);
    }

    fn arm_revealers(&self) {
        // The revealers were built with no transition so the rebuilt content
        // reaches full height in the very first layout pass; hand them their
        // real animation back once that pass is done, for user toggles.
        {
            let revs: Vec<gtk::Revealer> = self
                .revealers
                .values()
                .chain(self.custom_revealers.values())
                .chain(self.tree_row_revealers.values().flatten())
                .cloned()
                .chain(self.unified_revealer.clone())
                .chain(self.tag_sections.values().map(|w| w.revealer.clone()))
                .chain(self.kind_widgets.values().map(|w| w.revealer.clone()))
                .collect();
            gtk::glib::idle_add_local_once(move || {
                for r in &revs {
                    r.set_transition_duration(200);
                }
            });
        }
    }

    /// Grey the unread counts of mail already looked at (#343): a chip is
    /// "seen" unless one of the folders it counts has mail new since. With
    /// the setting off, none is.
    pub(super) fn restyle_seen(&self) {
        let seen = |keys: &mut dyn Iterator<Item = &(u32, u32)>| match &self.fresh {
            None => false,
            Some(fresh) => {
                for k in keys {
                    if fresh.contains(k) {
                        return false;
                    }
                }
                true
            }
        };
        let mark = |label: &gtk::Label, on: bool| {
            if on {
                label.add_css_class("seen");
            } else {
                label.remove_css_class("seen");
            }
        };
        for (key, label) in self
            .folder_badges
            .iter()
            .chain(&self.unified_inbox_badges)
            .chain(self.filtered_badges.values().flatten())
            .chain(self.kind_widgets.values().flat_map(|w| &w.row_badges))
        {
            mark(label, seen(&mut std::iter::once(key)));
        }
        if let Some(label) = &self.unified_badge {
            mark(label, seen(&mut self.unified_inbox_badges.keys()));
        }
        for w in self.kind_widgets.values() {
            if let Some(b) = &w.badge {
                mark(b, seen(&mut w.row_badges.keys()));
            }
        }
        for section in &self.sections {
            if let Some(label) = self.account_circle_badges.get(&section.account.id) {
                let inbox = section
                    .folders
                    .iter()
                    .find(|f| f.kind == FolderKind::Inbox)
                    .map(|f| (section.account.id, f.id));
                mark(label, seen(&mut inbox.iter()));
            }
        }
    }

    /// A row of the pinned footer: an icon and a name, the icon alone
    /// (the name in its tooltip) on the rail.
    fn footer_row(&self, icon: &str, name: &str) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::new();
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        hbox.add_css_class("folder-row");
        let img = gtk::Image::from_icon_name(icon);
        img.add_css_class("folder-icon");
        pin_icon_size(&img);
        if self.collapsed {
            hbox.set_halign(gtk::Align::Center);
            row.set_tooltip_text(Some(name));
            hbox.append(&img);
        } else {
            if self.chevrons_left {
                img.set_margin_start(ROW_LEFT_INSET);
            }
            hbox.append(&img);
            let label = gtk::Label::new(Some(name));
            label.set_hexpand(true);
            label.set_halign(gtk::Align::Start);
            label.add_css_class("account-name");
            hbox.append(&label);
        }
        row.set_child(Some(&hbox));
        row
    }
}

/// A list about to go keeps no keyboard focus: GTK would hand it to the
/// first list left, All Inboxes, and a list selects the row it focuses,
/// opening All Inboxes over the folder on screen. A folder's menu (Add to
/// Favorites, #367) rebuilds right under its own list.
fn release_focus(pinned: &gtk::Box, container: &gtk::Box, footer: &gtk::Box) {
    if let Some(root) = container.root() {
        let doomed = root.focus().is_some_and(|f| {
            [pinned, container, footer]
                .iter()
                .any(|b| f.is_ancestor(*b))
        });
        if doomed {
            root.set_focus(None::<&gtk::Widget>);
        }
    }
}

fn pin_scroll(scroller: Option<gtk::ScrolledWindow>, saved_scroll: Option<f64>) {
    // Restore the scroll offset before anything paints: an idle-time
    // restore let one frame render at the top first — a visible flash on
    // every rebuild. The adjustment's `changed` signal fires while the
    // fresh rows are being measured (same layout pass), so pinning the
    // value there means no frame ever shows the wrong offset. The pin
    // holds through the freeze-frame window — layout can keep settling
    // for a few frames — then a timer finalises and disconnects.
    if let (Some(pos), Some(scroller)) = (saved_scroll, scroller) {
        if pos > 0.0 {
            let adj = scroller.vadjustment();
            adj.set_value(pos);
            let handler: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SignalHandlerId>>> =
                std::rc::Rc::new(std::cell::RefCell::new(None));
            *handler.borrow_mut() = Some(adj.connect_changed(move |adj| {
                adj.set_value(pos);
            }));
            let adj = scroller.vadjustment();
            let handler = handler.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
                adj.set_value(pos);
                if let Some(id) = handler.borrow_mut().take() {
                    adj.disconnect(id);
                }
            });
        }
    }
}
