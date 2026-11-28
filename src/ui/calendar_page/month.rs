//! The month view: the weeks of a month in rows, each day's events in it.
//! Events of a day or more are bars across their days; a timed event is a
//! line with its calendar's dot and its time. What doesn't fit becomes
//! "N more", which lists the day.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{Datelike, Duration, NaiveDate, Weekday};
use gtk::prelude::*;
use gtk::{cairo, glib};

use super::ctx::{week_number, weekday_short, Act, Anchor, Shared};
use super::paint::{self, Font, Theme};
use crate::calendar::model::Occ;
use crate::calendar::time::{day_start, local_day, local_unix, local_wall};

const DAY_NUMBER_H: f64 = 26.0;
const LANE: f64 = 20.0;
const WEEKDAYS_H: f64 = 28.0;

#[derive(Debug, Clone)]
struct Item {
    occ: usize,
    week: usize,
    first: usize,
    last: usize,
    lane: usize,
    /// Drawn as a bar (all-day or over several days) rather than a line.
    bar: bool,
}

#[derive(Debug, Clone)]
enum Drag {
    Pending { x: f64, y: f64, on: Option<usize> },
    Move { item: usize, days: i64 },
    Create { from: NaiveDate, to: NaiveDate },
}

struct Month {
    ctx: Shared,
    month: NaiveDate,
    first: NaiveDate,
    weeks: usize,
    occs: Rc<Vec<Occ>>,
    items: Vec<Item>,
    drag: Option<Drag>,
    wheel: f64,
}

#[derive(Clone)]
pub(crate) struct MonthGrid {
    pub(crate) root: gtk::Box,
    head: gtk::DrawingArea,
    body: gtk::DrawingArea,
    model: Rc<RefCell<Month>>,
    /// Set by the second press of a double click: the press's release
    /// ends the drag gesture too, and must not count as a click of its own
    /// (it would close the popover the double click just opened).
    double: Rc<std::cell::Cell<bool>>,
}

impl Month {
    fn left(&self) -> f64 {
        if self.ctx.borrow().week_numbers {
            34.0
        } else {
            0.0
        }
    }

    fn cell(&self, w: f64, h: f64) -> (f64, f64) {
        ((w - self.left()) / 7.0, h / self.weeks.max(1) as f64)
    }

    fn day_at(&self, week: usize, col: usize) -> NaiveDate {
        self.first + Duration::days((week * 7 + col) as i64)
    }

    fn lanes(&self, cell_h: f64) -> usize {
        (((cell_h - DAY_NUMBER_H - 2.0) / LANE).floor().max(1.0)) as usize
    }

    /// Place every occurrence in its weeks: bars first, in the first lane
    /// free across their days, then each day's timed events below them.
    fn layout(&mut self) {
        let mut items = Vec::new();
        for week in 0..self.weeks {
            let wfirst = self.day_at(week, 0);
            let wlast = self.day_at(week, 6);
            let mut lanes: Vec<Vec<(usize, usize)>> = Vec::new();
            let take = |a: usize, b: usize, lanes: &mut Vec<Vec<(usize, usize)>>| -> usize {
                let l = lanes
                    .iter()
                    .position(|l| l.iter().all(|&(x, y)| b < x || a > y))
                    .unwrap_or_else(|| {
                        lanes.push(Vec::new());
                        lanes.len() - 1
                    });
                lanes[l].push((a, b));
                l
            };
            let mut here: Vec<(usize, &Occ)> = self
                .occs
                .iter()
                .enumerate()
                .filter(|(_, o)| o.last_day >= wfirst && o.first_day <= wlast)
                .collect();
            // Longer bars first, then by start.
            here.sort_by_key(|(_, o)| {
                (
                    !o.in_day_row() && o.first_day == o.last_day,
                    o.first_day.max(wfirst),
                    std::cmp::Reverse(o.last_day.min(wlast) - o.first_day.max(wfirst)),
                    !o.all_day,
                    o.start,
                )
            });
            for (i, o) in here {
                let a = (o.first_day.max(wfirst) - wfirst).num_days() as usize;
                let b = (o.last_day.min(wlast) - wfirst).num_days() as usize;
                let bar = o.in_day_row() || a != b;
                let lane = take(a, b, &mut lanes);
                items.push(Item {
                    occ: i,
                    week,
                    first: a,
                    last: b,
                    lane,
                    bar,
                });
            }
        }
        self.items = items;
    }

    fn item_rect(&self, it: &Item, w: f64, h: f64) -> (f64, f64, f64, f64) {
        let (cw, ch) = self.cell(w, h);
        let x = self.left() + it.first as f64 * cw + 3.0;
        let y = it.week as f64 * ch + DAY_NUMBER_H + it.lane as f64 * LANE;
        let width = (it.last - it.first + 1) as f64 * cw - 6.0;
        (x, y, width, LANE - 2.0)
    }

    /// Whether an item fits its cells, or goes into "N more".
    fn shown(&self, it: &Item, ch: f64) -> bool {
        let lanes = self.lanes(ch);
        let crowded = (it.first..=it.last).any(|c| {
            self.items
                .iter()
                .filter(|o| o.week == it.week && o.first <= c && o.last >= c)
                .count()
                > lanes
        });
        if crowded {
            it.lane + 1 < lanes
        } else {
            it.lane < lanes
        }
    }

    fn draw_head(&self, area: &gtk::DrawingArea, cr: &cairo::Context, w: f64) {
        let theme = Theme::of(area);
        let (cw, _) = self.cell(w, 1.0);
        for c in 0..7 {
            let day = self.day_at(0, c);
            let name = weekday_short(day);
            paint::centred(
                cr,
                area,
                &name,
                self.left() + c as f64 * cw + cw / 2.0,
                6.0,
                Font::new(0.85).bold(),
                theme.dim,
            );
        }
        theme.line.set(cr);
        cr.rectangle(0.0, WEEKDAYS_H - 1.0, w, 1.0);
        let _ = cr.fill();
    }

    fn draw(&self, area: &gtk::DrawingArea, cr: &cairo::Context, w: f64, h: f64) {
        let ctx = self.ctx.borrow();
        let theme = Theme::of(area);
        let (cw, ch) = self.cell(w, h);
        let today = chrono::Local::now().date_naive();
        let left = self.left();
        for week in 0..self.weeks {
            for c in 0..7 {
                let day = self.day_at(week, c);
                let x = left + c as f64 * cw;
                let y = week as f64 * ch;
                let other = day.month() != self.month.month();
                if other || matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
                    theme.shade.set(cr);
                    cr.rectangle(x, y, cw, ch);
                    let _ = cr.fill();
                }
                let num = if day.day() == 1 {
                    format!("{} {}", super::ctx::fmt(day, "%b"), day.day())
                } else {
                    day.day().to_string()
                };
                let font = Font::new(0.88).bold().tabular();
                let (nw, nh) = paint::measure(area, &num, font);
                let nx = x + 8.0;
                if day == today {
                    paint::rounded(cr, nx - 5.0, y + 4.0, nw + 10.0, nh + 4.0, (nh + 4.0) / 2.0);
                    theme.accent.set(cr);
                    let _ = cr.fill();
                    paint::text(cr, area, &num, nx, y + 6.0, None, font, theme.accent_fg);
                } else {
                    let tone = if other { theme.faint } else { theme.fg };
                    paint::text(cr, area, &num, nx, y + 6.0, None, font, tone);
                }
            }
            if ctx.week_numbers {
                let label = week_number(self.day_at(week, 0)).to_string();
                paint::centred(cr, area, &label, left / 2.0, week as f64 * ch + 7.0, Font::new(0.75), theme.faint);
            }
        }
        theme.line.set(cr);
        for week in 0..=self.weeks {
            cr.rectangle(0.0, (week as f64 * ch).round(), w, 1.0);
        }
        for c in 0..=7 {
            cr.rectangle((left + c as f64 * cw).round(), 0.0, 1.0, h);
        }
        let _ = cr.fill();

        let moving = match &self.drag {
            Some(Drag::Move { item, days }) => Some((*item, *days)),
            _ => None,
        };
        for (idx, it) in self.items.iter().enumerate() {
            if !self.shown(it, ch) {
                continue;
            }
            let occ = &self.occs[it.occ];
            let (x, y, iw, ih) = self.item_rect(it, w, h);
            let faded = moving.is_some_and(|(m, _)| m == idx);
            self.paint_item(area, cr, &ctx, &theme, occ, it, (x, y, iw, ih), faded);
        }
        // "N more" where a day holds more than its cell shows.
        for week in 0..self.weeks {
            for c in 0..7 {
                let hidden = self
                    .items
                    .iter()
                    .filter(|it| it.week == week && it.first <= c && it.last >= c && !self.shown(it, ch))
                    .count();
                if hidden > 0 {
                    let lanes = self.lanes(ch);
                    let y = week as f64 * ch + DAY_NUMBER_H + (lanes - 1) as f64 * LANE + 2.0;
                    let text = crate::i18n::ni18n_f("{n} more", "{n} more", hidden as u32, &[("n", &hidden.to_string())]);
                    paint::text(cr, area, &text, left + c as f64 * cw + 8.0, y, Some(cw - 12.0), Font::new(0.78).bold(), theme.dim);
                }
            }
        }
        // Where a drag would put it.
        if let Some((idx, days)) = moving {
            let it = &self.items[idx];
            let occ = &self.occs[it.occ];
            let start = self.day_at(it.week, it.first) + Duration::days(days);
            let dropped = (start - self.first).num_days();
            if (0..(self.weeks * 7) as i64).contains(&dropped) {
                let week = dropped as usize / 7;
                let col = dropped as usize % 7;
                let len = it.last - it.first;
                let ghost = Item {
                    occ: it.occ,
                    week,
                    first: col,
                    last: (col + len).min(6),
                    lane: 0,
                    bar: it.bar,
                };
                let (x, _, iw, ih) = self.item_rect(&ghost, w, h);
                let y = week as f64 * ch + DAY_NUMBER_H;
                let colors = theme.event(ctx.color(occ), true, false);
                paint::event_box(cr, &colors, x, y, iw, ih, false, false);
                paint::text(cr, area, &occ.comp.summary, x + 6.0, y + 1.0, Some(iw - 10.0), Font::new(0.8).bold(), colors.text);
            }
        }
        if let Some(Drag::Create { from, to }) = &self.drag {
            let (a, b) = ((*from).min(*to), (*from).max(*to));
            let colors = theme.event(theme.accent, false, false);
            let mut d = a;
            while d <= b {
                let i = (d - self.first).num_days();
                if (0..(self.weeks * 7) as i64).contains(&i) {
                    let (week, col) = (i as usize / 7, i as usize % 7);
                    let x = left + col as f64 * cw + 3.0;
                    let y = week as f64 * ch + DAY_NUMBER_H;
                    paint::event_box(cr, &colors, x, y, cw - 6.0, LANE - 2.0, false, false);
                }
                d += Duration::days(1);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_item(
        &self,
        area: &gtk::DrawingArea,
        cr: &cairo::Context,
        ctx: &super::ctx::Ctx,
        theme: &Theme,
        occ: &Occ,
        it: &Item,
        (x, y, w, h): (f64, f64, f64, f64),
        faded: bool,
    ) {
        let standing = ctx.standing(occ);
        let selected = ctx.is_selected(occ);
        let base = ctx.color(occ);
        let colors = theme.event(base, selected, standing.faded() || faded);
        let mut title = occ.comp.summary.clone();
        if title.is_empty() {
            title = crate::i18n::i18n("Untitled");
        }
        let font = Font::new(0.8).bold().strike(standing.struck());
        let (_, th) = paint::measure(area, "Ag", font);
        let ty = y + (h - th) / 2.0;
        if it.bar {
            paint::event_box(cr, &colors, x, y, w, h, standing.hatched(), false);
            let mut text = title;
            if occ.first_day < self.day_at(it.week, it.first) {
                text = format!("‹ {text}");
            }
            if !occ.all_day {
                text = format!("{} {text}", crate::datefmt::time(occ.start));
            }
            paint::text(cr, area, &text, x + 6.0, ty, Some(w - 10.0), font, colors.text);
            return;
        }
        if selected {
            paint::rounded(cr, x, y, w, h, 5.0);
            base.set(cr);
            let _ = cr.fill();
        }
        let dot_color = if selected { colors.text } else { base.with_alpha(if faded { 0.5 } else { 1.0 }) };
        paint::dot(cr, x + 7.0, y + h / 2.0, 3.5, dot_color);
        let time = crate::datefmt::time(occ.start);
        let tfont = Font::new(0.75).tabular();
        let (tw, _) = paint::measure(area, &time, tfont);
        let title_color = if selected { colors.text } else { theme.fg.with_alpha(if standing.faded() || faded { 0.5 } else { 1.0 }) };
        let time_color = if selected { colors.sub } else { theme.dim };
        let room = w - 16.0;
        if room > tw + 40.0 {
            paint::text(cr, area, &title, x + 15.0, ty, Some(room - tw - 6.0), Font::new(0.8).strike(standing.struck()), title_color);
            paint::text(cr, area, &time, x + w - tw - 4.0, ty + 0.5, None, tfont, time_color);
        } else {
            paint::text(cr, area, &title, x + 15.0, ty, Some(room), Font::new(0.8), title_color);
        }
        if standing.hatched() && !selected {
            paint::hatch(cr, x, y, w, h, base.with_alpha(0.14));
        }
    }
}

impl MonthGrid {
    pub(crate) fn new(ctx: Shared) -> MonthGrid {
        let model = Rc::new(RefCell::new(Month {
            ctx,
            month: chrono::Local::now().date_naive(),
            first: chrono::Local::now().date_naive(),
            weeks: 6,
            occs: Rc::new(Vec::new()),
            items: Vec::new(),
            drag: None,
            wheel: 0.0,
        }));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("calendar-month");
        let head = gtk::DrawingArea::new();
        head.set_content_height(WEEKDAYS_H as i32);
        let body = gtk::DrawingArea::new();
        body.set_vexpand(true);
        root.append(&head);
        root.append(&body);
        let grid = MonthGrid {
            root,
            head,
            body,
            model,
            double: Rc::new(std::cell::Cell::new(false)),
        };
        let m = grid.model.clone();
        grid.head
            .set_draw_func(move |a, cr, w, _| m.borrow().draw_head(a, cr, f64::from(w)));
        let m = grid.model.clone();
        grid.body
            .set_draw_func(move |a, cr, w, h| m.borrow().draw(a, cr, f64::from(w), f64::from(h)));
        grid.connect_gestures();
        grid
    }

    /// The first and the day after the last day shown for `month`.
    pub(crate) fn span(month: NaiveDate, week_start: Weekday) -> (NaiveDate, usize) {
        let first_of = super::ctx::month_of(month);
        let first = super::ctx::week_of(first_of, week_start);
        let days_in = crate::calendar::time::days_in_month(first_of.year(), first_of.month()) as i64;
        let last = first_of + Duration::days(days_in - 1);
        let weeks = ((last - first).num_days() / 7 + 1) as usize;
        (first, weeks)
    }

    pub(crate) fn set(&self, month: NaiveDate, occs: Rc<Vec<Occ>>) {
        {
            let mut m = self.model.borrow_mut();
            let start = m.ctx.borrow().week_start;
            let (first, weeks) = MonthGrid::span(month, start);
            m.month = super::ctx::month_of(month);
            m.first = first;
            m.weeks = weeks;
            m.occs = occs;
            m.drag = None;
            m.layout();
        }
        self.redraw();
    }

    pub(crate) fn redraw(&self) {
        self.head.queue_draw();
        self.body.queue_draw();
    }

    fn emit(&self, act: Act) {
        let ctx = self.model.borrow().ctx.clone();
        let ctx = ctx.borrow();
        ctx.emit(act);
    }

    /// The day under a point, and the item there if one is drawn there.
    fn hit(&self, x: f64, y: f64) -> (NaiveDate, Option<usize>, bool) {
        let m = self.model.borrow();
        let (w, h) = (f64::from(self.body.width()), f64::from(self.body.height()));
        let (cw, ch) = m.cell(w, h);
        let col = (((x - m.left()) / cw).floor().max(0.0) as usize).min(6);
        let week = ((y / ch).floor().max(0.0) as usize).min(m.weeks.saturating_sub(1));
        let day = m.day_at(week, col);
        let item = m.items.iter().position(|it| {
            if !m.shown(it, ch) {
                return false;
            }
            let (ix, iy, iw, ih) = m.item_rect(it, w, h);
            x >= ix && x <= ix + iw && y >= iy && y <= iy + ih
        });
        // The "N more" line, and the day number.
        let in_cell = y - week as f64 * ch;
        let lanes = m.lanes(ch);
        let more = item.is_none() && in_cell >= DAY_NUMBER_H + (lanes - 1) as f64 * LANE;
        let number = in_cell < DAY_NUMBER_H && x - m.left() - col as f64 * cw < 40.0;
        (day, item, more || number)
    }

    fn more_or_number(&self, day: NaiveDate, x: f64, y: f64) {
        let m = self.model.borrow();
        let (w, h) = (f64::from(self.body.width()), f64::from(self.body.height()));
        let (cw, ch) = m.cell(w, h);
        let week = ((y / ch).floor().max(0.0) as usize).min(m.weeks.saturating_sub(1));
        let col = (((x - m.left()) / cw).floor().max(0.0) as usize).min(6);
        let in_cell = y - week as f64 * ch;
        let ctx = m.ctx.clone();
        drop(m);
        if in_cell < DAY_NUMBER_H {
            ctx.borrow().emit(Act::Show(day, Some(crate::calendar::prefs::View::Day)));
        } else {
            let m = self.model.borrow();
            let anchor = Anchor::new(&self.body, m.left() + col as f64 * cw, week as f64 * ch, cw, ch);
            drop(m);
            ctx.borrow().emit(Act::DayList(day, anchor));
        }
    }

    fn connect_gestures(&self) {
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let this = self.clone();
        drag.connect_drag_begin(move |_, x, y| {
            let (_, item, _) = this.hit(x, y);
            this.model.borrow_mut().drag = Some(Drag::Pending { x, y, on: item });
        });
        let this = self.clone();
        drag.connect_drag_update(move |g, dx, dy| {
            let Some((sx, sy)) = g.start_point() else {
                return;
            };
            let (day, _, _) = this.hit(sx + dx, sy + dy);
            let current = this.model.borrow().drag.clone();
            let next = match current {
                Some(Drag::Pending { x, y, on }) => {
                    if dx.hypot(dy) < 5.0 {
                        return;
                    }
                    let (from, _, _) = this.hit(x, y);
                    match on {
                        Some(item) => {
                            let m = this.model.borrow();
                            let occ = &m.occs[m.items[item].occ];
                            if !m.ctx.borrow().can_change(occ) {
                                None
                            } else {
                                Some(Drag::Move {
                                    item,
                                    days: (day - from).num_days(),
                                })
                            }
                        }
                        None => Some(Drag::Create { from, to: day }),
                    }
                }
                Some(Drag::Move { item, .. }) => {
                    let (from, _, _) = this.hit(sx, sy);
                    Some(Drag::Move {
                        item,
                        days: (day - from).num_days(),
                    })
                }
                Some(Drag::Create { from, .. }) => Some(Drag::Create { from, to: day }),
                None => None,
            };
            this.model.borrow_mut().drag = next;
            this.body.queue_draw();
        });
        let this = self.clone();
        drag.connect_drag_end(move |g, _, _| {
            let current = this.model.borrow_mut().drag.take();
            match current {
                Some(Drag::Pending { .. }) if this.double.replace(false) => {}
                Some(Drag::Pending { x, y, on }) => match on {
                    Some(item) => {
                        let (key, anchor) = {
                            let m = this.model.borrow();
                            let (w, h) = (f64::from(this.body.width()), f64::from(this.body.height()));
                            let (ix, iy, iw, ih) = m.item_rect(&m.items[item], w, h);
                            (m.occs[m.items[item].occ].key.clone(), Anchor::new(&this.body, ix, iy, iw, ih))
                        };
                        this.emit(Act::Select(Some(key), Some(anchor)));
                    }
                    None => {
                        let (day, _, special) = this.hit(x, y);
                        if special {
                            this.more_or_number(day, x, y);
                        } else {
                            this.emit(Act::Select(None, None));
                        }
                    }
                },
                Some(Drag::Move { item, days }) if days != 0 => {
                    let (key, start, end) = {
                        let m = this.model.borrow();
                        let o = &m.occs[m.items[item].occ];
                        let shift = |t: i64| {
                            let day = local_day(t) + Duration::days(days);
                            local_unix(day.and_time(local_wall(t).time()))
                        };
                        (o.key.clone(), shift(o.start), shift(o.end))
                    };
                    this.emit(Act::Move { key, start, end });
                }
                Some(Drag::Create { from, to }) => {
                    let (a, b) = (from.min(to), from.max(to));
                    let anchor = g
                        .start_point()
                        .map(|(x, y)| Anchor::new(&this.body, x, y, 1.0, 1.0))
                        .unwrap_or_else(|| Anchor::new(&this.body, 0.0, 0.0, 1.0, 1.0));
                    this.emit(Act::Create {
                        start: day_start(a),
                        end: day_start(b + Duration::days(1)),
                        all_day: true,
                        anchor,
                    });
                }
                _ => {}
            }
            this.double.set(false);
            this.body.queue_draw();
        });
        self.body.add_controller(drag);

        let click = gtk::GestureClick::new();
        click.set_button(1);
        let this = self.clone();
        click.connect_pressed(move |_, n, x, y| {
            if n != 2 {
                return;
            }
            this.double.set(true);
            let (day, item, special) = this.hit(x, y);
            match item {
                Some(i) => {
                    let key = {
                        let m = this.model.borrow();
                        m.occs[m.items[i].occ].key.clone()
                    };
                    this.emit(Act::Open(key));
                }
                None if !special => {
                    // A new event on the day, at the start of the working
                    // day, as Calendar on the Mac makes one.
                    let (hour, minutes) = {
                        let m = this.model.borrow();
                        let c = m.ctx.borrow();
                        (c.work.0, 60)
                    };
                    let start = local_unix(day.and_hms_opt(hour, 0, 0).unwrap_or_default());
                    this.emit(Act::Create {
                        start,
                        end: start + minutes * 60,
                        all_day: false,
                        anchor: Anchor::new(&this.body, x, y, 1.0, 1.0),
                    });
                }
                None => {}
            }
        });
        self.body.add_controller(click);

        let menu = gtk::GestureClick::new();
        menu.set_button(3);
        let this = self.clone();
        menu.connect_pressed(move |_, _, x, y| {
            let (day, item, _) = this.hit(x, y);
            let key = item.map(|i| {
                let m = this.model.borrow();
                m.occs[m.items[i].occ].key.clone()
            });
            this.emit(Act::Menu {
                key,
                at: Some(day_start(day)),
                all_day: true,
                anchor: Anchor::new(&this.body, x, y, 1.0, 1.0),
            });
        });
        self.body.add_controller(menu);

        // The wheel turns the months.
        let scroll = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::KINETIC,
        );
        let this = self.clone();
        scroll.connect_scroll(move |_, _, dy| {
            let step = {
                let mut m = this.model.borrow_mut();
                m.wheel += dy;
                if m.wheel.abs() >= 1.0 {
                    let s = m.wheel.signum() as i32;
                    m.wheel = 0.0;
                    Some((s, m.month))
                } else {
                    None
                }
            };
            if let Some((s, month)) = step {
                this.emit(Act::Show(super::ctx::add_months(month, s), None));
            }
            glib::Propagation::Stop
        });
        self.body.add_controller(scroll);
    }
}
