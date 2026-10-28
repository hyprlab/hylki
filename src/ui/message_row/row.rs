//! One row: its widgets and state, wiring its gestures once, and binding it
//! to the list item it shows.

use super::build::build_widgets;
use super::shared::RowShared;
use super::{RowAction, RowData, RowItem, SwipeSurface};
use crate::config::ListColumn;
use crate::ui::column_bin::ColumnBin;
use crate::ui::message_list::MessageListInput;
use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use relm4::prelude::*;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// The row's widgets, built once.
pub(super) struct RowWidgets {
    pub(super) host: gtk::Box,
    pub(super) revealer: gtk::Revealer,
    pub(super) surface: SwipeSurface,
    pub(super) swipe_bg: gtk::Box,
    pub(super) swipe_inner: gtk::Box,
    pub(super) swipe_icon: gtk::Image,
    pub(super) swipe_label: gtk::Label,
    pub(super) overlay: gtk::Overlay,
    pub(super) rail_stub: gtk::Box,
    pub(super) node: gtk::Box,
    pub(super) actions_line: gtk::Box,
    pub(super) chevron: gtk::Button,
    pub(super) palette_clip: gtk::Overlay,
    pub(super) palette_spacer: gtk::Box,
    pub(super) palette_inner: gtk::Box,
    pub(super) content: gtk::Box,
    pub(super) avatar_revealer: gtk::Revealer,
    pub(super) avatar: adw::Avatar,
    pub(super) dot: gtk::Box,
    pub(super) text: gtk::Box,
    /// The three-line card's first line, which the single-line layout
    /// borrows its widgets from (#334).
    pub(super) top: gtk::Box,
    /// The single-line layout's one line (#334).
    pub(super) line: gtk::Box,
    /// The single-line layout's own columns, which a card has no place for,
    /// each in the bin that sizes its column.
    pub(super) recipients: gtk::Label,
    pub(super) recipients_col: ColumnBin,
    pub(super) people: gtk::Label,
    pub(super) people_col: ColumnBin,
    pub(super) importance: gtk::Image,
    pub(super) account: gtk::Label,
    pub(super) account_col: ColumnBin,
    pub(super) tags_col: ColumnBin,
    pub(super) due: gtk::Label,
    pub(super) due_col: ColumnBin,
    /// The bins the sender, the subject and the date move into on one line.
    pub(super) name_col: ColumnBin,
    pub(super) subject_col: ColumnBin,
    pub(super) date_col: ColumnBin,
    pub(super) name: gtk::Label,
    /// The replied and forwarded arrows (#454), and the box that keeps them
    /// together.
    pub(super) answer: gtk::Box,
    pub(super) replied: gtk::Image,
    pub(super) forwarded: gtk::Image,
    pub(super) clip: gtk::Image,
    pub(super) star: gtk::Image,
    /// Sent mail's read receipt checkmarks (#520).
    pub(super) receipt: gtk::Image,
    pub(super) date: gtk::Label,
    pub(super) chip: gtk::Button,
    pub(super) chip_count: gtk::Label,
    pub(super) chip_caret: gtk::Image,
    pub(super) subject_line: gtk::Box,
    /// The card's line for its pills under the text, when they go there.
    pub(super) pills_line: gtk::Box,
    pub(super) subject: gtk::Label,
    pub(super) tags_box: gtk::Box,
    pub(super) preview_line: gtk::Box,
    pub(super) lock: gtk::Image,
    pub(super) preview: gtk::Label,
}

/// The action palette's state-carrying buttons, once built.
pub(super) struct PaletteButtons {
    /// Absent for a draft: a draft is neither read nor unread.
    pub(super) read: Option<gtk::Button>,
    pub(super) star: gtk::Button,
    pub(super) tag: gtk::Button,
    /// Built for Junk ("Not spam") and Drafts (no read toggle) or not.
    pub(super) built_for: (bool, bool),
}

/// What a row's tag chips were last built for: its keywords, its Gmail
/// labels, the tags' generation and the folder pill it shows.
type TagsKey = (Vec<String>, Vec<String>, u64, Option<String>);

/// What a row remembers between binds, and while one message is bound.
#[derive(Default)]
pub(super) struct RowState {
    pub(super) item: Option<RowItem>,
    /// Bumped on every bind and unbind, so a timer or lookup started for
    /// the message the row showed before can tell.
    pub(super) gen: u64,
    pub(super) avatar_texture: Option<gtk::gdk::Texture>,
    /// The initials circle drawn when no picture is known, kept per name so
    /// the avatar is handed the same object on every refresh.
    pub(super) initials: Option<(String, crate::ui::initials::InitialsPaintable)>,
    /// The address and look generation the circle was last looked up for.
    pub(super) face_for: Option<(String, u64)>,
    /// The keywords, Gmail labels and tag generation the chips were built for.
    pub(super) tags_for: Option<TagsKey>,
    pub(super) avatar_shown: bool,
    pub(super) hovered: bool,
    /// The columns the widgets are laid out in on one line, or `None` for
    /// the card (#334).
    pub(super) columns: Option<Vec<ListColumn>>,
    pub(super) dragging: bool,
    pub(super) palette_open: bool,
    pub(super) palette: Option<PaletteButtons>,
    pub(super) palette_target: i32,
    pub(super) palette_anim: Option<adw::TimedAnimation>,
    pub(super) collapse_timer: Option<glib::SourceId>,
    /// Current swipe distance in px (negative = dragged left) — the source
    /// of truth while a gesture is live; reset to 0 the instant a release is
    /// resolved (the strip then animates back out of view).
    pub(super) swipe_progress: f64,
    /// Which side last had a nonzero `swipe_progress` (-1 left, 1 right),
    /// kept once it returns to 0 so the revealed side and action don't flip
    /// mid-shrink after a release.
    pub(super) swipe_side: i8,
    /// A mouse-button or trackpad gesture is actively dragging this row.
    pub(super) swipe_dragging: bool,
    /// The release cleared the commit distance: the row is flying out to
    /// `swipe_side` while its Revealer closes.
    pub(super) swipe_committing: bool,
    pub(super) swipe_exit_started: bool,
    /// From the first drag until the snap-back lands: the `.swiping` class
    /// squares the pill off for that whole span.
    pub(super) swipe_active: bool,
    pub(super) swipe_anim: Option<adw::TimedAnimation>,
}

pub(crate) struct Row {
    pub(super) me: Weak<Row>,
    shared: Weak<RowShared>,
    pub(super) w: RowWidgets,
    pub(super) st: RefCell<RowState>,
    pub(super) list_item: RefCell<glib::WeakRef<gtk::ListItem>>,
    /// Kept alive for as long as the row: the tracker stops firing once
    /// dropped.
    pub(super) tracker: RefCell<Option<adw::SwipeTracker>>,
}

impl Row {
    pub(super) fn new(shared: &Rc<RowShared>) -> Rc<Self> {
        let w = build_widgets();
        let row = Rc::new_cyclic(|me| Row {
            me: me.clone(),
            shared: Rc::downgrade(shared),
            w,
            st: RefCell::new(RowState {
                avatar_shown: true,
                ..Default::default()
            }),
            list_item: RefCell::new(glib::WeakRef::new()),
            tracker: RefCell::new(None),
        });
        row.wire();
        row
    }

    pub(super) fn shared(&self) -> Option<Rc<RowShared>> {
        self.shared.upgrade()
    }

    pub(super) fn send(&self, msg: MessageListInput) {
        if let Some(shared) = self.shared() {
            let _ = shared.input.send(msg);
        }
    }

    pub(crate) fn data(&self) -> Option<Rc<RowData>> {
        self.st.borrow().item.as_ref().map(|i| i.data())
    }

    /// The widget the view shows this row as, while bound.
    pub(crate) fn widget(&self) -> Option<gtk::Widget> {
        self.list_item.borrow().upgrade()?.child()
    }

    /// The list position this row shows, while bound.
    pub(crate) fn position(&self) -> Option<usize> {
        let item = self.list_item.borrow().upgrade()?;
        self.st.borrow().item.as_ref()?;
        let pos = item.position();
        (pos != gtk::INVALID_LIST_POSITION).then_some(pos as usize)
    }

    /// Whether the keyboard is on this row: on it, inside it, or on the list
    /// item around it, which is where the list view puts its focus.
    pub(crate) fn has_focus_within(&self) -> bool {
        let host = self.w.host.upcast_ref::<gtk::Widget>();
        host.root().and_then(|r| r.focus()).is_some_and(|f| {
            f == *host || f.is_ancestor(host) || host.parent().is_some_and(|p| p == f)
        })
    }

    /// Hook the row's own controllers up, once.
    fn wire(&self) {
        let weak = self.me.clone();
        let on = move |f: fn(&Row)| {
            let weak = weak.clone();
            move || {
                if let Some(row) = weak.upgrade() {
                    f(&row);
                }
            }
        };

        // Hover: the ⋯ fades in, and in hover mode the palette opens.
        let motion = gtk::EventControllerMotion::new();
        {
            let enter = on(|r| r.set_hover(true));
            motion.connect_enter(move |_, _, _| enter());
            let leave = on(|r| r.set_hover(false));
            motion.connect_leave(move |_| leave());
        }
        self.w.host.add_controller(motion);

        // Right-click: the row's menu, in the capture phase so the press
        // reaches it before anything inside the row (the count chip, the
        // palette's buttons) can take it; claimed, so none of them acts on
        // it afterwards, and the row is not selected.
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let weak = self.me.clone();
            click.connect_pressed(move |g, _, x, y| {
                g.set_state(gtk::EventSequenceState::Claimed);
                if let Some(row) = weak.upgrade() {
                    row.context_menu(x, y);
                }
            });
        }
        self.w.host.add_controller(click);

        // Drag a message onto a sidebar folder to move it there. The payload
        // carries one (account, source folder, UID, id) group per message —
        // the whole selection when this row is part of it (#23), and every
        // member of a conversation its row stands for (#171).
        let drag = gtk::DragSource::new();
        drag.set_actions(gtk::gdk::DragAction::MOVE);
        {
            let weak = self.me.clone();
            drag.connect_prepare(move |_, _, _| weak.upgrade().and_then(|row| row.drag_payload()));
        }
        {
            let weak = self.me.clone();
            drag.connect_drag_begin(move |_, drag| {
                let Some(row) = weak.upgrade() else { return };
                row.st.borrow_mut().dragging = true;
                row.sync_host_classes();
                // The message as a contact travels: its circle, subject and
                // sender, hanging from the pointer. Without an icon GTK
                // draws the payload text.
                if let Some(card) = row.drag_card() {
                    gtk::DragIcon::for_drag(drag).set_child(Some(&card));
                    drag.set_hotspot(0, 0);
                }
            });
        }
        {
            let end = on(|r| {
                r.st.borrow_mut().dragging = false;
                r.sync_host_classes();
            });
            drag.connect_drag_end(move |_, _, _| end());
        }
        {
            let cancel = on(|r| {
                r.st.borrow_mut().dragging = false;
                r.sync_host_classes();
            });
            drag.connect_drag_cancel(move |_, _, _| {
                cancel();
                false
            });
        }
        self.w.host.add_controller(drag);

        // The unread dot marks the message read (PR #380). The press is
        // claimed so the row is neither selected nor opened by it.
        {
            let click = gtk::GestureClick::new();
            click.set_button(gtk::gdk::BUTTON_PRIMARY);
            click.connect_pressed(|g, _, _, _| {
                g.set_state(gtk::EventSequenceState::Claimed);
            });
            let mark = on(|r| r.act(RowAction::ToggleRead));
            click.connect_released(move |_, _, _, _| mark());
            self.w.dot.add_controller(click);
            self.w.dot.set_cursor_from_name(Some("pointer"));
        }
        // The ⋯ and the palette.
        {
            let toggle = on(Row::toggle_palette);
            self.w.chevron.connect_clicked(move |_| toggle());
        }
        let palette_motion = gtk::EventControllerMotion::new();
        {
            let enter = on(Row::cancel_collapse);
            palette_motion.connect_enter(move |_, _, _| enter());
            let leave = on(|r| {
                if r.st.borrow().palette_open {
                    r.arm_collapse();
                }
            });
            palette_motion.connect_leave(move |_| leave());
        }
        self.w.palette_inner.add_controller(palette_motion);
        // The clip is set once: the palette hangs clipped over the spacer
        // whose width is the one thing animated.
        self.w
            .palette_clip
            .set_clip_overlay(&self.w.palette_inner, true);

        // The conversation chip.
        {
            let toggle = on(|r| {
                if let Some(key) = r.data().and_then(|d| d.meta.key.clone()) {
                    r.send(MessageListInput::ToggleThread(key));
                }
            });
            self.w.chip.connect_clicked(move |_| toggle());
        }

        self.wire_swipe();
    }

    // ── Binding ──

    pub(super) fn bind(self: &Rc<Self>, item: &RowItem) {
        let fresh = self.st.borrow().item.as_ref() != Some(item);
        if fresh {
            self.adopt(item);
        }
        self.refresh();
        if fresh {
            self.begin(item);
        }
    }

    /// Show another message in the item already bound, from the start, as
    /// binding would: a folder switch hands the row on rather than having
    /// the view build a new one (#401).
    pub(super) fn rebind(self: &Rc<Self>) {
        let Some(item) = self.st.borrow().item.clone() else {
            return;
        };
        self.adopt(&item);
        self.refresh();
        self.begin(&item);
    }

    fn adopt(self: &Rc<Self>, item: &RowItem) {
        self.reset();
        {
            let mut st = self.st.borrow_mut();
            st.item = Some(item.clone());
            st.gen += 1;
        }
        item.imp().row.replace(Rc::downgrade(self));
    }

    /// What a message newly on the row starts: a reply sliding open, and the
    /// question of how large its conversation is.
    fn begin(self: &Rc<Self>, item: &RowItem) {
        let data = item.data();
        if data.meta.appear {
            // A reply just put in by opening its conversation: shown
            // folded, then slid open once it has been measured.
            self.w.revealer.set_transition_duration(0);
            self.w.revealer.set_reveal_child(false);
            self.w.revealer.set_transition_duration(200);
            let weak = Rc::downgrade(self);
            let gen = self.st.borrow().gen;
            glib::idle_add_local_once(move || {
                let Some(row) = weak.upgrade() else { return };
                if row.st.borrow().gen != gen {
                    return;
                }
                if let (Some(shared), Some(pos)) = (row.shared(), row.position()) {
                    shared.model.update_row(pos, |d| d.meta.appear = false);
                }
                row.w.revealer.set_reveal_child(true);
            });
        }
        if let (Some(group), Some(shared)) = (data.meta.group.clone(), self.shared()) {
            shared.want(group);
        }
        if let (Some(_), Some(shared)) = (&data.meta.pending, self.shared()) {
            shared.finish_later(data.slot());
        }
    }

    pub(super) fn unbind(&self) {
        self.reset();
        let item = {
            let mut st = self.st.borrow_mut();
            st.gen += 1;
            st.item.take()
        };
        // Only if the item still points here: when GTK binds it to its next
        // row before unbinding it from this one, clearing it would leave the
        // row on screen deaf to every later change of the message, a read
        // mark included (#333).
        if let Some(item) = item {
            let mine = item
                .imp()
                .row
                .borrow()
                .upgrade()
                .is_none_or(|r| std::ptr::eq(Rc::as_ptr(&r), self));
            if mine {
                item.imp().row.replace(Weak::new());
            }
        }
    }

    /// Put away whatever the row was doing for the message it showed: a
    /// palette open, a swipe under way, a circle on loan from a lookup.
    fn reset(&self) {
        let (palette_anim, swipe_anim, timer) = {
            let mut st = self.st.borrow_mut();
            st.palette_open = false;
            st.palette_target = 0;
            st.hovered = false;
            st.dragging = false;
            st.avatar_shown = true;
            st.swipe_progress = 0.0;
            st.swipe_side = 0;
            st.swipe_dragging = false;
            st.swipe_committing = false;
            st.swipe_exit_started = false;
            st.swipe_active = false;
            st.avatar_texture = None;
            st.face_for = None;
            (
                st.palette_anim.take(),
                st.swipe_anim.take(),
                st.collapse_timer.take(),
            )
        };
        if let Some(a) = palette_anim {
            a.pause();
        }
        if let Some(a) = swipe_anim {
            a.pause();
        }
        if let Some(t) = timer {
            t.remove();
        }
        if let Some(shared) = self.shared() {
            let open = shared.open_palette.borrow().upgrade();
            if open.is_some_and(|r| std::ptr::eq(Rc::as_ptr(&r), self)) {
                shared.open_palette.replace(Weak::new());
            }
        }
        self.w.palette_spacer.set_width_request(0);
        self.w.palette_inner.set_visible(false);
        self.w.surface.set_progress_px(0.0);
        self.w.revealer.set_transition_duration(0);
        self.w.revealer.set_reveal_child(true);
        self.w.revealer.set_transition_duration(200);
        self.w.avatar_revealer.set_transition_duration(0);
        self.w.avatar_revealer.set_reveal_child(true);
        self.w
            .avatar_revealer
            .set_transition_duration(crate::ui::FOCUS_ANIM_MS);
    }

    // ── Names and faces ──

    // ── Focus Mode ──

    // ── The palette ──

    // ── The swipe ──
}
