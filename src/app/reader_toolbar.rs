//! The reader header's action buttons: their order, folding into the
//! overflow menu when the window is narrow, and the menu itself.

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::i18n::{i18n, i18n_f};
use crate::models::{FolderKind, Message};
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::ui::message_actions::{KeysOf, MessageAction};
use crate::ui::preferences::PrefInput;

use super::model::AppModel;
use super::msg::AppMsg;
use super::AppModelWidgets;

/// Fallback threshold for folding the reader header's right-hand actions
/// into the overflow menu. Normally the threshold is *measured* at startup from the
/// real row (see the breakpoint in init) so it tracks the user's decoration
/// layout; this value only stands in if that measurement comes back empty.
const READER_ACTIONS_BREAKPOINT: f64 = 490.0;

/// The reader header's action buttons and fold breakpoint, kept for
/// re-packing when the layout changes (see relayout_reader_toolbar).
pub(super) struct ReaderToolbarWidgets {
    /// Each action button inside its Focus Mode revealer.
    pub(super) buttons: Vec<(config::ToolbarItem, gtk::Widget)>,
    /// The Outbox's own buttons' revealers (Edit, Send, Send all).
    pub(super) outbox: Vec<gtk::Revealer>,
    spinner: gtk::Widget,
    bin: adw::BreakpointBin,
    breakpoint: adw::Breakpoint,
    /// The second breakpoint: below this the buttons draw narrower, or the
    /// left group, ⋯ and window controls would push past the pane's edge.
    /// The left group itself never folds.
    compact_breakpoint: adw::Breakpoint,
    /// What the folded row (left group, ⋯, window controls) needs at full
    /// size, and whether that was measured rather than estimated.
    compact_need: std::cell::Cell<i32>,
    compact_measured: std::cell::Cell<bool>,
    /// What the whole row needs with the right group showing, and whether
    /// that was measured: the fold comes when the two groups are one
    /// button gap apart.
    full_need: std::cell::Cell<i32>,
    full_measured: std::cell::Cell<bool>,
    /// The row's cost with every action button gone (padding, spacing…).
    base: i32,
    /// One action button's natural width.
    button_w: i32,
    /// The window controls' width (re-measured when the decoration layout
    /// changes).
    controls: std::cell::Cell<i32>,
}

/// A menu entry that sends `msg` to the app, built afresh on each click.
fn action_entry(
    sender: &ComponentSender<AppModel>,
    label: String,
    icon: &str,
    msg: impl Fn() -> AppMsg + 'static,
    enabled: bool,
) -> crate::ui::context_menu::MenuEntry {
    let s = sender.input_sender().clone();
    crate::ui::context_menu::MenuEntry::new(label, move || {
        let _ = s.send(msg());
    })
    .icon(format!("{icon}-symbolic"))
    .enabled(enabled)
}

impl AppModel {
    pub(super) fn remote_allowed(&self, m: &Message) -> bool {
        // A choice made for this one message wins over the standing policy,
        // in both directions: it is how remote content is shown (or put back)
        // for a single message when the banner is switched off.
        if let Some(&show) = self.remote_override.get(&(m.account_id, m.id)) {
            return show;
        }
        if self.auto_remote_content {
            return true;
        }
        let addr = m.from_addr.to_lowercase();
        self.allowed_senders.contains(&addr)
    }

    /// The tag entries of the reader's menus (#71): one per tag, its swatch
    /// filled where the reader's target message carries it. None without a
    /// target or a tag.
    pub(super) fn reader_tag_entries(
        &self,
        sender: &ComponentSender<Self>,
    ) -> Option<Vec<crate::ui::context_menu::MenuEntry>> {
        if self.tags.is_empty() {
            return None;
        }
        // Several selected (#313): a tag is ticked when every one carries
        // it, since only then does choosing it take it off.
        let many = self.multi_selection();
        let target = match many.first() {
            Some(first) => {
                let mut all = first.clone();
                all.keywords = first.tag_keywords(&self.tags);
                all.keywords
                    .retain(|k| many.iter().all(|m| self.carries_tag(m, k)));
                all
            }
            None => self.reply_target()?,
        };
        let s = sender.input_sender().clone();
        Some(crate::ui::message_list::tag_menu_entries(
            &self.tags,
            &target,
            move |keyword, _add| {
                let _ = s.send(AppMsg::ToggleTagCurrent(keyword));
            },
        ))
    }

    /// The collapsed reader header's overflow menu: the right group of the
    /// toolbar (Tags, Read/Unread, Spam, Move To, Find, Print), same icons,
    /// enabled under the same conditions. The left group never folds.
    fn show_reader_overflow_menu(&self, sender: &ComponentSender<Self>) {
        let has_current = self.current.is_some();
        let focus = self.focus.active(config::FocusPart::ReaderToolbar);
        let sections = if self.showing_outbox {
            self.outbox_overflow_sections(has_current, focus, sender)
        } else {
            self.toolbar_overflow_sections(has_current, focus, sender)
        };

        let btn = &self.reader_overflow_btn;
        show_context_menu(btn, (btn.width() / 2) as f64, btn.height() as f64, sections);
    }

    /// The Outbox's overflow: its own buttons (Edit, Send, Send all,
    /// Delete) sit in the always-visible left group; only View Source is
    /// left to fold here (queued rows have no context menu to carry it),
    /// unless Focus Mode folded that group too.
    fn outbox_overflow_sections(
        &self,
        has_current: bool,
        focus: bool,
        sender: &ComponentSender<Self>,
    ) -> Vec<Vec<MenuEntry>> {
        let mut sections = Vec::new();
        if focus {
            let many = has_current || self.many_selected();
            sections.push(vec![
                action_entry(
                    sender,
                    i18n("Edit this message"),
                    "document-edit",
                    || AppMsg::EditCurrentOutbox,
                    has_current,
                ),
                action_entry(
                    sender,
                    i18n("Try to send this message now"),
                    "mail-send",
                    || AppMsg::SendCurrentOutbox,
                    has_current,
                ),
                action_entry(
                    sender,
                    i18n("Send all"),
                    "mail-send",
                    || AppMsg::RetryAllOutbox,
                    true,
                ),
                action_entry(
                    sender,
                    i18n("Delete"),
                    "user-trash",
                    || AppMsg::Delete,
                    many,
                ),
            ]);
        }
        sections.push(vec![action_entry(
            sender,
            i18n("View Source"),
            "code",
            || AppMsg::ViewSource,
            has_current,
        )]);
        sections
    }

    /// The reader toolbar's overflow: the right group, in its own order;
    /// the left group stays on the bar at every width. Per-message actions
    /// act on the reply target: the open message, or, in a conversation,
    /// the one highlighted card. With none (or several) highlighted they
    /// grey out.
    fn toolbar_overflow_sections(
        &self,
        has_current: bool,
        focus: bool,
        sender: &ComponentSender<Self>,
    ) -> Vec<Vec<MenuEntry>> {
        use config::ToolbarItem as T;
        use MessageAction as A;
        let target = self.reply_target();
        let acts = target.is_some();
        let many = acts || self.many_selected();
        let starred = target.as_ref().is_some_and(|m| m.starred);
        let target_unread = target.as_ref().is_some_and(|m| m.unread);
        let restorable = target.as_ref().is_some_and(|m| {
            self.folder_kind(m.account_id, m.folder_id)
                .is_some_and(|k| matches!(k, FolderKind::Trash | FolderKind::Junk))
        });
        // Focus Mode folds the left group in as well, as a section of
        // its own ahead of the right group.
        let groups: Vec<&[T]> = if focus {
            vec![&self.reader_toolbar.left, &self.reader_toolbar.right]
        } else {
            vec![&self.reader_toolbar.right]
        };
        let item = |action: MessageAction, msg: fn() -> AppMsg, enabled: bool| {
            action_entry(sender, action.label(), action.icon(), msg, enabled)
                .keys(action.keys(KeysOf::Main))
        };
        let mut sections = Vec::new();
        for group in groups {
            let mut section = Vec::new();
            for item_kind in group {
                match item_kind {
                    T::Reply => section.push(item(A::Reply, || AppMsg::Reply, acts)),
                    T::ReplyAll => section.push(item(A::ReplyAll, || AppMsg::ReplyAll, acts)),
                    T::Forward => section.push(item(A::Forward, || AppMsg::Forward, acts)),
                    T::Star => section.push(item(
                        if starred { A::RemoveStar } else { A::Star },
                        || AppMsg::ToggleStar,
                        many,
                    )),
                    T::Archive => section.push(item(A::Archive, || AppMsg::Archive, many)),
                    // Snooze (#527): its times behind a submenu, as Tags.
                    T::Snooze => {
                        if self.snooze_offered() {
                            section.push(
                                MenuEntry::submenu(A::Snooze.label(), self.snooze_sections(sender))
                                    .icon(format!("{}-symbolic", A::Snooze.icon()))
                                    .keys(A::Snooze.keys(KeysOf::Main)),
                            );
                        }
                    }
                    T::Delete => section.push(item(A::Delete, || AppMsg::Delete, many)),
                    T::Spam => section.push(item(
                        if self.target_in_junk() {
                            A::NotSpam
                        } else {
                            A::MarkSpam
                        },
                        || AppMsg::MarkSpam,
                        many,
                    )),
                    T::ReadUnread => section.push(item(
                        if target_unread {
                            A::MarkRead
                        } else {
                            A::MarkUnread
                        },
                        || AppMsg::ToggleReadCurrent,
                        many,
                    )),
                    // Tags (#71), where there are any and something to tag —
                    // behind a submenu, as in the message list's menu.
                    T::Tags => {
                        if let Some(entries) = self.reader_tag_entries(sender) {
                            section.push(
                                MenuEntry::submenu(A::Tags.label(), vec![entries])
                                    .icon("tag-outline-symbolic")
                                    .keys(A::Tags.keys(KeysOf::Main)),
                            );
                        }
                    }
                    T::MoveTo => {
                        if restorable {
                            section.push(item(A::MoveToInbox, || AppMsg::MoveToInbox, acts));
                        }
                        section.push(item(A::MoveTo, || AppMsg::MoveToMenu, many));
                    }
                    T::Find => section.push(item(A::Find, || AppMsg::OpenReaderFind, has_current)),
                    // View Source is deliberately absent: it lives in the
                    // message list's context menu only (the Outbox variant
                    // above keeps it — queued rows have no such menu).
                    T::Print => {
                        section.push(item(A::PrintPreview, || AppMsg::PrintPreview, has_current))
                    }
                }
            }
            if !section.is_empty() {
                sections.push(section);
            }
        }
        sections
    }

    /// Whether the reader header shows `item` right now: it must be in the
    /// saved layout, and a right-group button also needs the pane wide
    /// enough (the left group never folds). Delete alone also serves the
    /// Outbox; the inline composer hides the whole row.
    pub(super) fn toolbar_visible(&self, item: config::ToolbarItem) -> bool {
        use config::ToolbarSide;
        self.reader_compose.is_none()
            && (item == config::ToolbarItem::Delete || !self.showing_outbox)
            && match self.reader_toolbar.side(item) {
                Some(ToolbarSide::Left) => true,
                // The compact breakpoint is applied alone, in place of the
                // fold, and folds the right group all the same.
                Some(ToolbarSide::Right) => !self.right_group_folded(),
                None => false,
            }
    }

    /// The ⋯ overflow stands in for the folded right group: only while
    /// collapsed, and only when that group has something to offer (the
    /// Outbox keeps View Source there regardless).
    pub(super) fn reader_overflow_wanted(&self) -> bool {
        // Focus Mode folds the whole row in, so the ⋯ is the toolbar.
        if self.focus.active(config::FocusPart::ReaderToolbar) {
            return true;
        }
        self.right_group_folded() && (self.showing_outbox || !self.reader_toolbar.right.is_empty())
    }

    /// Whether the right group is in the ⋯ menu: by choice, or for want of
    /// room.
    fn right_group_folded(&self) -> bool {
        self.toolbar_fold_right || self.reader_actions_collapsed || self.reader_compact
    }

    /// Pack the header's action buttons in the saved order: the left group
    /// via pack_start, the right group via pack_end (which fills right to
    /// left, so in reverse), then the attachments spinner so it keeps the
    /// innermost slot of the right group. Buttons on neither side are left
    /// unpacked. Re-folds the row for the new count afterwards.
    pub(super) fn relayout_reader_toolbar(&self) {
        let (Some(header), Some(tb)) =
            (self.reader_header.get(), self.reader_toolbar_widgets.get())
        else {
            return;
        };
        for (_, w) in &tb.buttons {
            if w.parent().is_some() {
                header.remove(w);
            }
        }
        if tb.spinner.parent().is_some() {
            header.remove(&tb.spinner);
        }
        let find =
            |item: config::ToolbarItem| tb.buttons.iter().find(|(i, _)| *i == item).map(|(_, w)| w);
        // Each button's Focus Mode revealer slides towards the edge its
        // group sits against: GTK's SlideRight moves a folding child off to
        // the left, SlideLeft keeps it anchored and shrinks it from its
        // right, which is what an end-packed group needs.
        for item in &self.reader_toolbar.left {
            if let Some(w) = find(*item) {
                if let Some(r) = w.downcast_ref::<gtk::Revealer>() {
                    r.set_transition_type(gtk::RevealerTransitionType::SlideRight);
                }
                header.pack_start(w);
            }
        }
        for item in self.reader_toolbar.right.iter().rev() {
            if let Some(w) = find(*item) {
                if let Some(r) = w.downcast_ref::<gtk::Revealer>() {
                    r.set_transition_type(gtk::RevealerTransitionType::SlideLeft);
                }
                header.pack_end(w);
            }
        }
        header.pack_end(&tb.spinner);
        self.refresh_reader_breakpoint();
    }

    /// Point the fold breakpoint at the width the current layout needs: the
    /// row's fixed cost, the window controls, and one button's worth per
    /// shown item (hidden-for-now buttons like Tags without tags count too —
    /// that slack keeps the fold a step ahead of a squeeze).
    pub(super) fn refresh_reader_breakpoint(&self) {
        let Some(tb) = self.reader_toolbar_widgets.get() else {
            return;
        };
        let shown = (self.reader_toolbar.left.len() + self.reader_toolbar.right.len()) as i32;
        let threshold = if tb.button_w <= 0 {
            READER_ACTIONS_BREAKPOINT
        } else {
            (tb.base + tb.controls.get() + shown * tb.button_w) as f64 + 24.0
        };
        tracing::info!(
            "reader toolbar: {shown} buttons × {}px + base {}px + controls {}px → collapse below {threshold:.0}px",
            tb.button_w,
            tb.base,
            tb.controls.get()
        );
        tb.breakpoint.set_condition(Some(&max_width(threshold)));
        // Until the folded row is measured, a generous estimate: each button
        // with the headerbar's spacing, and its padding at both ends.
        let left = (self.reader_toolbar.left.len() + 1) as i32;
        let estimate = left * (tb.button_w.max(36) + 6) + tb.controls.get() + 48;
        tb.compact_measured.set(false);
        tb.full_measured.set(false);
        tb.full_need.set(threshold as i32);
        tb.compact_need.set(estimate);
        tb.compact_breakpoint
            .set_condition(Some(&max_width(f64::from(estimate))));
        tb.bin.queue_resize();
        let s = self.self_sender.clone();
        self.after_paint(move || {
            let _ = s.send(AppMsg::MeasureReaderFold);
        });
    }

    /// The breakpoints' widths, from the row as it is drawn. With the right
    /// group showing, its minimum width is where the groups meet: the fold
    /// comes one button gap before. With it folded, the same measure is
    /// where the compact buttons take over, and the right group's buttons
    /// on top of it stand in for the whole row until it is seen again. The
    /// widest seen since the layout last changed counts, so a shorter row
    /// (the Outbox's, the toolbar without its Tags) cannot lower either.
    fn measure_reader_fold(&self) {
        let (Some(header), Some(tb)) =
            (self.reader_header.get(), self.reader_toolbar_widgets.get())
        else {
            return;
        };
        if self.reader_compact
            || self.reader_compose.is_some()
            || self.showing_outbox
            || self.focus.active(config::FocusPart::ReaderToolbar)
        {
            return;
        }
        // The revealers open after the first frames: measured before, the
        // row looks empty. Try again once they are open.
        let opening = tb.buttons.iter().any(|(_, w)| {
            w.downcast_ref::<gtk::Revealer>()
                .is_some_and(|r| r.get_visible() && !r.is_child_revealed())
        });
        if opening || !header.is_mapped() {
            let s = self.self_sender.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                let _ = s.send(AppMsg::MeasureReaderFold);
            });
            return;
        }
        // The minimum, not the natural width: a headerbar's natural width
        // keeps its title centred, as wide again as its wider side, while
        // the buttons only collide below the two sides side by side.
        let min = header.measure(gtk::Orientation::Horizontal, -1).0;
        let (button, gap) = self.toolbar_button_pitch(tb);
        let widest = |need: i32, cell: &std::cell::Cell<i32>, seen: &std::cell::Cell<bool>| {
            let need = if seen.get() {
                need.max(cell.get())
            } else {
                need
            };
            seen.set(true);
            (need != cell.get()).then(|| {
                cell.set(need);
                need
            })
        };
        if self.reader_actions_collapsed || self.toolbar_fold_right {
            if let Some(need) = widest(min + gap, &tb.compact_need, &tb.compact_measured) {
                tracing::info!("reader toolbar: compact below {need}px");
                tb.compact_breakpoint
                    .set_condition(Some(&max_width(f64::from(need))));
            }
            if !tb.full_measured.get() {
                let right = self
                    .reader_toolbar
                    .right
                    .iter()
                    .filter(|i| **i != config::ToolbarItem::Tags || !self.tags.is_empty())
                    .count() as i32;
                // The ⋯ goes as the right group comes.
                let need = min - button + right * (button + gap) + gap;
                tb.full_need.set(need);
                tb.breakpoint
                    .set_condition(Some(&max_width(f64::from(need))));
            }
        } else if let Some(need) = widest(min + gap, &tb.full_need, &tb.full_measured) {
            tracing::info!("reader toolbar: the right group folds below {need}px");
            tb.breakpoint
                .set_condition(Some(&max_width(f64::from(need))));
        }
    }

    /// One toolbar button's width and the gap between two, from the left
    /// group as it is drawn.
    fn toolbar_button_pitch(&self, tb: &ReaderToolbarWidgets) -> (i32, i32) {
        let shown: Vec<&gtk::Widget> = self
            .reader_toolbar
            .left
            .iter()
            .filter_map(|item| tb.buttons.iter().find(|(i, _)| i == item))
            .map(|(_, w)| w)
            .filter(|w| w.get_visible())
            .collect();
        let button = tb.button_w.max(1);
        let Some(row) = shown.first().and_then(|w| w.parent()) else {
            return (button, 6);
        };
        let mut children = 0;
        let mut sum = 0;
        let mut child = row.first_child();
        while let Some(c) = child {
            if c.get_visible() {
                children += 1;
                sum += c.measure(gtk::Orientation::Horizontal, -1).0;
            }
            child = c.next_sibling();
        }
        let row_min = row.measure(gtk::Orientation::Horizontal, -1).0;
        let gap = if children > 1 {
            (row_min - sum) / (children - 1)
        } else {
            6
        };
        (button, gap.max(0))
    }

    /// Tooltip for the toolbar's trash button: says when it will delete the
    /// whole multi-selection rather than just the open message.
    pub(super) fn delete_tooltip(&self) -> String {
        match self.list_selection.len() {
            n if n > 1 => i18n_f("Delete {n} messages", &[("n", &n.to_string())]),
            _ => i18n("Delete"),
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_reader_toolbar(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::SetReaderActionsCollapsed(on) => {
                self.reader_actions_collapsed = on;
                self.reader_overflow_btn
                    .set_visible(self.reader_overflow_wanted());
                let s = sender.input_sender().clone();
                self.after_paint(move || {
                    let _ = s.send(AppMsg::MeasureReaderFold);
                });
            }

            AppMsg::SetReaderCompact(on) => {
                self.reader_compact = on;
                self.reader_overflow_btn
                    .set_visible(self.reader_overflow_wanted());
                if let Some(header) = self.reader_header.get() {
                    if on {
                        header.add_css_class("compact");
                    } else {
                        header.remove_css_class("compact");
                    }
                }
            }

            AppMsg::MeasureReaderFold => self.measure_reader_fold(),

            AppMsg::ReaderControlsChanged(controls) => {
                if let Some(tb) = self.reader_toolbar_widgets.get() {
                    tb.controls.set(controls);
                }
                self.refresh_reader_breakpoint();
            }

            AppMsg::ReaderOverflowMenu => self.show_reader_overflow_menu(&sender),
            AppMsg::ReaderToolbarMenu { x, y } => {
                use crate::ui::context_menu::{show_context_menu, MenuEntry};
                if let Some(header) = self.reader_header.get() {
                    let s = sender.input_sender().clone();
                    let entry = MenuEntry::new(i18n("Customize Toolbar…"), move || {
                        let _ = s.send(AppMsg::CustomizeToolbar);
                    })
                    .icon("preferences-desktop-appearance-symbolic");
                    show_context_menu(header, x, y, vec![vec![entry]]);
                }
            }
            AppMsg::CustomizeToolbar => {
                self.open_settings_window(&sender, false, false);
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::ShowPageById("appearance".to_string()));
                }
            }
            AppMsg::CustomizeColumns => {
                self.open_settings_window(&sender, false, false);
                if let Some(p) = &self.prefs {
                    p.emit(PrefInput::RevealColumns);
                }
            }
            _ => unreachable!("not routed to on_reader_toolbar"),
        }
    }

    /// Right-click on the header's empty space (not a button) offers
    /// the way to the toolbar editor.
    pub(super) fn init_reader_header_menu(
        &self,
        widgets: &AppModelWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let click = gtk::GestureClick::new();
        click.set_button(3);
        // Capture phase: the header's own window handle (a child, so
        // earlier in the bubble phase) would otherwise take the click
        // for the compositor's window menu and never let it through.
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let header: gtk::Widget = widgets.reader_header.clone().upcast();
        let s = sender.input_sender().clone();
        click.connect_pressed(move |g, _, x, y| {
            // Anything clickable under the pointer keeps its own
            // right-click (or lack of one).
            let mut w = header.pick(x, y, gtk::PickFlags::DEFAULT);
            while let Some(cur) = w {
                if cur == header {
                    break;
                }
                if cur.is::<gtk::Button>() || cur.is::<gtk::WindowControls>() {
                    return;
                }
                w = cur.parent();
            }
            g.set_state(gtk::EventSequenceState::Claimed);
            let _ = s.send(AppMsg::ReaderToolbarMenu { x, y });
        });
        widgets.reader_header.add_controller(click);
    }

    /// Collapse the reader header's actions into the overflow menu when the
    /// pane can no longer fit the full row — squeezing it further must never
    /// push the window controls off the right edge. The threshold is
    /// measured, not assumed: the window-controls cost depends on the
    /// user's decoration layout (GNOME ships one button, others run three),
    /// so the row's cost is taken from the real headerbar now — while every
    /// action is still visible, before the breakpoint could hide any — and
    /// the controls' share is re-derived if the decoration layout changes.
    pub(super) fn init_reader_toolbar_fold(
        &self,
        widgets: &AppModelWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let header: gtk::Widget = widgets.reader_header.clone().upcast();
        let full = header.measure(gtk::Orientation::Horizontal, -1).1;
        let mut controls = 0;
        measure_controls(&header, &mut controls);
        let buttons = toolbar_buttons(widgets);
        let (packed_sum, button_w) = button_widths(&buttons);
        let base = if full <= 0 {
            0
        } else {
            (full - controls - packed_sum).max(0)
        };
        let bp = fold_breakpoint(
            sender,
            READER_ACTIONS_BREAKPOINT,
            AppMsg::SetReaderActionsCollapsed,
        );
        widgets.reader_bin.add_breakpoint(bp.clone());
        // Added last: where both match, the bin applies this one alone.
        let compact_bp = fold_breakpoint(sender, 0.0, AppMsg::SetReaderCompact);
        widgets.reader_bin.add_breakpoint(compact_bp.clone());
        let remeasure = remeasure_soon(self.self_sender.clone());
        for (_, w) in &buttons {
            if let Some(r) = w.downcast_ref::<gtk::Revealer>() {
                follow_button(r, &remeasure);
            }
        }
        for r in [
            &widgets.tb_outbox_edit,
            &widgets.tb_outbox_send,
            &widgets.tb_outbox_send_all,
        ] {
            follow_button(r, &remeasure);
        }
        let _ = self.reader_toolbar_widgets.set(ReaderToolbarWidgets {
            buttons,
            outbox: vec![
                widgets.tb_outbox_edit.clone(),
                widgets.tb_outbox_send.clone(),
                widgets.tb_outbox_send_all.clone(),
            ],
            spinner: widgets.tb_spinner.clone().upcast(),
            bin: widgets.reader_bin.clone(),
            breakpoint: bp,
            compact_breakpoint: compact_bp,
            compact_need: std::cell::Cell::new(0),
            compact_measured: std::cell::Cell::new(false),
            full_need: std::cell::Cell::new(0),
            full_measured: std::cell::Cell::new(false),
            base,
            button_w,
            controls: std::cell::Cell::new(controls),
        });
        // Packs the saved order and sets the real threshold.
        self.relayout_reader_toolbar();
        watch_decoration_layout(header, sender);
        self.connect_toolbar_menus(sender);
    }

    /// The overflow, Tags, Move To and Snooze buttons open their menus.
    fn connect_toolbar_menus(&self, sender: &ComponentSender<Self>) {
        let s = sender.input_sender().clone();
        self.reader_overflow_btn.connect_clicked(move |_| {
            let _ = s.send(AppMsg::ReaderOverflowMenu);
        });
        let s = sender.input_sender().clone();
        self.reader_tag_btn.connect_clicked(move |_| {
            let _ = s.send(AppMsg::ReaderTagMenu);
        });
        let s = sender.input_sender().clone();
        self.reader_move_btn.connect_clicked(move |_| {
            let _ = s.send(AppMsg::MoveToMenu);
        });
        let s = sender.input_sender().clone();
        self.reader_snooze_btn.connect_clicked(move |_| {
            let _ = s.send(AppMsg::SnoozeMenu);
        });
    }
}

/// The controls' width comes from the headerbar's own live
/// GtkWindowControls children (a detached WindowControls never
/// populates its buttons and measures 0).
fn measure_controls(w: &gtk::Widget, sum: &mut i32) {
    if let Some(c) = w.downcast_ref::<gtk::WindowControls>() {
        *sum += c.measure(gtk::Orientation::Horizontal, -1).1;
        return;
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        measure_controls(&c, sum);
        child = c.next_sibling();
    }
}

/// The reader toolbar's buttons, each with the item it stands for.
fn toolbar_buttons(widgets: &AppModelWidgets) -> Vec<(config::ToolbarItem, gtk::Widget)> {
    use config::ToolbarItem as T;
    vec![
        (T::Reply, widgets.tb_reply.clone().upcast()),
        (T::ReplyAll, widgets.tb_reply_all.clone().upcast()),
        (T::Forward, widgets.tb_forward.clone().upcast()),
        (T::Star, widgets.tb_star.clone().upcast()),
        (T::Archive, widgets.tb_archive.clone().upcast()),
        (T::Snooze, widgets.tb_snooze.clone().upcast()),
        (T::Delete, widgets.tb_delete.clone().upcast()),
        (T::Spam, widgets.tb_spam.clone().upcast()),
        (T::ReadUnread, widgets.tb_read.clone().upcast()),
        (T::Tags, widgets.tb_tags.clone().upcast()),
        (T::MoveTo, widgets.tb_move.clone().upcast()),
        (T::Find, widgets.tb_find.clone().upcast()),
        (T::Print, widgets.tb_print.clone().upcast()),
    ]
}

/// What the row counts for the buttons now, and one button's cost, from
/// the buttons themselves (a hidden one — Tags before any tag exists —
/// measures 0 and is skipped); what is left of the row after them and the
/// controls is the base.
///
/// Two different widths, and the difference is load-bearing. Each
/// button sits in a Focus Mode revealer, and a revealer that has
/// not been revealed yet measures 0 however wide its child is —
/// which is exactly the state this runs in, before
/// `sync_focus_chrome` opens them. So a button's own cost comes
/// from the button inside the revealer, while what is taken off
/// `full` is what the row counted for it *just now*: the revealer.
/// Subtracting the buttons from a row that never included them is
/// how the base went negative (and the fold stopped happening) the
/// moment Focus Mode wrapped the row in revealers.
fn button_widths(buttons: &[(config::ToolbarItem, gtk::Widget)]) -> (i32, i32) {
    let mut packed_sum = 0;
    let mut button_w = 0;
    for (_, w) in buttons {
        packed_sum += w.measure(gtk::Orientation::Horizontal, -1).1;
        let inner = w
            .downcast_ref::<gtk::Revealer>()
            .and_then(|r| r.child())
            .unwrap_or_else(|| w.clone());
        let nat = inner.measure(gtk::Orientation::Horizontal, -1).1;
        if nat > 0 {
            button_w = button_w.max(nat);
        }
    }
    (packed_sum, button_w)
}

/// Show a button's Focus Mode revealer only while the button shows: an
/// empty revealer still takes the headerbar's spacing, and a folded right
/// group left enough of it to push the window controls off a narrow pane.
/// The row is measured again whenever a button comes or goes (a first tag
/// brings Tags), once per burst of them.
fn follow_button(revealer: &gtk::Revealer, remeasure: &std::rc::Rc<dyn Fn()>) {
    let Some(child) = revealer.child() else {
        return;
    };
    // Tags, Move To and Snooze sit in a box of their own.
    let button = match child.downcast_ref::<gtk::Box>() {
        Some(b) => b.first_child().unwrap_or(child.clone()),
        None => child,
    };
    // The button's own flag: `is_visible` asks its ancestors too, and
    // the revealer is one of them.
    revealer.set_visible(button.get_visible());
    let r = revealer.downgrade();
    let remeasure = remeasure.clone();
    button.connect_visible_notify(move |b| {
        if let Some(r) = r.upgrade() {
            r.set_visible(b.get_visible());
        }
        remeasure();
    });
}

/// A measurement of the toolbar row in the next idle, however many ask.
fn remeasure_soon(sender: relm4::Sender<AppMsg>) -> std::rc::Rc<dyn Fn()> {
    let pending = std::rc::Rc::new(std::cell::Cell::new(false));
    std::rc::Rc::new(move || {
        if pending.replace(true) {
            return;
        }
        let (pending, sender) = (pending.clone(), sender.clone());
        gtk::glib::idle_add_local_once(move || {
            pending.set(false);
            let _ = sender.send(AppMsg::MeasureReaderFold);
        });
    })
}

/// A pane narrower than `px`.
fn max_width(px: f64) -> adw::BreakpointCondition {
    adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        px,
        adw::LengthUnit::Px,
    )
}

/// A breakpoint that folds reader actions away below `px`, saying so to the
/// app with `msg` as it applies and unapplies.
fn fold_breakpoint(
    sender: &ComponentSender<AppModel>,
    px: f64,
    msg: fn(bool) -> AppMsg,
) -> adw::Breakpoint {
    let bp = adw::Breakpoint::new(max_width(px));
    let s = sender.input_sender().clone();
    bp.connect_apply(move |_| {
        let _ = s.send(msg(true));
    });
    let s = sender.input_sender().clone();
    bp.connect_unapply(move |_| {
        let _ = s.send(msg(false));
    });
    bp
}

/// Measure the window controls again when the desktop moves them.
fn watch_decoration_layout(header: gtk::Widget, sender: &ComponentSender<AppModel>) {
    if let Some(settings) = gtk::Settings::default() {
        let s = sender.input_sender().clone();
        settings.connect_notify_local(Some("gtk-decoration-layout"), move |_, _| {
            // Re-measure in an idle: the headerbar's own controls
            // rebuild on this same notify, in unspecified order.
            let header = header.clone();
            let s = s.clone();
            gtk::glib::idle_add_local_once(move || {
                let mut controls = 0;
                measure_controls(&header, &mut controls);
                let _ = s.send(AppMsg::ReaderControlsChanged(controls));
            });
        });
    }
}
