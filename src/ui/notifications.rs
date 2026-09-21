//! Top-of-window status bar (notification area).
//!
//! A transient bar slides down for new messages and auto-collapses after a few
//! seconds. Messages that need attention (errors) are also kept in a list that
//! can be expanded into toast-like cards via a button in the header. The bar is
//! tinted with the desktop accent colour (`@accent_bg_color`, default GNOME
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
                set_icon_name: Some("co.hyprlab.Hylki-dialog-warning-symbolic"),
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
                set_icon_name: "co.hyprlab.Hylki-window-close-symbolic",
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
    panel_open: bool,
    /// Console mode (verbose live log): offered at all (Settings), and open.
    console_enabled: bool,
    console_open: bool,
    /// The dracula styling, applied the moment console mode is requested —
    /// the console's own reveal is staged (see ShowConsole), and the bar must
    /// never map in its normal colours first and fade over.
    console_theme: bool,
    /// Whether opening the console is what opened the bar — closing the
    /// console then takes the bar back down with it.
    console_opened_bar: bool,
    /// Newest console_log sequence already rendered.
    console_seq: u64,
    console_buf: gtk::TextBuffer,
    console_view: Option<gtk::TextView>,
    console_scroll: Option<gtk::ScrolledWindow>,
    console_grain: Option<gtk::DrawingArea>,
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
    /// Connectivity restored: drop any connection/sync error cards.
    ClearConnectivity,
    TogglePanel,
    /// Open the panel straight into console mode.
    ShowConsole,
    /// The console's Export button (#132).
    ExportLog,
    /// Settings: whether console mode is offered (button + menu).
    SetConsoleEnabled(bool),
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
}

#[relm4::component(pub)]
impl SimpleComponent for NotificationCenter {
    type Init = ();
    type Input = NotifyInput;
    type Output = NotifyOutput;

    view! {
        gtk::Revealer {
            set_transition_type: gtk::RevealerTransitionType::SlideDown,
            #[watch]
            set_reveal_child: model.transient_visible || model.panel_open,

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                #[watch]
                set_css_classes: &model.area_classes(),

                // A WindowHandle so the bar can drag the window, like a title bar.
                // Interactive children (the buttons) still receive their clicks.
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

                        gtk::Button {
                            #[watch]
                            set_icon_name: if model.panel_open { "co.hyprlab.Hylki-pan-up-symbolic" } else { "co.hyprlab.Hylki-pan-down-symbolic" },
                            set_tooltip_text: Some(i18n("Collapse status bar").as_str()),
                            add_css_class: "flat",
                            connect_clicked => NotifyInput::TogglePanel,
                        },

                        // Console mode (far right): the verbose live log.
                        gtk::Button {
                            #[watch]
                            set_visible: model.console_enabled,
                            set_icon_name: "co.hyprlab.Hylki-code-symbolic",
                            set_tooltip_text: Some(i18n("Console").as_str()),
                            add_css_class: "flat",
                            connect_clicked => NotifyInput::ShowConsole,
                        },
                        // While the console is open: save its log to a file.
                        gtk::Button {
                            #[watch]
                            set_visible: model.console_enabled && model.console_open,
                            set_icon_name: "co.hyprlab.Hylki-document-save-symbolic",
                            set_tooltip_text: Some(i18n("Export log").as_str()),
                            add_css_class: "flat",
                            connect_clicked => NotifyInput::ExportLog,
                        },
                    },
                },

                // Only the cards section expands; an empty panel stays at bar
                // height (just shows the status / "Status bar (0)" row).
                gtk::Revealer {
                    set_transition_type: gtk::RevealerTransitionType::SlideDown,
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
                // further down. Selectable text; a CSS-scanline + drawn-grain
                // overlay gives it the old-CRT cast.
                gtk::Revealer {
                    set_transition_type: gtk::RevealerTransitionType::SlideDown,
                    set_transition_duration: 350,
                    #[watch]
                    set_reveal_child: model.console_open,

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,

                        gtk::Overlay {
                            #[name = "console_scroll"]
                            gtk::ScrolledWindow {
                                set_min_content_height: 160,
                                set_max_content_height: 160,
                                set_hscrollbar_policy: gtk::PolicyType::Never,

                                #[name = "console_view"]
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
        // Right-gravity end mark: stays at the end through inserts, giving
        // the follow-scroll a stable target.
        console_buf.create_mark(Some("end"), &console_buf.end_iter(), false);

        let mut model = NotificationCenter {
            cards,
            ids: Vec::new(),
            next_id: 1,
            epoch: 0,
            transient_text: String::new(),
            transient_error: false,
            transient_visible: false,
            status_text: String::new(),
            panel_open: false,
            console_enabled: false,
            console_open: false,
            console_theme: false,
            console_opened_bar: false,
            console_seq: 0,
            console_buf: console_buf.clone(),
            console_view: None,
            console_scroll: None,
            console_grain: None,
        };

        let cards_box = model.cards.widget();
        let widgets = view_output!();
        model.console_view = Some(widgets.console_view.clone());
        model.console_scroll = Some(widgets.console_scroll.clone());
        model.console_grain = Some(widgets.console_grain.clone());
        widgets.console_grain.set_draw_func(draw_crt_grain);
        {
            // Dragging the grip resizes the console; 160px stays the floor.
            // The pointer is tracked in ROOT coordinates: the grip itself
            // moves as the console grows, so grip-local offsets feed back
            // into the resize and jitter. A ceiling keeps the console from
            // demanding more height than the window can give (which flashes
            // as the layout fights itself).
            let drag = gtk::GestureDrag::new();
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
                let h = (start_h + (now - start_y) as i32).clamp(160, ceiling);
                sw.set_min_content_height(h);
                sw.set_max_content_height(h);
            });
            widgets.console_grip.add_controller(drag);
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
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
                    sender.input(NotifyInput::PollConsole);
                }
            }

            NotifyInput::RevealConsole => {
                if self.panel_open && !self.console_open {
                    self.console_open = true;
                    sender.input(NotifyInput::PollConsole);
                }
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
                    // Follow the tail only while the user is at the bottom;
                    // scrolling up to read holds the view still.
                    let follow = self.console_scroll.as_ref().is_none_or(|sw| {
                        let adj = sw.vadjustment();
                        adj.value() + adj.page_size() >= adj.upper() - 24.0
                    });
                    let mut end = self.console_buf.end_iter();
                    for line in &lines {
                        self.console_buf.insert(&mut end, line);
                        self.console_buf.insert(&mut end, "\n");
                    }
                    // Keep the widget buffer bounded like the ring buffer.
                    let lc = self.console_buf.line_count();
                    if lc > 2200 {
                        let mut start = self.console_buf.start_iter();
                        if let Some(mut cut) = self.console_buf.iter_at_line(lc - 2000) {
                            self.console_buf.delete(&mut start, &mut cut);
                        }
                    }
                    if follow {
                        if let (Some(view), Some(mark)) =
                            (&self.console_view, self.console_buf.mark("end"))
                        {
                            view.scroll_to_mark(&mark, 0.0, true, 0.0, 1.0);
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
}

impl NotificationCenter {
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
            // CSS transitions on the bar colours fade it into (and out of)
            // the dracula terminal look — console_theme is set before the
            // bar maps, so a shortcut-opened console never flashes the
            // normal bar colours first.
            classes.push("console-on");
        }
        classes
    }

    fn bar_icon(&self) -> &'static str {
        if self.is_error_state() {
            "co.hyprlab.Hylki-dialog-warning-symbolic"
        } else {
            // The bell — the same icon as the toolbar button that opens this
            // panel, so the two read as one feature.
            "co.hyprlab.Hylki-preferences-system-notifications-symbolic"
        }
    }

    fn bar_label(&self) -> String {
        if self.panel_open {
            // Reveal the current activity when the user opens the panel.
            if !self.status_text.is_empty() {
                self.status_text.clone()
            } else {
                i18n_f("Status bar ({n})", &[("n", &self.ids.len().to_string())])
            }
        } else {
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

