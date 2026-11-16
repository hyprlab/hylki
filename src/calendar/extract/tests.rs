use super::*;

/// Monday, 16 November 2026.
fn monday() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 16).unwrap()
}

fn d(y: i32, m: u32, day: u32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(y, m, day)
}

fn t(h: u32, m: u32) -> Option<NaiveTime> {
    NaiveTime::from_hms_opt(h, m, 0)
}

fn us(text: &str) -> Found {
    find(text, &Words::english(false), monday())
}

fn uk(text: &str) -> Found {
    find(text, &Words::english(true), monday())
}

/// The date, the last day, the start and the end.
fn parts(
    f: &Found,
) -> (
    Option<NaiveDate>,
    Option<NaiveDate>,
    Option<NaiveTime>,
    Option<NaiveTime>,
) {
    (f.date, f.last, f.start, f.end)
}

#[test]
fn written_dates() {
    assert_eq!(us("November 20th, 2026").date, d(2026, 11, 20));
    assert_eq!(us("on Nov. 20").date, d(2026, 11, 20));
    assert_eq!(us("20 November").date, d(2026, 11, 20));
    assert_eq!(us("the 20th of November").date, d(2026, 11, 20));
    assert_eq!(us("Thursday, 20 November 2027").date, d(2027, 11, 20));
    // Passed this year: next year's.
    assert_eq!(us("March 3").date, d(2027, 3, 3));
    assert_eq!(us("on 2 Jan").date, d(2027, 1, 2));
}

#[test]
fn ranges_of_days() {
    let f = us("Conference Nov 20-22");
    assert_eq!((f.date, f.last), (d(2026, 11, 20), d(2026, 11, 22)));
    let f = us("20–22 November 2026");
    assert_eq!((f.date, f.last), (d(2026, 11, 20), d(2026, 11, 22)));
}

#[test]
fn numeric_dates_follow_the_order() {
    assert_eq!(us("2026-12-03").date, d(2026, 12, 3));
    assert_eq!(us("12/03/2026").date, d(2026, 12, 3));
    assert_eq!(uk("12/03/2026").date, d(2026, 3, 12));
    // A number past 12 decides it whatever the order.
    assert_eq!(us("25/12").date, d(2026, 12, 25));
    assert_eq!(uk("12/25/26").date, d(2026, 12, 25));
    // Dotted dates are day first.
    assert_eq!(us("am 3.12. um 15 Uhr").date, d(2026, 12, 3));
}

#[test]
fn weekdays_and_relative_days() {
    assert_eq!(us("see you Friday").date, d(2026, 11, 20));
    assert_eq!(us("Monday").date, d(2026, 11, 16));
    assert_eq!(us("next Monday").date, d(2026, 11, 23));
    assert_eq!(us("Wed.").date, d(2026, 11, 18));
    assert_eq!(us("tomorrow").date, d(2026, 11, 17));
    assert_eq!(us("Today").date, d(2026, 11, 16));
}

#[test]
fn times() {
    assert_eq!(us("at 3pm").start, t(15, 0));
    assert_eq!(us("at 3:30 p.m.").start, t(15, 30));
    assert_eq!(us("15:45").start, t(15, 45));
    assert_eq!(us("12am").start, t(0, 0));
    assert_eq!(us("12 PM").start, t(12, 0));
    assert_eq!(us("at noon").start, t(12, 0));
    assert_eq!(us("à 14h30").start, t(14, 30));
    assert_eq!(us("um 15 Uhr").start, t(15, 0));
    // A bare hour after "at" is a working one.
    assert_eq!(us("at 3").start, t(15, 0));
    assert_eq!(us("at 9").start, t(9, 0));
    // Numbers that are not times.
    assert_eq!(us("room 3").start, None);
    assert_eq!(us("version 3.30").start, None);
    assert_eq!(us("call 555-1234").start, None);
}

#[test]
fn ranges_of_times() {
    let f = us("3-4pm");
    assert_eq!((f.start, f.end), (t(15, 0), t(16, 0)));
    let f = us("11-1pm");
    assert_eq!((f.start, f.end), (t(11, 0), t(13, 0)));
    let f = us("10:30am to 12");
    assert_eq!((f.start, f.end), (t(10, 30), t(12, 0)));
    let f = us("from 14:00 – 15:30");
    assert_eq!((f.start, f.end), (t(14, 0), t(15, 30)));
    let f = us("between 2 and 4");
    assert_eq!((f.start, f.end), (t(14, 0), t(16, 0)));
}

#[test]
fn a_whole_sentence() {
    let f = us("Can we meet on Thursday, November 19 from 2:00 to 3:30 PM in room 4?");
    assert_eq!(parts(&f), (d(2026, 11, 19), None, t(14, 0), t(15, 30)));
    let f = us("Dentist 3pm tomorrow");
    assert_eq!(parts(&f), (d(2026, 11, 17), None, t(15, 0), None));
    // An ISO date and time run together.
    let f = us("2026-11-20T09:15");
    assert_eq!(parts(&f), (d(2026, 11, 20), None, t(9, 15), None));
}

#[test]
fn nothing_found() {
    assert_eq!(
        us("Thanks for the update, see the attached file."),
        Found::default()
    );
    assert_eq!(us("Order 20/40 shipped").date, None);
}

#[test]
fn titles_leave_the_date_out() {
    let title_of = |s: &str| title(s, &us(s).spans);
    assert_eq!(
        title_of("Lunch with Anna on Friday at 1pm.").as_deref(),
        Some("Lunch with Anna")
    );
    assert_eq!(
        title_of("Dinner on Friday at 7pm at Luigi's").as_deref(),
        Some("Dinner at Luigi's")
    );
    assert_eq!(
        title_of("  Team call, Nov 20, 10-11am ").as_deref(),
        Some("Team call")
    );
    assert_eq!(title_of("Friday at 3pm"), None);
    assert_eq!(title_of("Line one\nline two on Friday"), None);
}
