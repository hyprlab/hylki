//! Top-of-window status bar (notification area).
//!
//! A transient bar slides down for new messages and auto-collapses after a few
//! seconds. Messages that need attention (errors) are also kept in a list that
//! can be expanded into toast-like cards via a button in the header. The bar is
//! tinted with the desktop accent color (`@accent_bg_color`, default GNOME
//! blue) and turns amber for errors.

use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use relm4::factory::FactoryVecDeque;
use relm4::prelude::*;
use crate::i18n::{i18n, i18n_f};

const TRANSIENT_SECS: u64 = 5;

/// A single persisted notification (currently always an error).
#[derive(Debug, Clone)]
pub struct Notification {
    pub id: u32,
    pub text: String,
    /// Connection/sync error — cleared automatically once connectivity returns.
    pub connectivity: bool,
}

/// One expandable card.
pub struct NotificationCard {
    note: Notification,
}

#[derive(Debug)]
pub enum CardOutput {
    Dismiss(u32),
}

#[relm4::factory(pub)]
impl FactoryComponent for NotificationCard {
    type Init = Notification;
    type Input = ();
    type Output = CardOutput;
    type CommandOutput = ();
    type ParentWidget = gtk::Box;

    view! {
        gtk::Box {
            set_spacing: 10,
            add_css_class: "toast-card",

            gtk::Image {
                set_icon_name: Some("dialog-warning-symbolic"),
                set_valign: gtk::Align::Start,
                add_css_class: "toast-card-icon",
            },

            gtk::Label {
                set_label: &self.note.text,
                set_wrap: true,
                // Copyable (#246): an error is something to paste into a report.
                set_selectable: true,
                set_xalign: 0.0,
                set_hexpand: true,
                set_halign: gtk::Align::Start,
            },

            gtk::Button {
                set_icon_name: "window-close-symbolic",
                set_valign: gtk::Align::Start,
                add_css_class: "flat",
                add_css_class: "circular",
                connect_clicked[sender, id = self.note.id] => move |_| {
                    let _ = sender.output(CardOutput::Dismiss(id));
                },
            },
        }
    }

    fn init_model(note: Self::Init, _index: &DynamicIndex, _sender: FactorySender<Self>) -> Self {
        Self { note }
    }
}

pub struct NotificationCenter {
    cards: FactoryVecDeque<NotificationCard>,
    /// Card ids, index-aligned with the factory (newest first).
    ids: Vec<u32>,
    next_id: u32,
    /// Bumped on each new transient message so stale timers are ignored.
    epoch: u32,
    transient_text: String,
    transient_error: bool,
    transient_visible: bool,
    /// Current ongoing activity (connecting/syncing). Never auto-shown; only
    /// visible when the user opens the panel.
    status_text: String,
    /// Changes waiting to be sent (#442). Unlike the activity above this
    /// keeps the bar down, since it is something the user did that has not
    /// happened yet on the server; empty when nothing waits.
    pending_text: String,
    /// The bar was put away (its toggle, Ctrl+Shift+S) while changes were
    /// waiting: it stays away until the queue empties and fills again.
    pending_hidden: bool,
    panel_open: bool,
    /// Console mode (verbose live log): offered at all (Settings), and open.
    console_enabled: bool,
    console_open: bool,
    /// The dracula styling, applied the moment console mode is requested —
    /// the console's own reveal is staged (see ShowConsole), and the bar must
    /// never map in its normal colors first and fade over.
    console_theme: bool,
    /// Whether opening the console is what opened the bar — closing the
    /// console then takes the bar back down with it.
    console_opened_bar: bool,
    /// Newest console_log sequence already rendered.
    console_seq: u64,
    console_buf: gtk::TextBuffer,
    /// Whether the console sticks to its newest line: true until the user
    /// scrolls up to read, true again once they scroll back to the bottom.
    console_follow: std::rc::Rc<std::cell::Cell<bool>>,
    console_scroll: Option<gtk::ScrolledWindow>,
    console_grain: Option<gtk::DrawingArea>,
    /// Whether the bar sits at the top of the window rather than its foot
    /// (Settings, #433); shared with the console grip's drag.
    top: std::rc::Rc<std::cell::Cell<bool>>,
    /// What [`NotificationCenter::place`] puts in order for either end.
    layout: Option<Layout>,
}

/// The parts of the bar whose order follows the end of the window it is at:
/// the bar nearest the window's edge, the cards and the console opening
/// away from it, and the console's grip on its far side.
struct Layout {
    root: gtk::Revealer,
    area: gtk::Box,
    bar: gtk::WindowHandle,
    console_box: gtk::Box,
    grip: gtk::Box,
}

#[derive(Debug)]
pub enum NotifyInput {
    /// Auto-shows the transient bar for a few seconds; errors also add a card.
    Push {
        text: String,
        error: bool,
        connectivity: bool,
    },
    /// Updates the ongoing-activity text without auto-showing anything.
    SetStatus(String),
    /// Changes made offline waiting to be sent (#442), as the bar should say
    /// it ("3 changes waiting to sync"); empty once there are none. The bar
    /// stays down with it while it is not empty.
    SetPending(String),
    /// Connectivity restored: drop any connection/sync error cards.
    ClearConnectivity,
    TogglePanel,
    /// The menu item, Ctrl+Shift+S and a long-press on Refresh: hide the bar
    /// if it shows at all, a passing message included, or open it (#294).
    ToggleBar,
    /// Open the panel straight into console mode.
    ShowConsole,
    /// The console's Export button (#132).
    ExportLog,
    /// Settings: whether console mode is offered (button + menu).
    SetConsoleEnabled(bool),
    /// Settings: the bar at the top of the window (`true`) or its foot.
    SetAtTop(bool),
    /// Console tick: pull new log lines (and reschedule while open).
    PollConsole,
    /// Second stage of ShowConsole: slide the console open once the bar
    /// itself is mapped (an unmapped revealer skips its transition).
    RevealConsole,
    ClearAll,
    Dismiss(u32),
    CollapseTransient(u32),
}

#[derive(Debug)]
pub enum NotifyOutput {
    CountChanged(usize),
    /// The console's Export button: save the log to a file (#132).
    ExportLog,
    /// The bar came down or went up, for the menu item that names what it
    /// will do (#294).
    Shown(bool),
}

#[relm4::component(pub)]
impl SimpleComponent for NotificationCenter {
    type Init = ();
    type Input = NotifyInput;
    type Output = NotifyOutput;

    view! {
        // At the window's foot by default, so the bar coming up never moves
        // what is being read or written (#433); the cards and the console
        // open away from the window's edge. Settings can put it at the top.
        gtk::Revealer {
            #[watch]
            set_transition_type: model.slide(),
            #[watch]
            set_reveal_child: model.shown(),

            #[name = "area"]
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                #[watch]
                set_css_classes: &model.area_classes(),

                // Only the cards section expands; an empty panel stays at bar
                // height (just shows the status / "Status bar (0)" row).
                gtk::Revealer {
                    #[watch]
                    set_transition_type: model.slide(),
                    #[watch]
                    set_reveal_child: model.panel_open
                        && !model.console_open
                        && !model.ids.is_empty(),

                    gtk::ScrolledWindow {
                        set_propagate_natural_height: true,
                        set_max_content_height: 320,
                        set_hscrollbar_policy: gtk::PolicyType::Never,

                        #[local_ref]
                        cards_box -> gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 8,
                            add_css_class: "notify-cards",
                        },
                    },
                },

                // Console mode: the dracula terminal, expanding the bar
                // further. Selectable text; a CSS-scanline + drawn-grain
                // overlay gives it the old-CRT cast.
                gtk::Revealer {
                    #[watch]
                    set_transition_type: model.slide(),
                    set_transition_duration: 350,
                    #[watch]
                    set_reveal_child: model.console_open,

                    #[name = "console_box"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,

                        // Drag grip: pull the console taller (160px floor).
                        #[name = "console_grip"]
                        gtk::Box {
                            add_css_class: "console-grip",
                            set_cursor_from_name: Some("ns-resize"),
                            set_halign: gtk::Align::Fill,

                            gtk::Box {
                                add_css_class: "console-grip-bar",
                                set_halign: gtk::Align::Center,
                                set_valign: gtk::Align::Center,
                                set_hexpand: true,
                            },
                        },

                        gtk::Overlay {
                            #[name = "console_scroll"]
                            gtk::ScrolledWindow {
                                set_min_content_height: 160,
                                set_max_content_height: 160,
                                set_hscrollbar_policy: gtk::PolicyType::Never,

                                gtk::TextView {
                                    set_buffer: Some(&console_buf),
                                    add_css_class: "console-view",
                                    set_editable: false,
                                    set_cursor_visible: false,
                                    set_monospace: true,
                                    set_wrap_mode: gtk::WrapMode::WordChar,
                                    set_left_margin: 10,
                                    set_right_margin: 10,
                                    set_top_margin: 8,
                                    set_bottom_margin: 8,
                                },
                            },

                            add_overlay = &gtk::Box {
                                add_css_class: "console-scanlines",
                                set_can_target: false,
                            },

                            #[name = "console_grain"]
                            add_overlay = &gtk::DrawingArea {
                                set_can_target: false,
                            },
                        },
                    },
                },

                // A WindowHandle so the bar can drag the window, like a title bar.
                // Interactive children (the buttons) still receive their clicks.
                #[name = "bar"]
                gtk::WindowHandle {
                    gtk::Box {
                        add_css_class: "notify-bar",
                        set_spacing: 8,

                        gtk::Image {
                            #[watch]
                            set_icon_name: Some(model.bar_icon()),
                        },

                        gtk::Label {
                            #[watch]
                            set_label: &model.bar_label(),
                            set_hexpand: true,
                            set_halign: gtk::Align::Start,
                            set_ellipsize: gtk::pango::EllipsizeMode::End,
                        },

                        gtk::Button {
                            #[watch]
                            set_visible: model.panel_open && !model.ids.is_empty(),
                            set_label: &i18n("Clear all"),
                            add_css_class: "flat",
                            connect_clicked => NotifyInput::ClearAll,
                        },

                        // While the console is open: save its log to a file.
                        gtk::Button {
                            #[watch]
                            set_visible: model.console_enabled && model.console_open,
                            set_icon_name: "document-save-symbolic",
                            set_tooltip_text: Some(i18n("Export log").as_str()),
                            add_css_class: "flat",
                            connect_clicked => NotifyInput::ExportLog,
                        },
                        // Console mode: the verbose live log.
                        gtk::Button {
                            #[watch]
                            set_visible: model.console_enabled,
                            set_icon_name: "code-symbolic",
                            set_tooltip_text: Some(i18n("Console").as_str()),
                            add_css_class: "flat",
                            connect_clicked => NotifyInput::ShowConsole,
                        },
                        // The bar's own toggle, always last.
                        gtk::Button {
                            #[watch]
                            set_icon_name: model.chevron(),
                            set_tooltip_text: Some(i18n("Collapse status bar").as_str()),
                            add_css_class: "flat",
                            connect_clicked => NotifyInput::TogglePanel,
                        },
                    },
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let cards = FactoryVecDeque::builder()
            .launch(gtk::Box::default())
            .forward(sender.input_sender(), |out| match out {
                CardOutput::Dismiss(id) => NotifyInput::Dismiss(id),
            });

        let console_buf = gtk::TextBuffer::new(None);

        let mut model = NotificationCenter {
            cards,
            ids: Vec::new(),
            next_id: 1,
            epoch: 0,
            transient_text: String::new(),
            transient_error: false,
            transient_visible: false,
            status_text: String::new(),
            pending_text: String::new(),
            pending_hidden: false,
            panel_open: false,
            console_enabled: false,
            console_open: false,
            console_theme: false,
            console_opened_bar: false,
            console_seq: 0,
            console_buf: console_buf.clone(),
            console_follow: std::rc::Rc::new(std::cell::Cell::new(true)),
            console_scroll: None,
            console_grain: None,
            top: std::rc::Rc::default(),
            layout: None,
        };

        let cards_box = model.cards.widget();
        let widgets = view_output!();
        model.console_scroll = Some(widgets.console_scroll.clone());
        model.console_grain = Some(widgets.console_grain.clone());
        widgets.console_grain.set_draw_func(draw_crt_grain);
        {
            // Only the user's own scrolling stops the console following its
            // newest line. The text view moves the view by itself as it
            // measures lines (back to the top on its first layout, and when
            // old lines are cut), so a position read alone can't tell that
            // apart from the user scrolling up to read.
            let sw = widgets.console_scroll.clone();
            let adj = sw.vadjustment();
            // A button held in the console: what moves the view now is the
            // user (the scrollbar, a selection drag).
            let held = std::rc::Rc::new(std::cell::Cell::new(false));
            let follow = model.console_follow.clone();
            let h = held.clone();
            adj.connect_value_changed(move |a| {
                if a.value() + a.page_size() >= a.upper() - 1.0 {
                    follow.set(true);
                } else if h.get() {
                    follow.set(false);
                } else if follow.get() {
                    a.set_value(a.upper() - a.page_size());
                }
            });
            let follow = model.console_follow.clone();
            adj.connect_changed(move |a| {
                if follow.get() {
                    a.set_value(a.upper() - a.page_size());
                }
            });
            let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
            let follow = model.console_follow.clone();
            let a = adj.clone();
            wheel.connect_scroll(move |_, _, dy| {
                if dy < 0.0 && a.value() > a.lower() {
                    follow.set(false);
                }
                glib::Propagation::Proceed
            });
            sw.add_controller(wheel);
            let input = gtk::EventControllerLegacy::new();
            input.set_propagation_phase(gtk::PropagationPhase::Capture);
            let follow = model.console_follow.clone();
            input.connect_event(move |_, ev| {
                use gtk::gdk::{EventType as E, Key};
                match ev.event_type() {
                    E::ButtonPress | E::TouchBegin => held.set(true),
                    E::ButtonRelease | E::TouchEnd | E::TouchCancel => held.set(false),
                    // Keys are taken one at a time: a key held as the console
                    // closes never sees its release here.
                    E::KeyPress => {
                        let up = ev
                            .downcast_ref::<gtk::gdk::KeyEvent>()
                            .is_some_and(|k| {
                                matches!(
                                    k.keyval(),
                                    Key::Up | Key::KP_Up | Key::Page_Up | Key::KP_Page_Up
                                        | Key::Home | Key::KP_Home
                                )
                            });
                        if up && adj.value() > adj.lower() {
                            follow.set(false);
                        }
                    }
                    _ => {}
                }
                glib::Propagation::Proceed
            });
            sw.add_controller(input);
        }
        model.layout = Some(Layout {
            root: root.clone(),
            area: widgets.area.clone(),
            bar: widgets.bar.clone(),
            console_box: widgets.console_box.clone(),
            grip: widgets.console_grip.clone(),
        });
        {
            // Dragging the grip resizes the console; 160px stays the floor.
            // The pointer is tracked in ROOT coordinates: the grip itself
            // moves as the console grows, so grip-local offsets feed back
            // into the resize and jitter. A ceiling keeps the console from
            // demanding more height than the window can give (which flashes
            // as the layout fights itself).
            let drag = gtk::GestureDrag::new();
            let top = model.top.clone();
            let sw = widgets.console_scroll.clone();
            let grip = widgets.console_grip.clone();
            let start = std::rc::Rc::new(std::cell::Cell::new((160i32, 0f32)));
            let to_root_y = {
                let grip = grip.clone();
                move |x: f64, y: f64| -> Option<f32> {
                    let root = grip.root()?;
                    grip.compute_point(
                        root.upcast_ref::<gtk::Widget>(),
                        &gtk::graphene::Point::new(x as f32, y as f32),
                    )
                    .map(|p| p.y())
                }
            };
            {
                let sw = sw.clone();
                let start = start.clone();
                let to_root_y = to_root_y.clone();
                drag.connect_drag_begin(move |_, x, y| {
                    if let Some(ry) = to_root_y(x, y) {
                        start.set((sw.min_content_height(), ry));
                    }
                });
            }
            drag.connect_drag_update(move |g, ox, oy| {
                let Some((sx, sy)) = g.start_point() else { return };
                let Some(now) = to_root_y(sx + ox, sy + oy) else { return };
                let (start_h, start_y) = start.get();
                let ceiling = grip
                    .root()
                    .map(|r| r.height() - 340)
                    .unwrap_or(i32::MAX)
                    .max(160);
                // The grip sits on the console's far side from the bar:
                // below it with the bar at the top, above it at the foot.
                let moved = (now - start_y) as i32;
                let h = (start_h + if top.get() { moved } else { -moved }).clamp(160, ceiling);
                sw.set_min_content_height(h);
                sw.set_max_content_height(h);
            });
            widgets.console_grip.add_controller(drag);
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        let was_shown = self.shown();
        self.handle(msg, &sender);
        if self.shown() != was_shown {
            let _ = sender.output(NotifyOutput::Shown(self.shown()));
        }
    }
}

impl NotificationCenter {
    /// How the bar and its panels slide: away from the window's edge.
    fn slide(&self) -> gtk::RevealerTransitionType {
        if self.top.get() {
            gtk::RevealerTransitionType::SlideDown
        } else {
            gtk::RevealerTransitionType::SlideUp
        }
    }

    /// The bar's toggle points where the panel opens, or back where it folds.
    fn chevron(&self) -> &'static str {
        match (self.top.get(), self.panel_open) {
            (true, true) | (false, false) => "pan-up-symbolic",
            (true, false) | (false, true) => "pan-down-symbolic",
        }
    }

    /// Open the console on its newest line, whatever was being read the last
    /// time. The hidden view keeps its last measurements, so the jump lands
    /// even when nothing new arrives to resize it.
    fn follow_console(&self) {
        self.console_follow.set(true);
        if let Some(sw) = &self.console_scroll {
            let adj = sw.vadjustment();
            adj.set_value(adj.upper() - adj.page_size());
        }
    }

    /// Put the bar at its end of the window, and its parts in order for it.
    fn place(&self) {
        let Some(l) = &self.layout else { return };
        let top = self.top.get();
        let none = None::<&gtk::Widget>;
        if top {
            l.area.reorder_child_after(&l.bar, none);
            l.console_box.reorder_child_after(&l.grip, l.console_box.last_child().as_ref());
        } else {
            l.area.reorder_child_after(&l.bar, l.area.last_child().as_ref());
            l.console_box.reorder_child_after(&l.grip, none);
        }
        // The window's column: first child at the top, last at the foot.
        if let Some(column) = l.root.parent().and_downcast::<gtk::Box>() {
            if top {
                column.reorder_child_after(&l.root, none);
            } else if let Some(last) = column.last_child().filter(|w| w != l.root.upcast_ref::<gtk::Widget>()) {
                column.reorder_child_after(&l.root, Some(&last));
            }
        }
    }

    fn handle(&mut self, msg: NotifyInput, sender: &ComponentSender<Self>) {
        match msg {
            NotifyInput::Push {
                text,
                error,
                connectivity,
            } => {
                self.transient_text = text.clone();
                self.transient_error = error;
                self.transient_visible = true;
                self.epoch = self.epoch.wrapping_add(1);

                if error {
                    // Keep at most one connectivity error card around.
                    if connectivity {
                        self.remove_connectivity_cards();
                    }
                    let id = self.next_id;
                    self.next_id += 1;
                    self.ids.insert(0, id);
                    self.cards.guard().push_front(Notification {
                        id,
                        text,
                        connectivity,
                    });
                    let _ = sender.output(NotifyOutput::CountChanged(self.ids.len()));
                }

                // Auto-collapse the transient bar after a few seconds, unless a
                // newer message (or the expanded panel) supersedes it.
                let epoch = self.epoch;
                let sender = sender.clone();
                glib::timeout_add_local_once(Duration::from_secs(TRANSIENT_SECS), move || {
                    sender.input(NotifyInput::CollapseTransient(epoch));
                });
            }

            NotifyInput::SetStatus(text) => {
                self.status_text = text;
            }

            NotifyInput::SetPending(text) => {
                if text.is_empty() {
                    self.pending_hidden = false;
                }
                self.pending_text = text;
            }

            NotifyInput::ClearConnectivity => {
                let before = self.ids.len();
                self.remove_connectivity_cards();
                if self.ids.len() != before {
                    if self.ids.is_empty() {
                        self.panel_open = false;
                    }
                    let _ = sender.output(NotifyOutput::CountChanged(self.ids.len()));
                }
            }

            NotifyInput::CollapseTransient(epoch) => {
                if epoch == self.epoch {
                    self.transient_visible = false;
                }
            }

            NotifyInput::TogglePanel => {
                self.panel_open = !self.panel_open;
                if !self.panel_open {
                    // The bar's X is also console mode's exit.
                    self.console_open = false;
                    self.console_theme = false;
                }
            }

            NotifyInput::ToggleBar => {
                if self.shown() {
                    self.panel_open = false;
                    self.transient_visible = false;
                    self.console_open = false;
                    self.console_theme = false;
                    self.pending_hidden = !self.pending_text.is_empty();
                } else {
                    self.panel_open = true;
                }
            }

            NotifyInput::ExportLog => {
                let _ = sender.output(NotifyOutput::ExportLog);
            }
            NotifyInput::ShowConsole => {
                // The console button (and shortcut) toggles: a second press
                // folds the console back into the plain status bar.
                if self.console_open {
                    self.console_open = false;
                    self.console_theme = false;
                    if self.console_opened_bar {
                        // The bar only opened to host the console; take it
                        // back down with it.
                        self.panel_open = false;
                        self.transient_visible = false;
                    }
                    return;
                }
                let bar_was_closed = !self.panel_open && !self.transient_visible;
                self.console_opened_bar = bar_was_closed;
                self.console_theme = true;
                self.panel_open = true;
                if bar_was_closed {
                    // Two-stage open: the bar slides down first; once it's
                    // mapped, the console gets its own animated slide (set
                    // together, the inner revealer starts unmapped and snaps).
                    let s = sender.clone();
                    glib::timeout_add_local_once(Duration::from_millis(140), move || {
                        s.input(NotifyInput::RevealConsole);
                    });
                } else {
                    self.console_open = true;
                    self.follow_console();
                    sender.input(NotifyInput::PollConsole);
                }
            }

            NotifyInput::RevealConsole => {
                if self.panel_open && !self.console_open {
                    self.console_open = true;
                    self.follow_console();
                    sender.input(NotifyInput::PollConsole);
                }
            }

            NotifyInput::SetAtTop(on) => {
                self.top.set(on);
                self.place();
            }
            NotifyInput::SetConsoleEnabled(on) => {
                self.console_enabled = on;
                if !on {
                    self.console_open = false;
                    self.console_theme = false;
                }
            }

            NotifyInput::PollConsole => {
                if !self.console_open {
                    return;
                }
                let (seq, lines) = crate::console_log::lines_since(self.console_seq);
                self.console_seq = seq;
                if !lines.is_empty() {
                    // The adjustment's changed handler keeps a following
                    // view on the newest line as these land.
                    let mut end = self.console_buf.end_iter();
                    for line in &lines {
                        self.console_buf.insert(&mut end, line);
                        self.console_buf.insert(&mut end, "\n");
                    }
                    // Keep the widget buffer bounded like the ring buffer.
                    // Cutting from the top shifts whatever the user scrolled
                    // up to read, so that waits until they are back at the
                    // bottom, within a far looser ceiling.
                    let lc = self.console_buf.line_count();
                    let cap = if self.console_follow.get() { 2200 } else { 20_000 };
                    if lc > cap {
                        let mut start = self.console_buf.start_iter();
                        if let Some(mut cut) = self.console_buf.iter_at_line(lc - 2000) {
                            self.console_buf.delete(&mut start, &mut cut);
                        }
                    }
                }
                // A fresh sprinkle of grain each tick keeps the CRT alive.
                if let Some(g) = &self.console_grain {
                    g.queue_draw();
                }
                let s = sender.clone();
                glib::timeout_add_local_once(Duration::from_millis(350), move || {
                    s.input(NotifyInput::PollConsole);
                });
            }

            NotifyInput::Dismiss(id) => {
                if let Some(pos) = self.ids.iter().position(|x| *x == id) {
                    self.ids.remove(pos);
                    self.cards.guard().remove(pos);
                    if self.ids.is_empty() {
                        self.panel_open = false;
                    }
                    let _ = sender.output(NotifyOutput::CountChanged(self.ids.len()));
                }
            }

            NotifyInput::ClearAll => {
                self.cards.guard().clear();
                self.ids.clear();
                self.panel_open = false;
                self.transient_visible = false;
                let _ = sender.output(NotifyOutput::CountChanged(0));
            }
        }
    }

    /// Whether the bar is down: a passing message, the panel, or changes
    /// waiting to be sent.
    fn shown(&self) -> bool {
        self.transient_visible || self.panel_open || self.pending_shown()
    }

    /// Whether the bar is down for the changes waiting to be sent (#442).
    fn pending_shown(&self) -> bool {
        !self.pending_text.is_empty() && !self.pending_hidden
    }

    /// Remove every connectivity-flagged error card (newest-first indices).
    fn remove_connectivity_cards(&mut self) {
        let to_remove: Vec<usize> = (0..self.ids.len())
            .filter(|&i| self.cards.get(i).is_some_and(|c| c.note.connectivity))
            .collect();
        if to_remove.is_empty() {
            return;
        }
        {
            let mut guard = self.cards.guard();
            for &i in to_remove.iter().rev() {
                guard.remove(i);
            }
        }
        for &i in to_remove.iter().rev() {
            self.ids.remove(i);
        }
    }

    fn is_error_state(&self) -> bool {
        // Amber only when something error-related is actually present: an active
        // error transient, or existing error cards. Never the stale flag, which
        // used to tint the bar amber while collapsing an already-cleared panel.
        (self.transient_visible && self.transient_error) || !self.ids.is_empty()
    }

    fn area_classes(&self) -> Vec<&'static str> {
        let mut classes = if self.is_error_state() {
            vec!["notify-area", "error"]
        } else {
            vec!["notify-area"]
        };
        if self.console_theme || self.console_open {
            // CSS transitions on the bar colors fade it into (and out of)
            // the dracula terminal look — console_theme is set before the
            // bar maps, so a shortcut-opened console never flashes the
            // normal bar colors first.
            classes.push("console-on");
        }
        classes
    }

    fn bar_icon(&self) -> &'static str {
        if self.is_error_state() {
            "dialog-warning-symbolic"
        } else if !self.panel_open && !self.transient_visible && !self.pending_text.is_empty() {
            // The Refresh button's arrows: these changes sync when the
            // server is reached again.
            "view-refresh-symbolic"
        } else {
            // The bell — the same icon as the toolbar button that opens this
            // panel, so the two read as one feature.
            "preferences-system-notifications-symbolic"
        }
    }

    fn bar_label(&self) -> String {
        if self.panel_open {
            // Reveal the current activity when the user opens the panel.
            if !self.status_text.is_empty() {
                self.status_text.clone()
            } else if !self.pending_text.is_empty() {
                self.pending_text.clone()
            } else {
                i18n_f("Status bar ({n})", &[("n", &self.ids.len().to_string())])
            }
        } else if !self.transient_visible && self.pending_shown() {
            // Down only for the changes waiting.
            self.pending_text.clone()
        } else {
            // A passing message, or the last one as the bar slides away.
            self.transient_text.clone()
        }
    }
}

/// The CRT grain: a per-tick sprinkle of light and dark specks plus a soft
/// vignette. Redrawn on every console poll, so the noise "lives" like an old
/// tube; a cheap xorshift keeps it allocation-free.
fn draw_crt_grain(_a: &gtk::DrawingArea, cr: &gtk::cairo::Context, w: i32, h: i32) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEED: AtomicU32 = AtomicU32::new(0x9e3779b9);
    if w <= 0 || h <= 0 {
        return;
    }
    let mut s = SEED.fetch_add(0x6d2b79f5, Ordering::Relaxed) | 1;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        s
    };
    let specks = ((w * h) / 900).clamp(40, 2000) as u32;
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.045);
    for _ in 0..specks {
        let x = (next() % w as u32) as f64;
        let y = (next() % h as u32) as f64;
        cr.rectangle(x, y, 1.0, 1.0);
    }
    let _ = cr.fill();
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.09);
    for _ in 0..specks {
        let x = (next() % w as u32) as f64;
        let y = (next() % h as u32) as f64;
        cr.rectangle(x, y, 1.0, 1.0);
    }
    let _ = cr.fill();
    // Vignette: the corners fall off like a tube's face.
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let r = (cx * cx + cy * cy).sqrt();
    let grad = gtk::cairo::RadialGradient::new(cx, cy, r * 0.55, cx, cy, r);
    grad.add_color_stop_rgba(0.0, 0.0, 0.0, 0.0, 0.0);
    grad.add_color_stop_rgba(1.0, 0.0, 0.0, 0.0, 0.28);
    let _ = cr.set_source(&grad);
    let _ = cr.paint();
}

