//! The demo's calendars (HYLKI_DEMO): a made-up few weeks around today, kept
//! in memory so edits in a screenshot session stick until it ends.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{Datelike, Duration, Local, NaiveDate, Weekday};

use super::edit::Change;
use super::files::apply_to;
use super::model::Book;
use super::store::{CalInfo, CalKind, Loaded};
use super::time::Zones;
use super::tree::Node;

struct Demo {
    calendars: Vec<CalInfo>,
    nodes: HashMap<String, Vec<Node>>,
}

static DEMO: Mutex<Option<Demo>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Demo) -> T) -> T {
    let mut g = DEMO.lock().unwrap_or_else(|e| e.into_inner());
    let demo = g.get_or_insert_with(seed);
    f(demo)
}

fn cal(uid: &str, name: &str, account: Option<&str>, color: &str, kind: CalKind) -> CalInfo {
    CalInfo {
        uid: uid.into(),
        name: name.into(),
        account: account.map(Into::into),
        color: color.into(),
        writable: matches!(kind, CalKind::Local | CalKind::Remote),
        tasks: false,
        kind,
        parent: account.unwrap_or("local").into(),
    }
}

fn seed() -> Demo {
    let calendars = vec![
        cal("personal", "Personal", None, "#3584e4", CalKind::Local),
        cal("work", "Work", Some("Hyprlab"), "#e66100", CalKind::Remote),
        cal("family", "Family", Some("Nextcloud"), "#2ec27e", CalKind::Remote),
        cal("holidays", "Holidays", None, "#c01c28", CalKind::Subscription),
        cal("birthdays", "Birthdays", None, "#9141ac", CalKind::Birthdays),
    ];
    let today = Local::now().date_naive();
    let monday = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    let d = |offset: i64| monday + Duration::days(offset);
    let f = |day: NaiveDate| day.format("%Y%m%d").to_string();
    let mut n = 0;
    let mut ev = |summary: &str, body: String| -> Node {
        n += 1;
        Node::parse(&format!(
            "BEGIN:VEVENT\nUID:demo-{n}\nSUMMARY:{summary}\n{body}END:VEVENT\n"
        ))
        .unwrap_or_default()
    };
    let timed = |day: NaiveDate, from: &str, to: &str| {
        format!("DTSTART:{}T{from}00\nDTEND:{}T{to}00\n", f(day), f(day))
    };
    let mut nodes: HashMap<String, Vec<Node>> = HashMap::new();
    nodes.insert(
        "work".into(),
        vec![
            ev(
                "Standup",
                format!(
                    "{}RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR\nLOCATION:Video call\n",
                    timed(d(-14), "0930", "0945")
                ),
            ),
            ev(
                "Design review",
                format!(
                    "{}LOCATION:Room 2\nDESCRIPTION:Sidebar footer and the calendar views.\n\
                     ORGANIZER;CN=Ada Lovelace:mailto:ada@example.org\n\
                     ATTENDEE;CN=Ada Lovelace;PARTSTAT=ACCEPTED:mailto:ada@example.org\n\
                     ATTENDEE;CN=Sam Okafor;PARTSTAT=TENTATIVE:mailto:sam@example.org\n\
                     ATTENDEE;CN=You;PARTSTAT=ACCEPTED:mailto:you@example.org\n",
                    timed(d(1), "1400", "1530")
                ),
            ),
            ev(
                "1:1 with Sam",
                format!(
                    "{}RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=TH\nURL:https://meet.example.org/sam\n",
                    timed(d(3), "1100", "1130")
                ),
            ),
            ev("Release planning", timed(d(2), "1000", "1200")),
            ev("Customer call", timed(d(2), "1100", "1145")),
            ev(
                "Team offsite",
                format!(
                    "DTSTART;VALUE=DATE:{}\nDTEND;VALUE=DATE:{}\nLOCATION:Lakeside lodge\n",
                    f(d(8)),
                    f(d(11))
                ),
            ),
            ev("Write the quarterly notes", timed(d(4), "1500", "1700")),
            ev("Interview", timed(d(9), "1300", "1400")),
        ],
    );
    nodes.insert(
        "personal".into(),
        vec![
            ev(
                "Gym",
                format!(
                    "{}RRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR\nTRANSP:TRANSPARENT\n",
                    timed(d(-21), "0700", "0800")
                ),
            ),
            ev(
                "Lunch with Grace",
                format!("{}LOCATION:Corner Café\n", timed(today, "1230", "1330")),
            ),
            ev("Dentist", timed(d(4), "0830", "0900")),
            ev(
                "Flight to Lisbon",
                format!("{}LOCATION:Gate 14\n", timed(d(12), "0610", "0855")),
            ),
            ev("Book club", timed(d(1), "1900", "2100")),
        ],
    );
    nodes.insert(
        "family".into(),
        vec![
            ev(
                "School concert",
                format!("{}LOCATION:Town hall\n", timed(d(3), "1800", "1930")),
            ),
            ev(
                "Grandma visiting",
                format!(
                    "DTSTART;VALUE=DATE:{}\nDTEND;VALUE=DATE:{}\n",
                    f(d(5)),
                    f(d(7))
                ),
            ),
            ev(
                "Bins out",
                format!(
                    "DTSTART;VALUE=DATE:{}\nRRULE:FREQ=WEEKLY;BYDAY=TU\n",
                    f(d(-6))
                ),
            ),
        ],
    );
    let holiday = |month: u32, day: u32| {
        let y = today.year();
        format!(
            "DTSTART;VALUE=DATE:{y}{month:02}{day:02}\nRRULE:FREQ=YEARLY\nTRANSP:TRANSPARENT\n"
        )
    };
    nodes.insert(
        "holidays".into(),
        vec![
            ev("New Year's Day", holiday(1, 1)),
            ev("Christmas Day", holiday(12, 25)),
            ev(
                "Thanksgiving",
                format!(
                    "DTSTART;VALUE=DATE:{}1126\nRRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=4TH\nTRANSP:TRANSPARENT\n",
                    today.year()
                ),
            ),
        ],
    );
    let born = |day: NaiveDate| format!(
        "DTSTART;VALUE=DATE:1990{}\nRRULE:FREQ=YEARLY\nTRANSP:TRANSPARENT\n",
        day.format("%m%d")
    );
    let tomorrow = today + Duration::days(1);
    let next = if tomorrow.weekday() == Weekday::Sun {
        tomorrow + Duration::days(1)
    } else {
        tomorrow
    };
    nodes.insert(
        "birthdays".into(),
        vec![
            ev("Ada Lovelace", born(next)),
            ev("Sam Okafor", born(d(16))),
        ],
    );
    Demo { calendars, nodes }
}

pub(crate) fn calendars() -> Vec<CalInfo> {
    with(|d| d.calendars.clone())
}

pub(crate) fn load(cal: &CalInfo) -> Loaded {
    let texts: Vec<String> = with(|d| {
        d.nodes
            .get(&cal.uid)
            .map(|v| v.iter().map(Node::write).collect())
            .unwrap_or_default()
    });
    let mut zones = Zones::default();
    let book = Book::read(&texts, &mut zones);
    Loaded {
        book,
        zones,
        writable: Some(cal.writable),
        watch: None,
    }
}

pub(crate) fn apply(cal: &CalInfo, changes: &[Change]) -> Result<(), String> {
    with(|d| apply_to(d.nodes.entry(cal.uid.clone()).or_default(), changes));
    Ok(())
}

pub(crate) fn new_calendar(name: &str, color: &str) -> String {
    let uid = super::edit::new_uid();
    with(|d| {
        d.calendars
            .insert(1, cal(&uid, name, None, color, CalKind::Local));
    });
    uid
}

pub(crate) fn set_look(cal: &CalInfo, name: &str, color: &str) {
    with(|d| {
        if let Some(c) = d.calendars.iter_mut().find(|c| c.uid == cal.uid) {
            if !name.trim().is_empty() {
                c.name = name.trim().into();
            }
            c.color = color.into();
        }
    });
}

pub(crate) fn delete(cal: &CalInfo) {
    with(|d| {
        d.calendars.retain(|c| c.uid != cal.uid);
        d.nodes.remove(&cal.uid);
    });
}
