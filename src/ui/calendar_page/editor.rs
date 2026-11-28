//! The event editor: everything about an event, for a new one or one being
//! changed. The popover after a drag or a quick add covers the common case;
//! this is for the rest (how it repeats, its alarms, who is invited).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Weekday};

use super::repeat::{Preset, PRESETS};
use crate::calendar::edit::{Draft, Span};
use crate::calendar::model::Status;
use crate::calendar::recur::{Freq, Rule, Until};
use crate::calendar::store::CalInfo;
use crate::i18n::{i18n, i18n_f};
use crate::models::InvitePerson;
use crate::ui::calendar_item::{alarm_label as lead_label, alarm_row, Moment, ALARMS};

/// What the editor was closed with.
#[derive(Debug)]
pub(crate) enum Outcome {
    Save { draft: Box<Draft>, calendar: String },
    Delete,
}

/// An alarm's lead time, in words: "15 minutes before".
pub(crate) fn alarm_label(minutes: i64) -> String {
    if minutes < 0 {
        return i18n_f("{n} minutes after", &[("n", &(-minutes).to_string())]);
    }
    lead_label(Some(minutes as u32))
}

fn wall_local(w: NaiveDateTime) -> chrono::DateTime<Local> {
    Local
        .from_local_datetime(&w)
        .earliest()
        .unwrap_or_else(Local::now)
}

/// Open the editor over `parent`. `calendars` are those that take a new
/// event; `calendar` is where this one is. `existing` puts Delete in it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn open(
    parent: &impl IsA<gtk::Widget>,
    draft: Draft,
    calendars: &[CalInfo],
    calendar: &str,
    existing: bool,
    writable: bool,
    me: Option<InvitePerson>,
    done: impl Fn(Outcome) + 'static,
) {
    let dialog = adw::Dialog::new();
    dialog.set_title(&if !existing {
        i18n("New Event")
    } else if writable {
        i18n("Edit Event")
    } else {
        i18n("Event")
    });
    dialog.set_content_width(480);
    dialog.set_content_height(720);
    dialog.add_css_class("calendar-editor");

    let save = gtk::Button::with_label(&if existing { i18n("Save") } else { i18n("Add") });
    save.add_css_class("suggested-action");
    save.set_sensitive(writable);
    let cancel = gtk::Button::with_label(&i18n("Cancel"));
    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(false);
    header.set_show_start_title_buttons(false);
    header.pack_start(&cancel);
    header.pack_end(&save);

    let page = gtk::Box::new(gtk::Orientation::Vertical, 18);
    page.set_margin_top(12);
    page.set_margin_bottom(24);
    page.set_margin_start(12);
    page.set_margin_end(12);
    let group = || {
        let l = gtk::ListBox::new();
        l.add_css_class("boxed-list");
        l.set_selection_mode(gtk::SelectionMode::None);
        l
    };

    // What and where.
    let what = group();
    let title = adw::EntryRow::new();
    title.set_title(&i18n("Title"));
    title.set_text(&draft.summary);
    what.append(&title);
    let place = adw::EntryRow::new();
    place.set_title(&i18n("Location"));
    place.set_text(&draft.location);
    what.append(&place);
    page.append(&what);

    // Which calendar.
    let cal_group = group();
    let cal_row = adw::ComboRow::new();
    cal_row.set_title(&i18n("Calendar"));
    let names: Vec<String> = calendars.iter().map(CalInfo::full_name).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    cal_row.set_model(Some(&gtk::StringList::new(&names)));
    cal_row.set_factory(Some(&crate::ui::contacts_page::book_label_factory(true)));
    cal_row.set_list_factory(Some(&crate::ui::contacts_page::book_label_factory(false)));
    if let Some(i) = calendars.iter().position(|c| c.uid == calendar) {
        cal_row.set_selected(i as u32);
    }
    cal_group.append(&cal_row);
    page.append(&cal_group);

    // When.
    let when = group();
    let all_day = adw::SwitchRow::new();
    all_day.set_title(&i18n("All day"));
    let (start_wall, end_wall, tzid) = match &draft.span {
        Span::Timed { start, end, tzid } => (*start, *end, tzid.clone()),
        Span::AllDay { first, last } => {
            let s = first.and_hms_opt(9, 0, 0).unwrap_or_default();
            let e = last.and_hms_opt(10, 0, 0).unwrap_or_default();
            (s, e, None)
        }
    };
    all_day.set_active(matches!(draft.span, Span::AllDay { .. }));
    let starts = Moment::new(wall_local(start_wall));
    let ends = Moment::new(wall_local(end_wall));
    when.append(&all_day);
    when.append(&starts.row(&i18n("Starts")));
    when.append(&ends.row(&i18n("Ends")));
    if let Some(tz) = tzid.as_ref().filter(|t| Some(t.as_str()) != crate::calendar::time::local_tzid().as_deref()) {
        let zone = adw::ActionRow::new();
        zone.set_title(&i18n("Time zone"));
        zone.set_subtitle(tz);
        zone.add_css_class("property");
        when.append(&zone);
    }
    let show_times = {
        let (s, e) = (starts.time.clone(), ends.time.clone());
        move |on: bool| {
            s.set_visible(!on);
            e.set_visible(!on);
        }
    };
    show_times(all_day.is_active());
    all_day.connect_active_notify(move |r| show_times(r.is_active()));
    // The end follows the start, keeping the length.
    let length = Rc::new(Cell::new((end_wall - start_wall).num_seconds().max(0)));
    {
        let (s, e, len) = (starts.clone(), ends.clone(), length.clone());
        let busy = Rc::new(Cell::new(false));
        let b2 = busy.clone();
        starts.connect_changed(move || {
            if b2.get() {
                return;
            }
            if let Some(at) = s.at() {
                b2.set(true);
                e.set(at + Duration::seconds(len.get()));
                b2.set(false);
            }
        });
        let (s, e, len) = (starts.clone(), ends.clone(), length.clone());
        ends.connect_changed(move || {
            if busy.get() {
                return;
            }
            if let (Some(a), Some(b)) = (s.at(), e.at()) {
                len.set((b - a).num_seconds().max(0));
            }
        });
    }
    page.append(&when);

    // How it repeats.
    let rep = group();
    let first_day = start_wall.date();
    let repeat_row = adw::ComboRow::new();
    repeat_row.set_title(&i18n("Repeat"));
    let labels: Vec<String> = PRESETS.iter().map(|p| p.label()).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    repeat_row.set_model(Some(&gtk::StringList::new(&labels)));
    let preset = Preset::of(draft.repeat.as_ref(), first_day);
    repeat_row.set_selected(PRESETS.iter().position(|p| *p == preset).unwrap_or(0) as u32);
    rep.append(&repeat_row);
    let custom = Custom::new(draft.repeat.as_ref(), first_day);
    for r in custom.rows() {
        rep.append(r);
    }
    let end_row = adw::ComboRow::new();
    end_row.set_title(&i18n("End repeat"));
    end_row.set_model(Some(&gtk::StringList::new(&[
        &i18n("Never"),
        &i18n("On date"),
        &i18n("After a number of times"),
    ])));
    let until = Moment::new(wall_local(
        (first_day + Duration::days(30)).and_hms_opt(12, 0, 0).unwrap_or_default(),
    ));
    until.time.set_visible(false);
    let until_row = until.row(&i18n("Last day"));
    let count_row = adw::SpinRow::with_range(1.0, 999.0, 1.0);
    count_row.set_title(&i18n("Times"));
    match draft.repeat.as_ref() {
        Some(r) if r.count.is_some() => {
            end_row.set_selected(2);
            count_row.set_value(f64::from(r.count.unwrap_or(1)));
        }
        Some(r) if r.until.is_some() => {
            end_row.set_selected(1);
            let day = match r.until {
                Some(Until::Date(d)) => d,
                Some(Until::Floating(w)) => w.date(),
                Some(Until::Utc(u)) => crate::calendar::time::local_day(u),
                None => first_day,
            };
            until.set(wall_local(day.and_hms_opt(12, 0, 0).unwrap_or_default()));
        }
        _ => count_row.set_value(10.0),
    }
    rep.append(&end_row);
    rep.append(&until_row);
    rep.append(&count_row);
    let touched = Rc::new(Cell::new(false));
    let sync_repeat = {
        let (repeat_row, end_row, until_row, count_row) =
            (repeat_row.clone(), end_row.clone(), until_row.clone(), count_row.clone());
        let custom = custom.clone();
        move || {
            let p = PRESETS[repeat_row.selected() as usize];
            let repeating = p != Preset::Never;
            custom.show(p == Preset::Custom);
            end_row.set_visible(repeating);
            until_row.set_visible(repeating && end_row.selected() == 1);
            count_row.set_visible(repeating && end_row.selected() == 2);
        }
    };
    sync_repeat();
    {
        let s = sync_repeat.clone();
        let t = touched.clone();
        repeat_row.connect_selected_notify(move |_| {
            t.set(true);
            s();
        });
        let s = sync_repeat.clone();
        let t = touched.clone();
        end_row.connect_selected_notify(move |_| {
            t.set(true);
            s();
        });
        let t = touched.clone();
        count_row.connect_value_notify(move |_| t.set(true));
        let t = touched.clone();
        until.connect_changed(move || t.set(true));
        let t = touched.clone();
        custom.connect_changed(move || t.set(true));
    }
    page.append(&rep);

    // Alarms.
    let alarms = group();
    let mins: Vec<u32> = draft
        .alarms
        .iter()
        .filter(|s| **s >= 0)
        .map(|s| (*s / 60) as u32)
        .collect();
    let first_alarm = alarm_row(&i18n("Alert"), mins.first().copied());
    let second_alarm = alarm_row(&i18n("Second alert"), mins.get(1).copied());
    alarms.append(&first_alarm);
    alarms.append(&second_alarm);
    page.append(&alarms);

    // How it shows to others.
    let shows = group();
    let busy_row = adw::ComboRow::new();
    busy_row.set_title(&i18n("Show as"));
    busy_row.set_model(Some(&gtk::StringList::new(&[&i18n("Busy"), &i18n("Free")])));
    busy_row.set_selected(u32::from(draft.free));
    shows.append(&busy_row);
    let status_row = adw::ComboRow::new();
    status_row.set_title(&i18n("Status"));
    status_row.set_model(Some(&gtk::StringList::new(&[
        &i18n("Confirmed"),
        &i18n("Tentative"),
        &i18n("Cancelled"),
    ])));
    status_row.set_selected(match draft.status {
        Status::Tentative => 1,
        Status::Cancelled => 2,
        _ => 0,
    });
    shows.append(&status_row);
    let private = adw::SwitchRow::new();
    private.set_title(&i18n("Private"));
    private.set_subtitle(&i18n("Others who share the calendar see only that you are busy."));
    private.set_active(draft.private);
    shows.append(&private);
    page.append(&shows);

    // Who is invited.
    let people_title = gtk::Label::new(Some(&i18n("Invitees")));
    people_title.add_css_class("heading");
    people_title.set_xalign(0.0);
    page.append(&people_title);
    let people = group();
    let invitees: Rc<RefCell<Vec<InvitePerson>>> = Rc::new(RefCell::new(draft.attendees.clone()));
    let add_person = adw::EntryRow::new();
    add_person.set_title(&i18n("Add an address"));
    add_person.set_show_apply_button(true);
    let fill_people: Rc<dyn Fn()> = {
        let (people, invitees, add_person) = (people.clone(), invitees.clone(), add_person.clone());
        let organizer = draft.organizer.clone();
        Rc::new(move || {
            while let Some(c) = people.first_child() {
                if c == add_person {
                    break;
                }
                people.remove(&c);
            }
            for (i, p) in invitees.borrow().iter().enumerate() {
                let row = adw::ActionRow::new();
                let shown = if p.name.is_empty() { p.email.clone() } else { p.name.clone() };
                row.set_title(&shown);
                if !p.name.is_empty() {
                    row.set_subtitle(&p.email);
                }
                if organizer.as_ref().is_some_and(|o| o.email.eq_ignore_ascii_case(&p.email)) {
                    let o = gtk::Label::new(Some(&i18n("Organizer")));
                    o.add_css_class("dim-label");
                    row.add_suffix(&o);
                }
                let remove = gtk::Button::from_icon_name("list-remove-symbolic");
                remove.add_css_class("flat");
                remove.set_valign(gtk::Align::Center);
                remove.set_tooltip_text(Some(&i18n("Remove")));
                row.add_suffix(&remove);
                people.insert(&row, i as i32);
                let inv = invitees.clone();
                let refill = people.clone();
                let email = p.email.clone();
                remove.connect_clicked(move |_| {
                    inv.borrow_mut().retain(|x| x.email != email);
                    if let Some(r) = refill.row_at_index(i as i32) {
                        refill.remove(&r);
                    }
                });
            }
        })
    };
    people.append(&add_person);
    fill_people();
    {
        let inv = invitees.clone();
        let fill = fill_people.clone();
        add_person.connect_apply(move |e| {
            let text = e.text().trim().to_string();
            for (name, email) in crate::worker::parse_recipients(&text) {
                if email.contains('@') && !inv.borrow().iter().any(|p| p.email.eq_ignore_ascii_case(&email)) {
                    inv.borrow_mut().push(InvitePerson {
                        name,
                        email,
                        status: "NEEDS-ACTION".into(),
                        rsvp: true,
                        resource: false,
                    });
                }
            }
            e.set_text("");
            fill();
        });
    }
    page.append(&people);

    // A link, and notes.
    let more = group();
    let url = adw::EntryRow::new();
    url.set_title(&i18n("URL"));
    url.set_text(&draft.url);
    url.set_input_purpose(gtk::InputPurpose::Url);
    more.append(&url);
    page.append(&more);
    let notes_label = gtk::Label::new(Some(&i18n("Notes")));
    notes_label.add_css_class("heading");
    notes_label.set_xalign(0.0);
    page.append(&notes_label);
    let notes = gtk::TextView::new();
    notes.set_wrap_mode(gtk::WrapMode::WordChar);
    notes.set_top_margin(10);
    notes.set_bottom_margin(10);
    notes.set_left_margin(12);
    notes.set_right_margin(12);
    notes.buffer().set_text(&draft.description);
    let notes_scroll = gtk::ScrolledWindow::new();
    notes_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    notes_scroll.set_min_content_height(120);
    notes_scroll.add_css_class("card");
    notes_scroll.set_overflow(gtk::Overflow::Hidden);
    notes_scroll.set_child(Some(&notes));
    page.append(&notes_scroll);

    let done = Rc::new(done);
    if existing && writable {
        let delete = gtk::Button::with_label(&i18n("Delete Event"));
        delete.add_css_class("destructive-action");
        delete.add_css_class("pill");
        delete.set_halign(gtk::Align::Center);
        delete.set_margin_top(6);
        let d = dialog.clone();
        let done2 = done.clone();
        delete.connect_clicked(move |_| {
            d.close();
            done2(Outcome::Delete);
        });
        page.append(&delete);
    }
    if !writable {
        for w in [&what, &cal_group, &when, &rep, &alarms, &shows, &people, &more] {
            w.set_sensitive(false);
        }
        notes.set_editable(false);
    }

    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(560);
    clamp.set_child(Some(&page));
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_propagate_natural_height(true);
    scroller.set_child(Some(&clamp));
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&scroller));
    dialog.set_child(Some(&view));

    {
        let s = save.clone();
        let check = move |e: &adw::EntryRow| s.set_sensitive(writable && !e.text().trim().is_empty());
        check(&title);
        title.connect_changed(check);
        let s = save.clone();
        title.connect_entry_activated(move |_| {
            if s.is_sensitive() {
                s.emit_clicked();
            }
        });
    }
    let d = dialog.clone();
    cancel.connect_clicked(move |_| {
        d.close();
    });
    let focus = title.clone();
    let d = dialog.clone();
    let cal_uids: Vec<String> = calendars.iter().map(|c| c.uid.clone()).collect();
    let original = draft.clone();
    save.connect_clicked(move |_| {
        let mut out = original.clone();
        out.summary = title.text().trim().to_string();
        out.location = place.text().trim().to_string();
        out.url = url.text().trim().to_string();
        let b = notes.buffer();
        out.description = b.text(&b.start_iter(), &b.end_iter(), false).trim().to_string();
        let s = starts.at().map(|t| t.naive_local()).unwrap_or(start_wall);
        let e = ends.at().map(|t| t.naive_local()).unwrap_or(end_wall).max(s);
        out.span = if all_day.is_active() {
            Span::AllDay {
                first: s.date(),
                last: e.date().max(s.date()),
            }
        } else {
            Span::Timed {
                start: s,
                end: e,
                tzid: tzid.clone(),
            }
        };
        if touched.get() {
            let p = PRESETS[repeat_row.selected() as usize];
            out.repeat = match p {
                Preset::Never => None,
                Preset::Custom => Some(custom.rule(s.date())),
                p => p.rule(s.date()),
            };
            if let Some(r) = out.repeat.as_mut() {
                r.count = None;
                r.until = None;
                match end_row.selected() {
                    1 => {
                        if let Some(t) = until.at() {
                            r.until = Some(Until::Date(t.date_naive()));
                        }
                    }
                    2 => r.count = Some(count_row.value() as u32),
                    _ => {}
                }
            }
        }
        // Times written as a date when the event is all-day.
        if let (Some(r), Span::AllDay { .. }) = (out.repeat.as_mut(), &out.span) {
            if let Some(Until::Utc(u)) = r.until {
                r.until = Some(Until::Date(crate::calendar::time::local_day(u)));
            }
        }
        let mut leads: Vec<i64> = [first_alarm.selected(), second_alarm.selected()]
            .iter()
            .filter_map(|i| ALARMS.get(*i as usize).copied().flatten())
            .map(|m| i64::from(m) * 60)
            .collect();
        leads.dedup();
        // Alarms the menus can't show stay as they were.
        let offered: Vec<i64> = ALARMS.iter().flatten().map(|m| i64::from(*m) * 60).collect();
        leads.extend(original.alarms.iter().filter(|a| !offered.contains(a)).copied());
        out.alarms = leads;
        out.free = busy_row.selected() == 1;
        out.status = match status_row.selected() {
            1 => Status::Tentative,
            2 => Status::Cancelled,
            _ if original.status == Status::None => Status::None,
            _ => Status::Confirmed,
        };
        out.private = private.is_active();
        out.attendees = invitees.borrow().clone();
        if !out.attendees.is_empty() && out.organizer.is_none() {
            out.organizer = me.clone();
            if let Some(me) = &me {
                if !out.attendees.iter().any(|a| a.email.eq_ignore_ascii_case(&me.email)) {
                    out.attendees.insert(
                        0,
                        InvitePerson {
                            status: "ACCEPTED".into(),
                            rsvp: false,
                            ..me.clone()
                        },
                    );
                }
            }
        }
        let calendar = cal_uids
            .get(cal_row.selected() as usize)
            .cloned()
            .unwrap_or_default();
        d.close();
        done(Outcome::Save {
            draft: Box::new(out),
            calendar,
        });
    });
    dialog.present(Some(parent));
    focus.grab_focus();
}

/// The Custom repeat rows: every how many of what, on which weekdays, and
/// for a month, by its date or by its weekday.
#[derive(Clone)]
struct Custom {
    every: adw::SpinRow,
    unit: adw::ComboRow,
    days_row: adw::ActionRow,
    days: Vec<(Weekday, gtk::ToggleButton)>,
    monthly: adw::ComboRow,
}

const UNITS: [Freq; 4] = [Freq::Daily, Freq::Weekly, Freq::Monthly, Freq::Yearly];

impl Custom {
    fn new(rule: Option<&Rule>, day: NaiveDate) -> Custom {
        let every = adw::SpinRow::with_range(1.0, 99.0, 1.0);
        every.set_title(&i18n("Every"));
        let unit = adw::ComboRow::new();
        unit.set_title(&i18n("Unit"));
        unit.set_model(Some(&gtk::StringList::new(&[
            &i18n("Days"),
            &i18n("Weeks"),
            &i18n("Months"),
            &i18n("Years"),
        ])));
        let days_row = adw::ActionRow::new();
        days_row.set_title(&i18n("On"));
        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        strip.add_css_class("linked");
        strip.set_valign(gtk::Align::Center);
        let start = crate::calendar::prefs::system_week_start();
        let mut days = Vec::new();
        for i in 0..7 {
            let wd = weekday_from(start.num_days_from_monday() + i);
            let monday = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap_or_default();
            let sample = monday + Duration::days(i64::from(wd.num_days_from_monday()));
            let label: String = super::ctx::fmt(sample, "%a").chars().take(2).collect();
            let b = gtk::ToggleButton::with_label(&label);
            b.add_css_class("calendar-weekday-toggle");
            strip.append(&b);
            days.push((wd, b));
        }
        days_row.add_suffix(&strip);
        let nth = (day.day() as i32 - 1) / 7 + 1;
        let last = day.day() + 7 > crate::calendar::time::days_in_month(day.year(), day.month());
        let wname = super::ctx::fmt(day, "%A");
        let monthly = adw::ComboRow::new();
        monthly.set_title(&i18n("On"));
        let by_date = i18n_f("Day {n}", &[("n", &day.day().to_string())]);
        let by_nth = i18n_f(
            "The {nth} {day}",
            &[("nth", &ordinal_word(nth)), ("day", &wname)],
        );
        let by_last = i18n_f("The last {day}", &[("day", &wname)]);
        let mut options = vec![by_date, by_nth];
        if last {
            options.push(by_last);
        }
        let opts: Vec<&str> = options.iter().map(String::as_str).collect();
        monthly.set_model(Some(&gtk::StringList::new(&opts)));
        let c = Custom {
            every,
            unit,
            days_row,
            days,
            monthly,
        };
        let r = rule.cloned().unwrap_or_else(|| Rule::new(Freq::Weekly));
        c.every.set_value(f64::from(r.interval.max(1)));
        c.unit
            .set_selected(UNITS.iter().position(|u| *u == r.freq).unwrap_or(1) as u32);
        let on: Vec<Weekday> = if r.by_day.is_empty() {
            vec![day.weekday()]
        } else {
            r.by_day.iter().map(|(_, d)| *d).collect()
        };
        for (wd, b) in &c.days {
            b.set_active(on.contains(wd));
        }
        if r.freq == Freq::Monthly {
            match r.by_day.first() {
                Some((n, _)) if *n < 0 && last => c.monthly.set_selected(2),
                Some((n, _)) if *n != 0 => c.monthly.set_selected(1),
                _ => c.monthly.set_selected(0),
            }
        }
        let me = c.clone();
        c.unit.connect_selected_notify(move |_| me.show(true));
        c
    }

    fn rows(&self) -> [&gtk::Widget; 4] {
        [
            self.every.upcast_ref(),
            self.unit.upcast_ref(),
            self.days_row.upcast_ref(),
            self.monthly.upcast_ref(),
        ]
    }

    fn show(&self, on: bool) {
        let unit = UNITS[self.unit.selected() as usize % 4];
        self.every.set_visible(on);
        self.unit.set_visible(on);
        self.days_row.set_visible(on && unit == Freq::Weekly);
        self.monthly.set_visible(on && unit == Freq::Monthly);
    }

    fn connect_changed(&self, f: impl Fn() + 'static) {
        let f = Rc::new(f);
        let g = f.clone();
        self.every.connect_value_notify(move |_| g());
        let g = f.clone();
        self.unit.connect_selected_notify(move |_| g());
        let g = f.clone();
        self.monthly.connect_selected_notify(move |_| g());
        for (_, b) in &self.days {
            let g = f.clone();
            b.connect_toggled(move |_| g());
        }
    }

    fn rule(&self, day: NaiveDate) -> Rule {
        let unit = UNITS[self.unit.selected() as usize % 4];
        let mut r = Rule::new(unit);
        r.interval = self.every.value().max(1.0) as u32;
        match unit {
            Freq::Weekly => {
                r.by_day = self
                    .days
                    .iter()
                    .filter(|(_, b)| b.is_active())
                    .map(|(d, _)| (0, *d))
                    .collect();
                if r.by_day.is_empty() {
                    r.by_day = vec![(0, day.weekday())];
                }
            }
            Freq::Monthly => match self.monthly.selected() {
                1 => r.by_day = vec![((day.day() as i32 - 1) / 7 + 1, day.weekday())],
                2 => r.by_day = vec![(-1, day.weekday())],
                _ => {}
            },
            _ => {}
        }
        r
    }
}

fn weekday_from(n: u32) -> Weekday {
    match n % 7 {
        0 => Weekday::Mon,
        1 => Weekday::Tue,
        2 => Weekday::Wed,
        3 => Weekday::Thu,
        4 => Weekday::Fri,
        5 => Weekday::Sat,
        _ => Weekday::Sun,
    }
}

fn ordinal_word(n: i32) -> String {
    match n {
        1 => i18n("first"),
        2 => i18n("second"),
        3 => i18n("third"),
        4 => i18n("fourth"),
        _ => i18n("fifth"),
    }
}
