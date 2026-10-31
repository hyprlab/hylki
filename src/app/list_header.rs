//! The message list's header: its Focus Mode fold, the sort and filter
//! popover, and the wide/narrow switch.

use adw::prelude::*;
use relm4::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::config;
use crate::i18n::{i18n, i18n_f, i18n_noop};
use crate::ui::message_list::{DateBound, MessageListInput, QuickFilter, QuickFilters};
use crate::ui::message_view::MessageViewInput;

use super::model::AppModel;
use super::msg::AppMsg;
use super::AppModelWidgets;

/// How wide the list pane has to be dragged before Automatic lays the list
/// out on one line, and how far back before it returns to cards (#334).
pub(super) const LIST_WIDE_ON: i32 = 600;

const LIST_WIDE_OFF: i32 = 560;

/// The message list header's controls that Focus Mode folds into a ⋯ menu:
/// each sits in a revealer so it can slide away, and the toggles are kept so
/// the menu can flip them (their own handlers then do the filtering).
pub(super) struct ListHeaderWidgets {
    pub(super) revealers: Vec<gtk::Revealer>,
    pub(super) overflow: gtk::Revealer,
    filter: SortFilterPopover,
    sort: gtk::gio::SimpleAction,
}

/// The orders the list header offers, by their keys in `SortOrder`. The
/// column headings can set more (#443); none of these is ticked then.
const SORT_ORDERS: [(&str, &str); 6] = [
    (i18n_noop("Date (Newest first)"), "date_newest"),
    (i18n_noop("Date (Oldest first)"), "date_oldest"),
    (i18n_noop("Sender (A–Z)"), "sender"),
    (i18n_noop("Subject (A–Z)"), "subject"),
    (i18n_noop("Unread first"), "unread"),
    (i18n_noop("Starred first"), "flagged"),
];

/// The list header's sort and filter button (#441): the sort orders in one
/// column of its popover, the quick filters in the next, the date range in
/// the last.
struct SortFilterPopover {
    button: gtk::MenuButton,
    filters: Vec<(QuickFilter, gtk::CheckButton)>,
    dates: Rc<DateRange>,
    clear: gtk::Button,
}

/// The popover's date range, its last column: From and To, each a toggle
/// naming its day, a calendar that sets the one toggled, and Clear Dates.
/// The calendar sits in the popover itself, as a popover of its own inside
/// this one would leave this one unable to close on a click outside.
struct DateRange {
    ends: [(DateBound, gtk::ToggleButton, gtk::Label); 2],
    calendar: gtk::Calendar,
    clear: gtk::Button,
    days: Rc<Cell<[Option<chrono::NaiveDate>; 2]>>,
    /// Set while the toggles or the calendar are moved to show the range in
    /// force, so the move is not taken for a pick.
    quiet: Rc<Cell<bool>>,
}

fn end_index(end: DateBound) -> usize {
    match end {
        DateBound::From => 0,
        DateBound::To => 1,
    }
}

impl DateRange {
    fn new(sender: &relm4::Sender<AppMsg>) -> Rc<Self> {
        let width = widest_day_label();
        let toggle = |end: DateBound| {
            // As wide as the longest date it can name, so the popover
            // keeps its width whichever day is picked.
            let label = gtk::Label::new(None);
            label.set_width_chars(width);
            let button = gtk::ToggleButton::new();
            button.set_child(Some(&label));
            button.set_hexpand(true);
            (end, button, label)
        };
        let ends = [toggle(DateBound::From), toggle(DateBound::To)];
        ends[1].1.set_group(Some(&ends[0].1));
        let clear = gtk::Button::with_label(&i18n("Clear Dates"));
        clear.add_css_class("flat");
        clear.set_halign(gtk::Align::Start);
        let this = Rc::new(Self {
            ends,
            calendar: gtk::Calendar::new(),
            clear,
            days: Rc::default(),
            quiet: Rc::default(),
        });

        // Toggling an end shows its day on the calendar; an end left open
        // takes the day in blue.
        for (end, button, _) in &this.ends {
            let (end, weak, s) = (*end, Rc::downgrade(&this), sender.clone());
            button.connect_toggled(move |b| {
                let Some(this) = weak.upgrade() else { return };
                if this.quiet.get() || !b.is_active() {
                    return;
                }
                match this.days.get()[end_index(end)] {
                    Some(day) => this.show_on_calendar(day),
                    None => s.emit(AppMsg::SetFilterDate(end, calendar_day(&this.calendar))),
                }
            });
        }
        // The arrows and the keyboard move the day in blue, and the end
        // being set follows once they rest, rather than on every step.
        let pending: Rc<RefCell<Option<gtk::glib::SourceId>>> = Rc::default();
        {
            let (weak, s, pending) = (Rc::downgrade(&this), sender.clone(), pending.clone());
            this.calendar.connect_day_selected(move |cal| {
                let Some(this) = weak.upgrade() else { return };
                let Some(end) = this.editing() else { return };
                if this.quiet.get() {
                    return;
                }
                if let Some(id) = pending.take() {
                    id.remove();
                }
                let (cal, s, slot) = (cal.clone(), s.clone(), pending.clone());
                let id = gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(250),
                    move || {
                        slot.take();
                        s.emit(AppMsg::SetFilterDate(end, calendar_day(&cal)));
                    },
                );
                pending.replace(Some(id));
            });
        }
        // A click on a day sets the end toggled, From if neither is, the
        // day already in blue too, which the calendar does not report as
        // selected again. Captured ahead of the calendar's own click, and
        // read once that has moved the selection.
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let (weak, s) = (Rc::downgrade(&this), sender.clone());
            click.connect_pressed(move |_, presses, x, y| {
                let Some(this) = weak.upgrade() else { return };
                let on_day = this
                    .calendar
                    .pick(x, y, gtk::PickFlags::DEFAULT)
                    .is_some_and(|w| w.has_css_class("day-number"));
                if presses != 1 || !on_day {
                    return;
                }
                let (weak, s, pending) = (weak.clone(), s.clone(), pending.clone());
                gtk::glib::idle_add_local_once(move || {
                    let Some(this) = weak.upgrade() else { return };
                    if let Some(id) = pending.take() {
                        id.remove();
                    }
                    let end = this.editing().unwrap_or(DateBound::From);
                    this.quiet.set(true);
                    this.ends[end_index(end)].1.set_active(true);
                    this.quiet.set(false);
                    s.emit(AppMsg::SetFilterDate(end, calendar_day(&this.calendar)));
                });
            });
        }
        this.calendar.add_controller(click);
        let s = sender.clone();
        this.clear
            .connect_clicked(move |_| s.emit(AppMsg::ClearFilterDates));
        this.show(QuickFilters::default());
        this
    }

    /// The end the calendar sets: the one toggled, if either is.
    fn editing(&self) -> Option<DateBound> {
        self.ends
            .iter()
            .find(|(_, b, _)| b.is_active())
            .map(|(end, _, _)| *end)
    }

    fn show_on_calendar(&self, day: chrono::NaiveDate) {
        use chrono::Datelike;
        if calendar_day(&self.calendar) == Some(day) {
            return;
        }
        if let Ok(at) = gtk::glib::DateTime::from_local(
            day.year(),
            day.month() as i32,
            day.day() as i32,
            12,
            0,
            0.0,
        ) {
            self.quiet.set(true);
            self.calendar.select_day(&at);
            self.quiet.set(false);
        }
    }

    /// Show the range in force: each end's day on its toggle, the end being
    /// set on the calendar, and neither toggled once both are open.
    fn show(&self, filters: QuickFilters) {
        let days = [filters.date(DateBound::From), filters.date(DateBound::To)];
        self.days.set(days);
        for ((_, button, label), day) in self.ends.iter().zip(days) {
            label.set_label(&day.map_or_else(|| i18n("Any Date"), day_label));
            if day.is_none() && days.iter().all(Option::is_none) && button.is_active() {
                self.quiet.set(true);
                button.set_active(false);
                self.quiet.set(false);
            }
        }
        if let Some(day) = self.editing().and_then(|end| days[end_index(end)]) {
            self.show_on_calendar(day);
        }
        self.clear.set_sensitive(filters.has_dates());
    }
}

/// The day selected on `cal`.
fn calendar_day(cal: &gtk::Calendar) -> Option<chrono::NaiveDate> {
    let d = cal.date();
    chrono::NaiveDate::from_ymd_opt(d.year(), d.month() as u32, d.day_of_month() as u32)
}

/// The most characters a date button names: Any Date, or a two-digit day
/// of the longest month written as the list writes dates.
fn widest_day_label() -> i32 {
    (1..=12)
        .filter_map(|m| chrono::NaiveDate::from_ymd_opt(2026, m, 28))
        .map(day_label)
        .chain([i18n("Any Date")])
        .map(|t| t.chars().count())
        .max()
        .unwrap_or(12) as i32
        + 1
}

/// A day as the list writes dates, at its noon so no clock change moves it.
fn day_label(day: chrono::NaiveDate) -> String {
    use chrono::TimeZone;
    day.and_hms_opt(12, 0, 0)
        .and_then(|t| chrono::Local.from_local_datetime(&t).earliest())
        .map(|t| crate::datefmt::day_month_year(t.timestamp()))
        .unwrap_or_default()
}

/// The date range for the button's tooltip.
fn range_label(filters: QuickFilters) -> Option<String> {
    let from = filters.date(DateBound::From).map(day_label);
    let to = filters.date(DateBound::To).map(day_label);
    match (from, to) {
        (Some(from), Some(to)) => Some(i18n_f("{from} to {to}", &[("from", &from), ("to", &to)])),
        (Some(date), None) => Some(i18n_f("Since {date}", &[("date", &date)])),
        (None, Some(date)) => Some(i18n_f("Until {date}", &[("date", &date)])),
        (None, None) => None,
    }
}

impl SortFilterPopover {
    /// The popover stays open as rows are clicked, so an order and several
    /// filters can be set in one visit.
    pub(super) fn new(
        button: gtk::MenuButton,
        sort: &gtk::gio::SimpleAction,
        sender: &relm4::Sender<AppMsg>,
    ) -> Self {
        let column = |title: &str| {
            let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
            col.add_css_class("sort-filter-column");
            let caption = gtk::Label::new(Some(title));
            caption.set_xalign(0.0);
            caption.add_css_class("dim-label");
            caption.add_css_class("caption-heading");
            col.append(&caption);
            col
        };

        let sort_col = column(&i18n("Sort by"));
        let mut radios: Vec<(&'static str, gtk::CheckButton)> = Vec::new();
        for (label, key) in SORT_ORDERS {
            let radio = gtk::CheckButton::with_label(&i18n(label));
            if let Some((_, first)) = radios.first() {
                radio.set_group(Some(first));
            }
            let action = sort.clone();
            radio.connect_toggled(move |r| {
                let current = action.state().and_then(|v| v.str().map(String::from));
                if r.is_active() && current.as_deref() != Some(key) {
                    action.activate(Some(&key.to_variant()));
                }
            });
            sort_col.append(&radio);
            radios.push((key, radio));
        }
        // The headings and the ⋯ menu sort through the same action, so the
        // radios follow its state wherever the change came from.
        let show_sort = move |key: Option<String>| {
            let key = key.unwrap_or_default();
            match radios.iter().find(|(k, _)| *k == key) {
                Some((_, r)) => r.set_active(true),
                None => radios.iter().for_each(|(_, r)| r.set_active(false)),
            }
        };
        show_sort(sort.state().and_then(|v| v.str().map(String::from)));
        sort.connect_state_notify(move |a| {
            show_sort(a.state().and_then(|v| v.str().map(String::from)))
        });

        let filter_col = column(&i18n("Show only"));
        let mut filters = Vec::new();
        for f in QuickFilter::ALL {
            let check = gtk::CheckButton::with_label(&f.label());
            let s = sender.clone();
            check.connect_toggled(move |c| s.emit(AppMsg::SetQuickFilter(f, c.is_active())));
            filter_col.append(&check);
            filters.push((f, check));
        }
        let clear = gtk::Button::with_label(&i18n("Clear Filters"));
        clear.add_css_class("flat");
        clear.set_halign(gtk::Align::Start);
        clear.set_sensitive(false);
        let s = sender.clone();
        clear.connect_clicked(move |_| s.emit(AppMsg::ClearQuickFilters));
        filter_col.append(&clear);

        let dates_col = column(&i18n("Dates"));
        let dates = gtk::Grid::new();
        dates.add_css_class("sort-filter-dates");
        dates.set_row_spacing(6);
        dates.set_column_spacing(8);
        let range = DateRange::new(sender);
        for (row, (end, button, _)) in range.ends.iter().enumerate() {
            let label = gtk::Label::new(Some(&match end {
                DateBound::From => i18n("From"),
                DateBound::To => i18n("To"),
            }));
            label.set_xalign(0.0);
            dates.attach(&label, 0, row as i32, 1, 1);
            dates.attach(button, 1, row as i32, 1, 1);
        }
        dates.attach(&range.calendar, 0, 2, 2, 1);
        dates_col.append(&dates);
        dates_col.append(&range.clear);

        let columns = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        columns.add_css_class("sort-filter-popover");
        columns.append(&sort_col);
        columns.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        columns.append(&filter_col);
        columns.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        columns.append(&dates_col);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&columns));
        button.set_popover(Some(&popover));
        Self {
            button,
            filters,
            dates: range,
            clear,
        }
    }

    /// Show `filters` on the checks, and tint the button while any is on
    /// so a narrowed list never passes for an empty folder.
    pub(super) fn sync(&self, filters: QuickFilters) {
        for (f, check) in &self.filters {
            if check.is_active() != filters.has(*f) {
                check.set_active(filters.has(*f));
            }
        }
        self.dates.show(filters);
        self.clear.set_sensitive(!filters.is_empty());
        if filters.is_empty() {
            self.button.remove_css_class("list-filter-active");
            self.button
                .set_tooltip_text(Some(&i18n("Sort and filter messages")));
        } else {
            self.button.add_css_class("list-filter-active");
            let names: Vec<String> = QuickFilter::ALL
                .iter()
                .filter(|f| filters.has(**f))
                .map(|f| f.label())
                .chain(range_label(filters))
                .collect();
            self.button.set_tooltip_text(Some(&i18n_f(
                "Filtered: {names}",
                &[("names", &names.join(", "))],
            )));
        }
    }
}

impl AppModel {
    /// Narrow the list to `filters`, and show them on the header's popover.
    pub(super) fn set_quick_filters(&mut self, filters: QuickFilters) {
        self.quick_filters = filters;
        let contacts = if filters.has(QuickFilter::Contacts) {
            self.saved_addresses.get()
        } else {
            None
        };
        self.message_list
            .emit(MessageListInput::SetQuickFilters { filters, contacts });
        if let Some(lh) = self.list_header_widgets.get() {
            lh.filter.sync(filters);
        }
    }

    /// The message list's search, filters, count and sort as a menu: what
    /// the ⋯ in its header offers while Focus Mode has them folded away.
    /// The toggles are flipped through their own buttons, so the filtering
    /// happens where it always does.
    fn show_list_overflow_menu(&self, sender: &ComponentSender<Self>) {
        use crate::ui::context_menu::{show_context_menu_with_header, MenuEntry};
        let Some(lh) = self.list_header_widgets.get() else {
            return;
        };
        let s = sender.input_sender().clone();
        let search = MenuEntry::new(i18n("Search Messages…"), move || {
            let _ = s.send(AppMsg::OpenListSearch);
        })
        .icon("system-search-symbolic");
        let filters: Vec<MenuEntry> = QuickFilter::ALL
            .into_iter()
            .map(|f| {
                let on = self.quick_filters.has(f);
                let s = sender.input_sender().clone();
                MenuEntry::new(f.label(), move || s.emit(AppMsg::SetQuickFilter(f, !on)))
                    .icon(f.icon())
                    .selected(on)
            })
            .collect();
        let s = sender.input_sender().clone();
        let clear = MenuEntry::new(i18n("Clear Filters"), move || {
            s.emit(AppMsg::ClearQuickFilters)
        })
        .enabled(!self.quick_filters.is_empty());
        let filter =
            MenuEntry::submenu(i18n("Filter"), vec![filters, vec![clear]]).icon("filter-symbolic");
        let current = lh
            .sort
            .state()
            .and_then(|v| v.str().map(String::from))
            .unwrap_or_default();
        let orders: Vec<MenuEntry> = SORT_ORDERS
            .into_iter()
            .map(|(label, key)| {
                let action = lh.sort.clone();
                // The same route the sort menu takes, so the list and the
                // action's own state both follow.
                MenuEntry::new(i18n(label), move || {
                    action.activate(Some(&key.to_variant()))
                })
                .selected(current == key)
            })
            .collect();
        let sort =
            MenuEntry::submenu(i18n("Sort"), vec![orders]).icon("view-sort-descending-symbolic");
        let sections = vec![vec![search], vec![filter, sort]];
        // The message count heads the menu, where the header showed it.
        let header = (!self.list_count.is_empty()).then(|| self.list_count.clone());
        let btn = lh.overflow.clone();
        show_context_menu_with_header(
            &btn,
            (btn.width() / 2) as f64,
            btn.height() as f64,
            header.as_deref(),
            sections,
        );
    }

    /// Where the list's search field and bulk bar open (#496).
    pub(super) fn push_list_bars(&self) {
        self.message_list.emit(MessageListInput::SetBarsPlacement {
            search_bottom: self.list_search_bottom,
            bulk_bottom: self.list_bulk_bar_bottom,
        });
    }

    /// Tell the list whether to lay messages out on one line (#334): the
    /// Layout setting, or for Automatic, the width of the pane.
    pub(super) fn push_single_line(&self) {
        // With the reader below or off the list is a table (#475).
        let on = match self.list_layout {
            _ if self.reading_pane != config::ReadingPane::Right => true,
            config::ListLayout::Cards => false,
            config::ListLayout::SingleLine => true,
            config::ListLayout::Automatic => self.list_wide,
        };
        self.message_list.emit(MessageListInput::SetSingleLine(on));
    }

    /// Hand the list its look; `animate` slides the avatars away or back
    /// (a Focus Mode toggle) rather than rebuilding the rows outright.
    pub(super) fn push_list_look(&self, animate: bool) {
        self.message_list.emit(MessageListInput::SetLook {
            avatars: self.list_avatars(),
            preview_lines: self.list_preview_lines(),
            subject: self.list_show_subject(),
            animate,
        });
        // A folded card in a conversation shows what a list row does (#326).
        self.message_view.emit(MessageViewInput::SetFoldLook {
            preview_lines: self.list_preview_lines(),
        });
    }

    /// The avatars the list draws: the setting, unless Focus Mode hides them.
    pub(super) fn list_avatars(&self) -> bool {
        self.avatars && !self.focus.active(config::FocusPart::HideAvatars)
    }

    /// The preview lines the list shows: the setting, hidden outright or
    /// capped at one by Focus Mode (previews switched off stay off).
    ///
    /// Only what is *drawn* changes — the setting itself, and with it what
    /// the workers fetch, is left alone. So the preview text is in hand the
    /// moment Focus Mode ends, rather than waiting for the next sync the way
    /// switching previews off in Settings does.
    pub(super) fn list_preview_lines(&self) -> u32 {
        if self.focus.active(config::FocusPart::HidePreview) {
            0
        } else if self.focus.active(config::FocusPart::OnePreviewLine) {
            self.preview_lines.min(1)
        } else {
            self.preview_lines
        }
    }

    /// Whether the list draws subject lines: always, unless Focus Mode's
    /// subject part is in force (then a row is its sender and its date).
    pub(super) fn list_show_subject(&self) -> bool {
        !self.focus.active(config::FocusPart::HideSubject)
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_list_header(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ListOverflowMenu => self.show_list_overflow_menu(&sender),
            AppMsg::ListPaneWidth(width) => {
                // A margin each way, so a drag that rests near the line does
                // not flick the rows back and forth.
                let wide = if self.list_wide {
                    width >= LIST_WIDE_OFF
                } else {
                    width >= LIST_WIDE_ON
                };
                if wide != self.list_wide {
                    self.list_wide = wide;
                    if self.list_layout == config::ListLayout::Automatic {
                        self.push_single_line();
                    }
                }
            }

            AppMsg::ListCount(text) => self.list_count = text,
            AppMsg::ListColumnWidths(widths) => {
                pref!(self.list_column_widths = widths);
            }
            AppMsg::ListSortChanged(key) => {
                if let Some(lh) = self.list_header_widgets.get() {
                    lh.sort.set_state(&key.to_variant());
                }
            }
            _ => unreachable!("not routed to on_list_header"),
        }
    }

    /// The list header's sort menu (moved out of the message list's own
    /// toolbar): a stateful radio action feeding the list's SetSort.
    pub(super) fn init_list_sort_menu(
        &self,
        widgets: &AppModelWidgets,
        sender: &ComponentSender<Self>,
    ) {
        use crate::ui::message_list::SortOrder;
        let sort_group = gtk::gio::SimpleActionGroup::new();
        let sort_action = gtk::gio::SimpleAction::new_stateful(
            "order",
            Some(gtk::glib::VariantTy::STRING),
            &"date_newest".to_variant(),
        );
        let list = self.message_list.sender().clone();
        sort_action.connect_activate(move |action, param| {
            if let Some(key) = param.and_then(|v| v.str()) {
                action.set_state(&key.to_variant());
                let _ = list.send(MessageListInput::SetSort(SortOrder::from_key(key)));
            }
        });
        sort_group.add_action(&sort_action);
        widgets
            .list_filter_btn
            .insert_action_group("sortmenu", Some(&sort_group));

        let _ = self.list_header_widgets.set(ListHeaderWidgets {
            revealers: vec![
                widgets.lh_search.clone(),
                widgets.lh_filter.clone(),
                widgets.lh_count.clone(),
            ],
            overflow: widgets.lh_overflow.clone(),
            filter: SortFilterPopover::new(
                widgets.list_filter_btn.clone(),
                &sort_action,
                sender.input_sender(),
            ),
            sort: sort_action,
        });
        // Focus Mode's chrome as saved: nothing is mapped yet, so the
        // revealers jump to their state without animating.
        self.sync_focus_chrome();
        self.reader_overflow_btn
            .set_visible(self.reader_overflow_wanted());
    }
}
