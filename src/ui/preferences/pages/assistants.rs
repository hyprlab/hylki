//! Settings → MCP: what an assistant may reach through `crate::mcp`.
//! Nothing until the user allows it, account by account, capability by
//! capability, folder by folder. The rows of an account are made when the page
//! shows, since the window is built ahead of time and accounts come and go.

use std::cell::{Cell, RefCell};
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
        AssistantCap::Trash => (i18n("Move to Trash"), None),
        AssistantCap::OpenConversation => (
            i18n("See open conversation"),
            Some(i18n("The one you are reading, as you read it.")),
        ),
        AssistantCap::OpenDrafts => (
            i18n("Read and edit open drafts"),
            Some(i18n("Those agents prepared. You accept each edit.")),
        ),
        AssistantCap::OwnDrafts => (
            i18n("Include drafts you started"),
            Some(i18n("What you are writing, unsaved text too.")),
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

        let connection = adw::PreferencesGroup::builder()
            .description(i18n(
                "Hylki contains no AI agent of its own but you can allow outside agents to access your mail using MCP (Model Context Protocol)",
            ))
            .build();
        let enable = adw::SwitchRow::builder()
            .title(i18n("Allow AI agent access"))
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
                "Nothing is shared until you allow it. Agents can't send mail, erase it permanently (moving to Trash is ok) or read encrypted mail.",
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
        .title(i18n("Command for the agent"))
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

/// One account's row: its switch shares it, and it opens on what is allowed,
/// with one switch above the rest for all of it.
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

    let full_row = adw::SwitchRow::builder()
        .title(i18n("Full access"))
        .subtitle(i18n(
            "Grant access to entire account (except Trash and Junk)",
        ))
        .active(grant.full)
        .build();
    row.add_row(&full_row);

    // Under full access the switches show on and hold still, while the choices
    // behind them are kept: setting a row from here is not the user's change.
    let showing_full = Rc::new(Cell::new(false));
    let mut cap_rows = Vec::new();
    for cap in AssistantCap::ALL {
        let (title, subtitle) = cap_text(cap);
        let switch = adw::SwitchRow::builder()
            .title(title)
            .active((grant.full && cap.in_full_access()) || grant.switch(cap))
            .build();
        if let Some(subtitle) = subtitle {
            switch.set_subtitle(&subtitle);
        }
        let state = state.clone();
        let address_for = address.to_string();
        let showing_full = showing_full.clone();
        switch.connect_active_notify(move |row| {
            if showing_full.get() {
                return;
            }
            let on = row.is_active();
            state.change(|p| p.grant_mut(&address_for).set_switch(cap, on));
        });
        row.add_row(&switch);
        cap_rows.push((cap, switch));
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
    let reset_row = adw::ActionRow::builder()
        .title(i18n("Reset to Defaults"))
        .activatable(true)
        .build();
    row.add_row(&reset_row);

    let row_for = |wanted: AssistantCap| {
        cap_rows
            .iter()
            .find(|(cap, _)| *cap == wanted)
            .map(|(_, row)| row.clone())
    };
    let (
        Some(list_row),
        Some(read_row),
        Some(search_row),
        Some(draft_row),
        Some(open_row),
        Some(drafts_row),
        Some(own_row),
    ) = (
        row_for(AssistantCap::List),
        row_for(AssistantCap::Read),
        row_for(AssistantCap::SearchServer),
        row_for(AssistantCap::Draft),
        row_for(AssistantCap::OpenConversation),
        row_for(AssistantCap::OpenDrafts),
        row_for(AssistantCap::OwnDrafts),
    )
    else {
        return row;
    };
    // What is done to a message needs the list that names it, and searching
    // on the server needs leave to read: see `AccountGrant::allows`. A new
    // message names no other, so drafting goes on without the list. Weak, for
    // the rows this is connected to: they would otherwise never be let go of
    // when the page rebuilds them.
    let follow = {
        let others: Vec<adw::SwitchRow> = cap_rows
            .iter()
            .filter(|(cap, _)| {
                cap.in_full_access()
                    && !matches!(
                        cap,
                        AssistantCap::List
                            | AssistantCap::Read
                            | AssistantCap::SearchServer
                            | AssistantCap::Draft
                    )
            })
            .map(|(_, row)| row.clone())
            .collect();
        let (full, list, read, drafts) = (
            full_row.downgrade(),
            list_row.downgrade(),
            read_row.downgrade(),
            drafts_row.downgrade(),
        );
        Rc::new(move || {
            let (Some(full), Some(list), Some(read), Some(drafts)) = (
                full.upgrade(),
                list.upgrade(),
                read.upgrade(),
                drafts.upgrade(),
            ) else {
                return;
            };
            let free = !full.is_active();
            let listed = list.is_active();
            list.set_sensitive(free);
            draft_row.set_sensitive(free);
            for row in &others {
                row.set_sensitive(free && listed);
            }
            // Junk and Trash are still chosen there under full access.
            folders.set_sensitive(!free || listed);
            read.set_sensitive(free && listed);
            search_row.set_sensitive(free && listed && read.is_active());
            // Full access does not decide these, but it does stand in for
            // what they depend on.
            open_row.set_sensitive(!free || (listed && read.is_active()));
            own_row.set_sensitive(drafts.is_active());
        })
    };
    follow();
    for switch in [&list_row, &read_row, &drafts_row] {
        let follow = follow.clone();
        switch.connect_active_notify(move |_| follow());
    }
    // Nothing here holds the reset row, so holding these from it makes no cycle.
    let reset_now = {
        let state = state.clone();
        let address = address.to_string();
        let full_row = full_row.clone();
        let cap_rows = cap_rows.clone();
        let showing_full = showing_full.clone();
        let follow = follow.clone();
        move || {
            full_row.set_active(false);
            state.change(|p| p.grant_mut(&address).reset());
            let defaults = AccountGrant::default();
            showing_full.set(true);
            for (cap, row) in &cap_rows {
                row.set_active(defaults.switch(*cap));
            }
            showing_full.set(false);
            follow();
        }
    };
    reset_row.connect_activated(move |row| confirm_reset(row, reset_now.clone()));
    {
        let state = state.clone();
        let address = address.to_string();
        full_row.connect_active_notify(move |full| {
            let on = full.is_active();
            state.change(|p| p.grant_mut(&address).full = on);
            let kept = state
                .policy
                .borrow()
                .accounts
                .get(&address.to_lowercase())
                .cloned()
                .unwrap_or_default();
            showing_full.set(true);
            for (cap, row) in &cap_rows {
                row.set_active((on && cap.in_full_access()) || kept.switch(*cap));
            }
            showing_full.set(false);
            follow();
        });
    }
    row
}

/// Ask before an account's choices are lost: nothing brings them back.
fn confirm_reset(parent: &adw::ActionRow, reset: impl Fn() + 'static) {
    let dialog = adw::AlertDialog::new(
        Some(&i18n("Reset permissions?")),
        Some(&i18n(
            "The account stays shared, but only for reading and preparing messages. Junk and Trash are hidden again.",
        )),
    );
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("reset", &i18n("Reset"));
    dialog.set_response_appearance("reset", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_, response| {
        if response == "reset" {
            reset();
        }
    });
    dialog.present(Some(parent));
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
            .sensitive(!grant.full || !AccountGrant::full_covers(folder.kind))
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
