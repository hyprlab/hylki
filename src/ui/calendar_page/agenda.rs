//! The agenda: what is coming up, day by day, as a list. The same list
//! shows search results, and one day's events for the month view's
//! "N more".

use std::rc::Rc;

use adw::prelude::*;
use chrono::{Duration, NaiveDate};

use super::ctx::{long_day, Act, Anchor, Shared};
use crate::calendar::model::Occ;
use crate::i18n::i18n;

#[derive(Clone)]
pub(crate) struct Agenda {
    pub(crate) root: gtk::Stack,
    list: gtk::ListBox,
    empty: adw::StatusPage,
    ctx: Shared,
}

impl Agenda {
    pub(crate) fn new(ctx: Shared) -> Agenda {
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("calendar-agenda");
        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(760);
        clamp.set_child(Some(&list));
        clamp.set_margin_top(6);
        clamp.set_margin_bottom(18);
        clamp.set_margin_start(12);
        clamp.set_margin_end(12);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&clamp));
        let empty = adw::StatusPage::new();
        empty.set_icon_name(Some("x-office-calendar-symbolic"));
        empty.add_css_class("compact");
        let root = gtk::Stack::new();
        root.add_named(&scroller, Some("list"));
        root.add_named(&empty, Some("empty"));
        Agenda {
            root,
            list,
            empty,
            ctx,
        }
    }

    /// Show `occs` (sorted by start), from `from` on, grouped by day. An
    /// event over several days is listed on each day it takes, up to
    /// `days` days.
    pub(crate) fn set(&self, occs: &[Occ], from: NaiveDate, days: i64, empty: (&str, &str)) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        let mut any = false;
        for i in 0..days {
            let day = from + Duration::days(i);
            let mut here: Vec<&Occ> = occs
                .iter()
                .filter(|o| o.first_day <= day && o.last_day >= day)
                .collect();
            if here.is_empty() {
                continue;
            }
            here.sort_by_key(|o| (!o.all_day, o.start));
            any = true;
            self.list.append(&day_heading(day));
            for o in here {
                self.list.append(&event_row(&self.ctx, o, Some(day)));
            }
        }
        self.empty.set_title(empty.0);
        self.empty.set_description(Some(empty.1));
        self.root
            .set_visible_child_name(if any { "list" } else { "empty" });
    }

    /// Search results: each occurrence once, under its own day.
    pub(crate) fn results(&self, occs: &[Occ]) {
        while let Some(row) = self.list.first_child() {
            self.list.remove(&row);
        }
        let mut last: Option<NaiveDate> = None;
        for o in occs {
            if last != Some(o.first_day) {
                self.list.append(&day_heading(o.first_day));
                last = Some(o.first_day);
            }
            self.list.append(&event_row(&self.ctx, o, None));
        }
        self.empty.set_title(&i18n("No Events Found"));
        self.empty.set_description(Some(&i18n("Try other words.")));
        self.root
            .set_visible_child_name(if occs.is_empty() { "empty" } else { "list" });
    }
}

fn day_heading(day: NaiveDate) -> gtk::ListBoxRow {
    let today = chrono::Local::now().date_naive();
    let text = if day == today {
        format!("{} · {}", i18n("Today"), long_day(day))
    } else if day == today + Duration::days(1) {
        format!("{} · {}", i18n("Tomorrow"), long_day(day))
    } else {
        long_day(day)
    };
    let label = gtk::Label::new(Some(&text));
    label.set_halign(gtk::Align::Start);
    label.add_css_class("heading");
    if day == today {
        label.add_css_class("accent");
    }
    let row = gtk::ListBoxRow::new();
    row.set_activatable(false);
    row.set_selectable(false);
    row.add_css_class("calendar-agenda-day");
    row.set_child(Some(&label));
    row
}

/// One event, as the agenda, a search or a day's list shows it.
pub(crate) fn event_row(ctx: &Shared, o: &Occ, on: Option<NaiveDate>) -> gtk::ListBoxRow {
    let c = ctx.borrow();
    let color = c.color(o);
    let standing = c.standing(o);
    drop(c);
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    hbox.add_css_class("calendar-agenda-event");
    let swatch = gtk::DrawingArea::new();
    swatch.set_content_width(10);
    swatch.set_content_height(10);
    swatch.set_valign(gtk::Align::Center);
    swatch.set_draw_func(move |_, cr, w, h| {
        super::paint::dot(cr, f64::from(w) / 2.0, f64::from(h) / 2.0, 5.0, color);
    });
    hbox.append(&swatch);
    let time = gtk::Label::new(Some(&time_text(o, on)));
    time.add_css_class("dim-label");
    time.add_css_class("numeric");
    time.set_xalign(0.0);
    time.set_width_chars(18);
    time.set_valign(gtk::Align::Start);
    hbox.append(&time);
    let words = gtk::Box::new(gtk::Orientation::Vertical, 2);
    words.set_hexpand(true);
    let mut title_text = o.comp.summary.clone();
    if title_text.is_empty() {
        title_text = i18n("Untitled");
    }
    let title = gtk::Label::new(Some(&title_text));
    title.set_xalign(0.0);
    title.set_wrap(true);
    title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    if standing.faded() {
        title.add_css_class("dim-label");
    }
    if standing.struck() {
        let attrs = gtk::pango::AttrList::new();
        attrs.insert(gtk::pango::AttrInt::new_strikethrough(true));
        title.set_attributes(Some(&attrs));
    }
    words.append(&title);
    if !o.comp.location.is_empty() {
        let place = gtk::Label::new(Some(&o.comp.location));
        place.set_xalign(0.0);
        place.add_css_class("dim-label");
        place.add_css_class("caption");
        place.set_ellipsize(gtk::pango::EllipsizeMode::End);
        words.append(&place);
    }
    hbox.append(&words);
    if o.in_series {
        let icon = gtk::Image::from_icon_name("media-playlist-repeat-symbolic");
        icon.add_css_class("dim-label");
        icon.set_tooltip_text(Some(&i18n("Repeats")));
        hbox.append(&icon);
    }
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    let key = o.key.clone();
    let ctx2 = ctx.clone();
    let click = gtk::GestureClick::new();
    click.connect_released(move |g, n, _, _| {
        let Some(w) = g.widget() else {
            return;
        };
        let act = if n == 2 {
            Act::Open(key.clone())
        } else {
            let (ww, hh) = (f64::from(w.width()), f64::from(w.height()));
            Act::Select(Some(key.clone()), Some(Anchor::new(&w, 0.0, 0.0, ww, hh)))
        };
        ctx2.borrow().emit(act);
    });
    row.add_controller(click);
    let key = o.key.clone();
    let ctx3 = ctx.clone();
    let menu = gtk::GestureClick::new();
    menu.set_button(3);
    menu.connect_pressed(move |g, _, x, y| {
        let Some(w) = g.widget() else {
            return;
        };
        ctx3.borrow().emit(Act::Menu {
            key: Some(key.clone()),
            at: None,
            all_day: false,
            anchor: Anchor::new(&w, x, y, 1.0, 1.0),
        });
    });
    row.add_controller(menu);
    row
}

/// "All day", "9:30 – 10:00", or for one day of a longer event, where it
/// starts or ends.
fn time_text(o: &Occ, on: Option<NaiveDate>) -> String {
    if o.all_day {
        return i18n("All day");
    }
    let t = crate::datefmt::time;
    match on {
        Some(day) if o.first_day != o.last_day => {
            if day == o.first_day {
                format!("{} –", t(o.start))
            } else if day == o.last_day {
                format!("– {}", t(o.end))
            } else {
                i18n("All day")
            }
        }
        _ if o.end > o.start => format!("{} – {}", t(o.start), t(o.end)),
        _ => t(o.start),
    }
}

/// A popover listing one day's events, for the month view's "N more".
pub(crate) fn day_popover(ctx: &Shared, day: NaiveDate, occs: &[Occ], anchor: &Anchor) {
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("calendar-agenda");
    let mut here: Vec<&Occ> = occs
        .iter()
        .filter(|o| o.first_day <= day && o.last_day >= day)
        .collect();
    here.sort_by_key(|o| (!o.all_day, o.start));
    for o in here {
        list.append(&event_row(ctx, o, Some(day)));
    }
    let title = gtk::Label::new(Some(&long_day(day)));
    title.add_css_class("heading");
    title.set_halign(gtk::Align::Start);
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 6);
    bx.append(&title);
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_propagate_natural_height(true);
    scroller.set_max_content_height(420);
    scroller.set_min_content_width(320);
    scroller.set_child(Some(&list));
    bx.append(&scroller);
    let pop = gtk::Popover::new();
    pop.set_child(Some(&bx));
    pop.set_parent(&anchor.widget);
    pop.set_pointing_to(Some(&anchor.rect));
    pop.connect_closed(|p| {
        let p = p.clone();
        gtk::glib::idle_add_local_once(move || p.unparent());
    });
    pop.popup();
    let _ = Rc::new(());
}
