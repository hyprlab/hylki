//! The day and week views: the days across the top, the all-day row under
//! them, and the hours below, scrolling.
//!
//! Pointer work, as in Apple's Calendar and GNOME's: drag across empty time
//! to make an event, drag an event to move it, drag its bottom edge to make
//! it longer or shorter, double-click to open it or to make a new one,
//! right-click for its menu. Times snap to the quarter hour. Ctrl and the
//! wheel change how tall an hour is.

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{Duration, NaiveDate};
use gtk::glib;
use gtk::prelude::*;

use super::ctx::{Act, Anchor, Shared};
use super::grid_draw::{at_minute, layout, Bar, Placed};
use crate::calendar::model::Occ;
use crate::calendar::time::day_start;

/// The hour ruler's width.
pub(super) const GUTTER: f64 = 72.0;
pub(super) const HEADER_H: f64 = 44.0;
pub(super) const ALLDAY_LANE: f64 = 24.0;
/// The shortest an event is drawn, whatever its length.
pub(super) const MIN_EVENT_PX: f64 = 20.0;
const SNAP: f64 = 15.0;
/// How far the pointer goes before a press becomes a drag.
const DRAG_START: f64 = 4.0;

#[derive(Debug, Clone)]
pub(super) enum Drag {
    /// Pressed, not yet moved far enough to say what it is.
    Pending { x: f64, y: f64, on: Option<usize>, edge: bool },
    Create { day: usize, from: f64, to: f64 },
    Move { occ: usize, day: usize, top: f64, grab: f64, grab_day: i64 },
    Resize { piece: usize, bottom: f64 },
    BarPending { x: f64, on: Option<usize> },
    BarMove { occ: usize, from_day: usize, days: i64 },
    BarCreate { from: usize, to: usize },
}

impl Drag {
    pub(super) fn moving_occ(&self) -> Option<usize> {
        match self {
            Drag::Move { occ, .. } => Some(*occ),
            _ => None,
        }
    }
    pub(super) fn bar_occ(&self) -> Option<usize> {
        match self {
            Drag::BarMove { occ, .. } => Some(*occ),
            _ => None,
        }
    }
}

pub(super) struct Grid {
    pub(super) ctx: Shared,
    pub(super) days: Vec<NaiveDate>,
    pub(super) occs: Rc<Vec<Occ>>,
    pub(super) timed: Vec<Placed>,
    pub(super) bars: Vec<Bar>,
    pub(super) lanes: usize,
    pub(super) expanded: bool,
    pub(super) hour_h: f64,
    pub(super) drag: Option<Drag>,
    pub(super) default_minutes: u32,
    /// Where the hours were scrolled to, kept when the days change.
    scrolled: bool,
}

/// What a Ctrl+wheel zoom reports its new hour height to.
type ZoomCallback = Rc<RefCell<Option<Box<dyn Fn(f64)>>>>;

#[derive(Clone)]
pub(crate) struct TimeGrid {
    pub(crate) root: gtk::Box,
    header: gtk::DrawingArea,
    allday: gtk::DrawingArea,
    body: gtk::DrawingArea,
    scroller: gtk::ScrolledWindow,
    model: Rc<RefCell<Grid>>,
    /// Ctrl+wheel zoom reports the new height here, to be remembered.
    on_zoom: ZoomCallback,
    /// Set by the second press of a double click: the press's release
    /// ends the drag gesture too, and must not count as a click of its own
    /// (it would close the popover the double click just opened).
    double: Rc<std::cell::Cell<bool>>,
}

impl TimeGrid {
    pub(crate) fn new(ctx: Shared, hour_h: f64) -> TimeGrid {
        let model = Rc::new(RefCell::new(Grid {
            ctx,
            days: Vec::new(),
            occs: Rc::new(Vec::new()),
            timed: Vec::new(),
            bars: Vec::new(),
            lanes: 0,
            expanded: false,
            hour_h,
            drag: None,
            default_minutes: 60,
            scrolled: false,
        }));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("calendar-grid");
        let header = gtk::DrawingArea::new();
        header.set_content_height(HEADER_H as i32);
        let allday = gtk::DrawingArea::new();
        let body = gtk::DrawingArea::new();
        body.set_focusable(false);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_vexpand(true);
        scroller.set_child(Some(&body));
        root.append(&header);
        root.append(&allday);
        root.append(&scroller);
        let grid = TimeGrid {
            root,
            header,
            allday,
            body,
            scroller,
            model,
            on_zoom: Rc::new(RefCell::new(None)),
            double: Rc::new(std::cell::Cell::new(false)),
        };
        grid.connect_drawing();
        grid.connect_body_gestures();
        grid.connect_allday_gestures();
        grid.connect_header_clicks();
        grid.connect_zoom();
        grid.resize();
        // The line at the current time moves once a minute.
        let body = grid.body.downgrade();
        glib::timeout_add_seconds_local(30, move || match body.upgrade() {
            Some(b) => {
                b.queue_draw();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        grid
    }

    pub(crate) fn on_zoom(&self, f: impl Fn(f64) + 'static) {
        *self.on_zoom.borrow_mut() = Some(Box::new(f));
    }

    /// Show `days` with `occs`.
    pub(crate) fn set(&self, days: Vec<NaiveDate>, occs: Rc<Vec<Occ>>, default_minutes: u32) {
        {
            let mut m = self.model.borrow_mut();
            let (timed, bars, lanes) = layout(&occs, &days, m.hour_h);
            m.timed = timed;
            m.bars = bars;
            m.lanes = lanes;
            m.days = days;
            m.occs = occs;
            m.default_minutes = default_minutes;
            m.drag = None;
        }
        self.resize();
        self.redraw();
    }

    /// Leave out the row of day names, where the day is named elsewhere.
    pub(crate) fn hide_days(&self) {
        self.header.set_visible(false);
    }

    /// Where an occurrence is drawn, for a popover to point at.
    pub(crate) fn anchor_of(&self, key: &crate::calendar::model::OccKey) -> Option<Anchor> {
        let m = self.model.borrow();
        let i = m.timed.iter().position(|p| m.occs[p.occ].key == *key)?;
        drop(m);
        Some(self.piece_anchor(i))
    }

    pub(crate) fn redraw(&self) {
        self.header.queue_draw();
        self.allday.queue_draw();
        self.body.queue_draw();
    }

    fn resize(&self) {
        let m = self.model.borrow();
        let shown = m.shown_lanes();
        let extra = if m.lanes > shown { 1.0 } else { 0.0 };
        let h = 4.0 + (shown as f64 + extra).max(1.0) * ALLDAY_LANE;
        self.allday.set_content_height(h as i32);
        self.body.set_content_height((24.0 * m.hour_h + 2.0) as i32);
    }

    /// Scroll so the working day starts near the top, or the current time
    /// shows when today is in view. Done once; later visits keep the place.
    pub(crate) fn scroll_to_start(&self, force: bool) {
        if self.model.borrow().scrolled && !force {
            return;
        }
        let this = self.clone();
        // The scroller has no height until it is drawn.
        self.scroller.add_tick_callback(move |sc, _| {
            let adj = sc.vadjustment();
            if adj.upper() <= 1.0 || adj.page_size() <= 1.0 {
                return glib::ControlFlow::Continue;
            }
            let mut m = this.model.borrow_mut();
            let today = chrono::Local::now().date_naive();
            let work = f64::from(m.ctx.borrow().work.0);
            let hour = if m.days.contains(&today) {
                let wall = crate::calendar::time::local_wall(crate::datefmt::now());
                let now = f64::from(chrono::Timelike::hour(&wall));
                // The current hour a third of the way down, but never
                // above the start of the working day if both fit.
                (now - adj.page_size() / m.hour_h / 3.0).min(work).max(0.0)
            } else {
                work - 0.5
            };
            // A little above the hour, so its label isn't cut in half.
            let y = hour.max(0.0) * m.hour_h - 10.0;
            adj.set_value(y.clamp(0.0, adj.upper() - adj.page_size()));
            m.scrolled = true;
            glib::ControlFlow::Break
        });
    }

    fn connect_drawing(&self) {
        let m = self.model.clone();
        self.header.set_draw_func(move |a, cr, w, _| m.borrow().draw_header(a, cr, f64::from(w)));
        let m = self.model.clone();
        self.allday
            .set_draw_func(move |a, cr, w, h| m.borrow().draw_allday(a, cr, f64::from(w), f64::from(h)));
        let m = self.model.clone();
        self.body
            .set_draw_func(move |a, cr, w, h| m.borrow().draw_body(a, cr, f64::from(w), f64::from(h)));
    }

    /// The day column and minute under a point of the hour grid.
    fn slot(&self, x: f64, y: f64) -> (usize, f64) {
        let m = self.model.borrow();
        let w = f64::from(self.body.width());
        let col = ((x - GUTTER) / m.col_w(w)).floor().max(0.0) as usize;
        let col = col.min(m.days.len().saturating_sub(1));
        let minute = (y / m.hour_h * 60.0).clamp(0.0, 1440.0);
        (col, minute)
    }

    /// The piece of an event under a point, the topmost first, and whether
    /// the point is on its bottom edge.
    fn hit(&self, x: f64, y: f64) -> Option<(usize, bool)> {
        let m = self.model.borrow();
        let w = f64::from(self.body.width());
        m.timed.iter().enumerate().rev().find_map(|(i, p)| {
            let (rx, ry, rw, rh) = m.rect(p, w);
            (x >= rx && x <= rx + rw && y >= ry && y <= ry + rh)
                .then_some((i, p.ends_here && y >= ry + rh - 6.0 && rh > 14.0))
        })
    }

    fn emit(&self, act: Act) {
        let ctx = self.model.borrow().ctx.clone();
        let ctx = ctx.borrow();
        ctx.emit(act);
    }

    fn piece_anchor(&self, piece: usize) -> Anchor {
        let m = self.model.borrow();
        let w = f64::from(self.body.width());
        let (x, y, pw, ph) = m.rect(&m.timed[piece], w);
        Anchor::new(&self.body, x, y, pw, ph)
    }

    fn connect_body_gestures(&self) {
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let this = self.clone();
        drag.connect_drag_begin(move |_, x, y| {
            let hit = this.hit(x, y);
            this.model.borrow_mut().drag = Some(Drag::Pending {
                x,
                y,
                on: hit.map(|h| h.0),
                edge: hit.is_some_and(|h| h.1),
            });
        });
        let this = self.clone();
        drag.connect_drag_update(move |g, dx, dy| {
            let Some((sx, sy)) = g.start_point() else {
                return;
            };
            let (x, y) = (sx + dx, sy + dy);
            this.drag_to(x, y, dx, dy);
            this.autoscroll(y);
        });
        let this = self.clone();
        drag.connect_drag_end(move |_, _, _| this.drop_drag());
        self.body.add_controller(drag);

        let click = gtk::GestureClick::new();
        click.set_button(1);
        let this = self.clone();
        click.connect_pressed(move |_, n, x, y| {
            if n != 2 {
                return;
            }
            this.double.set(true);
            match this.hit(x, y) {
                Some((piece, _)) => {
                    let key = {
                        let m = this.model.borrow();
                        m.occs[m.timed[piece].occ].key.clone()
                    };
                    this.emit(Act::Open(key));
                }
                None => {
                    let (col, minute) = this.slot(x, y);
                    let (day, len) = {
                        let m = this.model.borrow();
                        (m.days[col], f64::from(m.default_minutes))
                    };
                    let from = (minute / 30.0).floor() * 30.0;
                    let start = at_minute(day, from);
                    let end = at_minute(day, (from + len).min(1440.0));
                    let anchor = this.minute_anchor(col, from, from + len);
                    this.emit(Act::Create {
                        start,
                        end,
                        all_day: false,
                        anchor,
                    });
                }
            }
        });
        self.body.add_controller(click);

        let menu = gtk::GestureClick::new();
        menu.set_button(3);
        let this = self.clone();
        menu.connect_pressed(move |_, _, x, y| {
            let hit = this.hit(x, y);
            let (col, minute) = this.slot(x, y);
            let (key, at) = {
                let m = this.model.borrow();
                let key = hit.map(|(p, _)| m.occs[m.timed[p].occ].key.clone());
                (key, at_minute(m.days[col], (minute / 30.0).floor() * 30.0))
            };
            this.emit(Act::Menu {
                key,
                at: Some(at),
                all_day: false,
                anchor: Anchor::new(&this.body, x, y, 1.0, 1.0),
            });
        });
        self.body.add_controller(menu);

        let motion = gtk::EventControllerMotion::new();
        let this = self.clone();
        motion.connect_motion(move |_, x, y| {
            let cursor = match this.hit(x, y) {
                Some((p, true)) if this.writable_piece(p) => Some("ns-resize"),
                _ => None,
            };
            this.body.set_cursor_from_name(cursor);
        });
        self.body.add_controller(motion);
    }

    fn writable_piece(&self, piece: usize) -> bool {
        let m = self.model.borrow();
        let occ = &m.occs[m.timed[piece].occ];
        let can = m.ctx.borrow().can_change(occ);
        can
    }

    fn minute_anchor(&self, col: usize, from: f64, to: f64) -> Anchor {
        let m = self.model.borrow();
        let w = f64::from(self.body.width());
        let cw = m.col_w(w);
        let x = GUTTER + col as f64 * cw;
        Anchor::new(&self.body, x, from / 60.0 * m.hour_h, cw - 4.0, (to - from) / 60.0 * m.hour_h)
    }

    fn drag_to(&self, x: f64, y: f64, dx: f64, dy: f64) {
        let (col, minute) = self.slot(x, y);
        let snapped = (minute / SNAP).round() * SNAP;
        let drag = self.model.borrow().drag.clone();
        let next = match drag {
            Some(Drag::Pending { x: sx, y: sy, on, edge }) => {
                if dx.hypot(dy) < DRAG_START {
                    return;
                }
                let (scol, sminute) = self.slot(sx, sy);
                match on {
                    Some(p) if !self.writable_piece(p) => None,
                    Some(p) if edge => Some(Drag::Resize {
                        piece: p,
                        bottom: snapped,
                    }),
                    Some(p) => {
                        let m = self.model.borrow();
                        let piece = &m.timed[p];
                        let occ = &m.occs[piece.occ];
                        // How far into the event the pointer took hold,
                        // counted from its real start (it may have begun
                        // the day before).
                        let start_day = day_start(m.days[piece.day]);
                        let real_top = (occ.start - start_day) as f64 / 60.0;
                        Some(Drag::Move {
                            occ: piece.occ,
                            day: piece.day,
                            top: real_top.max(0.0),
                            grab: sminute - real_top,
                            grab_day: scol as i64,
                        })
                    }
                    None => Some(Drag::Create {
                        day: scol,
                        from: (sminute / SNAP).floor() * SNAP,
                        to: snapped,
                    }),
                }
            }
            Some(Drag::Create { day, from, .. }) => Some(Drag::Create {
                day,
                from,
                to: snapped,
            }),
            Some(Drag::Move { occ, grab, grab_day, .. }) => {
                let top = ((minute - grab) / SNAP).round() * SNAP;
                let _ = grab_day;
                Some(Drag::Move {
                    occ,
                    day: col,
                    top: top.clamp(-1440.0, 1440.0 - SNAP),
                    grab,
                    grab_day,
                })
            }
            Some(Drag::Resize { piece, .. }) => {
                let top = self.model.borrow().timed[piece].top;
                Some(Drag::Resize {
                    piece,
                    bottom: snapped.max(top + SNAP),
                })
            }
            other => other,
        };
        self.model.borrow_mut().drag = next;
        self.body.queue_draw();
    }

    /// Scroll the hours while a drag nears the top or bottom of the view.
    fn autoscroll(&self, y: f64) {
        if !matches!(
            self.model.borrow().drag,
            Some(Drag::Create { .. } | Drag::Move { .. } | Drag::Resize { .. })
        ) {
            return;
        }
        let adj = self.scroller.vadjustment();
        let top = adj.value();
        let bottom = top + adj.page_size();
        if y < top + 24.0 {
            adj.set_value((top - 12.0).max(0.0));
        } else if y > bottom - 24.0 {
            adj.set_value((top + 12.0).min(adj.upper() - adj.page_size()));
        }
    }

    fn drop_drag(&self) {
        let drag = self.model.borrow_mut().drag.take();
        match drag {
            Some(Drag::Pending { .. }) if self.double.replace(false) => {}
            Some(Drag::Pending { on, .. }) => {
                let act = match on {
                    Some(p) => {
                        let key = {
                            let m = self.model.borrow();
                            m.occs[m.timed[p].occ].key.clone()
                        };
                        Act::Select(Some(key), Some(self.piece_anchor(p)))
                    }
                    None => Act::Select(None, None),
                };
                self.emit(act);
            }
            Some(Drag::Create { day, from, to }) => {
                let (a, b) = (from.min(to), from.max(to).max(from.min(to) + SNAP));
                let d = self.model.borrow().days[day];
                let anchor = self.minute_anchor(day, a, b);
                self.emit(Act::Create {
                    start: at_minute(d, a),
                    end: at_minute(d, b),
                    all_day: false,
                    anchor,
                });
            }
            Some(Drag::Move { occ, day, top, .. }) => {
                let (key, start, end) = {
                    let m = self.model.borrow();
                    let o = &m.occs[occ];
                    let d = m.days[day];
                    let start = if top < 0.0 {
                        at_minute(d - Duration::days(1), 1440.0 + top)
                    } else {
                        at_minute(d, top)
                    };
                    (o.key.clone(), start, start + (o.end - o.start))
                };
                let was = {
                    let m = self.model.borrow();
                    m.occs[occ].start
                };
                if start != was {
                    self.emit(Act::Move { key, start, end });
                }
            }
            Some(Drag::Resize { piece, bottom }) => {
                let (key, start, end, was) = {
                    let m = self.model.borrow();
                    let p = &m.timed[piece];
                    let o = &m.occs[p.occ];
                    (o.key.clone(), o.start, at_minute(m.days[p.day], bottom), o.end)
                };
                if end != was {
                    self.emit(Act::Move { key, start, end });
                }
            }
            _ => {}
        }
        self.double.set(false);
        self.body.queue_draw();
    }

    /// The bar under a point of the all-day row, and the day under it.
    fn bar_hit(&self, x: f64, y: f64) -> (Option<usize>, usize) {
        let m = self.model.borrow();
        let w = f64::from(self.allday.width());
        let shown = m.shown_lanes();
        let col = (((x - GUTTER) / m.col_w(w)).floor().max(0.0) as usize).min(m.days.len().saturating_sub(1));
        let bar = m.bars.iter().position(|b| {
            if b.lane >= shown {
                return false;
            }
            let (bx, by, bw, bh) = m.bar_rect(b, w);
            x >= bx && x <= bx + bw && y >= by && y <= by + bh
        });
        (bar, col)
    }

    fn connect_allday_gestures(&self) {
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let this = self.clone();
        drag.connect_drag_begin(move |_, x, y| {
            let (bar, _) = this.bar_hit(x, y);
            this.model.borrow_mut().drag = Some(Drag::BarPending { x, on: bar });
        });
        let this = self.clone();
        drag.connect_drag_update(move |g, dx, dy| {
            let Some((sx, sy)) = g.start_point() else {
                return;
            };
            let (_, col) = this.bar_hit(sx + dx, sy + dy);
            let drag = this.model.borrow().drag.clone();
            let next = match drag {
                Some(Drag::BarPending { x, on }) => {
                    if dx.hypot(dy) < DRAG_START {
                        return;
                    }
                    let (_, start_col) = this.bar_hit(x, sy);
                    match on {
                        Some(b) => {
                            let m = this.model.borrow();
                            let occ = m.bars[b].occ;
                            if !m.ctx.borrow().can_change(&m.occs[occ]) {
                                None
                            } else {
                                Some(Drag::BarMove {
                                    occ,
                                    from_day: start_col,
                                    days: col as i64 - start_col as i64,
                                })
                            }
                        }
                        None => Some(Drag::BarCreate {
                            from: start_col,
                            to: col,
                        }),
                    }
                }
                Some(Drag::BarMove { occ, from_day, .. }) => Some(Drag::BarMove {
                    occ,
                    from_day,
                    days: col as i64 - from_day as i64,
                }),
                Some(Drag::BarCreate { from, .. }) => Some(Drag::BarCreate { from, to: col }),
                other => other,
            };
            this.model.borrow_mut().drag = next;
            this.allday.queue_draw();
        });
        let this = self.clone();
        drag.connect_drag_end(move |_, _, _| {
            let drag = this.model.borrow_mut().drag.take();
            match drag {
                Some(Drag::BarPending { .. }) if this.double.replace(false) => {}
                Some(Drag::BarPending { on, .. }) => match on {
                    Some(b) => {
                        let (key, anchor) = {
                            let m = this.model.borrow();
                            let w = f64::from(this.allday.width());
                            let (x, y, bw, bh) = m.bar_rect(&m.bars[b], w);
                            (
                                m.occs[m.bars[b].occ].key.clone(),
                                Anchor::new(&this.allday, x, y, bw, bh),
                            )
                        };
                        this.emit(Act::Select(Some(key), Some(anchor)));
                    }
                    None => {
                        // A click below the bars shows them all.
                        let fold = {
                            let m = this.model.borrow();
                            m.lanes > 3
                        };
                        if fold {
                            let mut m = this.model.borrow_mut();
                            m.expanded = !m.expanded;
                        }
                        if fold {
                            this.resize();
                        } else {
                            this.emit(Act::Select(None, None));
                        }
                    }
                },
                Some(Drag::BarMove { occ, days, .. }) if days != 0 => {
                    let (key, start, end) = {
                        let m = this.model.borrow();
                        let o = &m.occs[occ];
                        let shift = |t: i64| {
                            let day = crate::calendar::time::local_day(t) + Duration::days(days);
                            let wall = crate::calendar::time::local_wall(t);
                            crate::calendar::time::local_unix(day.and_time(wall.time()))
                        };
                        (o.key.clone(), shift(o.start), shift(o.end))
                    };
                    this.emit(Act::Move { key, start, end });
                }
                Some(Drag::BarCreate { from, to }) => {
                    let (a, b) = (from.min(to), from.max(to));
                    let (start, end, anchor) = {
                        let m = this.model.borrow();
                        let w = f64::from(this.allday.width());
                        let cw = m.col_w(w);
                        (
                            day_start(m.days[a]),
                            day_start(m.days[b] + Duration::days(1)),
                            Anchor::new(&this.allday, GUTTER + a as f64 * cw, 2.0, (b - a + 1) as f64 * cw, ALLDAY_LANE),
                        )
                    };
                    this.emit(Act::Create {
                        start,
                        end,
                        all_day: true,
                        anchor,
                    });
                }
                _ => {}
            }
            this.double.set(false);
            this.allday.queue_draw();
        });
        self.allday.add_controller(drag);

        let click = gtk::GestureClick::new();
        let this = self.clone();
        click.connect_pressed(move |_, n, x, y| {
            if n != 2 {
                return;
            }
            this.double.set(true);
            let (bar, col) = this.bar_hit(x, y);
            match bar {
                Some(b) => {
                    let key = {
                        let m = this.model.borrow();
                        m.occs[m.bars[b].occ].key.clone()
                    };
                    this.emit(Act::Open(key));
                }
                None => {
                    let (start, anchor) = {
                        let m = this.model.borrow();
                        let w = f64::from(this.allday.width());
                        let cw = m.col_w(w);
                        (
                            day_start(m.days[col]),
                            Anchor::new(&this.allday, GUTTER + col as f64 * cw, 2.0, cw, ALLDAY_LANE),
                        )
                    };
                    this.emit(Act::Create {
                        start,
                        end: start + 86_400,
                        all_day: true,
                        anchor,
                    });
                }
            }
        });
        self.allday.add_controller(click);

        let menu = gtk::GestureClick::new();
        menu.set_button(3);
        let this = self.clone();
        menu.connect_pressed(move |_, _, x, y| {
            let (bar, col) = this.bar_hit(x, y);
            let (key, at) = {
                let m = this.model.borrow();
                (bar.map(|b| m.occs[m.bars[b].occ].key.clone()), day_start(m.days[col]))
            };
            this.emit(Act::Menu {
                key,
                at: Some(at),
                all_day: true,
                anchor: Anchor::new(&this.allday, x, y, 1.0, 1.0),
            });
        });
        self.allday.add_controller(menu);
    }

    fn connect_header_clicks(&self) {
        let click = gtk::GestureClick::new();
        let this = self.clone();
        click.connect_released(move |_, _, x, _| {
            let day = {
                let m = this.model.borrow();
                let w = f64::from(this.header.width());
                if x < GUTTER || m.days.len() < 2 {
                    return;
                }
                let col = (((x - GUTTER) / m.col_w(w)).floor() as usize).min(m.days.len() - 1);
                m.days[col]
            };
            this.emit(Act::Show(day, Some(crate::calendar::prefs::View::Day)));
        });
        self.header.add_controller(click);
        self.header.set_cursor_from_name(Some("pointer"));
    }

    fn connect_zoom(&self) {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        // Ahead of the scroller's own handling, which would scroll instead.
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        let this = self.clone();
        scroll.connect_scroll(move |c, _, dy| {
            if !c.current_event_state().contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                return glib::Propagation::Proceed;
            }
            let adj = this.scroller.vadjustment();
            let (old, new) = {
                let mut m = this.model.borrow_mut();
                let old = m.hour_h;
                m.hour_h = (old * if dy < 0.0 { 1.12 } else { 1.0 / 1.12 }).clamp(24.0, 160.0);
                let (timed, bars, lanes) = layout(&m.occs, &m.days, m.hour_h);
                m.timed = timed;
                m.bars = bars;
                m.lanes = lanes;
                (old, m.hour_h)
            };
            // Keep the hour in the middle of the view where it was.
            let mid = adj.value() + adj.page_size() / 2.0;
            let hour = mid / old;
            this.resize();
            let adj2 = adj.clone();
            glib::idle_add_local_once(move || {
                adj2.set_value((hour * new - adj2.page_size() / 2.0).max(0.0));
            });
            this.body.queue_draw();
            if let Some(f) = this.on_zoom.borrow().as_ref() {
                f(new);
            }
            glib::Propagation::Stop
        });
        self.scroller.add_controller(scroll);
    }
}
