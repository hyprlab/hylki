//! Settings → MCP: what an assistant may reach through `crate::mcp`.
//! Nothing until the user allows it, account by account, capability by
//! capability, folder by folder. The rows of an account are made when the page
//! shows, since the window is built ahead of time and accounts come and go.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use super::super::system::hide_edit_icon;
use super::super::{PrefOutput, Preferences, ViewSender};
use crate::config::{AccountGrant, AssistantCap, AssistantPolicy};
use crate::i18n::i18n;

/// The page, and the rows on it that the window reaches later.
#[derive(Debug)]
pub(in crate::ui::preferences) struct AssistantsPage {
    pub(in crate::ui::preferences) page: adw::PreferencesPage,
    activity_row: adw::ActionRow,
    state: Rc<PageState>,
}

/// What the rows' handlers share.
struct PageState {
    policy: RefCell<AssistantPolicy>,
    sender: ViewSender,
    /// The account rows on the page now.
    account_rows: RefCell<Vec<adw::ExpanderRow>>,
    /// What assistants did, newest first: (title, subtitle).
    activity: RefCell<Vec<(String, String)>>,
}

impl std::fmt::Debug for PageState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageState").finish_non_exhaustive()
    }
}

impl PageState {
    /// Change the policy and tell the app.
    fn change(&self, edit: impl FnOnce(&mut AssistantPolicy)) {
        let policy = {
            let mut policy = self.policy.borrow_mut();
            edit(&mut policy);
            policy.clone()
        };
        let _ = self
            .sender
            .output(PrefOutput::SetAssistantPolicy(Box::new(policy)));
    }
}

/// A capability's row: its title and, where the title cannot say it, more.
fn cap_text(cap: AssistantCap) -> (String, Option<String>) {
    match cap {
        AssistantCap::List => (
            i18n("See message lists"),
            Some(i18n("Senders, subjects, dates, previews and folders.")),
        ),
        AssistantCap::Read => (i18n("Read message text"), None),
        AssistantCap::SearchServer => (
            i18n("Search on the mail server"),
            Some(i18n("Finds mail not downloaded yet.")),
        ),
        AssistantCap::Attachments => (i18n("Read attachments"), None),
        AssistantCap::Draft => (
            i18n("Prepare messages"),
            Some(i18n("Each opens in a window for you to check and send.")),
        ),
        AssistantCap::MarkRead => (i18n("Mark as read or unread"), None),
        AssistantCap::Star => (i18n("Add and remove stars"), None),
        AssistantCap::Tag => (i18n("Add and remove tags"), None),
        AssistantCap::Move => (i18n("Move and archive"), None),
        AssistantCap::Trash => (
            i18n("Move to Trash"),
            Some(i18n("Mail already in Trash stays there.")),
        ),
    }
}

impl AssistantsPage {
    pub(in crate::ui::preferences) fn build(model: &Preferences, sender: &ViewSender) -> Self {
        let page = adw::PreferencesPage::new();
        let state = Rc::new(PageState {
            policy: RefCell::new(model.assistant_policy.clone()),
            sender: sender.clone(),
            account_rows: RefCell::default(),
            activity: RefCell::new(model.assistant_activity.clone()),
        });

        let connection = adw::PreferencesGroup::new();
        let enable = adw::SwitchRow::builder()
            .title(i18n("Allow AI assistants"))
            .subtitle(i18n("Connects through the Model Context Protocol."))
            .active(model.assistant_policy.enabled)
            .build();
        {
            let state = state.clone();
            enable.connect_active_notify(move |row| {
                let on = row.is_active();
                state.change(|p| p.enabled = on);
            });
        }
        connection.add(&enable);
        connection.add(&command_row());

        let activity_row = adw::ActionRow::builder()
            .title(i18n("Recent Activity"))
            .subtitle(&model.assistant_status)
            .activatable(true)
            .build();
        activity_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        {
            let state = state.clone();
            activity_row.connect_activated(move |row| show_activity(row, &state));
        }
        connection.add(&activity_row);
        page.add(&connection);

        let accounts = adw::PreferencesGroup::builder()
            .title(i18n("Accounts"))
            .description(i18n(
                "Nothing is shared until you allow it. Encrypted mail is never shared.",
            ))
            .build();
        page.add(&accounts);

        page.add(&contacts_group(&state));

        // The window is built ahead of time, and accounts change after.
        {
            let state = state.clone();
            let accounts = accounts.clone();
            page.connect_map(move |_| rebuild_accounts(&accounts, &state));
        }
        rebuild_accounts(&accounts, &state);

        Self {
            page,
            activity_row,
            state,
        }
    }

    /// The view's `#[watch]` setters for this page (see the module
    /// comment of `pages`).
    pub(in crate::ui::preferences) fn update_view(&self, model: &Preferences) {
        self.activity_row
            .set_subtitle(&gtk::glib::markup_escape_text(&model.assistant_status));
        *self.state.activity.borrow_mut() = model.assistant_activity.clone();
    }
}

/// The command an assistant's client starts, ready to copy.
fn command_row() -> adw::EntryRow {
    let command = crate::mcp::connect_command();
    let row = adw::EntryRow::builder()
        .title(i18n("Command for the assistant"))
        .text(&command)
        .editable(false)
        .build();
    row.add_css_class("monospace");
    hide_edit_icon(row.upcast_ref());
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .valign(gtk::Align::Center)
        .tooltip_text(i18n("Copy"))
        .css_classes(["flat"])
        .build();
    copy.connect_clicked(move |b| {
        b.clipboard().set_text(&command);
        b.set_icon_name("verified-checkmark-symbolic");
        let b = b.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
            b.set_icon_name("edit-copy-symbolic");
        });
    });
    row.add_suffix(&copy);
    row
}

fn contacts_group(state: &Rc<PageState>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(i18n("Contacts"))
        .build();
    let policy = state.policy.borrow();

    let books = adw::SwitchRow::builder()
        .title(i18n("Address books and directories"))
        .active(policy.address_books)
        .build();
    let s = state.clone();
    books.connect_active_notify(move |row| {
        let on = row.is_active();
        s.change(|p| p.address_books = on);
    });
    group.add(&books);

    let people = adw::SwitchRow::builder()
        .title(i18n("People in your mail"))
        .subtitle(i18n("From every account, shared or not."))
        .active(policy.mail_people)
        .build();
    let s = state.clone();
    people.connect_active_notify(move |row| {
        let on = row.is_active();
        s.change(|p| p.mail_people = on);
    });
    group.add(&people);
    group
}

/// The accounts' rows, made anew from the accounts as they are now.
fn rebuild_accounts(group: &adw::PreferencesGroup, state: &Rc<PageState>) {
    for row in state.account_rows.borrow_mut().drain(..) {
        group.remove(&row);
    }
    let accounts = crate::config::load().unwrap_or_default();
    for (index, account) in accounts.iter().enumerate().filter(|(_, a)| a.enabled) {
        let row = account_row(
            &account.email,
            account.label.as_deref().unwrap_or(&account.name),
            index as u32 + 1,
            state,
        );
        group.add(&row);
        state.account_rows.borrow_mut().push(row);
    }
}

/// One account's row: its switch shares it, and it opens on what is allowed.
fn account_row(
    address: &str,
    name: &str,
    account_id: u32,
    state: &Rc<PageState>,
) -> adw::ExpanderRow {
    let grant: AccountGrant = state
        .policy
        .borrow()
        .accounts
        .get(&address.to_lowercase())
        .cloned()
        .unwrap_or_default();
    let row = adw::ExpanderRow::builder()
        .title(gtk::glib::markup_escape_text(address).as_str())
        .show_enable_switch(true)
        .enable_expansion(grant.on)
        .expanded(grant.on)
        .build();
    if !name.is_empty() && !name.eq_ignore_ascii_case(address) {
        row.set_subtitle(&gtk::glib::markup_escape_text(name));
    }
    {
        let state = state.clone();
        let address = address.to_string();
        row.connect_enable_expansion_notify(move |row| {
            let on = row.enables_expansion();
            state.change(|p| p.grant_mut(&address).on = on);
        });
    }

    let mut needs_list = Vec::new();
    let mut list_row = None;
    let mut read_row = None;
    let mut search_row = None;
    for cap in AssistantCap::ALL {
        let (title, subtitle) = cap_text(cap);
        let switch = adw::SwitchRow::builder()
            .title(title)
            .active(grant.switch(cap))
            .build();
        if let Some(subtitle) = subtitle {
            switch.set_subtitle(&subtitle);
        }
        let state = state.clone();
        let address_for = address.to_string();
        switch.connect_active_notify(move |row| {
            let on = row.is_active();
            state.change(|p| p.grant_mut(&address_for).set_switch(cap, on));
        });
        row.add_row(&switch);
        match cap {
            AssistantCap::List => list_row = Some(switch),
            // A new message names no other, so drafting goes on without it.
            AssistantCap::Draft => {}
            // These two follow the list as well, and each other: see below.
            AssistantCap::Read => read_row = Some(switch),
            AssistantCap::SearchServer => search_row = Some(switch),
            _ => needs_list.push(switch),
        }
    }
    let folders = adw::ActionRow::builder()
        .title(i18n("Folders"))
        .subtitle(i18n("Junk and Trash are hidden until you allow them."))
        .activatable(true)
        .build();
    folders.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    {
        let state = state.clone();
        let address = address.to_string();
        folders.connect_activated(move |row| show_folders(row, &address, account_id, &state));
    }
    row.add_row(&folders);

    // What is done to a message needs the list that names it, and searching
    // on the server needs leave to read: see `AccountGrant::allows`.
    if let (Some(list_row), Some(read_row), Some(search_row)) = (list_row, read_row, search_row) {
        let follow = {
            let (list_row, read_row) = (list_row.clone(), read_row.clone());
            Rc::new(move || {
                let on = list_row.is_active();
                for row in &needs_list {
                    row.set_sensitive(on);
                }
                folders.set_sensitive(on);
                read_row.set_sensitive(on);
                search_row.set_sensitive(on && read_row.is_active());
            })
        };
        follow();
        for switch in [&list_row, &read_row] {
            let follow = follow.clone();
            switch.connect_active_notify(move |_| follow());
        }
    }
    row
}

/// An account's folders, each with its own switch.
fn show_folders(parent: &adw::ActionRow, address: &str, account_id: u32, state: &Rc<PageState>) {
    let folders = crate::cache::Cache::open_reader()
        .map(|cache| cache.load_folders(account_id))
        .unwrap_or_default();
    let grant = state
        .policy
        .borrow()
        .accounts
        .get(&address.to_lowercase())
        .cloned()
        .unwrap_or_default();
    let group = adw::PreferencesGroup::new();
    for folder in folders {
        let row = adw::SwitchRow::builder()
            .title(gtk::glib::markup_escape_text(&folder.path).as_str())
            .active(grant.folder_allowed(&folder.path, folder.kind))
            .build();
        let state = state.clone();
        let address = address.to_string();
        let path = folder.path.clone();
        row.connect_active_notify(move |row| {
            let on = row.is_active();
            state.change(|p| {
                p.grant_mut(&address).folders.insert(path.clone(), on);
            });
        });
        group.add(&row);
    }
    present_list(parent, &i18n("Folders"), group);
}

/// The record of what assistants did.
fn show_activity(parent: &adw::ActionRow, state: &Rc<PageState>) {
    let group = adw::PreferencesGroup::new();
    let lines = state.activity.borrow();
    if lines.is_empty() {
        group.add(
            &adw::ActionRow::builder()
                .title(i18n("No activity yet."))
                .build(),
        );
    }
    for (title, subtitle) in lines.iter() {
        group.add(
            &adw::ActionRow::builder()
                .title(gtk::glib::markup_escape_text(title).as_str())
                .subtitle(gtk::glib::markup_escape_text(subtitle).as_str())
                .build(),
        );
    }
    present_list(parent, &i18n("Recent Activity"), group);
}

/// A dialog holding one group of rows.
fn present_list(parent: &impl IsA<gtk::Widget>, title: &str, group: adw::PreferencesGroup) {
    let page = adw::PreferencesPage::new();
    page.add(&group);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(520)
        .content_height(560)
        .child(&toolbar)
        .build();
    dialog.present(Some(parent));
}
