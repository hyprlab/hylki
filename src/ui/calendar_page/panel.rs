//! The day panel (#715): one day's hours down the right of the window,
//! beside the mail, Contacts or the attachments, with what is coming up in
//! the week after it underneath. It is the calendar's own Day view, made
//! narrow: events are made, moved, opened and answered there the same way.
//! The app shows it anywhere but over the calendar itself.

use adw::prelude::*;
use chrono::{Duration, NaiveDate};
use relm4::prelude::*;

use super::agenda::Agenda;
use super::ctx::{long_day, Shared};
use super::grid::TimeGrid;
use super::{CalendarInput, CalendarOutput, CalendarPage};
use crate::i18n::i18n;

/// The narrowest and widest the panel is dragged to.
const MIN_WIDTH: i32 = 260;
const MAX_WIDTH: i32 = 640;
/// The days "Coming Up" looks ahead, after the day shown.
pub(super) const AHEAD: i64 = 7;

#[derive(Clone)]
pub(crate) struct DayPanel {
    pub(crate) root: gtk::Box,
    header: adw::HeaderBar,
    title: gtk::Label,
    today: gtk::Button,
    pub(super) grid: TimeGrid,
    pub(super) agenda: Agenda,
}

impl DayPanel {
    pub(super) fn new(ctx: Shared, hour_h: f64, sender: &ComponentSender<CalendarPage>) -> DayPanel {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.add_css_class("calendar-day-panel");
        // The left edge is a handle: dragged, it makes the panel wider or
        // narrower, as the mail panes' dividers do.
        let edge = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        edge.add_css_class("calendar-day-panel-edge");
        edge.set_cursor_from_name(Some("col-resize"));
        edge.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        root.append(&edge);

        let header = adw::HeaderBar::new();
        header.add_css_class("flat");
        header.set_show_start_title_buttons(false);
        let title = gtk::Label::new(None);
        title.add_css_class("heading");
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        header.set_title_widget(Some(&title));
        let nav = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        nav.add_css_class("linked");
        for (icon, step, tip) in [
            ("go-previous-symbolic", -1, i18n("Previous Day")),
            ("go-next-symbolic", 1, i18n("Next Day")),
        ] {
            let b = gtk::Button::from_icon_name(icon);
            b.set_tooltip_text(Some(&tip));
            let s = sender.input_sender().clone();
            b.connect_clicked(move |_| {
                let _ = s.send(CalendarInput::PanelStep(step));
            });
            nav.append(&b);
        }
        header.pack_start(&nav);
        let today = gtk::Button::from_icon_name("x-office-calendar-symbolic");
        today.set_tooltip_text(Some(&i18n("Today")));
        let s = sender.input_sender().clone();
        today.connect_clicked(move |_| {
            let _ = s.send(CalendarInput::PanelToday);
        });
        header.pack_start(&today);
        let open = gtk::Button::from_icon_name("adw-external-link-symbolic");
        open.set_tooltip_text(Some(&i18n("Open in Calendar")));
        let s = sender.input_sender().clone();
        open.connect_clicked(move |_| {
            let _ = s.send(CalendarInput::PanelOpen);
        });
        header.pack_end(&open);

        let grid = TimeGrid::new(ctx.clone(), hour_h);
        grid.root.set_vexpand(true);
        // The header's title names the day already.
        grid.hide_days();
        let agenda = Agenda::compact(ctx);
        let coming = gtk::Label::new(Some(&i18n("Coming Up")));
        coming.add_css_class("heading");
        coming.set_xalign(0.0);
        coming.add_css_class("calendar-day-panel-heading");
        let lower = gtk::Box::new(gtk::Orientation::Vertical, 0);
        lower.append(&coming);
        lower.append(&agenda.root);
        let split = gtk::Paned::new(gtk::Orientation::Vertical);
        split.set_start_child(Some(&grid.root));
        split.set_end_child(Some(&lower));
        split.set_resize_start_child(true);
        split.set_shrink_start_child(false);
        split.set_shrink_end_child(false);
        split.set_wide_handle(false);
        // Most of it to the hours, the rest to the week ahead.
        split.set_position(520);

        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.set_width_request(
            crate::calendar::prefs::CalendarPrefs::load()
                .day_panel_width
                .clamp(MIN_WIDTH, MAX_WIDTH),
        );
        // Measured on the window's surface, not on the edge: the edge moves
        // as the panel widens, and offsets taken from it would chase their
        // own tail.
        let drag = gtk::GestureDrag::new();
        let start = std::rc::Rc::new(std::cell::Cell::new((0, 0.0)));
        let surface_x = |g: &gtk::GestureDrag| g.current_event().and_then(|e| e.position()).map(|(x, _)| x);
        let (c, st) = (column.clone(), start.clone());
        drag.connect_drag_begin(move |g, _, _| {
            if let Some(x) = surface_x(g) {
                st.set((c.width(), x));
            }
        });
        let (c, st) = (column.clone(), start.clone());
        drag.connect_drag_update(move |g, _, _| {
            let Some(x) = surface_x(g) else {
                return;
            };
            let (width, from) = st.get();
            c.set_width_request((width + (from - x) as i32).clamp(MIN_WIDTH, MAX_WIDTH));
        });
        let c = column.clone();
        drag.connect_drag_end(move |_, _, _| {
            let mut p = crate::calendar::prefs::CalendarPrefs::load();
            p.day_panel_width = c.width_request();
            p.save();
        });
        edge.add_controller(drag);
        column.append(&header);
        column.append(&split);
        root.append(&column);
        DayPanel {
            root,
            header,
            title,
            today,
            grid,
            agenda,
        }
    }

    pub(super) fn set_title(&self, day: NaiveDate) {
        let today = chrono::Local::now().date_naive();
        let text = if day == today {
            i18n("Today")
        } else if day == today + Duration::days(1) {
            i18n("Tomorrow")
        } else if day == today - Duration::days(1) {
            i18n("Yesterday")
        } else {
            long_day(day)
        };
        self.title.set_label(&text);
        self.title.set_tooltip_text(Some(&long_day(day)));
        self.today.set_sensitive(day != today);
    }
}

impl CalendarPage {
    /// Whether the panel's header carries the window's controls: it does
    /// while it is the rightmost pane.
    pub(crate) fn set_panel_controls(&self, on: bool) {
        self.parts.panel.header.set_show_end_title_buttons(on);
    }

    /// The panel's widget, for the app to place on the window's right.
    pub(crate) fn panel_widget(&self) -> gtk::Widget {
        self.parts.panel.root.clone().upcast()
    }

    /// Fill the panel for its day: the hours, and the week after.
    pub(super) fn refresh_panel(&mut self) {
        let day = self.panel_day;
        let from = crate::calendar::time::day_start(day);
        let to = crate::calendar::time::day_start(day + Duration::days(1));
        let occs = std::rc::Rc::new(self.occurrences(from, to));
        let next = day + Duration::days(1);
        let ahead = self.occurrences(to, crate::calendar::time::day_start(next + Duration::days(AHEAD)));
        let mut both = (*occs).clone();
        both.extend(ahead.iter().cloned());
        self.panel_shown = std::rc::Rc::new(both);
        self.parts.panel.set_title(day);
        self.parts.panel.grid.set(vec![day], occs, self.prefs.default_minutes);
        self.parts.panel.agenda.set(
            &ahead,
            next,
            AHEAD,
            (&i18n("Nothing Coming Up"), &i18n("No events in the week ahead.")),
        );
    }

    /// The panel came on screen: read the calendars if nothing has been.
    pub(super) fn panel_shown(&mut self, sender: &ComponentSender<Self>) {
        if self.listed_at.is_none() {
            self.list(sender, false);
        }
        self.refresh_panel();
        self.parts.panel.grid.scroll_to_start(true);
    }

    /// Open the calendar on the panel's day.
    pub(super) fn panel_open(&mut self, sender: &ComponentSender<Self>) {
        self.anchor = self.panel_day;
        self.refresh();
        let _ = sender.output(CalendarOutput::Open);
    }
}
