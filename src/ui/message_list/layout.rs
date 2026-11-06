//! The list's layout: the bars over and under it, columns and headings,
//! the look of a row, scroll keeping, and the per-account tint.

use super::sort::{column_sort, SortOrder};
use super::types::MessageListInput;
use super::MessageList;
use crate::ui::message_row::RowShared;
use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;
use std::rc::Rc;

/// The message-list pane's floor. Rows ellipsize, so this is what keeps a
/// card's sender, date and subject readable beside its avatar, not a limit the
/// rows set themselves.
pub(super) const LIST_MIN_WIDTH: i32 = 260;

/// The pane width the selection bar's buttons and its count need at full size.
/// Narrower, the buttons draw tighter so the last still fits at the floor.
pub(super) const BULK_BAR_ROOM: i32 = 344;

/// What an expanded conversation needs beyond [`LIST_MIN_WIDTH`]: the member
/// cards' 10px rail indent plus their card margin/padding beyond a plain
/// pill's. The pane's floor grows by this while any thread is open, so the
/// cards' (and the head pill's) right inset is never clipped off the pane.
const THREAD_EXPANDED_EXTRA: i32 = 12;

/// Fire `DayChanged` shortly after the next local midnight (re-armed each time).
pub(super) fn schedule_midnight_refresh(sender: &ComponentSender<MessageList>) {
    use chrono::Timelike;
    let secs = 86_400u32
        .saturating_sub(chrono::Local::now().num_seconds_from_midnight())
        .saturating_add(2);
    let input = sender.input_sender().clone();
    gtk::glib::timeout_add_seconds_local(secs, move || {
        let _ = input.send(MessageListInput::DayChanged);
        gtk::glib::ControlFlow::Break
    });
}

/// Run `f` once, when `clock` has painted its next frame, or after a short
/// wait for a window that is not drawing.
pub(crate) fn after_next_paint(clock: &gtk::gdk::FrameClock, f: impl FnOnce() + 'static) {
    let f = Rc::new(std::cell::Cell::new(Some(Box::new(f) as Box<dyn FnOnce()>)));
    let handler = Rc::new(std::cell::Cell::new(None::<glib::SignalHandlerId>));
    let once = {
        let (f, handler, clock) = (f.clone(), handler.clone(), clock.clone());
        move || {
            if let Some(id) = handler.take() {
                clock.disconnect(id);
            }
            if let Some(f) = f.take() {
                f();
            }
        }
    };
    let on_paint = once.clone();
    handler.set(Some(clock.connect_after_paint(move |_| on_paint())));
    glib::timeout_add_local_once(std::time::Duration::from_millis(100), once);
}

/// Keep `adj` at the very top for a moment (#435). A `scroll_to` the first
/// row lines that row up with the top once the view is laid out, which
/// leaves the date heading above the row out of sight. Only a move no
/// further than that heading is undone, and only briefly, so a scroll of the
/// user's own goes on. It is undone after the layout, not during it: the
/// view does not follow a move made while it is being laid out.
pub(super) fn hold_top(adj: &gtk::Adjustment, shared: &Rc<RowShared>) {
    let shared = Rc::downgrade(shared);
    let id = adj.connect_value_changed(move |adj| {
        let heading = shared
            .upgrade()
            .and_then(|s| s.first_heading_height())
            .map_or(f64::MAX, f64::from);
        if adj.value() > 0.0 && adj.value() <= heading + 1.0 {
            let adj = adj.clone();
            glib::idle_add_local_once(move || adj.set_value(0.0));
        }
    });
    let adj = adj.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
        adj.disconnect(id)
    });
}

/// The search field's and the bulk bar's revealers, and the boxes above
/// and below the rows that hold them (#496).
pub(super) struct BarSlots {
    pub(super) top: gtk::Box,
    pub(super) bottom: gtk::Box,
    pub(super) search: gtk::Revealer,
    pub(super) bulk: gtk::Revealer,
}

impl MessageList {
    /// A point in the rows list, in the window's coordinates (the app
    /// anchors popovers on the window; falls back to the point as given).
    pub(super) fn window_point(&self, x: f64, y: f64) -> (f64, f64) {
        let list = &self.list_view;
        list.root()
            .and_then(|root| {
                let root: gtk::Widget = root.upcast();
                list.compute_point(&root, &gtk::graphene::Point::new(x as f32, y as f32))
            })
            .map_or((x, y), |p| (p.x() as f64, p.y() as f64))
    }

    /// Toolbar count: every message the filter lets through, all listed.
    pub(super) fn count_label(&self) -> String {
        format!("{}", self.total_matches)
    }

    /// Whether the bottom loading spinner should show: the folder's index is
    /// still streaming in and the user has reached the end of what is here.
    pub(super) fn is_loading_more(&self) -> bool {
        !self.index_complete && (self.at_bottom || self.shown.is_empty())
    }

    /// Put the search field and the bulk bar above or under the rows, as
    /// set (#496). Sharing a side, the search field is the outer one, so
    /// a selection coming and going never moves it.
    pub(super) fn place_bars(&self) {
        let Some(bars) = &self.bars else { return };
        for revealer in [&bars.search, &bars.bulk] {
            if let Some(parent) = revealer.parent().and_downcast::<gtk::Box>() {
                parent.remove(revealer);
            }
        }
        let slide = |bottom: bool| {
            if bottom {
                gtk::RevealerTransitionType::SlideUp
            } else {
                gtk::RevealerTransitionType::SlideDown
            }
        };
        bars.search.set_transition_type(slide(self.search_bottom));
        bars.bulk.set_transition_type(slide(self.bulk_bottom));
        if !self.search_bottom {
            bars.top.append(&bars.search);
        }
        if self.bulk_bottom {
            bars.bottom.append(&bars.bulk);
        } else {
            bars.top.append(&bars.bulk);
        }
        if self.search_bottom {
            bars.bottom.append(&bars.search);
        }
        if let Some(picker) = &self.scope_picker {
            // The scope menu opens toward the rows.
            picker.button.set_direction(if self.search_bottom {
                gtk::ArrowType::Up
            } else {
                gtk::ArrowType::Down
            });
            picker.arrow.set_icon_name(Some(if self.search_bottom {
                "pan-up-symbolic"
            } else {
                "pan-down-symbolic"
            }));
        }
    }

    /// Whether to show the empty-folder placeholder: the folder has loaded,
    /// nothing is in it, no search is filtering it, and no more rows are on
    /// their way (the loading indicator covers that state instead).
    pub(super) fn is_empty_state(&self) -> bool {
        self.loaded && self.shown.is_empty() && self.query.is_empty() && self.index_complete
    }

    /// Runs `f` (typically some change to the rows in `self.rows`/`self.shown`)
    /// without letting the scroll position move — including a scroll GTK
    /// performs on its own to keep the *selected* row in view whenever the
    /// list's contents change, even if that row was never touched by `f`.
    /// Saves the current position, pins the adjustment there through every
    /// change `f` (or GTK) makes, and does a final restore once layout has
    /// had a tick to settle — a single restore right after `f` can lose the
    /// race against GTK's own scroll, which can land after this returns.
    pub(super) fn preserving_scroll(&mut self, f: impl FnOnce(&mut Self)) {
        let Some(scroller) = self.scroller.clone() else {
            f(self);
            return;
        };
        let adj = scroller.vadjustment();
        let pos = adj.value();
        let hid = adj.connect_notify_local(Some("value"), move |adj, _| {
            if (adj.value() - pos).abs() > f64::EPSILON {
                adj.set_value(pos);
            }
        });
        f(self);
        let adj2 = adj.clone();
        gtk::glib::idle_add_local_once(move || {
            adj2.set_value(pos);
            adj2.disconnect(hid);
        });
    }

    /// Back to the top of the list (a switch, a new search or sort).
    pub(super) fn scroll_top(&self) {
        if let Some(s) = &self.scroller {
            s.vadjustment().set_value(0.0);
        }
        // The view keeps its place by a row. A list that grows under it
        // before it is laid out (another folder in kept rows, #401) would
        // keep the old last row in view, and the adjustment does not reach
        // past the old size yet: move the view's own anchor too.
        if self.shared.model.len() > 0 {
            self.list_view
                .scroll_to(0, gtk::ListScrollFlags::NONE, None);
            self.reveal_first_heading(0);
        }
    }

    /// The list's settings as every row reads them, pushed to the rows on
    /// screen. The rest pick them up when they are shown.
    pub(super) fn sync_look(&self) {
        {
            let mut look = self.shared.look.borrow_mut();
            look.gravatar = self.gravatar;
            look.avatars = self.avatars;
            look.sender_logos = self.sender_logos;
            look.preview_lines = self.preview_lines;
            look.show_subject = self.show_subject;
            look.pills_below = self.pills_below;
            look.single_line = self.single_line;
            look.columns = self
                .columns
                .iter()
                .copied()
                .filter(|c| *c != crate::config::ListColumn::Due || self.graph_in_view)
                .collect();
            look.in_drafts = self.in_drafts;
            look.show_recipient = self.show_recipient;
            look.thread_expansion = self.thread_expansion;
            look.ringed = if self.colorize {
                self.account_colors.keys().copied().collect()
            } else {
                Default::default()
            };
            look.face_gen = self.face_gen;
            look.tags_gen = self.tags_gen;
            look.folder_pills = Rc::new(if self.searching_pool() {
                self.pool_folders
                    .iter()
                    .map(|(k, f)| (*k, f.name.clone()))
                    .collect()
            } else {
                Default::default()
            });
            look.widths = self.fitted_widths();
            look.min_width = self.row_min_width();
        }
        // The rows on screen are about to be another view's, filled anyway.
        if self.switched {
            self.look_stale.set(true);
        } else {
            self.shared.refresh_all();
        }
        self.sync_headings();
    }

    /// What a single line's columns take, apart from the subject's room:
    /// the room the fixed ones need (with the pill's own and the spacing),
    /// and each column whose width can change, at its own or the one it
    /// was dragged to.
    fn column_room(&self) -> (i32, Vec<(crate::config::ListColumn, i32)>) {
        use crate::config::ListColumn as C;
        use crate::ui::message_row::{default_width, resizable};
        /// What a date as wide as its text takes, about.
        const DATE_PX: i32 = 64;
        /// Room kept for the subject.
        const SUBJECT_PX: i32 = 100;
        let columns: Vec<C> = self
            .columns
            .iter()
            .copied()
            .filter(|c| *c != C::Due || self.graph_in_view)
            .collect();
        // The pill's margins and padding, the unread dot and the spacing.
        let mut fixed = 12 + 18 + 18 + 8 * columns.len().saturating_sub(1) as i32;
        if self.avatars {
            fixed += 16 + 8;
        }
        let mut names = Vec::new();
        for c in &columns {
            let px = self
                .widths
                .get(c)
                .copied()
                .unwrap_or_else(|| default_width(*c));
            match c {
                C::Star | C::Attachment => fixed += 12,
                C::Replied => fixed += crate::ui::message_row::REPLIED_PX,
                C::Importance => fixed += 16,
                C::Subject => fixed += SUBJECT_PX,
                // A date cut short says nothing: it keeps its width.
                C::Due | C::Date => fixed += if px > 0 { px } else { DATE_PX },
                c if resizable(*c) => names.push((*c, px.max(1))),
                _ => {}
            }
        }
        (fixed, names)
    }

    /// The narrowest a scrolling single line goes (#443): every column at
    /// its width and the subject at a little room. A pane narrower than
    /// that scrolls sideways to the columns past its edge. -1 when the
    /// columns fit the pane instead.
    pub(super) fn row_min_width(&self) -> i32 {
        if !(self.single_line && self.column_scroll) {
            return -1;
        }
        let (fixed, names) = self.column_room();
        fixed + names.iter().map(|(_, px)| px).sum::<i32>()
    }

    /// The columns' widths as the rows and the headings draw them (#334):
    /// as set, unless the columns fit the pane and together leave the
    /// subject too little room, when the name and tag columns give way in
    /// proportion (the dates keep theirs). Fitted here rather than left to
    /// each row's box, which shares out a shortage by what that row holds,
    /// so a heading and its column would part.
    pub(super) fn fitted_widths(
        &self,
    ) -> std::collections::HashMap<crate::config::ListColumn, i32> {
        let mut widths = self.widths.clone();
        if self.column_scroll || !self.single_line || self.pane_width <= 0 {
            return widths;
        }
        let (fixed, names) = self.column_room();
        let sum: i32 = names.iter().map(|(_, px)| px).sum();
        let room = (self.pane_width - fixed).max(0);
        if sum > room && sum > 0 {
            for (c, px) in names {
                widths.insert(c, ((px as i64 * room as i64 / sum as i64) as i32).max(40));
            }
        }
        widths
    }

    /// Fill the column headings from the look and the sort (#334). Only
    /// while they are shown: they follow the next change once they are.
    pub(super) fn sync_headings(&self) {
        if !(self.single_line && self.headings) {
            return;
        }
        let look = self.shared.look.borrow().clone();
        let show_recipient = self.show_recipient;
        let sorted = look
            .columns
            .iter()
            .copied()
            .find(|c| match (column_sort(*c, show_recipient), self.sort) {
                (SortOrder::DateNewest, SortOrder::DateOldest) => true,
                (order, current) => order == current,
            })
            .map(|c| (c, self.sort.downwards() != self.sort_reversed));
        let input = self.shared.input.clone();
        let resized = self.shared.input.clone();
        *self.heading_bins.borrow_mut() = crate::ui::message_row::fill_headings(
            &self.headings_bar,
            &look,
            sorted,
            Rc::new(move |c| {
                let _ = input.send(MessageListInput::SortByColumn(c));
            }),
            Rc::new(move |column, width, done| {
                let _ = resized.send(MessageListInput::ResizeColumn {
                    column,
                    width,
                    done,
                });
            }),
        );
    }

    /// The date headings, on or off with the rows already in their sections,
    /// so the view never asks for a section of rows built without them.
    pub(super) fn show_headings(&mut self, by_date: bool) {
        if by_date != self.day_headings {
            self.day_headings = by_date;
            let factory = by_date.then(|| crate::ui::message_row::header_factory(&self.shared));
            self.list_view.set_header_factory(factory.as_ref());
        } else if by_date {
            self.shared.refresh_headers();
        }
    }

    /// Expanded conversations indent their member cards; give the pane the
    /// extra floor that needs while any thread is open, so nothing is
    /// clipped at the right edge (see THREAD_EXPANDED_EXTRA).
    pub(super) fn fit_expanded(&self, any_expanded: bool) {
        if let Some(s) = &self.scroller {
            let floor = LIST_MIN_WIDTH
                + if any_expanded {
                    THREAD_EXPANDED_EXTRA
                } else {
                    0
                };
            s.set_size_request(floor, -1);
        }
    }

    /// Update the display-wide CSS that rings each account's avatar with its
    /// color (used in the unified "All Inboxes" view to identify the account).
    /// The ring is the sidebar's, a pixel off the disc, so an account's
    /// avatar reads the same in both places.
    pub(super) fn refresh_tint_css(&self) {
        // In account order, so the same colors read as the same rules.
        let colors: std::collections::BTreeMap<_, _> = self.account_colors.iter().collect();
        let mut css = String::new();
        for (id, color) in colors {
            css.push_str(&format!(
                ".hylki-acct-ring-{0} {{ border-radius: 9999px; outline: 2px solid {1}; outline-offset: 1px; }}\n",
                id, color
            ));
        }
        self.color_provider.load(css);
    }
}
