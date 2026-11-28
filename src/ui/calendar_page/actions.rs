//! What the page does with a click, a drag, a menu or a button: selecting
//! and showing an event, making, moving, editing and deleting events (and
//! asking which occurrences a change to a repeating one covers), answering
//! invitations, and the calendars' own menus.

use std::sync::Arc;

use adw::prelude::*;
use chrono::{Duration, Timelike};
use relm4::prelude::*;

use super::ctx::{Act, Anchor};
use super::data::Undo;
use super::editor::{self, Outcome};
use super::inspect::{self, Ask};
use super::quick::{self, NewEvent};
use super::side::SideAct;
use super::{find_occ, CalendarInput, CalendarOutput, CalendarPage};
use crate::calendar::edit::{self, Change, Draft, Scope, Span};
use crate::calendar::model::{Comp, Occ, OccKey};
use crate::calendar::prefs::View;
use crate::calendar::store::{self, CalInfo};
use crate::calendar::time::{local_unix, local_wall};
use crate::i18n::{i18n, i18n_f};

impl CalendarPage {
    pub(super) fn on_act(&mut self, act: Act, sender: &ComponentSender<Self>) {
        match act {
            Act::Select(key, anchor) => {
                self.close_popover();
                self.ctx.borrow_mut().selected = key.clone();
                self.redraw();
                if let (Some(key), Some(anchor)) = (key, anchor) {
                    self.inspect(&key, &anchor, sender);
                }
            }
            Act::Open(key) => {
                self.close_popover();
                self.ctx.borrow_mut().selected = Some(key.clone());
                self.redraw();
                self.edit(&key, sender);
            }
            Act::Create {
                start,
                end,
                all_day,
                anchor,
            } => self.create_at(start, end, all_day, &anchor, sender),
            Act::Move { key, start, end } => self.move_to(&key, start, end, sender),
            Act::Show(day, view) => {
                self.anchor = day;
                if let Some(v) = view {
                    sender.input(CalendarInput::View(v));
                }
                self.refresh();
            }
            Act::DayList(day, anchor) => {
                let shown = self.shown.clone();
                super::agenda::day_popover(&self.ctx, day, &shown, &anchor);
            }
            Act::Menu {
                key,
                at,
                all_day,
                anchor,
            } => self.menu(key, at, all_day, &anchor, sender),
        }
    }

    fn close_popover(&mut self) {
        if let Some(p) = self.popover.take() {
            p.popdown();
        }
    }

    fn redraw(&self) {
        self.parts.grid.redraw();
        self.parts.month.redraw();
        self.parts.year.redraw();
    }

    fn cal(&self, uid: &str) -> Option<&CalInfo> {
        self.calendars.iter().find(|c| c.uid == uid)
    }

    /// The occurrence `key` names, from the screen, else from its calendar.
    fn occ(&mut self, key: &OccKey) -> Option<Occ> {
        if let Some(o) = find_occ(&self.shown, key) {
            return Some(o.clone());
        }
        let l = self.loaded.get_mut(&key.calendar)?;
        let comp = l.book.comp(&key.uid, &key.rid)?;
        let start = comp.anchor().map(|s| l.zones.unix(s))?;
        l.book
            .occurrences(&key.calendar, &mut l.zones, start - 86_400, start + 86_400 * 2)
            .into_iter()
            .find(|o| o.key == *key)
    }

    fn master(&self, key: &OccKey) -> Option<Arc<Comp>> {
        self.loaded.get(&key.calendar)?.book.comp(&key.uid, "")
    }

    /// The calendars a new event can go to, in the side pane's order.
    fn writable(&self) -> Vec<CalInfo> {
        let ctx = self.ctx.borrow();
        self.calendars
            .iter()
            .filter(|c| ctx.writable.contains(&c.uid))
            .cloned()
            .collect()
    }

    /// Where a new event goes: the chosen default, if it still takes
    /// events, else the first shown calendar that does.
    fn default_calendar(&self) -> Option<String> {
        let w = self.writable();
        w.iter()
            .find(|c| c.uid == self.prefs.default_calendar)
            .or_else(|| w.iter().find(|c| !self.hidden.contains(&c.uid)))
            .or(w.first())
            .map(|c| c.uid.clone())
    }

    fn inspect(&mut self, key: &OccKey, anchor: &Anchor, sender: &ComponentSender<Self>) {
        let Some(occ) = self.occ(key) else {
            return;
        };
        let master = self.master(key);
        let cal = self.cal(&key.calendar).cloned();
        let s = sender.input_sender().clone();
        let pop = inspect::show(&self.ctx, &occ, master.as_deref(), cal.as_ref(), anchor, move |ask| {
            let _ = s.send(CalendarInput::Ask(ask));
        });
        self.popover = Some(pop);
    }

    pub(super) fn on_ask(&mut self, ask: Ask, sender: &ComponentSender<Self>) {
        let Some(key) = self.ctx.borrow().selected.clone() else {
            return;
        };
        match ask {
            Ask::Edit => self.edit(&key, sender),
            Ask::Delete => self.delete(&key, sender),
            Ask::Duplicate => self.duplicate(&key, sender),
            Ask::Answer(status) => self.answer(&key, &status, sender),
            Ask::Email => {
                if let Some(occ) = self.occ(&key) {
                    let me = self.ctx.borrow().me.clone();
                    let to: Vec<String> = occ
                        .comp
                        .attendees
                        .iter()
                        .filter(|a| !a.resource && !me.iter().any(|m| m.eq_ignore_ascii_case(&a.email)))
                        .map(|a| {
                            if a.name.is_empty() {
                                a.email.clone()
                            } else {
                                format!("{} <{}>", a.name, a.email)
                            }
                        })
                        .collect();
                    let _ = sender.output(CalendarOutput::Compose {
                        to,
                        subject: occ.comp.summary.clone(),
                    });
                }
            }
        }
    }

    /// A new event over a span: the quick popover, which can open the
    /// editor.
    fn create_at(&mut self, start: i64, end: i64, all_day: bool, anchor: &Anchor, sender: &ComponentSender<Self>) {
        self.close_popover();
        let Some(calendar) = self.default_calendar() else {
            self.toast(&i18n("No calendar here takes new events."));
            return;
        };
        let span = if all_day {
            Span::AllDay {
                first: crate::calendar::time::local_day(start),
                last: crate::calendar::time::local_day((end - 1).max(start)),
            }
        } else {
            Span::Timed {
                start: local_wall(start),
                end: local_wall(end),
                tzid: None,
            }
        };
        let when = super::inspect::when_text(&self.preview(&span));
        let calendars = self.writable();
        let s = sender.input_sender().clone();
        let span2 = span.clone();
        let pop = quick::new_event(anchor, &when, &calendars, &calendar, move |r| {
            let _ = s.send(CalendarInput::NewEventDone(r, span2.clone()));
        });
        self.popover = Some(pop);
    }

    /// An occurrence standing in for a span not yet an event, to word it.
    fn preview(&self, span: &Span) -> Occ {
        let draft = Draft::new(span.clone());
        let node = edit::create(&draft);
        let comp = Arc::new(Comp::read(&node).unwrap_or_else(|| Comp::read(&crate::calendar::tree::Node::parse("BEGIN:VEVENT\nUID:x\nEND:VEVENT\n").unwrap_or_default()).expect("a bare event reads")));
        let (start, end, all_day, first, last) = match span {
            Span::AllDay { first, last } => (
                crate::calendar::time::day_start(*first),
                crate::calendar::time::day_start(*last + Duration::days(1)),
                true,
                *first,
                *last,
            ),
            Span::Timed { start, end, .. } => (local_unix(*start), local_unix(*end), false, start.date(), end.date()),
        };
        Occ {
            key: OccKey {
                calendar: String::new(),
                uid: String::new(),
                rid: String::new(),
            },
            comp,
            start,
            end,
            all_day,
            first_day: first,
            last_day: last,
            in_series: false,
            rid: None,
        }
    }

    /// What the new-event popover was left with.
    pub(super) fn new_event_done(&mut self, r: NewEvent, span: Span, sender: &ComponentSender<Self>) {
        match r {
            NewEvent::Add { title, calendar } => {
                let mut d = self.new_draft(span);
                d.summary = title;
                self.save_new(d, &calendar, sender);
            }
            NewEvent::More { title, calendar } => {
                let mut d = self.new_draft(span);
                d.summary = title;
                self.open_editor(d, &calendar, None, sender);
            }
        }
    }

    fn new_draft(&self, span: Span) -> Draft {
        let mut d = Draft::new(span);
        if let (Some(m), false) = (self.prefs.default_alarm, matches!(d.span, Span::AllDay { .. })) {
            d.alarms = vec![i64::from(m) * 60];
        }
        d
    }

    fn save_new(&mut self, draft: Draft, calendar: &str, sender: &ComponentSender<Self>) {
        let node = edit::create(&draft);
        let uid = node.text("UID");
        let attendees = !draft.attendees.is_empty();
        self.write(
            calendar,
            vec![Change::Create(node.clone())],
            Some(Undo {
                calendar: calendar.to_string(),
                changes: vec![Change::Remove { uid }],
            }),
            sender,
        );
        let text = i18n_f("Added “{title}”", &[("title", &draft.summary)]);
        self.done_toast(&text, sender);
        if attendees {
            self.send_invites(&node, &draft, false, sender);
        }
    }

    /// A new event from the header's button: the next hour, today or on
    /// the day in view.
    pub(super) fn new_event_now(&mut self, sender: &ComponentSender<Self>) {
        let Some(calendar) = self.default_calendar() else {
            self.toast(&i18n("No calendar here takes new events."));
            return;
        };
        let now = local_wall(crate::datefmt::now());
        let day = if self.view == View::Day || self.view == View::Week && self.anchor != now.date() {
            self.anchor
        } else {
            now.date()
        };
        let hour = if day == now.date() { (now.hour() + 1).min(23) } else { self.prefs.work_start.clamp(9, 23) };
        let start = day.and_hms_opt(hour, 0, 0).unwrap_or(now);
        let end = start + Duration::minutes(i64::from(self.prefs.default_minutes));
        let draft = self.new_draft(Span::Timed {
            start,
            end,
            tzid: None,
        });
        self.open_editor(draft, &calendar, None, sender);
    }

    pub(super) fn quick_add(&mut self, text: &str, sender: &ComponentSender<Self>) {
        let now = local_wall(crate::datefmt::now());
        let Some((title, span)) = quick::parse(text, now.date(), now.time(), self.prefs.default_minutes) else {
            // No date or time in it: a new event with that title, in the
            // editor, to say when.
            let mut d = self.new_draft(Span::Timed {
                start: now.date().and_hms_opt((now.hour() + 1).min(23), 0, 0).unwrap_or(now),
                end: now.date().and_hms_opt((now.hour() + 1).min(23), 0, 0).unwrap_or(now)
                    + Duration::minutes(i64::from(self.prefs.default_minutes)),
                tzid: None,
            });
            d.summary = text.trim().to_string();
            if let Some(c) = self.default_calendar() {
                self.open_editor(d, &c, None, sender);
            }
            return;
        };
        let Some(calendar) = self.default_calendar() else {
            self.toast(&i18n("No calendar here takes new events."));
            return;
        };
        let mut d = self.new_draft(span.clone());
        d.summary = title;
        let day = match &span {
            Span::Timed { start, .. } => start.date(),
            Span::AllDay { first, .. } => *first,
        };
        self.anchor = day;
        self.save_new(d, &calendar, sender);
    }

    fn open_editor(&mut self, draft: Draft, calendar: &str, existing: Option<(Occ, Option<Arc<Comp>>)>, sender: &ComponentSender<Self>) {
        let writable = existing
            .as_ref()
            .is_none_or(|(o, _)| self.ctx.borrow().can_change(o));
        let mut calendars = self.writable();
        if !calendars.iter().any(|c| c.uid == calendar) {
            if let Some(c) = self.cal(calendar) {
                calendars.insert(0, c.clone());
            }
        }
        let s = sender.input_sender().clone();
        let parent = self.parts.views.clone();
        let original_cal = calendar.to_string();
        let is_existing = existing.is_some();
        editor::open(
            &parent,
            draft,
            &calendars,
            calendar,
            is_existing,
            writable,
            self.organizer.clone(),
            move |outcome| {
                let _ = s.send(CalendarInput::EditorDone {
                    outcome,
                    was: original_cal.clone(),
                    existing: existing.clone(),
                });
            },
        );
    }

    /// What the editor was closed with: `was` is the calendar the event
    /// was in, `existing` the occurrence edited (none for a new event).
    pub(super) fn editor_done(
        &mut self,
        outcome: Outcome,
        was_cal: String,
        existing: Option<(Occ, Option<Arc<Comp>>)>,
        sender: &ComponentSender<Self>,
    ) {
        match (outcome, existing) {
            (Outcome::Save { draft, calendar }, None) => self.save_new(*draft, &calendar, sender),
            (Outcome::Save { draft, calendar }, Some((occ, master))) => {
                let draft = *draft;
                if calendar != was_cal {
                    self.move_calendar(&occ, &draft, &was_cal, &calendar, sender);
                    return;
                }
                let series = master.as_ref().is_some_and(|m| m.repeats());
                let s = sender.input_sender().clone();
                if series {
                    self.ask_scope(true, move |scope| {
                        let _ = s.send(CalendarInput::ScopeDone(Scoped::Edit(
                            occ.clone(),
                            master.clone(),
                            Box::new(draft.clone()),
                            scope,
                        )));
                    });
                } else {
                    self.apply_edit(&occ, master, &draft, Scope::All, sender);
                }
            }
            (Outcome::Delete, Some((occ, _))) => self.delete(&occ.key, sender),
            (Outcome::Delete, None) => {}
        }
    }

    /// Which occurrences a change covers, answered.
    pub(super) fn scope_done(&mut self, s: Scoped, sender: &ComponentSender<Self>) {
        match s {
            Scoped::Edit(occ, master, draft, scope) => self.apply_edit(&occ, master, &draft, scope, sender),
            Scoped::Remove(occ, master, scope) => self.apply_remove(&occ, master, scope, sender),
        }
    }

    fn apply_edit(&mut self, occ: &Occ, master: Option<Arc<Comp>>, draft: &Draft, scope: Scope, sender: &ComponentSender<Self>) {
        let cal = occ.key.calendar.clone();
        let Some(l) = self.loaded.get_mut(&cal) else {
            return;
        };
        let before = store::whole(&l.book, &occ.key.uid);
        let changes = edit::edit(occ, master.as_deref(), draft, scope, &mut l.zones);
        let mut undo = vec![Change::Remove { uid: occ.key.uid.clone() }];
        undo.extend(before.into_iter().map(Change::Create));
        // A split series made a new one, which undoing removes.
        for c in &changes {
            if let Change::Create(n) = c {
                undo.push(Change::Remove { uid: n.text("UID") });
            }
        }
        let organized = !draft.attendees.is_empty() && !occ.comp.invited(&self.ctx.borrow().me);
        let sent = changes.iter().find_map(|c| match c {
            Change::Replace(n) | Change::Create(n) => Some(n.clone()),
            _ => None,
        });
        self.write(&cal, changes, Some(Undo { calendar: cal.clone(), changes: undo }), sender);
        self.done_toast(&i18n("Event saved"), sender);
        if organized {
            if let Some(n) = sent {
                self.send_invites(&n, draft, false, sender);
            }
        }
    }

    /// An edit that also moves the event to another calendar: the whole
    /// series goes, its edit made, and leaves the first calendar.
    fn move_calendar(&mut self, occ: &Occ, draft: &Draft, from: &str, to: &str, sender: &ComponentSender<Self>) {
        let master = self.master(&occ.key);
        let Some(l) = self.loaded.get_mut(from) else {
            return;
        };
        let before = store::whole(&l.book, &occ.key.uid);
        let changed = edit::edit(occ, master.as_deref(), draft, Scope::All, &mut l.zones);
        let mut nodes: Vec<crate::calendar::tree::Node> = before.clone();
        for c in changed {
            if let Change::Replace(n) | Change::Create(n) = c {
                let id = |x: &crate::calendar::tree::Node| (x.text("UID"), x.text("RECURRENCE-ID"));
                match nodes.iter().position(|x| id(x) == id(&n)) {
                    Some(i) => nodes[i] = n,
                    None => nodes.push(n),
                }
            }
        }
        let uid = occ.key.uid.clone();
        self.write(to, nodes.into_iter().map(Change::Create).collect(), None, sender);
        self.write(
            from,
            vec![Change::Remove { uid: uid.clone() }],
            Some(Undo {
                calendar: from.to_string(),
                changes: before.into_iter().map(Change::Create).collect(),
            }),
            sender,
        );
        let name = self.cal(to).map(CalInfo::full_name).unwrap_or_default();
        self.toast(&i18n_f("Moved to {calendar}", &[("calendar", &name)]));
    }

    fn edit(&mut self, key: &OccKey, sender: &ComponentSender<Self>) {
        let Some(occ) = self.occ(key) else {
            return;
        };
        let master = self.master(key);
        let Some(l) = self.loaded.get_mut(&key.calendar) else {
            return;
        };
        let draft = Draft::from_occ(&occ, master.as_deref(), &mut l.zones);
        let cal = key.calendar.clone();
        self.open_editor(draft, &cal, Some((occ, master)), sender);
    }

    fn move_to(&mut self, key: &OccKey, start: i64, end: i64, sender: &ComponentSender<Self>) {
        let Some(occ) = self.occ(key) else {
            return;
        };
        let master = self.master(key);
        let Some(l) = self.loaded.get_mut(&key.calendar) else {
            return;
        };
        let draft = edit::moved_draft(&occ, master.as_deref(), start, end, &mut l.zones);
        if master.as_ref().is_some_and(|m| m.repeats()) {
            let s = sender.input_sender().clone();
            let o = occ.clone();
            self.ask_scope(true, move |scope| {
                let _ = s.send(CalendarInput::ScopeDone(Scoped::Edit(
                    o.clone(),
                    master.clone(),
                    Box::new(draft.clone()),
                    scope,
                )));
            });
            // Back where it was until the answer.
            self.refresh();
        } else {
            self.apply_edit(&occ, master, &draft, Scope::All, sender);
        }
    }

    fn delete(&mut self, key: &OccKey, sender: &ComponentSender<Self>) {
        let Some(occ) = self.occ(key) else {
            return;
        };
        if !self.ctx.borrow().can_change(&occ) {
            self.toast(&i18n("This calendar is read-only."));
            return;
        }
        self.close_popover();
        let master = self.master(key);
        if master.as_ref().is_some_and(|m| m.repeats()) {
            let s = sender.input_sender().clone();
            self.ask_scope(false, move |scope| {
                let _ = s.send(CalendarInput::ScopeDone(Scoped::Remove(
                    occ.clone(),
                    master.clone(),
                    scope,
                )));
            });
        } else {
            self.apply_remove(&occ, master, Scope::All, sender);
        }
    }

    fn apply_remove(&mut self, occ: &Occ, master: Option<Arc<Comp>>, scope: Scope, sender: &ComponentSender<Self>) {
        let cal = occ.key.calendar.clone();
        let Some(l) = self.loaded.get_mut(&cal) else {
            return;
        };
        let before = store::whole(&l.book, &occ.key.uid);
        let changes = edit::remove(occ, master.as_deref(), scope, &mut l.zones);
        let mut undo = vec![Change::Remove { uid: occ.key.uid.clone() }];
        undo.extend(before.iter().cloned().map(Change::Create));
        let me = self.ctx.borrow().me.clone();
        let organized = !occ.comp.attendees.is_empty() && !occ.comp.invited(&me) && scope == Scope::All;
        self.ctx.borrow_mut().selected = None;
        self.write(&cal, changes, Some(Undo { calendar: cal.clone(), changes: undo }), sender);
        let title = if occ.comp.summary.is_empty() { i18n("Untitled") } else { occ.comp.summary.clone() };
        self.done_toast(&i18n_f("Deleted “{title}”", &[("title", &title)]), sender);
        if organized {
            if let Some(n) = before.first() {
                let d = Draft::from_occ(occ, master.as_deref(), &mut crate::calendar::time::Zones::default());
                self.send_invites(n, &d, true, sender);
            }
        }
    }

    fn duplicate(&mut self, key: &OccKey, sender: &ComponentSender<Self>) {
        let Some(occ) = self.occ(key) else {
            return;
        };
        let master = self.master(key);
        let Some(l) = self.loaded.get_mut(&key.calendar) else {
            return;
        };
        let mut d = Draft::from_occ(&occ, master.as_deref(), &mut l.zones);
        d.repeat = None;
        d.attendees.clear();
        d.organizer = None;
        let cal = if self.ctx.borrow().writable.contains(&key.calendar) {
            key.calendar.clone()
        } else {
            match self.default_calendar() {
                Some(c) => c,
                None => return,
            }
        };
        self.open_editor(d, &cal, None, sender);
    }

    /// The user's answer to an invitation: kept in their copy, and mailed
    /// to the organizer.
    fn answer(&mut self, key: &OccKey, status: &str, sender: &ComponentSender<Self>) {
        let Some(occ) = self.occ(key) else {
            return;
        };
        let master = self.master(key);
        let me = self.ctx.borrow().me.clone();
        let base = master.as_deref().unwrap_or(&occ.comp);
        let mut node = base.node.clone();
        let mut address = String::new();
        let lines: Vec<String> = node
            .lines
            .iter()
            .map(|l| {
                let Some(p) = crate::mail::invite::ics::split_line(l) else {
                    return l.clone();
                };
                if !p.name.eq_ignore_ascii_case("ATTENDEE") {
                    return l.clone();
                }
                let email = p.raw.trim().trim_start_matches("mailto:").trim_start_matches("MAILTO:");
                if !me.iter().any(|m| m.eq_ignore_ascii_case(email)) {
                    return l.clone();
                }
                address = email.to_string();
                edit::set_partstat(l, status)
            })
            .collect();
        node.lines = lines;
        node.replace(format!("DTSTAMP:{}", crate::calendar::time::utc_now_value()));
        let cal = key.calendar.clone();
        let uid = key.uid.clone();
        let before = self.loaded.get(&cal).map(|l| store::whole(&l.book, &uid)).unwrap_or_default();
        let mut undo = vec![Change::Remove { uid }];
        undo.extend(before.into_iter().map(Change::Create));
        self.write(&cal, vec![Change::Replace(node)], Some(Undo { calendar: cal.clone(), changes: undo }), sender);
        let c = &occ.comp;
        if let Some(org) = &c.organizer {
            let invite = crate::models::Invite {
                method: "REQUEST".into(),
                uid: c.uid.clone(),
                sequence: c.sequence,
                summary: c.summary.clone(),
                organizer: Some(org.clone()),
                ..Default::default()
            };
            let name = c
                .attendees
                .iter()
                .find(|a| a.email.eq_ignore_ascii_case(&address))
                .map(|a| a.name.clone())
                .unwrap_or_default();
            let ics = crate::mail::invite::reply_ics(&invite, &name, &address, status);
            let _ = sender.output(CalendarOutput::Answer {
                ics,
                organizer: org.email.clone(),
                summary: c.summary.clone(),
                status: status.to_string(),
                from: address,
            });
        }
    }

    /// Mail the attendees of an event the user organizes: the invitation,
    /// or with `cancel` the news that it's off.
    fn send_invites(&self, node: &crate::calendar::tree::Node, draft: &Draft, cancel: bool, sender: &ComponentSender<Self>) {
        let Some(org) = draft.organizer.clone().or_else(|| self.organizer.clone()) else {
            return;
        };
        let to: Vec<String> = draft
            .attendees
            .iter()
            .filter(|a| !a.email.eq_ignore_ascii_case(&org.email))
            .map(|a| a.email.clone())
            .collect();
        if to.is_empty() {
            return;
        }
        let mut event = node.clone();
        if cancel {
            event.replace("STATUS:CANCELLED".into());
            let seq: i64 = event.text("SEQUENCE").parse().unwrap_or(0);
            event.replace(format!("SEQUENCE:{}", seq + 1));
        }
        let mut cal = crate::calendar::tree::Node::new("VCALENDAR");
        cal.push(format!("PRODID:-//Hyprlab//{}//EN", crate::APP_NAME));
        cal.push("VERSION:2.0".into());
        cal.push(format!("METHOD:{}", if cancel { "CANCEL" } else { "REQUEST" }));
        for line in event.props() {
            if let Some(tz) = line.param("TZID") {
                if !cal.children.iter().any(|c| c.text("TZID") == tz) {
                    if let Some(z) = crate::calendar::time::vtimezone(tz) {
                        cal.children.push(z);
                    }
                }
            }
        }
        cal.children.push(event);
        let occ = self.preview(&draft.span);
        let when = super::inspect::when_text(&occ);
        let mut body = format!("{}\n{when}", draft.summary);
        if !draft.location.is_empty() {
            body.push_str(&format!("\n{}", draft.location));
        }
        if !draft.description.is_empty() {
            body.push_str(&format!("\n\n{}", draft.description));
        }
        let _ = sender.output(CalendarOutput::Invite {
            ics: cal.write(),
            to,
            summary: draft.summary.clone(),
            body,
            from: org.email,
            cancel,
        });
    }

    /// Ask which occurrences of a repeating event a change covers.
    fn ask_scope(&self, editing: bool, done: impl Fn(Scope) + 'static) {
        let (heading, body) = if editing {
            (i18n("Change a Repeating Event?"), i18n("Change only this event, this and the ones after it, or every event in the series."))
        } else {
            (i18n("Delete a Repeating Event?"), i18n("Delete only this event, this and the ones after it, or every event in the series."))
        };
        let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("this", &i18n("Only This Event"));
        dialog.add_response("future", &i18n("This and Following"));
        dialog.add_response("all", &i18n("All Events"));
        dialog.set_response_appearance(
            "all",
            if editing {
                adw::ResponseAppearance::Suggested
            } else {
                adw::ResponseAppearance::Destructive
            },
        );
        dialog.set_prefer_wide_layout(true);
        dialog.set_close_response("cancel");
        dialog.connect_response(None, move |_, r| match r {
            "this" => done(Scope::This),
            "future" => done(Scope::ThisAndFuture),
            "all" => done(Scope::All),
            _ => {}
        });
        dialog.present(Some(&self.parts.views));
    }

    fn menu(&mut self, key: Option<OccKey>, at: Option<i64>, all_day: bool, anchor: &Anchor, sender: &ComponentSender<Self>) {
        use crate::ui::context_menu::{show_context_menu, MenuEntry};
        self.close_popover();
        let s = sender.input_sender().clone();
        let mut groups: Vec<Vec<MenuEntry>> = Vec::new();
        if let Some(key) = key {
            self.ctx.borrow_mut().selected = Some(key.clone());
            self.redraw();
            let writable = self.occ(&key).is_some_and(|o| self.ctx.borrow().can_change(&o));
            let ask = |label: String, a: Ask| {
                let s = s.clone();
                MenuEntry::new(label, move || {
                    let _ = s.send(CalendarInput::Ask(a.clone()));
                })
            };
            groups.push(vec![
                ask(if writable { i18n("Edit…") } else { i18n("Details…") }, Ask::Edit).icon("document-edit-symbolic"),
                ask(i18n("Duplicate"), Ask::Duplicate).icon("edit-copy-symbolic"),
            ]);
            if let Some(o) = self.occ(&key) {
                let me = self.ctx.borrow().me.clone();
                if o.comp.invited(&me) && writable {
                    groups.push(vec![
                        ask(i18n("Accept"), Ask::Answer("ACCEPTED".into())),
                        ask(i18n("Maybe"), Ask::Answer("TENTATIVE".into())),
                        ask(i18n("Decline"), Ask::Answer("DECLINED".into())),
                    ]);
                }
                if !o.comp.attendees.is_empty() {
                    groups.push(vec![ask(i18n("Email Attendees"), Ask::Email).icon("mail-send-symbolic")]);
                }
            }
            if writable {
                groups.push(vec![ask(i18n("Delete"), Ask::Delete).icon("user-trash-symbolic")]);
            }
        } else if let Some(at) = at {
            let minutes = i64::from(self.prefs.default_minutes) * 60;
            let a2 = anchor.clone();
            let s2 = s.clone();
            groups.push(vec![MenuEntry::new(i18n("New Event Here"), move || {
                let _ = s2.send(CalendarInput::Act(Act::Create {
                    start: at,
                    end: if all_day { at + 86_400 } else { at + minutes },
                    all_day,
                    anchor: a2.clone(),
                }));
            })
            .icon("list-add-symbolic")]);
            let s3 = s.clone();
            groups.push(vec![MenuEntry::new(i18n("Go to Today"), move || {
                let _ = s3.send(CalendarInput::Today);
            })]);
        }
        if !groups.is_empty() {
            let r = anchor.rect;
            show_context_menu(&anchor.widget, f64::from(r.x()), f64::from(r.y()), groups);
        }
    }

    pub(super) fn on_side(&mut self, act: SideAct, sender: &ComponentSender<Self>) {
        match act {
            SideAct::Show(uid, on) => {
                let changed = if on { self.hidden.remove(&uid) } else { self.hidden.insert(uid) };
                if changed {
                    self.save_hidden();
                    self.parts.side.show_hidden(!self.hidden.is_empty(), self.hidden.len());
                    self.refresh();
                }
            }
            SideAct::ShowAll => {
                self.hidden.clear();
                self.save_hidden();
                self.parts.side.set_calendars(&self.calendars, &self.hidden);
                self.refresh();
            }
            SideAct::Only(uid) => {
                self.hidden = self.calendars.iter().filter(|c| c.uid != uid).map(|c| c.uid.clone()).collect();
                self.save_hidden();
                self.parts.side.set_calendars(&self.calendars, &self.hidden);
                self.refresh();
            }
            SideAct::Look(uid) => {
                if let Some(c) = self.cal(&uid).cloned() {
                    self.look_dialog(c, sender);
                }
            }
            SideAct::Delete(uid) => {
                if let Some(c) = self.cal(&uid).cloned() {
                    self.delete_calendar(c, sender);
                }
            }
            SideAct::Refresh(uid) => {
                if let Some(c) = self.cal(&uid).cloned() {
                    let backend = self.backend;
                    std::thread::spawn(move || store::refresh(backend, &c));
                    sender.input(CalendarInput::Changed(uid));
                }
            }
            SideAct::NewCalendar => self.new_calendar_dialog(sender),
            SideAct::Subscribe => self.subscribe_dialog(sender),
            SideAct::Import => self.import_dialog(sender),
            SideAct::AddAccount => {
                let _ = sender.output(CalendarOutput::AddAccount);
            }
        }
    }

    fn save_hidden(&mut self) {
        let mut list: Vec<String> = self.hidden.iter().cloned().collect();
        list.sort();
        self.prefs.hidden = list;
        let mut p = crate::calendar::prefs::CalendarPrefs::load();
        p.hidden = self.prefs.hidden.clone();
        p.save();
    }
}

/// A change to a repeating event, waiting on which occurrences it covers.
#[derive(Debug)]
pub(crate) enum Scoped {
    Edit(Occ, Option<Arc<Comp>>, Box<Draft>, Scope),
    Remove(Occ, Option<Arc<Comp>>, Scope),
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_answer_replaces_the_partstat() {
        assert_eq!(
            crate::calendar::edit::set_partstat("ATTENDEE;CN=\"Me, Myself\";PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:me@x.org", "ACCEPTED"),
            "ATTENDEE;CN=\"Me, Myself\";PARTSTAT=ACCEPTED:mailto:me@x.org"
        );
    }
}
