//! The settings search (#260): an index of every row's text, the results
//! list, and taking the user to the row picked.

use super::navigation::SIDE_PAGES;
use super::*;

/// One searchable place in Settings (#260): a row or a group, found by its
/// own title and subtitle as shown, so a search works in the language the
/// app is in.
#[derive(Clone)]
struct SearchHit {
    page: &'static str,
    title: String,
    /// Where it sits: the page, and the group or expander row around it.
    place: String,
    /// Lowercased title, subtitle and place, to match against.
    haystack: String,
    widget: gtk::glib::WeakRef<gtk::Widget>,
}

/// The settings search (#260): a search button in the sidebar's header
/// opens an entry above the categories; while it holds text the sidebar
/// lists the matching rows instead, and picking one shows its page and
/// scrolls to it. libadwaita's own preferences search only covers pages
/// added to its own window, which this two-pane window is not.
pub(super) struct SettingsSearch {
    pub(super) bar: gtk::SearchBar,
    pub(super) entry: gtk::SearchEntry,
    /// "pages" (the categories), "results" or "empty".
    stack: gtk::Stack,
    results: gtk::ListBox,
    /// Built on the first search, from the pages as they are then; rows
    /// hidden later are skipped when matching.
    index: std::cell::RefCell<Option<Vec<SearchHit>>>,
    /// What the results list shows, by position.
    shown: std::cell::RefCell<Vec<SearchHit>>,
}

/// Most results listed at once.
const SEARCH_LIMIT: usize = 60;

impl SettingsSearch {
    pub(super) fn build(
        toolbar: &adw::ToolbarView,
        header: &adw::HeaderBar,
        scroller: &gtk::ScrolledWindow,
        window: &gtk::Widget,
        sender: &ComponentSender<Preferences>,
    ) -> Self {
        let button = gtk::ToggleButton::new();
        button.set_icon_name("system-search-symbolic");
        button.set_tooltip_text(Some(&i18n("Search Settings")));
        header.pack_start(&button);

        let entry = gtk::SearchEntry::new();
        entry.set_placeholder_text(Some(&i18n("Search settings")));
        entry.set_hexpand(true);
        let bar = gtk::SearchBar::new();
        bar.set_child(Some(&entry));
        bar.connect_entry(&entry);
        bar.bind_property("search-mode-enabled", &button, "active")
            .bidirectional()
            .sync_create()
            .build();
        toolbar.add_top_bar(&bar);

        let results = gtk::ListBox::new();
        results.add_css_class("navigation-sidebar");
        results.set_selection_mode(gtk::SelectionMode::None);
        let s = sender.clone();
        results.connect_row_activated(move |_, row| {
            s.input(PrefInput::SearchPick(row.index() as usize));
        });
        let results_scroller = gtk::ScrolledWindow::new();
        results_scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        results_scroller.set_child(Some(&results));

        let empty = adw::StatusPage::new();
        empty.set_icon_name(Some("system-search-symbolic"));
        empty.set_title(&i18n("No Results"));
        empty.add_css_class("compact");

        let stack = gtk::Stack::new();
        toolbar.set_content(None::<&gtk::Widget>);
        stack.add_named(scroller, Some("pages"));
        stack.add_named(&results_scroller, Some("results"));
        stack.add_named(&empty, Some("empty"));
        toolbar.set_content(Some(&stack));

        let s = sender.clone();
        entry.connect_search_changed(move |e| s.input(PrefInput::Search(e.text().to_string())));
        // Closing the search puts the categories back.
        let e = entry.clone();
        bar.connect_search_mode_enabled_notify(move |bar| {
            if !bar.is_search_mode() {
                e.set_text("");
            }
        });

        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_scope(gtk::ShortcutScope::Global);
        let s = sender.clone();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("<Control>f"),
            Some(gtk::CallbackAction::new(move |_, _| {
                s.input(PrefInput::ToggleSearch);
                gtk::glib::Propagation::Stop
            })),
        ));
        // Escape closes the search from anywhere in the window, not only
        // from its entry: after a result is picked, focus is on the list or
        // the page. With the search closed the key goes on as before.
        let b = bar.clone();
        shortcuts.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("Escape"),
            Some(gtk::CallbackAction::new(move |_, _| {
                if b.is_search_mode() {
                    b.set_search_mode(false);
                    gtk::glib::Propagation::Stop
                } else {
                    gtk::glib::Propagation::Proceed
                }
            })),
        ));
        window.add_controller(shortcuts);
        // The window is only hidden on close and kept for the next open,
        // which should not come back to an old search (#513).
        let b = bar.clone();
        window.connect_hide(move |_| b.set_search_mode(false));

        Self {
            bar,
            entry,
            stack,
            results,
            index: std::cell::RefCell::new(None),
            shown: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn toggle(&self) {
        let open = !self.bar.is_search_mode();
        self.bar.set_search_mode(open);
        if open {
            self.entry.grab_focus();
        }
    }

    fn hit(&self, i: usize) -> Option<SearchHit> {
        self.shown.borrow().get(i).cloned()
    }

    /// List what matches `text`, or the categories again when it is empty.
    pub(super) fn show(&self, text: &str, pages: &gtk::Stack) {
        let query = text.trim().to_lowercase();
        if query.is_empty() {
            self.stack.set_visible_child_name("pages");
            return;
        }
        if self.index.borrow().is_none() {
            *self.index.borrow_mut() = Some(build_search_index(pages));
        }
        let words: Vec<&str> = query.split_whitespace().collect();
        let index = self.index.borrow();
        let mut found: Vec<(u8, usize, &SearchHit)> = index
            .iter()
            .flatten()
            .enumerate()
            .filter(|(_, h)| words.iter().all(|w| h.haystack.contains(w)))
            .filter(|(_, h)| h.widget.upgrade().is_some_and(|w| shown_in_page(&w)))
            .map(|(i, h)| {
                let title = h.title.to_lowercase();
                let rank = if title.contains(&query) {
                    0
                } else if words.iter().all(|w| title.contains(w)) {
                    1
                } else {
                    2
                };
                (rank, i, h)
            })
            .collect();
        found.sort_by_key(|(rank, i, _)| (*rank, *i));
        found.truncate(SEARCH_LIMIT);

        while let Some(row) = self.results.row_at_index(0) {
            self.results.remove(&row);
        }
        let mut shown = self.shown.borrow_mut();
        shown.clear();
        for (_, _, hit) in &found {
            let line = gtk::Box::new(gtk::Orientation::Vertical, 2);
            line.set_margin_top(4);
            line.set_margin_bottom(4);
            let title = gtk::Label::new(Some(&hit.title));
            title.set_xalign(0.0);
            title.set_wrap(true);
            let place = gtk::Label::new(Some(&hit.place));
            place.set_xalign(0.0);
            place.set_wrap(true);
            place.add_css_class("caption");
            place.add_css_class("dim-label");
            line.append(&title);
            line.append(&place);
            let row = gtk::ListBoxRow::new();
            row.set_child(Some(&line));
            self.results.append(&row);
            shown.push((*hit).clone());
        }
        self.stack
            .set_visible_child_name(if shown.is_empty() { "empty" } else { "results" });
    }
}

/// Every titled group and row on the pages the window builds itself: the
/// account pages are their own components, with lists of their own, and
/// are found by their category's name.
fn build_search_index(pages: &gtk::Stack) -> Vec<SearchHit> {
    let mut out = Vec::new();
    for page in SIDE_PAGES.iter().flat_map(|(_, pages)| pages.iter()) {
        let page_title = i18n(page.title);
        let child = if page.accounts {
            None
        } else {
            pages.child_by_name(page.id)
        };
        // The category itself, for the account pages and anyone who types
        // a page's name, or what used to be a page of its own.
        let target = child.clone().unwrap_or_else(|| pages.clone().upcast());
        out.push(search_hit(
            page.id,
            &page_title,
            &page_keywords(page.id),
            &i18n("Settings"),
            &target,
        ));
        if let Some(child) = child {
            index_widget(page.id, &child, &page_title, &mut out);
        }
    }
    out
}

/// Words a category is found by beyond its name: what is on it but out of
/// the index, such as the Contacts page's own list of address books, which
/// took in the CardDAV and LDAP pages (#408).
fn page_keywords(id: &str) -> String {
    match id {
        // Translators: words the settings search finds the Contacts page by,
        // separated by spaces; keep "CardDAV" and "LDAP".
        "calendar" => i18n("calendar week working hours events alert reminders declined CalDAV"),
        "contacts" => i18n("address books CardDAV LDAP directory directories Online Accounts Nextcloud iCloud Google Microsoft vCard"),
        _ => String::new(),
    }
}

fn search_hit(
    page: &'static str,
    title: &str,
    subtitle: &str,
    place: &str,
    widget: &gtk::Widget,
) -> SearchHit {
    let clean = |t: &str| t.replace("&amp;", "&").trim().to_string();
    let (title, subtitle) = (clean(title), clean(subtitle));
    SearchHit {
        page,
        haystack: format!("{title} {subtitle} {place}").to_lowercase(),
        title,
        place: place.to_string(),
        widget: widget.downgrade(),
    }
}

/// The class that keeps a row, and what it holds, out of the settings search.
pub(crate) const NOT_SEARCHED: &str = "settings-not-searched";

/// Walk a page for its groups and rows; `place` is where they sit so far.
fn index_widget(page: &'static str, w: &gtk::Widget, place: &str, out: &mut Vec<SearchHit>) {
    // Rows that list the user's data (OpenPGP keys) are not settings, and a
    // keyring of other people's keys would bury the real results (#432).
    if w.has_css_class(NOT_SEARCHED) {
        return;
    }
    let mut place = place.to_string();
    if let Some(group) = w.downcast_ref::<adw::PreferencesGroup>() {
        let title = group.title();
        if !title.is_empty() {
            out.push(search_hit(
                page,
                &title,
                &group.description().unwrap_or_default(),
                &place,
                w,
            ));
            // A page's first group is often named as the page is.
            if !place.ends_with(title.as_str()) {
                place = format!("{place} \u{203a} {title}");
            }
        }
    } else if let Some(row) = w.downcast_ref::<adw::PreferencesRow>() {
        let title = row.title();
        let subtitle = w
            .downcast_ref::<adw::ActionRow>()
            .and_then(|r| r.subtitle())
            .or_else(|| w.downcast_ref::<adw::ExpanderRow>().map(|r| r.subtitle()))
            .unwrap_or_default();
        if !title.is_empty() {
            out.push(search_hit(page, &title, &subtitle, &place, w));
            // An expander's own rows sit inside it.
            if w.is::<adw::ExpanderRow>() {
                place = format!("{place} \u{203a} {title}");
            } else {
                return;
            }
        }
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        index_widget(page, &c, &place, out);
        child = c.next_sibling();
    }
}

/// Whether a found widget is showing on its page: it and everything up to
/// the page are visible (a row hidden for a setup that does not need it is
/// not offered).
fn shown_in_page(w: &gtk::Widget) -> bool {
    let mut cur = Some(w.clone());
    while let Some(c) = cur {
        if !c.is_visible() {
            return false;
        }
        if c.is::<gtk::Stack>() {
            return true;
        }
        cur = c.parent();
    }
    true
}

/// Scroll a found row or group into view and mark it for a moment.
fn reveal(w: &gtk::Widget) {
    scroll_to(w);
    w.add_css_class("settings-search-hit");
    let w = w.clone();
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1600), move || {
        w.remove_css_class("settings-search-hit");
    });
}

/// Scroll a row or group of a page to the top of the view.
pub(super) fn scroll_to(w: &gtk::Widget) {
    if let Some(sw) = w
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>()
    {
        // Measured against what the viewport scrolls, not the viewport:
        // the viewport's own coordinates are already scrolled.
        let content = sw.child().map(|c| match c.downcast_ref::<gtk::Viewport>() {
            Some(vp) => vp.child().unwrap_or(c.clone()),
            None => c,
        });
        if let Some(content) = content {
            if let Some(p) = w.compute_point(&content, &gtk::graphene::Point::new(0.0, 0.0)) {
                let adj = sw.vadjustment();
                let top = (f64::from(p.y()) - 24.0).clamp(
                    adj.lower(),
                    (adj.upper() - adj.page_size()).max(adj.lower()),
                );
                adj.set_value(top);
            }
        }
    }
}

impl Preferences {
    pub(super) fn on_search(&mut self, text: String) {
        // Every page has to be in the stack to be searched.
        self.mount_pages();
        if let (Some(search), Some(stack)) = (&self.search, &self.panels_stack) {
            search.show(&text, stack);
        }
    }

    pub(super) fn on_search_pick(&mut self, i: usize, sender: ComponentSender<Self>) {
        let Some(hit) = self.search.as_ref().and_then(|s| s.hit(i)) else {
            return;
        };
        // The same way a click on the category goes, so an open
        // editor still asks before it is left.
        self.select_row(hit.page);
        sender.input(PrefInput::SelectPage(hit.page.to_string()));
        if let Some(widget) = hit.widget.upgrade() {
            // After the page has been laid out: until then the row
            // has no place to scroll to.
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(80), move || {
                reveal(&widget);
            });
        }
    }

    pub(super) fn on_toggle_search(&mut self) {
        if let Some(search) = &self.search {
            search.toggle();
        }
    }
}
