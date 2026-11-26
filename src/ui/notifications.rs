//! Top-of-window status bar (notification area).
//!
//! A transient bar slides down for new messages and auto-collapses after a few
//! seconds. Messages that need attention (errors) are also kept in a list that
//! can be expanded into toast-like cards via a button in the header. The bar is
//! tinted with the desktop accent color (`@accent_bg_color`, default GNOME
//! blue) and turns amber for errors.

use std::time::Duration;

use crate::i18n::{i18n, i18n_f};
use gtk::glib;
use gtk::prelude::*;
use relm4::factory::FactoryVecDeque;
use relm4::prelude::*;

mod bar;
mod cards;
mod console;

use console::draw_crt_grain;

const TRANSIENT_SECS: u64 = 5;

/// A single persisted notification (currently always an error).
#[derive(Debug, Clone)]
struct Notification {
    id: u32,
    text: String,
    /// Connection/sync error — cleared automatically once connectivity returns.
    connectivity: bool,
}

/// One expandable card.
struct NotificationCard {
    note: Notification,
}

#[derive(Debug)]
enum CardOutput {
    Dismiss(u32),
}

#[relm4::factory(pub(crate))]
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

pub(crate) struct NotificationCenter {
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
    /// The network is down: the opened bar explains it.
    offline: bool,
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
pub(crate) enum NotifyInput {
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
    /// The network went down or came back. Never shows the bar by itself:
    /// the Refresh button turns orange, and the bar, once opened, says why
    /// (#442).
    SetOffline(bool),
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
pub(crate) enum NotifyOutput {
    CountChanged(usize),
    /// The console's Export button: save the log to a file (#132).
    ExportLog,
    /// The bar came down or went up, for the menu item that names what it
    /// will do (#294).
    Shown(bool),
}

#[relm4::component(pub(crate))]
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
            set_reveal_child: model.transient_visible || model.panel_open,

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
                            set_xalign: 0.0,
                            // The offline explanation is read whole; anything
                            // else is one line.
                            #[watch]
                            set_wrap: model.showing_offline(),
                            #[watch]
                            set_ellipsize: if model.showing_offline() {
                                gtk::pango::EllipsizeMode::None
                            } else {
                                gtk::pango::EllipsizeMode::End
                            },
                        },

                        gtk::Button {
                            #[watch]
                            set_visible: model.panel_open && !model.ids.is_empty(),
                            set_label: &i18n("Clear All"),
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
            offline: !crate::desktop::power::online(),
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
        model.follow_console_until_user_scrolls(&widgets);
        model.layout = Some(Layout {
            root: root.clone(),
            area: widgets.area.clone(),
            bar: widgets.bar.clone(),
            console_box: widgets.console_box.clone(),
            grip: widgets.console_grip.clone(),
        });
        model.attach_grip_drag(&widgets);

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
            l.console_box
                .reorder_child_after(&l.grip, l.console_box.last_child().as_ref());
        } else {
            l.area
                .reorder_child_after(&l.bar, l.area.last_child().as_ref());
            l.console_box.reorder_child_after(&l.grip, none);
        }
        // The window's column: first child at the top, last at the foot.
        if let Some(column) = l.root.parent().and_downcast::<gtk::Box>() {
            if top {
                column.reorder_child_after(&l.root, none);
            } else if let Some(last) = column
                .last_child()
                .filter(|w| w != l.root.upcast_ref::<gtk::Widget>())
            {
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
            } => self.on_push(text, error, connectivity, sender),
            NotifyInput::SetStatus(text) => {
                self.status_text = text;
            }
            NotifyInput::SetOffline(offline) => {
                self.offline = offline;
            }
            NotifyInput::ClearConnectivity => self.on_clear_connectivity(sender),
            NotifyInput::CollapseTransient(epoch) => self.on_collapse_transient(epoch),
            NotifyInput::TogglePanel => self.on_toggle_panel(),
            NotifyInput::ToggleBar => self.on_toggle_bar(),
            NotifyInput::ExportLog => {
                let _ = sender.output(NotifyOutput::ExportLog);
            }
            NotifyInput::ShowConsole => self.on_show_console(sender),
            NotifyInput::RevealConsole => self.on_reveal_console(sender),
            NotifyInput::SetAtTop(on) => self.on_set_at_top(on),
            NotifyInput::SetConsoleEnabled(on) => self.on_set_console_enabled(on),
            NotifyInput::PollConsole => self.on_poll_console(sender),
            NotifyInput::Dismiss(id) => self.on_dismiss(id, sender),
            NotifyInput::ClearAll => self.on_clear_all(sender),
        }
    }

    /// Whether the bar is down: a passing message, or the panel.
    fn shown(&self) -> bool {
        self.transient_visible || self.panel_open
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
        (self.transient_visible && self.transient_error)
            || !self.ids.is_empty()
            || self.showing_offline()
    }

    /// Whether the bar's own line is the offline explanation: the bar was
    /// opened while the network is down.
    fn showing_offline(&self) -> bool {
        self.offline && self.panel_open && !self.console_open
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
        if self.showing_offline() {
            "network-offline-symbolic"
        } else if self.is_error_state() {
            "dialog-warning-symbolic"
        } else {
            // The bell — the same icon as the toolbar button that opens this
            // panel, so the two read as one feature.
            "preferences-system-notifications-symbolic"
        }
    }

    fn bar_label(&self) -> String {
        if self.panel_open {
            // Nothing syncs while the network is down, so that is the
            // activity to report.
            if self.showing_offline() {
                i18n(
                    "Offline: there is no network connection, so mail is not being checked or sent. Messages you send wait in the Outbox. Everything syncs again once the network is back.",
                )
            // Reveal the current activity when the user opens the panel.
            } else if !self.status_text.is_empty() {
                self.status_text.clone()
            } else {
                i18n_f("Status bar ({n})", &[("n", &self.ids.len().to_string())])
            }
        } else {
            self.transient_text.clone()
        }
    }
}
