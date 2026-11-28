//! The Calendar (#715), shown in the main content area like Contacts: the
//! day, week, month, year and agenda views over every calendar on the
//! desktop (Evolution Data Server's: GNOME Online Accounts, CalDAV accounts
//! set up here, local calendars). The calendars to show are picked in a
//! popover in the header, as the message list's filters are.
//!
//! The page does its own reading and writing, off the UI thread, and tells
//! the app only what needs the rest of it: mail to send (an invitation, an
//! answer to one), a notice for the status bar, the sidebar's toggle.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use chrono::{Datelike, Duration, NaiveDate};
use relm4::prelude::*;

use crate::calendar::model::{Occ, OccKey};
use crate::calendar::prefs::{CalendarPrefs, View};
use crate::calendar::store::{Backend, CalInfo, Loaded};
use crate::calendar::AccountNames;
use crate::i18n::i18n;
use crate::models::InvitePerson;

mod actions;
mod agenda;
mod ctx;
mod data;
mod editor;
mod grid;
mod grid_draw;
mod inspect;
mod manage;
mod month;
mod paint;
mod quick;
mod reminders;
mod repeat;
mod side;
mod year;

use ctx::{add_months, week_of, Act, Ctx, Shared};
use side::{Side, SideAct};

pub(crate) use inspect::Ask;

/// An hour as the calendar writes it: "9 AM" or "09:00".
pub(crate) fn hour_name(hour: u32) -> String {
    ctx::hour_label(hour)
}

/// What the app hands the page when it starts.
pub(crate) struct CalendarInit {
    pub(crate) names: AccountNames,
    /// The user's addresses, for their own answers to meetings.
    pub(crate) me: Vec<String>,
    /// Who a meeting made here is organized by.
    pub(crate) organizer: Option<InvitePerson>,
}

#[derive(Debug)]
pub(crate) enum CalendarInput {
    /// The page came on screen.
    Shown,
    /// Read everything again (a write from elsewhere in the app).
    Reload,
    Act(Act),
    Side(SideAct),
    Ask(Ask),
    Calendars(Result<Vec<CalInfo>, String>),
    Loaded(String, Result<Loaded, String>),
    /// A calendar's revision moved: another app, or the server, changed it.
    Changed(String),
    Written(String, Result<(), String>),
    /// Put the last change back.
    Undo,
    /// Time to look for alarms due.
    Tick,
    /// The new-event popover's answer, for the span it was opened over.
    NewEventDone(quick::NewEvent, crate::calendar::edit::Span),
    EditorDone {
        outcome: editor::Outcome,
        was: String,
        existing: Option<(Occ, Option<std::sync::Arc<crate::calendar::model::Comp>>)>,
    },
    ScopeDone(actions::Scoped),
    View(View),
    Today,
    Step(i32),
    Search(String),
    QuickAdd(String),
    NewEvent,
    DeleteSelected,
    OpenSelected,
    Prefs(CalendarPrefs),
    Identities {
        me: Vec<String>,
        organizer: Option<InvitePerson>,
        names: AccountNames,
    },
    /// A screenshot's staging (demo only): `edit` opens the first event on
    /// screen in the editor, `inspect` shows its details.
    Showcase(String),
}

/// What the page's background work answers with: everything here crosses
/// threads, which the page's own inputs (they carry widgets) can't.
#[derive(Debug)]
pub(crate) enum Cmd {
    Calendars(Result<Vec<CalInfo>, String>),
    Loaded(String, Result<Loaded, String>),
    Written(String, Result<(), String>),
    Changed(String),
    Reload,
}

impl From<Cmd> for CalendarInput {
    fn from(c: Cmd) -> CalendarInput {
        match c {
            Cmd::Calendars(r) => CalendarInput::Calendars(r),
            Cmd::Loaded(u, r) => CalendarInput::Loaded(u, r),
            Cmd::Written(u, r) => CalendarInput::Written(u, r),
            Cmd::Changed(u) => CalendarInput::Changed(u),
            Cmd::Reload => CalendarInput::Reload,
        }
    }
}

#[derive(Debug)]
pub(crate) enum CalendarOutput {
    ToggleSidebar,
    Notice { text: String, error: bool },
    /// Mail the organizer the user's answer.
    Answer {
        ics: String,
        organizer: String,
        summary: String,
        status: String,
        from: String,
    },
    /// Mail an invitation (or a cancellation) to the attendees.
    Invite {
        ics: String,
        to: Vec<String>,
        summary: String,
        body: String,
        from: String,
        cancel: bool,
    },
    /// Write to the attendees.
    Compose { to: Vec<String>, subject: String },
    /// Settings → Contacts, where CalDAV accounts are added.
    AddAccount,
}

/// The widgets the page keeps hold of.
#[derive(Clone)]
pub(crate) struct Parts {
    title: gtk::Label,
    views: gtk::Stack,
    grid: grid::TimeGrid,
    month: month::MonthGrid,
    year: year::YearGrid,
    agenda: agenda::Agenda,
    side: Side,
    toggle: adw::ToggleGroup,
    toasts: adw::ToastOverlay,
    spinner: adw::Spinner,
}

pub(crate) struct CalendarPage {
    backend: Backend,
    prefs: CalendarPrefs,
    calendars: Vec<CalInfo>,
    loaded: HashMap<String, Loaded>,
    errors: HashMap<String, String>,
    hidden: HashSet<String>,
    view: View,
    anchor: NaiveDate,
    search: String,
    ctx: Shared,
    parts: Parts,
    popover: Option<gtk::Popover>,
    /// Calendars watched for changes: UID → subscription.
    watched: HashMap<String, gtk::gio::SignalSubscriptionId>,
    /// Reloads waiting out a burst of changes.
    reloading: HashSet<String>,
    names: AccountNames,
    organizer: Option<InvitePerson>,
    /// The last change made, and what undoes it.
    undo: Option<data::Undo>,
    listed_at: Option<std::time::Instant>,
    /// Occurrences on screen, for the selection and the day lists.
    shown: Rc<Vec<Occ>>,
    /// Alarms are shown up to here (Unix seconds).
    reminded_to: Option<i64>,
}

impl Component for CalendarPage {
    type Init = CalendarInit;
    type Input = CalendarInput;
    type Output = CalendarOutput;
    type CommandOutput = Cmd;
    type Root = gtk::Box;
    type Widgets = ();

    fn init_root() -> gtk::Box {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("calendar-page");
        root
    }

    fn init(init: CalendarInit, root: gtk::Box, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let prefs = CalendarPrefs::load();
        let input = sender.input_sender().clone();
        let ctx: Shared = Rc::new(RefCell::new(Ctx {
            colors: HashMap::new(),
            writable: HashSet::new(),
            selected: None,
            week_start: prefs.week_start.weekday(),
            week_numbers: prefs.week_numbers,
            work: (prefs.work_start, prefs.work_end),
            me: init.me.clone(),
            act: Rc::new(move |a| {
                let _ = input.send(CalendarInput::Act(a));
            }),
        }));
        let parts = build(&root, &ctx, &prefs, &sender);
        let today = chrono::Local::now().date_naive();
        let mut model = CalendarPage {
            backend: crate::calendar::store::backend(),
            hidden: ctx::hidden_set(&prefs.hidden),
            view: prefs.view,
            prefs,
            calendars: Vec::new(),
            loaded: HashMap::new(),
            errors: HashMap::new(),
            anchor: today,
            search: String::new(),
            ctx,
            parts,
            popover: None,
            watched: HashMap::new(),
            reloading: HashSet::new(),
            names: init.names,
            organizer: init.organizer,
            undo: None,
            listed_at: None,
            shown: Rc::new(Vec::new()),
            reminded_to: None,
        };
        reminders::start(&sender);
        model.show_view();
        ComponentParts { model, widgets: () }
    }

    fn update_cmd(&mut self, cmd: Cmd, sender: ComponentSender<Self>, root: &gtk::Box) {
        self.update(cmd.into(), sender, root);
    }

    fn update(&mut self, msg: CalendarInput, sender: ComponentSender<Self>, _root: &gtk::Box) {
        match msg {
            CalendarInput::Shown => self.on_shown(&sender),
            CalendarInput::Reload => self.list(&sender, true),
            CalendarInput::Calendars(r) => self.on_calendars(r, &sender),
            CalendarInput::Loaded(uid, r) => self.on_loaded(uid, r, &sender),
            CalendarInput::Changed(uid) => self.on_changed(uid, &sender),
            CalendarInput::Written(uid, r) => self.on_written(uid, r, &sender),
            CalendarInput::Undo => self.undo_last(&sender),
            CalendarInput::Tick => self.on_tick(&sender),
            CalendarInput::NewEventDone(r, span) => self.new_event_done(r, span, &sender),
            CalendarInput::EditorDone {
                outcome,
                was,
                existing,
            } => self.editor_done(outcome, was, existing, &sender),
            CalendarInput::ScopeDone(s) => self.scope_done(s, &sender),
            CalendarInput::Act(a) => self.on_act(a, &sender),
            CalendarInput::Side(a) => self.on_side(a, &sender),
            CalendarInput::Ask(a) => self.on_ask(a, &sender),
            CalendarInput::View(v) => {
                if self.view != v {
                    self.view = v;
                    self.prefs.view = v;
                    self.prefs.save();
                    self.show_view();
                }
            }
            CalendarInput::Today => {
                self.anchor = chrono::Local::now().date_naive();
                self.refresh();
                if matches!(self.view, View::Day | View::Week) {
                    self.parts.grid.scroll_to_start(true);
                }
            }
            CalendarInput::Step(n) => {
                self.anchor = match self.view {
                    View::Day => self.anchor + Duration::days(i64::from(n)),
                    View::Week => self.anchor + Duration::days(7 * i64::from(n)),
                    View::Month | View::Agenda => add_months(self.anchor, n),
                    View::Year => add_months(self.anchor, 12 * n),
                };
                self.refresh();
            }
            CalendarInput::Showcase(step) if step == "calendars" => self.parts.side.button.popup(),
            CalendarInput::Showcase(step) => {
                // The first timed event, which is the one a person would
                // click first.
                let first = self
                    .shown
                    .iter()
                    .find(|o| !o.all_day)
                    .or(self.shown.first())
                    .map(|o| o.key.clone());
                if let Some(key) = first {
                    if step == "edit" {
                        sender.input(CalendarInput::Act(Act::Open(key)));
                    } else if let Some(anchor) = self.parts.grid.anchor_of(&key) {
                        sender.input(CalendarInput::Act(Act::Select(Some(key), Some(anchor))));
                    }
                }
            }
            CalendarInput::Search(text) => {
                self.search = text.trim().to_string();
                self.refresh();
            }
            CalendarInput::QuickAdd(text) => self.quick_add(&text, &sender),
            CalendarInput::NewEvent => self.new_event_now(&sender),
            CalendarInput::DeleteSelected => {
                if self.ctx.borrow().selected.is_some() {
                    self.on_ask(Ask::Delete, &sender);
                }
            }
            CalendarInput::OpenSelected => {
                if let Some(key) = self.ctx.borrow().selected.clone() {
                    sender.input(CalendarInput::Act(Act::Open(key)));
                }
            }
            CalendarInput::Prefs(p) => self.set_prefs(p),
            CalendarInput::Identities {
                me,
                organizer,
                names,
            } => {
                self.ctx.borrow_mut().me = me;
                self.organizer = organizer;
                self.names = names;
            }
        }
    }
}

/// Build the page: the views under their header.
fn build(root: &gtk::Box, ctx: &Shared, prefs: &CalendarPrefs, sender: &ComponentSender<CalendarPage>) -> Parts {
    let input = sender.input_sender().clone();
    let side = Side::new(move |a| {
        let _ = input.send(CalendarInput::Side(a));
    });
    let header = adw::HeaderBar::new();
    header.add_css_class("flat");
    // The month or day in view, large, over the view as Calendar on the
    // Mac has it: the header keeps its room for the buttons.
    let title = gtk::Label::new(None);
    title.add_css_class("calendar-title");
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_xalign(0.0);
    title.set_hexpand(true);
    let sidebar_btn = gtk::Button::from_icon_name("sidebar-show-symbolic");
    sidebar_btn.set_tooltip_text(Some(&i18n("Toggle sidebar")));
    sidebar_btn.add_css_class("flat");
    let s = sender.clone();
    sidebar_btn.connect_clicked(move |_| {
        let _ = s.output(CalendarOutput::ToggleSidebar);
    });
    header.pack_start(&sidebar_btn);
    let today = gtk::Button::with_label(&i18n("Today"));
    today.set_tooltip_text(Some(&i18n("Go to today (T)")));
    let s = sender.input_sender().clone();
    today.connect_clicked(move |_| {
        let _ = s.send(CalendarInput::Today);
    });
    header.pack_start(&today);
    let nav = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    nav.add_css_class("linked");
    for (icon, step, tip) in [
        ("go-previous-symbolic", -1, i18n("Previous")),
        ("go-next-symbolic", 1, i18n("Next")),
    ] {
        let b = gtk::Button::from_icon_name(icon);
        b.set_tooltip_text(Some(&tip));
        let s = sender.input_sender().clone();
        b.connect_clicked(move |_| {
            let _ = s.send(CalendarInput::Step(step));
        });
        nav.append(&b);
    }
    header.pack_start(&nav);

    let quick = gtk::MenuButton::new();
    quick.set_icon_name("list-add-symbolic");
    quick.set_tooltip_text(Some(&i18n("New Event (Ctrl+N)")));
    quick.set_popover(Some(&quick_add_popover(sender)));
    header.pack_end(&quick);
    let search_btn = gtk::ToggleButton::new();
    search_btn.set_icon_name("system-search-symbolic");
    search_btn.set_tooltip_text(Some(&i18n("Search Events (Ctrl+F)")));
    header.pack_end(&search_btn);
    header.pack_end(&side.button);
    let toggle = adw::ToggleGroup::new();
    for (name, label, tip) in [
        ("day", i18n("Day"), "Ctrl+1"),
        ("week", i18n("Week"), "Ctrl+2"),
        ("month", i18n("Month"), "Ctrl+3"),
        ("year", i18n("Year"), "Ctrl+4"),
        ("agenda", i18n("List"), "Ctrl+5"),
    ] {
        let t = adw::Toggle::new();
        t.set_name(Some(name));
        t.set_label(Some(&label));
        t.set_tooltip(&format!("{label} ({tip})"));
        toggle.add(t);
    }
    let s = sender.input_sender().clone();
    toggle.connect_active_name_notify(move |g| {
        let view = match g.active_name().as_deref() {
            Some("day") => View::Day,
            Some("month") => View::Month,
            Some("year") => View::Year,
            Some("agenda") => View::Agenda,
            _ => View::Week,
        };
        let _ = s.send(CalendarInput::View(view));
    });
    header.set_title_widget(Some(&toggle));
    let spinner = adw::Spinner::new();
    spinner.set_visible(false);
    header.pack_end(&spinner);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some(&i18n("Search events")));
    search.set_hexpand(true);
    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(480);
    clamp.set_child(Some(&search));
    let search_bar = gtk::SearchBar::new();
    search_bar.set_child(Some(&clamp));
    search_bar.connect_entry(&search);
    search_btn
        .bind_property("active", &search_bar, "search-mode-enabled")
        .bidirectional()
        .sync_create()
        .build();
    let s = sender.input_sender().clone();
    search.connect_search_changed(move |e| {
        let _ = s.send(CalendarInput::Search(e.text().to_string()));
    });
    let s = sender.input_sender().clone();
    search_bar.connect_search_mode_enabled_notify(move |b| {
        if !b.is_search_mode() {
            let _ = s.send(CalendarInput::Search(String::new()));
        }
    });

    let grid = grid::TimeGrid::new(ctx.clone(), prefs.hour_height);
    grid.on_zoom(|h| {
        let mut p = CalendarPrefs::load();
        p.hour_height = h;
        p.save();
    });
    let month = month::MonthGrid::new(ctx.clone());
    let year = year::YearGrid::new(ctx.clone());
    let agenda = agenda::Agenda::new(ctx.clone());
    let views = gtk::Stack::new();
    views.set_transition_type(gtk::StackTransitionType::Crossfade);
    views.set_transition_duration(120);
    views.add_named(&grid.root, Some("grid"));
    views.add_named(&month.root, Some("month"));
    views.add_named(&year.root, Some("year"));
    views.add_named(&agenda.root, Some("agenda"));
    views.set_vexpand(true);
    views.set_hexpand(true);
    // The keyboard goes to the views, so the calendar's keys come before
    // the window's mail shortcuts; a click in them takes it back.
    views.set_focusable(true);
    let grab = gtk::GestureClick::new();
    grab.set_propagation_phase(gtk::PropagationPhase::Capture);
    let v = views.clone();
    grab.connect_pressed(move |_, _, _, _| {
        v.grab_focus();
    });
    views.add_controller(grab);
    let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    title_row.add_css_class("calendar-title-row");
    title_row.append(&title);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&title_row);
    content.append(&views);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&content));

    let main = adw::ToolbarView::new();
    main.add_top_bar(&header);
    main.add_top_bar(&search_bar);
    main.set_content(Some(&toasts));
    main.set_hexpand(true);
    root.append(&main);

    connect_keys(root, sender, &search_bar);
    Parts {
        title,
        views,
        grid,
        month,
        year,
        agenda,
        side,
        toggle,
        toasts,
        spinner,
    }
}

/// The quick-add popover: one line read as an event.
fn quick_add_popover(sender: &ComponentSender<CalendarPage>) -> gtk::Popover {
    let pop = gtk::Popover::new();
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 8);
    bx.set_width_request(340);
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(&i18n("Lunch with Sam tomorrow at 12:30")));
    bx.append(&entry);
    let hint = gtk::Label::new(Some(&i18n("Type what and when, then press Enter.")));
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    bx.append(&hint);
    let full = gtk::Button::with_label(&i18n("New Event…"));
    full.add_css_class("flat");
    full.set_halign(gtk::Align::Start);
    bx.append(&full);
    pop.set_child(Some(&bx));
    let s = sender.input_sender().clone();
    let p = pop.clone();
    entry.connect_activate(move |e| {
        let text = e.text().to_string();
        if text.trim().is_empty() {
            return;
        }
        e.set_text("");
        p.popdown();
        let _ = s.send(CalendarInput::QuickAdd(text));
    });
    let s = sender.input_sender().clone();
    let p = pop.clone();
    full.connect_clicked(move |_| {
        p.popdown();
        let _ = s.send(CalendarInput::NewEvent);
    });
    let e = entry.clone();
    pop.connect_show(move |_| {
        e.grab_focus();
    });
    pop
}

/// The calendar's keys: Ctrl+1 to Ctrl+5 for the views, T for today, the
/// arrows and Page Up and Down to move, Delete, Enter, Ctrl+N, Ctrl+F.
fn connect_keys(root: &gtk::Box, sender: &ComponentSender<CalendarPage>, bar: &gtk::SearchBar) {
    use gtk::gdk::{Key, ModifierType};
    let keys = gtk::EventControllerKey::new();
    let s = sender.input_sender().clone();
    let bar = bar.clone();
    keys.connect_key_pressed(move |c, key, _, mods| {
        // Typing in an entry is not a shortcut.
        let typing = c
            .widget()
            .and_then(|w| w.root())
            .and_then(|r| r.focus())
            .is_some_and(|f| f.is::<gtk::Text>() || f.is::<gtk::TextView>());
        let ctrl = mods.contains(ModifierType::CONTROL_MASK);
        let send = |m: CalendarInput| {
            let _ = s.send(m);
            gtk::glib::Propagation::Stop
        };
        if ctrl {
            return match key {
                Key::_1 | Key::KP_1 => send(CalendarInput::View(View::Day)),
                Key::_2 | Key::KP_2 => send(CalendarInput::View(View::Week)),
                Key::_3 | Key::KP_3 => send(CalendarInput::View(View::Month)),
                Key::_4 | Key::KP_4 => send(CalendarInput::View(View::Year)),
                Key::_5 | Key::KP_5 => send(CalendarInput::View(View::Agenda)),
                Key::n | Key::N => send(CalendarInput::NewEvent),
                Key::f | Key::F => {
                    bar.set_search_mode(true);
                    gtk::glib::Propagation::Stop
                }
                Key::r | Key::R => send(CalendarInput::Reload),
                _ => gtk::glib::Propagation::Proceed,
            };
        }
        if typing {
            return gtk::glib::Propagation::Proceed;
        }
        match key {
            Key::t | Key::T => send(CalendarInput::Today),
            Key::Page_Up | Key::Left => send(CalendarInput::Step(-1)),
            Key::Page_Down | Key::Right => send(CalendarInput::Step(1)),
            Key::Delete | Key::BackSpace => send(CalendarInput::DeleteSelected),
            Key::Return | Key::KP_Enter => send(CalendarInput::OpenSelected),
            Key::Escape => send(CalendarInput::Act(Act::Select(None, None))),
            Key::d | Key::D => send(CalendarInput::View(View::Day)),
            Key::w | Key::W => send(CalendarInput::View(View::Week)),
            Key::m | Key::M => send(CalendarInput::View(View::Month)),
            Key::y | Key::Y => send(CalendarInput::View(View::Year)),
            Key::l | Key::L => send(CalendarInput::View(View::Agenda)),
            _ => gtk::glib::Propagation::Proceed,
        }
    });
    root.add_controller(keys);
}

impl CalendarPage {
    /// The days the current view covers: its first, and how many.
    fn span(&self) -> (NaiveDate, i64) {
        let start = self.ctx.borrow().week_start;
        match self.view {
            View::Day => (self.anchor, 1),
            View::Week if self.prefs.work_week => {
                let monday = week_of(self.anchor, chrono::Weekday::Mon);
                (monday, 5)
            }
            View::Week => (week_of(self.anchor, start), 7),
            View::Month => {
                let (first, weeks) = month::MonthGrid::span(self.anchor, start);
                (first, weeks as i64 * 7)
            }
            View::Year => {
                let jan = NaiveDate::from_ymd_opt(self.anchor.year(), 1, 1).unwrap_or(self.anchor);
                let next = NaiveDate::from_ymd_opt(self.anchor.year() + 1, 1, 1).unwrap_or(self.anchor);
                (jan, (next - jan).num_days())
            }
            View::Agenda => (self.anchor, 60),
        }
    }

    /// The header's title for the view: the month and year, a day, or the
    /// year alone.
    fn title(&self) -> String {
        match self.view {
            View::Day => ctx::long_day(self.anchor),
            View::Year => self.anchor.year().to_string(),
            View::Week => {
                let (first, n) = self.span();
                let last = first + Duration::days(n - 1);
                if first.month() == last.month() {
                    ctx::month_year(first)
                } else if first.year() == last.year() {
                    format!("{} – {}", ctx::fmt(first, "%b"), ctx::fmt(last, "%b %Y"))
                } else {
                    format!("{} – {}", ctx::fmt(first, "%b %Y"), ctx::fmt(last, "%b %Y"))
                }
            }
            View::Month | View::Agenda => ctx::month_year(self.anchor),
        }
    }

    /// Switch the stack and the header to the current view, and fill it.
    fn show_view(&mut self) {
        let (child, name) = match self.view {
            View::Day => ("grid", "day"),
            View::Week => ("grid", "week"),
            View::Month => ("month", "month"),
            View::Year => ("year", "year"),
            View::Agenda => ("agenda", "agenda"),
        };
        self.parts.views.set_visible_child_name(child);
        if self.parts.toggle.active_name().as_deref() != Some(name) {
            self.parts.toggle.set_active_name(Some(name));
        }
        self.refresh_view_mut();
        if child == "grid" {
            self.parts.grid.scroll_to_start(false);
        }
    }

    fn refresh(&mut self) {
        self.refresh_view_mut();
    }

    /// Every visible occurrence in `[from, to)`, sorted by start, declined
    /// meetings left out unless the settings keep them.
    fn occurrences(&mut self, from: i64, to: i64) -> Vec<Occ> {
        let me = self.ctx.borrow().me.clone();
        let mut out = Vec::new();
        for (uid, l) in self.loaded.iter_mut() {
            if self.hidden.contains(uid) {
                continue;
            }
            out.extend(l.book.occurrences(uid, &mut l.zones, from, to));
        }
        if !self.prefs.show_declined {
            out.retain(|o| o.comp.my_status(&me) != Some("DECLINED"));
        }
        out.sort_by(|a, b| {
            (a.start, b.end - b.start, &a.comp.summary).cmp(&(b.start, a.end - a.start, &b.comp.summary))
        });
        out
    }

    fn refresh_view_mut(&mut self) {
        self.parts.title.set_label(&self.title());
        let (first, n) = self.span();
        let from = crate::calendar::time::day_start(first);
        let to = crate::calendar::time::day_start(first + Duration::days(n));
        if !self.search.is_empty() {
            let found = self.search_results();
            self.shown = Rc::new(found.clone());
            self.parts.views.set_visible_child_name("agenda");
            self.parts.agenda.results(&found);
            return;
        }
        let occs = Rc::new(self.occurrences(from, to));
        self.shown = occs.clone();
        match self.view {
            View::Day | View::Week => {
                self.parts.views.set_visible_child_name("grid");
                let days = ctx::days(first, n);
                self.parts.grid.set(days, occs, self.prefs.default_minutes);
            }
            View::Month => {
                self.parts.views.set_visible_child_name("month");
                self.parts.month.set(self.anchor, occs);
            }
            View::Year => {
                self.parts.views.set_visible_child_name("year");
                let busy = self.busy_days(&occs);
                self.parts.year.set(self.anchor.year(), busy);
            }
            View::Agenda => {
                self.parts.views.set_visible_child_name("agenda");
                let empty = if self.loaded.is_empty() {
                    (i18n("No Calendars"), i18n("Calendars set up on this computer show here."))
                } else {
                    (i18n("Nothing Coming Up"), i18n("No events in the next 60 days."))
                };
                self.parts.agenda.set(&occs, first, n, (&empty.0, &empty.1));
            }
        }
    }

    /// Days with something on them, and the color of their first event.
    fn busy_days(&self, occs: &[Occ]) -> HashMap<NaiveDate, (usize, paint::Rgba)> {
        let ctx = self.ctx.borrow();
        let mut busy: HashMap<NaiveDate, (usize, paint::Rgba)> = HashMap::new();
        for o in occs {
            let mut d = o.first_day;
            while d <= o.last_day {
                let e = busy.entry(d).or_insert((0, ctx.color(o)));
                e.0 += 1;
                d += Duration::days(1);
            }
        }
        busy
    }

    /// Events whose title, place, notes or people hold every word, from a
    /// year back to two years on.
    fn search_results(&mut self) -> Vec<Occ> {
        let words: Vec<String> = self
            .search
            .to_lowercase()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let today = chrono::Local::now().date_naive();
        let from = crate::calendar::time::day_start(today - Duration::days(365));
        let to = crate::calendar::time::day_start(today + Duration::days(730));
        let mut found: Vec<Occ> = self
            .occurrences(from, to)
            .into_iter()
            .filter(|o| {
                let c = &o.comp;
                let hay = format!(
                    "{} {} {} {} {}",
                    c.summary,
                    c.location,
                    c.description,
                    c.categories.join(" "),
                    c.attendees
                        .iter()
                        .map(|a| format!("{} {}", a.name, a.email))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
                .to_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .collect();
        // What is coming first, then what has been, nearest first.
        let now = crate::datefmt::now();
        found.sort_by_key(|o| (o.end < now, if o.end < now { -o.start } else { o.start }));
        found.truncate(300);
        found.sort_by_key(|o| o.start);
        found
    }

    fn set_prefs(&mut self, p: CalendarPrefs) {
        {
            let mut c = self.ctx.borrow_mut();
            c.week_start = p.week_start.weekday();
            c.week_numbers = p.week_numbers;
            c.work = (p.work_start, p.work_end);
        }
        // The layout the page keeps itself is not the Settings window's.
        let keep = (self.prefs.view, self.prefs.hidden.clone(), self.prefs.hour_height);
        self.prefs = p;
        (self.prefs.view, self.prefs.hidden, self.prefs.hour_height) = keep;
        self.refresh();
    }

    fn toast(&self, text: &str) {
        let t = adw::Toast::new(text);
        t.set_timeout(4);
        self.parts.toasts.add_toast(t);
    }
}

/// The selected occurrence, from the ones on screen.
fn find_occ<'a>(shown: &'a [Occ], key: &OccKey) -> Option<&'a Occ> {
    shown.iter().find(|o| o.key == *key)
}
