//! Making an event quickly: the popover a drag or a double click opens
//! (a title and Enter, as in GNOME Calendar), and quick add, which reads a
//! sentence ("Lunch with Sam tomorrow at 12:30") into an event.

use std::rc::Rc;

use adw::prelude::*;
use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime};

use super::ctx::Anchor;
use crate::calendar::edit::Span;
use crate::calendar::store::CalInfo;
use crate::i18n::i18n;

/// What the new-event popover was left with.
#[derive(Debug)]
pub(crate) enum NewEvent {
    /// Make it, with this title, in this calendar.
    Add { title: String, calendar: String },
    /// Open the editor with what was typed.
    More { title: String, calendar: String },
}

/// The popover for a new event over `span`.
pub(crate) fn new_event(
    anchor: &Anchor,
    when: &str,
    calendars: &[CalInfo],
    calendar: &str,
    done: impl Fn(NewEvent) + 'static,
) -> gtk::Popover {
    let pop = gtk::Popover::new();
    let bx = gtk::Box::new(gtk::Orientation::Vertical, 10);
    bx.set_width_request(300);
    let heading = gtk::Label::new(Some(&i18n("New Event")));
    heading.add_css_class("heading");
    heading.set_xalign(0.0);
    bx.append(&heading);
    let span = gtk::Label::new(Some(when));
    span.add_css_class("dim-label");
    span.set_xalign(0.0);
    span.set_wrap(true);
    bx.append(&span);
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(&i18n("Title")));
    entry.set_activates_default(true);
    bx.append(&entry);
    let names: Vec<String> = calendars.iter().map(CalInfo::full_name).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let pick = gtk::DropDown::from_strings(&names);
    if let Some(i) = calendars.iter().position(|c| c.uid == calendar) {
        pick.set_selected(i as u32);
    }
    pick.set_visible(calendars.len() > 1);
    bx.append(&pick);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let more = gtk::Button::with_label(&i18n("More Details…"));
    more.add_css_class("flat");
    let add = gtk::Button::with_label(&i18n("Add"));
    add.add_css_class("suggested-action");
    add.set_sensitive(false);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    buttons.append(&more);
    buttons.append(&spacer);
    buttons.append(&add);
    bx.append(&buttons);
    pop.set_child(Some(&bx));
    pop.set_default_widget(Some(&add));
    let a = add.clone();
    entry.connect_changed(move |e| a.set_sensitive(!e.text().trim().is_empty()));
    let uids: Rc<Vec<String>> = Rc::new(calendars.iter().map(|c| c.uid.clone()).collect());
    let done = Rc::new(done);
    let chosen = {
        let (pick, uids) = (pick.clone(), uids.clone());
        move || uids.get(pick.selected() as usize).cloned().unwrap_or_default()
    };
    let chosen = Rc::new(chosen);
    {
        let (e, p, d, c) = (entry.clone(), pop.clone(), done.clone(), chosen.clone());
        add.connect_clicked(move |_| {
            let title = e.text().trim().to_string();
            if title.is_empty() {
                return;
            }
            p.popdown();
            d(NewEvent::Add {
                title,
                calendar: c(),
            });
        });
    }
    {
        let (e, p, d, c) = (entry.clone(), pop.clone(), done.clone(), chosen.clone());
        more.connect_clicked(move |_| {
            p.popdown();
            d(NewEvent::More {
                title: e.text().trim().to_string(),
                calendar: c(),
            });
        });
    }
    pop.set_parent(&anchor.widget);
    pop.set_pointing_to(Some(&anchor.rect));
    pop.set_position(gtk::PositionType::Right);
    pop.connect_closed(|p| {
        let p = p.clone();
        gtk::glib::idle_add_local_once(move || {
            if p.parent().is_some() {
                p.unparent();
            }
        });
    });
    pop.popup();
    entry.grab_focus();
    pop
}

/// A sentence read into an event: its title and when. A day with no time
/// is an all-day event; a time with no day is today, or tomorrow once the
/// time has passed. `minutes` is the length of an event with no end.
pub(crate) fn parse(text: &str, today: NaiveDate, now: NaiveTime, minutes: u32) -> Option<(String, Span)> {
    let found = crate::calendar::find(text, &crate::calendar::Words::system(), today);
    let title = crate::calendar::title(text, &found.spans)
        .unwrap_or_else(|| text.trim().to_string());
    if title.trim().is_empty() {
        return None;
    }
    let span = match (found.date, found.start) {
        (day, Some(start)) => {
            let day = day.unwrap_or(if start < now { today + Duration::days(1) } else { today });
            let s = NaiveDateTime::new(day, start);
            let e = match found.end {
                Some(end) if end > start => NaiveDateTime::new(day, end),
                Some(end) => NaiveDateTime::new(day + Duration::days(1), end),
                None => s + Duration::minutes(i64::from(minutes)),
            };
            Span::Timed {
                start: s,
                end: e,
                tzid: None,
            }
        }
        (Some(day), None) => Span::AllDay {
            first: day,
            last: found.last.unwrap_or(day).max(day),
        },
        (None, None) => return None,
    };
    Some((title, span))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sentence_becomes_an_event() {
        // Friday 27 November 2026, 10:00.
        let today = NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
        let now = NaiveTime::from_hms_opt(10, 0, 0).unwrap();
        let (title, span) = parse("Lunch with Sam tomorrow at 12:30", today, now, 60).unwrap();
        assert_eq!(title, "Lunch with Sam");
        let Span::Timed { start, end, .. } = span else {
            panic!("{span:?}")
        };
        assert_eq!(start.to_string(), "2026-11-28 12:30:00");
        assert_eq!(end.to_string(), "2026-11-28 13:30:00");
        // A time already past today is tomorrow's.
        let (_, span) = parse("Call Ada at 9:00", today, now, 30).unwrap();
        let Span::Timed { start, .. } = span else { panic!() };
        assert_eq!(start.date(), today + Duration::days(1));
        assert!(parse("just words", today, now, 60).is_none());
    }
}
