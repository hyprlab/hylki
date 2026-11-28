//! The day and week views' layout and drawing: events placed side by side
//! where they overlap, the all-day row's lanes, the hours, and the line at
//! the current time.

use chrono::{Datelike, Duration, NaiveDate, NaiveTime, Timelike, Weekday};
use gtk::cairo;

use super::ctx::{hour_label, week_number, weekday_short, Ctx};
use super::grid::{Drag, Grid, ALLDAY_LANE, GUTTER, HEADER_H, MIN_EVENT_PX};
use super::paint::{self, Font, Theme, NOW};
use crate::calendar::model::Occ;
use crate::calendar::time::{day_start, local_unix};

/// Where a drag would put a piece: its day, top and bottom in minutes,
/// and the event moved (`None` for a new one).
type Ghost = (usize, f64, f64, Option<usize>);

/// An event's piece on one day of the hour grid.
#[derive(Debug, Clone)]
pub(super) struct Placed {
    pub(super) occ: usize,
    pub(super) day: usize,
    /// Minutes from the day's midnight.
    pub(super) top: f64,
    pub(super) bottom: f64,
    pub(super) col: usize,
    pub(super) cols: usize,
    pub(super) span: usize,
    /// The event ends on this piece (it can be stretched from here).
    pub(super) ends_here: bool,
}

/// An event's bar across the all-day row.
#[derive(Debug, Clone)]
pub(super) struct Bar {
    pub(super) occ: usize,
    pub(super) first: usize,
    pub(super) last: usize,
    pub(super) lane: usize,
    /// It began before the first day shown, or goes on after the last.
    pub(super) cut_start: bool,
    pub(super) cut_end: bool,
}

/// The instant a minute of `day` stands for, local.
pub(super) fn at_minute(day: NaiveDate, minute: f64) -> i64 {
    let m = minute.clamp(0.0, 1440.0) as i64;
    if m >= 1440 {
        return day_start(day + Duration::days(1));
    }
    let t = NaiveTime::from_num_seconds_from_midnight_opt((m * 60) as u32, 0).unwrap_or(NaiveTime::MIN);
    local_unix(day.and_time(t))
}

/// Lay out the occurrences over `days`: the timed pieces, packed into
/// columns where they overlap, and the all-day bars in lanes.
pub(super) fn layout(occs: &[Occ], days: &[NaiveDate], hour_h: f64) -> (Vec<Placed>, Vec<Bar>, usize) {
    let mut placed = Vec::new();
    let mut bars = Vec::new();
    let Some((&first, &last)) = days.first().zip(days.last()) else {
        return (placed, bars, 0);
    };
    let min_minutes = MIN_EVENT_PX / hour_h * 60.0;
    for (di, &day) in days.iter().enumerate() {
        let from = day_start(day);
        let to = day_start(day + Duration::days(1));
        let mut today: Vec<Placed> = occs
            .iter()
            .enumerate()
            .filter(|(_, o)| !o.in_day_row() && o.start < to && (o.end > from || o.start == o.end && o.start >= from))
            .map(|(i, o)| {
                let s = o.start.max(from);
                let e = o.end.min(to).max(s);
                let top = (s - from) as f64 / 60.0;
                let bottom = ((e - from) as f64 / 60.0).max(top + min_minutes).min(1440.0);
                Placed {
                    occ: i,
                    day: di,
                    top,
                    bottom,
                    col: 0,
                    cols: 1,
                    span: 1,
                    ends_here: o.end <= to,
                }
            })
            .collect();
        today.sort_by(|a, b| {
            a.top
                .partial_cmp(&b.top)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.bottom.partial_cmp(&a.bottom).unwrap_or(std::cmp::Ordering::Equal))
        });
        pack(&mut today);
        placed.extend(today);
    }
    // All-day and multi-day events: one bar each, in the first lane free
    // across its days.
    let mut row: Vec<(usize, usize, usize, bool, bool)> = occs
        .iter()
        .enumerate()
        .filter(|(_, o)| o.in_day_row() && o.last_day >= first && o.first_day <= last)
        .map(|(i, o)| {
            let a = (o.first_day.max(first) - first).num_days() as usize;
            let b = (o.last_day.min(last) - first).num_days() as usize;
            (i, a, b, o.first_day < first, o.last_day > last)
        })
        .collect();
    row.sort_by_key(|&(i, a, b, _, _)| (a, std::cmp::Reverse(b - a), occs[i].start));
    let mut lanes: Vec<Vec<(usize, usize)>> = Vec::new();
    for (i, a, b, cs, ce) in row {
        let lane = lanes
            .iter()
            .position(|l| l.iter().all(|&(x, y)| b < x || a > y))
            .unwrap_or_else(|| {
                lanes.push(Vec::new());
                lanes.len() - 1
            });
        lanes[lane].push((a, b));
        bars.push(Bar {
            occ: i,
            first: a,
            last: b,
            lane,
            cut_start: cs,
            cut_end: ce,
        });
    }
    (placed, bars, lanes.len())
}

/// Columns for a day's overlapping events: each takes the first column
/// free at its start, a run of events that overlap shares the count, and
/// each widens into columns to its right that stay free beside it.
fn pack(day: &mut [Placed]) {
    let mut start = 0;
    while start < day.len() {
        let mut end_of_run = day[start].bottom;
        let mut stop = start + 1;
        while stop < day.len() && day[stop].top < end_of_run {
            end_of_run = end_of_run.max(day[stop].bottom);
            stop += 1;
        }
        let run = &mut day[start..stop];
        let mut col_ends: Vec<f64> = Vec::new();
        for p in run.iter_mut() {
            let c = col_ends
                .iter()
                .position(|&e| e <= p.top)
                .unwrap_or_else(|| {
                    col_ends.push(0.0);
                    col_ends.len() - 1
                });
            col_ends[c] = p.bottom;
            p.col = c;
        }
        let cols = col_ends.len();
        let spans: Vec<usize> = run
            .iter()
            .map(|p| {
                let mut span = 1;
                while p.col + span < cols
                    && !run
                        .iter()
                        .any(|q| q.col == p.col + span && q.top < p.bottom && q.bottom > p.top)
                {
                    span += 1;
                }
                span
            })
            .collect();
        for (p, s) in run.iter_mut().zip(spans) {
            p.cols = cols;
            p.span = s;
        }
        start = stop;
    }
}

impl Grid {
    pub(super) fn col_w(&self, width: f64) -> f64 {
        ((width - GUTTER) / self.days.len().max(1) as f64).max(1.0)
    }

    /// A piece's box on the hour grid.
    pub(super) fn rect(&self, p: &Placed, width: f64) -> (f64, f64, f64, f64) {
        let cw = self.col_w(width);
        let inner = cw - 6.0;
        let each = inner / p.cols as f64;
        let x = GUTTER + p.day as f64 * cw + 1.0 + p.col as f64 * each;
        let w = (each * p.span as f64 - 2.0).max(4.0);
        let y = p.top / 60.0 * self.hour_h + 1.0;
        let h = ((p.bottom - p.top) / 60.0 * self.hour_h - 2.0).max(MIN_EVENT_PX - 2.0);
        (x, y, w, h)
    }

    /// A bar's box on the all-day row.
    pub(super) fn bar_rect(&self, b: &Bar, width: f64) -> (f64, f64, f64, f64) {
        let cw = self.col_w(width);
        let x = GUTTER + b.first as f64 * cw + 2.0;
        let w = (b.last - b.first + 1) as f64 * cw - 4.0;
        let y = 3.0 + b.lane as f64 * ALLDAY_LANE;
        (x, y, w, ALLDAY_LANE - 3.0)
    }

    /// Lanes shown in the all-day row: all of them, or three and a count
    /// of the rest.
    pub(super) fn shown_lanes(&self) -> usize {
        if self.expanded || self.lanes <= 3 {
            self.lanes
        } else {
            2
        }
    }

    pub(super) fn draw_header(&self, area: &gtk::DrawingArea, cr: &cairo::Context, width: f64) {
        let ctx = self.ctx.borrow();
        let theme = Theme::of(area);
        let cw = self.col_w(width);
        let today = chrono::Local::now().date_naive();
        if ctx.week_numbers {
            if let Some(first) = self.days.first() {
                let text = format!("W{}", week_number(*first));
                paint::centred(cr, area, &text, GUTTER / 2.0, 14.0, Font::new(0.8).bold(), theme.dim);
            }
        }
        for (i, day) in self.days.iter().enumerate() {
            let cx = GUTTER + i as f64 * cw + cw / 2.0;
            let weekend = matches!(day.weekday(), Weekday::Sat | Weekday::Sun);
            let tone = if weekend { theme.dim } else { theme.fg };
            let name = weekday_short(*day);
            let num = day.day().to_string();
            let font = Font::new(1.0).bold().tabular();
            let (nw, nh) = paint::measure(area, &num, font);
            let (aw, _) = paint::measure(area, &name, Font::new(0.9));
            let total = aw + 6.0 + nw.max(nh);
            let x0 = cx - total / 2.0;
            paint::text(cr, area, &name, x0, 14.0, None, Font::new(0.9), theme.dim);
            let ncx = x0 + aw + 6.0 + nw.max(nh) / 2.0;
            if *day == today {
                // A size smaller inside the circle, so it sits with room
                // around it rather than filling it.
                let cy = 12.0 + nh / 2.0;
                let small = Font::new(0.8).bold().tabular();
                let (sw, sh) = paint::measure(area, &num, small);
                let r = nh / 2.0 + 2.0;
                paint::dot(cr, ncx, cy, r.max(sw / 2.0 + 5.0), theme.accent);
                paint::centred(cr, area, &num, ncx, cy - sh / 2.0, small, theme.accent_fg);
            } else {
                paint::centred(cr, area, &num, ncx, 12.0, font, tone);
            }
        }
        theme.line.set(cr);
        cr.rectangle(0.0, HEADER_H - 1.0, width, 1.0);
        let _ = cr.fill();
    }

    pub(super) fn draw_allday(&self, area: &gtk::DrawingArea, cr: &cairo::Context, width: f64, height: f64) {
        let ctx = self.ctx.borrow();
        let theme = Theme::of(area);
        let cw = self.col_w(width);
        theme.line.set(cr);
        for i in 0..=self.days.len() {
            cr.rectangle(GUTTER + i as f64 * cw, 0.0, 1.0, height);
        }
        cr.rectangle(0.0, height - 1.0, width, 1.0);
        let _ = cr.fill();
        let label = crate::i18n::i18n("all-day");
        paint::text(cr, area, &label, 6.0, 5.0, Some(GUTTER - 10.0), Font::new(0.72), theme.dim);
        let shown = self.shown_lanes();
        let dragging = self.drag.as_ref().and_then(Drag::bar_occ);
        for b in &self.bars {
            if b.lane >= shown {
                continue;
            }
            let occ = &self.occs[b.occ];
            let (mut x, y, mut w, h) = self.bar_rect(b, width);
            let faded = Some(b.occ) == dragging;
            if let Some(Drag::BarMove { days, .. }) = &self.drag {
                if faded {
                    x += *days as f64 * cw;
                    let _ = &mut w;
                }
            }
            self.paint_bar(area, cr, &ctx, &theme, occ, (x, y, w, h), b.cut_start, b.cut_end);
        }
        if self.lanes > 3 {
            // A count per day of the bars not shown, or a fold when all are.
            for (i, _) in self.days.iter().enumerate() {
                let hidden = self
                    .bars
                    .iter()
                    .filter(|b| b.lane >= shown && b.first <= i && b.last >= i)
                    .count();
                if hidden > 0 {
                    let text = crate::i18n::ni18n_f("{n} more", "{n} more", hidden as u32, &[("n", &hidden.to_string())]);
                    let x = GUTTER + i as f64 * cw + 6.0;
                    let y = 3.0 + shown as f64 * ALLDAY_LANE + 2.0;
                    paint::text(cr, area, &text, x, y, Some(cw - 10.0), Font::new(0.78).bold(), theme.dim);
                }
            }
        }
        if let Some(Drag::BarCreate { from, to }) = &self.drag {
            let (a, b) = ((*from).min(*to), (*from).max(*to));
            let x = GUTTER + a as f64 * cw + 2.0;
            let w = (b - a + 1) as f64 * cw - 4.0;
            let colors = theme.event(theme.accent, false, false);
            paint::event_box(cr, &colors, x, 3.0, w, ALLDAY_LANE - 3.0, false, false);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn paint_bar(
        &self,
        area: &gtk::DrawingArea,
        cr: &cairo::Context,
        ctx: &Ctx,
        theme: &Theme,
        occ: &Occ,
        (x, y, w, h): (f64, f64, f64, f64),
        cut_start: bool,
        _cut_end: bool,
    ) {
        let standing = ctx.standing(occ);
        let colors = theme.event(ctx.color(occ), ctx.is_selected(occ), standing.faded());
        paint::event_box(cr, &colors, x, y, w, h, standing.hatched(), false);
        let mut title = occ.comp.summary.clone();
        if title.is_empty() {
            title = crate::i18n::i18n("Untitled");
        }
        if cut_start {
            title = format!("‹ {title}");
        }
        let font = Font::new(0.8).bold().strike(standing.struck());
        let (_, th) = paint::measure(area, &title, font);
        paint::text(cr, area, &title, x + 6.0, y + (h - th) / 2.0, Some(w - 10.0), font, colors.text);
    }

    pub(super) fn draw_body(&self, area: &gtk::DrawingArea, cr: &cairo::Context, width: f64, height: f64) {
        let ctx = self.ctx.borrow();
        let theme = Theme::of(area);
        let cw = self.col_w(width);
        let hh = self.hour_h;
        // Outside the working day, and the weekend, a shade darker.
        let (ws, we) = ctx.work;
        for (i, day) in self.days.iter().enumerate() {
            let x = GUTTER + i as f64 * cw;
            theme.shade.set(cr);
            if matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
                cr.rectangle(x, 0.0, cw, height);
            } else {
                cr.rectangle(x, 0.0, cw, f64::from(ws) * hh);
                cr.rectangle(x, f64::from(we) * hh, cw, height - f64::from(we) * hh);
            }
            let _ = cr.fill();
        }
        // Hours, half hours, and the days' edges.
        for h in 0..24 {
            let y = (f64::from(h) * hh).round();
            theme.line.set(cr);
            cr.rectangle(GUTTER - 6.0, y, width - GUTTER + 6.0, 1.0);
            let _ = cr.fill();
            if hh >= 40.0 {
                theme.line.with_alpha(theme.line.a * 0.45).set(cr);
                cr.rectangle(GUTTER, y + (hh / 2.0).round(), width - GUTTER, 1.0);
                let _ = cr.fill();
            }
            if h > 0 {
                let label = hour_label(h);
                let font = Font::new(0.75).tabular();
                let (lw, lh) = paint::measure(area, &label, font);
                paint::text(cr, area, &label, GUTTER - 10.0 - lw, y - lh / 2.0, None, font, theme.dim);
            }
        }
        theme.line.set(cr);
        for i in 0..=self.days.len() {
            cr.rectangle((GUTTER + i as f64 * cw).round(), 0.0, 1.0, height);
        }
        let _ = cr.fill();

        let moving = self.drag.as_ref().and_then(Drag::moving_occ);
        for p in &self.timed {
            let occ = &self.occs[p.occ];
            let r = self.rect(p, width);
            let faded = moving == Some(p.occ);
            self.paint_event(area, cr, &ctx, &theme, occ, r, faded);
        }
        if let Some(ghost) = self.ghost() {
            for (day, top, bottom, occ) in ghost {
                let x = GUTTER + day as f64 * cw + 1.0;
                let y = top / 60.0 * hh + 1.0;
                let h = ((bottom - top) / 60.0 * hh - 2.0).max(MIN_EVENT_PX - 2.0);
                match occ {
                    Some(i) => {
                        let occ = &self.occs[i];
                        let base = ctx.color(occ);
                        let colors = theme.event(base, true, false);
                        paint::event_box(cr, &colors, x, y, cw - 6.0, h, false, true);
                        self.event_text(area, cr, occ, &colors, (x, y, cw - 6.0, h), Some((day, top, bottom)), false);
                    }
                    None => {
                        let colors = theme.event(theme.accent, false, false);
                        paint::event_box(cr, &colors, x, y, cw - 6.0, h, false, true);
                        let label = format!(
                            "{} – {}",
                            crate::datefmt::time(at_minute(self.days[day], top)),
                            crate::datefmt::time(at_minute(self.days[day], bottom))
                        );
                        paint::text(cr, area, &label, x + 7.0, y + 3.0, Some(cw - 16.0), Font::new(0.78).bold(), colors.text);
                    }
                }
            }
        }
        // The current time: a line across today, a fainter one across
        // the other days, and the time in the ruler.
        let now = crate::datefmt::now();
        let today = chrono::Local::now().date_naive();
        if let Some(ti) = self.days.iter().position(|d| *d == today) {
            let wall = crate::calendar::time::local_wall(now);
            let minutes = f64::from(wall.hour() * 60 + wall.minute());
            let y = (minutes / 60.0 * hh).round();
            NOW.with_alpha(0.35).set(cr);
            cr.rectangle(GUTTER, y, width - GUTTER, 1.0);
            let _ = cr.fill();
            let x = GUTTER + ti as f64 * cw;
            NOW.set(cr);
            cr.rectangle(x, y - 0.5, cw, 2.0);
            let _ = cr.fill();
            paint::dot(cr, x + 1.0, y + 0.5, 4.5, NOW);
            let label = crate::datefmt::time(now);
            let font = Font::new(0.72).bold().tabular();
            let (lw, lh) = paint::measure(area, &label, font);
            // A pill over the hour label it would otherwise collide with.
            let px = (GUTTER - 8.0 - lw - 8.0).max(1.0);
            paint::rounded(cr, px, y - lh / 2.0 - 1.0, lw + 8.0, lh + 2.0, (lh + 2.0) / 2.0);
            NOW.set(cr);
            let _ = cr.fill();
            paint::text(cr, area, &label, px + 4.0, y - lh / 2.0, None, font, paint::WHITE);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_event(
        &self,
        area: &gtk::DrawingArea,
        cr: &cairo::Context,
        ctx: &Ctx,
        theme: &Theme,
        occ: &Occ,
        r: (f64, f64, f64, f64),
        faded: bool,
    ) {
        let standing = ctx.standing(occ);
        let selected = ctx.is_selected(occ) && !faded;
        let colors = theme.event(ctx.color(occ), selected, standing.faded() || faded);
        let (x, y, w, h) = r;
        paint::event_box(cr, &colors, x, y, w, h, standing.hatched(), true);
        self.event_text(area, cr, occ, &colors, r, None, standing.struck());
    }

    /// An event's words: its title, then its time and place where the box
    /// is tall enough.
    #[allow(clippy::too_many_arguments)]
    fn event_text(
        &self,
        area: &gtk::DrawingArea,
        cr: &cairo::Context,
        occ: &Occ,
        colors: &paint::EventColors,
        (x, y, w, h): (f64, f64, f64, f64),
        moved: Option<(usize, f64, f64)>,
        struck: bool,
    ) {
        let mut title = occ.comp.summary.clone();
        if title.is_empty() {
            title = crate::i18n::i18n("Untitled");
        }
        let (start, end) = match moved {
            Some((day, top, bottom)) => (at_minute(self.days[day], top), at_minute(self.days[day], bottom)),
            None => (occ.start, occ.end),
        };
        let time = crate::datefmt::time(start);
        let tw = w - 11.0;
        let _ = cr.save();
        cr.rectangle(x, y, w, h);
        cr.clip();
        let title_font = Font::new(0.82).bold().strike(struck);
        let (_, line_h) = paint::measure(area, "Ag", title_font);
        if h < line_h * 2.0 + 4.0 {
            let one = format!("{title}, {time}");
            paint::text(cr, area, &one, x + 7.0, y + ((h - line_h) / 2.0).max(1.0), Some(tw), title_font, colors.text);
        } else {
            // A narrow box would break its title letter by letter.
            let lines = if tw < 70.0 {
                1
            } else {
                (((h - 4.0) / line_h) as i32 - 1).clamp(1, 3)
            };
            let mut ty = y + 3.0;
            ty += paint::text(cr, area, &title, x + 7.0, ty, Some(tw), title_font.lines(lines), colors.text);
            let sub = Font::new(0.75).tabular();
            if ty + line_h * 0.8 < y + h {
                let span = if occ.end > start && tw >= 130.0 {
                    format!("{time} – {}", crate::datefmt::time(end))
                } else {
                    time
                };
                ty += paint::text(cr, area, &span, x + 7.0, ty, Some(tw), sub, colors.sub);
            }
            if !occ.comp.location.is_empty() && ty + line_h * 0.8 < y + h {
                paint::text(cr, area, &occ.comp.location, x + 7.0, ty, Some(tw), Font::new(0.75), colors.sub);
            }
        }
        let _ = cr.restore();
    }

    /// Where a drag would put things: (day, top, bottom, the event moved or
    /// `None` for a new one).
    fn ghost(&self) -> Option<Vec<Ghost>> {
        match self.drag.as_ref()? {
            Drag::Create { day, from, to, .. } => {
                let (a, b) = (from.min(*to), from.max(*to).max(from.min(*to) + 15.0));
                Some(vec![(*day, a, b, None)])
            }
            Drag::Move { occ, day, top, .. } => {
                let o = &self.occs[*occ];
                let len = ((o.end - o.start) as f64 / 60.0).max(MIN_EVENT_PX / self.hour_h * 60.0);
                let mut out = Vec::new();
                // Split where it runs past midnight.
                let (mut d, mut t, mut left) = (*day, *top, len);
                while left > 0.0 && d < self.days.len() {
                    let b = (t + left).min(1440.0);
                    out.push((d, t, b, Some(*occ)));
                    left -= b - t;
                    d += 1;
                    t = 0.0;
                }
                Some(out)
            }
            Drag::Resize { piece, bottom, .. } => {
                let p = &self.timed[*piece];
                Some(vec![(p.day, p.top, *bottom, Some(p.occ))])
            }
            _ => None,
        }
    }
}
