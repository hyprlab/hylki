//! The year view: twelve small months, each day shaded by how much is on
//! it. A day opens the day view; a month's name opens the month.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use chrono::{Datelike, Duration, NaiveDate};
use gtk::cairo;
use gtk::prelude::*;

use super::ctx::{month_name, week_of, Act, Shared};
use super::paint::{self, Font, Theme};
use crate::calendar::prefs::View;

const MONTH_W: f64 = 230.0;
const TITLE_H: f64 = 30.0;
const ROW_H: f64 = 26.0;

struct Year {
    ctx: Shared,
    year: i32,
    /// Events per day, and the color of the first.
    busy: HashMap<NaiveDate, (usize, paint::Rgba)>,
}

impl Year {
    fn cols(&self, w: f64) -> usize {
        ((w / MONTH_W).floor() as usize).clamp(1, 4)
    }

    fn month_h() -> f64 {
        TITLE_H + ROW_H * 7.0 + 12.0
    }

    fn origin(&self, m: usize, w: f64) -> (f64, f64, f64) {
        let cols = self.cols(w);
        let cw = w / cols as f64;
        let (row, col) = (m / cols, m % cols);
        (col as f64 * cw, row as f64 * Year::month_h(), cw)
    }

    fn first_day(&self, m: usize) -> NaiveDate {
        NaiveDate::from_ymd_opt(self.year, m as u32 + 1, 1).unwrap_or_default()
    }

    fn draw(&self, area: &gtk::DrawingArea, cr: &cairo::Context, w: f64) {
        let ctx = self.ctx.borrow();
        let theme = Theme::of(area);
        let today = chrono::Local::now().date_naive();
        for m in 0..12 {
            let (ox, oy, cw) = self.origin(m, w);
            let first = self.first_day(m);
            let pad = 14.0;
            let cell = (cw - 2.0 * pad) / 7.0;
            let current = today.year() == self.year && today.month0() as usize == m;
            let title_color = if current { theme.accent } else { theme.fg };
            paint::text(cr, area, &month_name(first), ox + pad + 4.0, oy + 8.0, None, Font::new(1.05).heavy(), title_color);
            let start = week_of(first, ctx.week_start);
            for c in 0..7 {
                let d = start + Duration::days(c);
                let initial: String = super::ctx::weekday_short(d).chars().take(1).collect();
                paint::centred(cr, area, &initial, ox + pad + (c as f64 + 0.5) * cell, oy + TITLE_H, Font::new(0.75).bold(), theme.faint);
            }
            for i in 0..42 {
                let d = start + Duration::days(i);
                if d.month0() as usize != m || d.year() != self.year {
                    continue;
                }
                let (row, col) = ((i / 7) as f64, (i % 7) as f64);
                let cx = ox + pad + (col + 0.5) * cell;
                let cy = oy + TITLE_H + ROW_H * (row + 1.0) + ROW_H / 2.0;
                let r = (cell.min(ROW_H) / 2.0 - 1.5).max(6.0);
                let num = d.day().to_string();
                let font = Font::new(0.82).tabular();
                if d == today {
                    paint::dot(cr, cx, cy, r, theme.accent);
                    let (_, nh) = paint::measure(area, &num, font.bold());
                    paint::centred(cr, area, &num, cx, cy - nh / 2.0, font.bold(), theme.accent_fg);
                    continue;
                }
                let tone = match self.busy.get(&d) {
                    Some(&(n, color)) => {
                        let a = match n {
                            1 => 0.18,
                            2 | 3 => 0.32,
                            _ => 0.5,
                        };
                        paint::dot(cr, cx, cy, r, color.with_alpha(a * if theme.dark { 1.3 } else { 1.0 }));
                        theme.fg
                    }
                    None => theme.dim,
                };
                let (_, nh) = paint::measure(area, &num, font);
                paint::centred(cr, area, &num, cx, cy - nh / 2.0, font, tone);
            }
        }
    }

    fn hit(&self, x: f64, y: f64, w: f64) -> Option<(NaiveDate, bool)> {
        let ctx = self.ctx.borrow();
        for m in 0..12 {
            let (ox, oy, cw) = self.origin(m, w);
            if x < ox || x >= ox + cw || y < oy || y >= oy + Year::month_h() {
                continue;
            }
            let first = self.first_day(m);
            if y < oy + TITLE_H {
                return Some((first, true));
            }
            let pad = 14.0;
            let cell = (cw - 2.0 * pad) / 7.0;
            let col = ((x - ox - pad) / cell).floor();
            let row = ((y - oy - TITLE_H) / ROW_H).floor() - 1.0;
            if !(0.0..7.0).contains(&col) || !(0.0..6.0).contains(&row) {
                return None;
            }
            let d = week_of(first, ctx.week_start) + Duration::days((row * 7.0 + col) as i64);
            return (d.month0() as usize == m).then_some((d, false));
        }
        None
    }
}

#[derive(Clone)]
pub(crate) struct YearGrid {
    pub(crate) root: gtk::ScrolledWindow,
    area: gtk::DrawingArea,
    model: Rc<RefCell<Year>>,
}

impl YearGrid {
    pub(crate) fn new(ctx: Shared) -> YearGrid {
        let model = Rc::new(RefCell::new(Year {
            ctx,
            year: chrono::Local::now().year(),
            busy: HashMap::new(),
        }));
        let area = gtk::DrawingArea::new();
        area.set_hexpand(true);
        let root = gtk::ScrolledWindow::new();
        root.set_hscrollbar_policy(gtk::PolicyType::Never);
        root.set_vexpand(true);
        root.set_child(Some(&area));
        root.add_css_class("calendar-year");
        let grid = YearGrid { root, area, model };
        let m = grid.model.clone();
        grid.area.set_draw_func(move |a, cr, w, _| m.borrow().draw(a, cr, f64::from(w)));
        // Rows of months: as many as the width leaves.
        let m = grid.model.clone();
        grid.area.connect_resize(move |a, w, _| {
            let cols = m.borrow().cols(f64::from(w));
            let rows = 12_usize.div_ceil(cols);
            a.set_content_height((rows as f64 * Year::month_h() + 8.0) as i32);
        });
        let click = gtk::GestureClick::new();
        let this = grid.clone();
        click.connect_released(move |_, _, x, y| {
            let w = f64::from(this.area.width());
            let hit = this.model.borrow().hit(x, y, w);
            if let Some((day, title)) = hit {
                let view = if title { View::Month } else { View::Day };
                let ctx = this.model.borrow().ctx.clone();
                ctx.borrow().emit(Act::Show(day, Some(view)));
            }
        });
        grid.area.add_controller(click);
        grid
    }

    pub(crate) fn set(&self, year: i32, busy: HashMap<NaiveDate, (usize, paint::Rgba)>) {
        {
            let mut m = self.model.borrow_mut();
            m.year = year;
            m.busy = busy;
        }
        self.area.queue_draw();
    }

    pub(crate) fn redraw(&self) {
        self.area.queue_draw();
    }
}
