//! The Calendars popover in the calendar's header, laid out like the
//! message list's sort and filter popover: the calendars grouped by
//! account in columns, each with a check that shows or hides it and a menu,
//! and the ways to add one. The header button is tinted while any calendar
//! is hidden, as the filter button is while the list is narrowed.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;

use crate::calendar::store::{CalInfo, CalKind};
use crate::i18n::{i18n, ni18n_f};

/// What the popover asks of the page.
#[derive(Debug, Clone)]
pub(crate) enum SideAct {
    Show(String, bool),
    Only(String),
    ShowAll,
    Look(String),
    Delete(String),
    Refresh(String),
    NewCalendar,
    Subscribe,
    Import,
    AddAccount,
}

/// Rows a column holds before the groups spread into another.
const COLUMN_ROWS: usize = 10;
const MAX_COLUMNS: usize = 3;

#[derive(Clone)]
pub(crate) struct Side {
    /// The header button that opens it.
    pub(crate) button: gtk::MenuButton,
    columns: gtk::Box,
    show_all: gtk::Button,
    css: gtk::CssProvider,
    on: Rc<dyn Fn(SideAct)>,
}

impl Side {
    pub(crate) fn new(on: impl Fn(SideAct) + 'static) -> Side {
        let on: Rc<dyn Fn(SideAct)> = Rc::new(on);
        let button = gtk::MenuButton::new();
        button.set_icon_name("x-office-calendar-symbolic");
        button.set_tooltip_text(Some(&i18n("Calendars")));

        let columns = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        columns.add_css_class("sort-filter-popover");
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_propagate_natural_height(true);
        scroller.set_propagate_natural_width(true);
        scroller.set_max_content_height(560);
        scroller.set_child(Some(&columns));

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        footer.add_css_class("calendar-popover-footer");
        let show_all = gtk::Button::with_label(&i18n("Show All"));
        show_all.add_css_class("flat");
        show_all.set_sensitive(false);
        footer.append(&show_all);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        footer.append(&spacer);
        let add = gtk::MenuButton::new();
        add.set_label(&i18n("Add Calendar"));
        add.add_css_class("flat");
        add.set_always_show_arrow(true);
        let menu = gtk::gio::Menu::new();
        menu.append(Some(&i18n("New Calendar…")), Some("calside.new"));
        menu.append(Some(&i18n("Subscribe to Calendar…")), Some("calside.subscribe"));
        menu.append(Some(&i18n("Import Events…")), Some("calside.import"));
        menu.append(Some(&i18n("Add CalDAV Account…")), Some("calside.account"));
        add.set_menu_model(Some(&menu));
        footer.append(&add);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&scroller);
        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        body.append(&footer);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&body));
        button.set_popover(Some(&popover));

        let group = gtk::gio::SimpleActionGroup::new();
        for (name, act) in [
            ("new", SideAct::NewCalendar),
            ("subscribe", SideAct::Subscribe),
            ("import", SideAct::Import),
            ("account", SideAct::AddAccount),
        ] {
            let a = gtk::gio::SimpleAction::new(name, None);
            let (on, p) = (on.clone(), popover.clone());
            a.connect_activate(move |_, _| {
                p.popdown();
                on(act.clone());
            });
            group.add_action(&a);
        }
        button.insert_action_group("calside", Some(&group));
        let o = on.clone();
        show_all.connect_clicked(move |_| o(SideAct::ShowAll));

        let css = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 2,
            );
        }
        Side {
            button,
            columns,
            show_all,
            css,
            on,
        }
    }

    /// List `calendars`, `hidden` unchecked.
    pub(crate) fn set_calendars(&self, calendars: &[CalInfo], hidden: &HashSet<String>) {
        while let Some(c) = self.columns.first_child() {
            self.columns.remove(&c);
        }
        let mut groups: Vec<(String, Vec<&CalInfo>)> = Vec::new();
        for c in calendars.iter().filter(|c| !c.tasks) {
            let heading = match c.kind {
                CalKind::Local => i18n("On This Computer"),
                CalKind::Subscription => i18n("Subscriptions"),
                CalKind::Birthdays | CalKind::Other => i18n("Other"),
                CalKind::Remote => c.account.clone().unwrap_or_else(|| i18n("Online")),
            };
            match groups.iter_mut().find(|(h, _)| *h == heading) {
                Some((_, v)) => v.push(c),
                None => groups.push((heading, vec![c])),
            }
        }
        let index: HashMap<&str, usize> = calendars
            .iter()
            .enumerate()
            .map(|(i, c)| (c.uid.as_str(), i))
            .collect();
        let mut css = String::new();
        for c in calendars {
            let i = index.get(c.uid.as_str()).copied().unwrap_or(0);
            css.push_str(&check_css(i, &c.color));
        }
        self.css.load_from_string(&css);

        // Groups stay whole; a new column starts once one is long enough.
        let rows: usize = groups.iter().map(|(_, v)| v.len() + 1).sum();
        let ncols = rows.div_ceil(COLUMN_ROWS).clamp(1, MAX_COLUMNS);
        let per = rows.div_ceil(ncols);
        let mut column = new_column();
        let mut filled = 0;
        for (heading, cals) in groups {
            let n = cals.len();
            if filled > 0 && filled + cals.len() + 1 > per && self.column_count() + 1 < ncols {
                self.push_column(&column);
                column = new_column();
                filled = 0;
            }
            let caption = gtk::Label::new(Some(&heading));
            caption.set_xalign(0.0);
            caption.add_css_class("dim-label");
            caption.add_css_class("caption-heading");
            caption.set_ellipsize(gtk::pango::EllipsizeMode::End);
            caption.set_max_width_chars(28);
            if filled > 0 {
                caption.add_css_class("sort-filter-caption-next");
            }
            column.append(&caption);
            for c in cals {
                let i = index.get(c.uid.as_str()).copied().unwrap_or(0);
                column.append(&self.row(c, i, !hidden.contains(&c.uid)));
            }
            filled += n + 1;
        }
        self.push_column(&column);
        self.show_hidden(hidden.iter().any(|h| calendars.iter().any(|c| &c.uid == h)), hidden.len());
    }

    fn column_count(&self) -> usize {
        let mut n = 0;
        let mut w = self.columns.first_child();
        while let Some(c) = w {
            if c.has_css_class("sort-filter-column") {
                n += 1;
            }
            w = c.next_sibling();
        }
        n
    }

    fn push_column(&self, column: &gtk::Box) {
        if self.columns.first_child().is_some() {
            self.columns.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        }
        self.columns.append(column);
    }

    /// Tint the button, and say how many are hidden, while any is.
    pub(crate) fn show_hidden(&self, any: bool, n: usize) {
        self.show_all.set_sensitive(any);
        if any {
            self.button.add_css_class("list-filter-active");
            self.button.set_tooltip_text(Some(&ni18n_f(
                "Calendars: {n} hidden",
                "Calendars: {n} hidden",
                n as u32,
                &[("n", &n.to_string())],
            )));
        } else {
            self.button.remove_css_class("list-filter-active");
            self.button.set_tooltip_text(Some(&i18n("Calendars")));
        }
    }

    fn row(&self, c: &CalInfo, i: usize, shown: bool) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("calendar-popover-row");
        let check = gtk::CheckButton::with_label(&c.name);
        check.add_css_class(&format!("cal-c-{i}"));
        check.set_active(shown);
        check.set_hexpand(true);
        check.set_tooltip_text(Some(&c.full_name()));
        if let Some(label) = check.last_child().and_downcast::<gtk::Label>() {
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(28);
        }
        let uid = c.uid.clone();
        let on = self.on.clone();
        check.connect_toggled(move |b| on(SideAct::Show(uid.clone(), b.is_active())));
        row.append(&check);
        if !c.writable {
            let lock = gtk::Image::from_icon_name("changes-prevent-symbolic");
            lock.add_css_class("dim-label");
            lock.set_tooltip_text(Some(&i18n("Read-only")));
            lock.set_margin_end(4);
            row.append(&lock);
        }
        let more = gtk::Button::from_icon_name("view-more-symbolic");
        more.add_css_class("flat");
        more.add_css_class("calendar-popover-more");
        more.set_valign(gtk::Align::Center);
        more.set_tooltip_text(Some(&i18n("More")));
        let (info, on) = (c.clone(), self.on.clone());
        more.connect_clicked(move |b| {
            let (w, h) = (f64::from(b.width()), f64::from(b.height()));
            calendar_menu(b.upcast_ref(), w / 2.0, h, &info, on.clone());
        });
        row.append(&more);
        let menu = gtk::GestureClick::new();
        menu.set_button(3);
        let (info, on) = (c.clone(), self.on.clone());
        menu.connect_pressed(move |g, _, x, y| {
            if let Some(w) = g.widget() {
                calendar_menu(&w, x, y, &info, on.clone());
            }
        });
        row.add_controller(menu);
        row
    }
}

fn new_column() -> gtk::Box {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.add_css_class("sort-filter-column");
    col
}

/// A check in its calendar's color.
fn check_css(i: usize, color: &str) -> String {
    let fg = super::paint::Rgba::hex(color);
    let mark = if fg.luminance() > 0.62 { "black" } else { "white" };
    format!(
        "checkbutton.cal-c-{i} check {{ border-color: {color}; box-shadow: inset 0 0 0 2px {color}; }}\n\
         checkbutton.cal-c-{i} check:checked {{ background-color: {color}; color: {mark}; box-shadow: none; }}\n"
    )
}

fn calendar_menu(w: &gtk::Widget, x: f64, y: f64, c: &CalInfo, on: Rc<dyn Fn(SideAct)>) {
    use crate::ui::context_menu::{show_context_menu, MenuEntry};
    let mut entries = Vec::new();
    let uid = c.uid.clone();
    let o = on.clone();
    entries.push(MenuEntry::new(i18n("Show Only This Calendar"), move || o(SideAct::Only(uid.clone()))));
    let uid = c.uid.clone();
    let o = on.clone();
    let look = if matches!(c.kind, CalKind::Local | CalKind::Subscription) {
        i18n("Name and Color…")
    } else {
        i18n("Color…")
    };
    entries.push(MenuEntry::new(look, move || o(SideAct::Look(uid.clone()))));
    if c.kind != CalKind::Local {
        let uid = c.uid.clone();
        let o = on.clone();
        entries.push(MenuEntry::new(i18n("Refresh"), move || o(SideAct::Refresh(uid.clone()))));
    }
    let mut groups = vec![entries];
    if matches!(c.kind, CalKind::Local | CalKind::Subscription) {
        let uid = c.uid.clone();
        groups.push(vec![MenuEntry::new(i18n("Delete Calendar…"), move || {
            on(SideAct::Delete(uid.clone()))
        })
        .icon("user-trash-symbolic")]);
    }
    show_context_menu(w, x, y, groups);
}
