//! How the list looks: the row look, columns and headings, sorting, the
//! bars, swipes and the palette.

use super::layout::schedule_midnight_refresh;
use super::sort::{column_sort, SortOrder};
use super::types::{MessageListInput, MessageListOutput};
use super::MessageList;
use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

impl MessageList {
    pub(super) fn on_set_list_palette(&mut self, on: bool) {
        if self.list_palette != on {
            self.list_palette = on;
            self.sync_look();
        }
    }

    pub(super) fn on_set_sender_logos(&mut self, on: bool) {
        if self.sender_logos != on {
            self.sender_logos = on;
            self.face_gen += 1;
            self.sync_look();
        }
    }

    pub(super) fn on_set_single_line(&mut self, on: bool) {
        if self.single_line != on {
            self.single_line = on;
            // The date headings line up with a single line's text.
            if on {
                self.list_view.add_css_class("single-line");
            } else {
                self.list_view.remove_css_class("single-line");
            }
            self.sync_look();
        }
    }

    pub(super) fn on_set_columns(&mut self, columns: Vec<crate::config::ListColumn>) {
        if self.columns != columns {
            // The Correspondents column is worked out by the rebuild.
            let people = |c: &[crate::config::ListColumn]| {
                c.contains(&crate::config::ListColumn::Correspondents)
            };
            let rebuild = people(&columns) != people(&self.columns);
            self.columns = columns;
            self.sync_look();
            if rebuild {
                self.queue_rebuild(true);
            }
        }
    }

    pub(super) fn on_set_account_names(
        &mut self,
        names: std::collections::HashMap<u32, String>,
        graph: std::collections::HashSet<u32>,
    ) {
        let changed = *self.shared.account_names.borrow() != names || self.graph_accounts != graph;
        if changed {
            *self.shared.account_names.borrow_mut() = names;
            self.graph_accounts = graph;
            self.graph_in_view = self
                .listed_folders
                .iter()
                .any(|(a, _)| self.graph_accounts.contains(a));
            self.sync_look();
        }
    }

    pub(super) fn on_set_look(
        &mut self,
        avatars: bool,
        preview_lines: u32,
        subject: bool,
        animate: bool,
        sender: &ComponentSender<Self>,
    ) {
        let preview_lines = preview_lines.min(3);
        let avatars_changed = self.avatars != avatars;
        let lines_changed = self.preview_lines != preview_lines;
        let subject_changed = self.show_subject != subject;
        if !avatars_changed && !lines_changed && !subject_changed {
            return;
        }
        self.show_subject = subject;
        if animate && avatars_changed && !avatars {
            // Slide every circle on screen away (and the preview to
            // its new height in place); the rows give the slot back
            // once they have gone.
            self.preview_lines = preview_lines;
            for row in self.shared.bound_rows() {
                row.slide_avatar_away();
                if lines_changed {
                    row.set_preview_lines(preview_lines);
                }
            }
            let s = sender.clone();
            gtk::glib::timeout_add_local_once(
                std::time::Duration::from_millis(u64::from(crate::ui::FOCUS_ANIM_MS) + 40),
                move || s.input(MessageListInput::LookSettled),
            );
        } else {
            self.avatars = avatars;
            self.preview_lines = preview_lines;
            self.sync_look();
            // Circles coming back start folded and slide in.
            if animate && avatars_changed && avatars {
                for row in self.shared.bound_rows() {
                    row.slide_avatar_in();
                }
            }
        }
    }

    pub(super) fn on_set_avatars(&mut self, on: bool) {
        if self.avatars != on {
            self.avatars = on;
            self.sync_look();
        }
    }

    pub(super) fn on_set_gravatar(&mut self, on: bool) {
        if self.gravatar != on {
            self.gravatar = on;
            self.face_gen += 1;
            self.sync_look();
        }
    }

    pub(super) fn on_set_show_recipient(&mut self, on: bool) {
        if self.show_recipient != on {
            self.show_recipient = on;
            self.sync_look();
        }
    }

    pub(super) fn on_set_in_junk(&mut self, on: bool) {
        if self.in_junk != on {
            self.in_junk = on;
            self.sync_look();
        }
    }

    pub(super) fn on_set_in_drafts(&mut self, on: bool) {
        if self.in_drafts != on {
            self.in_drafts = on;
            self.sync_look();
        }
    }

    pub(super) fn on_contact_photos_changed(&mut self) {
        // The circles on screen look again; the rest do when shown.
        self.face_gen += 1;
        self.sync_look();
    }

    pub(super) fn on_contact_names_changed(&mut self) {
        // Rows showing the same message are kept as they are unless the
        // look is stale, and every name on them may be another now.
        self.look_stale.set(true);
        self.rebuild();
    }

    pub(super) fn on_set_colorize(&mut self, on: bool) {
        if self.colorize != on {
            self.colorize = on;
            self.sync_look();
        }
    }

    pub(super) fn on_day_changed(&mut self, sender: &ComponentSender<Self>) {
        // Re-render so relative labels like "Today" reflect the new date.
        self.rebuild();
        schedule_midnight_refresh(sender);
    }

    pub(super) fn on_set_account_colors(&mut self, colors: std::collections::HashMap<u32, String>) {
        self.account_colors = colors;
        self.refresh_tint_css();
        self.sync_look();
    }

    pub(super) fn on_set_sort(&mut self, order: SortOrder) {
        if self.sort != order || self.sort_reversed {
            self.sort = order;
            self.sort_reversed = false;
            self.rebuild();
            self.scroll_top();
            self.sync_headings();
        }
    }

    pub(super) fn on_resize_column(
        &mut self,
        column: crate::config::ListColumn,
        width: Option<i32>,
        done: bool,
        sender: &ComponentSender<Self>,
    ) {
        match width {
            Some(px) => self.widths.insert(column, px),
            None => self.widths.remove(&column),
        };
        // The rows follow every step; the headings are left alone
        // while their handle is held, as rebuilding them would drop
        // it mid-drag. It has moved its own heading already.
        let min_width = self.row_min_width();
        let fitted = self.fitted_widths();
        for (c, bin) in self.heading_bins.borrow().iter() {
            bin.set_width(
                fitted
                    .get(c)
                    .copied()
                    .unwrap_or_else(|| crate::ui::message_row::default_width(*c)),
            );
        }
        self.headings_bar.set_size_request(min_width, -1);
        {
            let mut look = self.shared.look.borrow_mut();
            look.widths = fitted;
            look.min_width = min_width;
        }
        self.shared.refresh_all();
        if done {
            let input = self.shared.input.clone();
            glib::idle_add_local_once(move || {
                let _ = input.send(MessageListInput::RefreshHeadings);
            });
            let _ = sender.output(MessageListOutput::ColumnWidths(self.widths.clone()));
        }
    }

    pub(super) fn on_set_column_widths(
        &mut self,
        widths: std::collections::HashMap<crate::config::ListColumn, i32>,
    ) {
        if self.widths != widths {
            self.widths = widths;
            self.sync_look();
        }
    }

    pub(super) fn on_set_bars_placement(&mut self, search_bottom: bool, bulk_bottom: bool) {
        if (self.search_bottom, self.bulk_bottom) != (search_bottom, bulk_bottom) {
            self.search_bottom = search_bottom;
            self.bulk_bottom = bulk_bottom;
            self.place_bars();
        }
    }

    pub(super) fn on_set_search_pinned(&mut self, on: bool) {
        self.search_pinned = on;
        // Unpinned, it stays only while it holds a search.
        if !on {
            self.search_open = !self.query.is_empty();
        }
    }

    pub(super) fn on_set_column_scroll(&mut self, on: bool) {
        if self.column_scroll != on {
            self.column_scroll = on;
            self.sync_look();
        }
    }

    pub(super) fn on_pane_width(&mut self, width: i32) {
        if self.pane_width != width {
            let fits = self.single_line && !self.column_scroll;
            let before = fits.then(|| self.fitted_widths());
            self.pane_width = width;
            if before.is_some_and(|b| b != self.fitted_widths()) {
                self.sync_look();
            }
        }
    }

    pub(super) fn on_set_group_by_date(&mut self, on: bool) {
        if self.group_by_date != on {
            self.group_by_date = on;
            self.queue_rebuild(true);
        }
    }

    pub(super) fn on_set_pills_below(&mut self, on: bool) {
        if self.pills_below != on {
            self.pills_below = on;
            self.sync_look();
        }
    }

    pub(super) fn on_sort_by_column(
        &mut self,
        column: crate::config::ListColumn,
        sender: &ComponentSender<Self>,
    ) {
        let order = column_sort(column, self.show_recipient);
        match (order, self.sort) {
            // Date has an order each way of its own.
            (SortOrder::DateNewest, SortOrder::DateNewest) => self.sort = SortOrder::DateOldest,
            (SortOrder::DateNewest, SortOrder::DateOldest) => self.sort = SortOrder::DateNewest,
            (order, current) if order == current => self.sort_reversed = !self.sort_reversed,
            (order, _) => {
                self.sort = order;
                self.sort_reversed = false;
            }
        }
        self.rebuild();
        self.scroll_top();
        self.sync_headings();
        let _ = sender.output(MessageListOutput::SortChanged(self.sort.key()));
    }

    pub(super) fn on_set_swipe_enabled(&mut self, on: bool) {
        self.shared.swipe_enabled.set(on);
        // The rows on screen switch their trackers; the rest do so
        // when shown.
        self.shared.refresh_all();
    }
}
