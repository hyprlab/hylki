//! Showcase hooks for the windows over the main one: shortcuts, Settings
//! and the account editor, Add to Contacts, message dialogs and Contacts.

use adw::prelude::*;
use relm4::prelude::*;

use crate::app::model::AppModel;
use crate::app::msg::AppMsg;
use crate::i18n::i18n;
use crate::ui::contacts_page::ContactsPageInput;
use crate::ui::showcase::showcase_capture;
use crate::ui::sidebar::CtxAction;

use super::capture::input_after;

/// HYLKI_SHOWCASE_SHORTCUTS=<height> opens the shortcut list at
/// 4 s, that tall, so all of it fits a capture (with
/// HYLKI_SHOWCASE_TOP).
pub(super) fn stage_shortcuts(sender: &ComponentSender<AppModel>) {
    if let Some(h) = std::env::var("HYLKI_SHOWCASE_SHORTCUTS")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
    {
        input_after(sender, 4, AppMsg::ShowShortcuts);
        gtk::glib::timeout_add_seconds_local_once(5, move || {
            let tops = gtk::Window::toplevels();
            if let Some(w) = (0..tops.n_items())
                .filter_map(|i| tops.item(i))
                .filter_map(|o| o.downcast::<adw::Window>().ok())
                .find(|w| {
                    w.is_visible() && w.title().is_some_and(|t| t == i18n("Keyboard Shortcuts"))
                })
            {
                w.set_default_size(w.default_width(), h);
            }
        });
    }
}

/// HYLKI_SHOWCASE_ACCOUNT=new[:<provider label>] opens a blank account
/// editor a beat after the Settings window (with
/// HYLKI_SHOWCASE_SETTINGS), so the editor itself can be captured.
pub(super) fn stage_new_account(sender: &ComponentSender<AppModel>) {
    if let Some(rest) = std::env::var("HYLKI_SHOWCASE_ACCOUNT")
        .ok()
        .and_then(|v| v.strip_prefix("new").map(str::to_string))
    {
        let provider = rest.strip_prefix(':').map(str::to_string);
        input_after(sender, 5, AppMsg::ShowcaseNewAccount(provider));
    }
}

/// HYLKI_SHOWCASE_ACCOUNT=N opens account N's editor a beat
/// after the Settings window (with HYLKI_SHOWCASE_SETTINGS),
/// so the editor itself can be captured.
pub(super) fn stage_account_settings(sender: &ComponentSender<AppModel>) {
    if let Some(Ok(n)) = std::env::var("HYLKI_SHOWCASE_ACCOUNT")
        .ok()
        .map(|v| v.parse::<u32>())
    {
        input_after(
            sender,
            5,
            AppMsg::SidebarContext(CtxAction::OpenAccountSettings(n + 1)),
        );
    }
}

/// HYLKI_SHOWCASE_EDITOR_DIRTY=1 types into the open account
/// editor's Label field at 7s, so leaving an edited editor can
/// be exercised without a keyboard.
pub(super) fn stage_editor_dirty(sender: &ComponentSender<AppModel>) {
    if std::env::var("HYLKI_SHOWCASE_EDITOR_DIRTY").is_ok() {
        input_after(sender, 7, AppMsg::ShowcaseDirtyEditor);
    }
}

/// HYLKI_SHOWCASE_SETTINGS_GO=<category> picks that sidebar
/// category at 8s, exactly as a click on its row does.
pub(super) fn stage_settings_go(sender: &ComponentSender<AppModel>) {
    if let Ok(page) = std::env::var("HYLKI_SHOWCASE_SETTINGS_GO") {
        input_after(sender, 8, AppMsg::ShowSettingsPage(page));
    }
}

/// HYLKI_SHOWCASE_ADD_CONTACT=<address> opens Add to Contacts
/// for it at 6s and captures the dialog at 10s, beside the
/// HYLKI_SHOWCASE file, so the books it offers can be checked.
#[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
pub(super) fn stage_add_contact(sender: &ComponentSender<AppModel>) {
    if let Ok(addr) = std::env::var("HYLKI_SHOWCASE_ADD_CONTACT") {
        input_after(sender, 6, AppMsg::AddContactAddr(addr));
        gtk::glib::timeout_add_seconds_local_once(10, move || {
            let tops = gtk::Window::toplevels();
            let dialog = (0..tops.n_items())
                .filter_map(|i| tops.item(i))
                .filter_map(|o| o.downcast::<adw::MessageDialog>().ok())
                .find(|d| d.is_visible());
            match (dialog, std::env::var("HYLKI_SHOWCASE")) {
                (Some(d), Ok(path)) => {
                    showcase_capture(d.upcast_ref(), &format!("{path}.contact.png"))
                }
                _ => tracing::warn!("showcase: no Add to Contacts dialog"),
            }
        });
    }
}

/// HYLKI_SHOWCASE_DIALOG=save|discard|cancel answers whatever
/// message dialog is on screen at 10s, captured first beside
/// the HYLKI_SHOWCASE file.
#[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
pub(super) fn stage_dialog() {
    if let Ok(answer) = std::env::var("HYLKI_SHOWCASE_DIALOG") {
        gtk::glib::timeout_add_seconds_local_once(10, move || {
            let tops = gtk::Window::toplevels();
            let dialog = (0..tops.n_items())
                .filter_map(|i| tops.item(i))
                .filter_map(|o| o.downcast::<adw::MessageDialog>().ok())
                .find(|d| d.is_visible());
            match dialog {
                Some(d) => {
                    if let Ok(path) = std::env::var("HYLKI_SHOWCASE") {
                        showcase_capture(d.upcast_ref(), &format!("{path}.dialog.png"));
                    }
                    // Answering is what a button click emits; the
                    // click also takes the dialog off screen, so
                    // hide it (closing would answer a second time).
                    d.response(&answer);
                    d.set_visible(false);
                }
                None => tracing::warn!("showcase: no dialog to answer"),
            }
        });
    }
}

impl AppModel {
    /// HYLKI_SHOWCASE_CONTACTS=list|new|group|search:<text>[@<n>]|filter:<n>|multi:<n>|import:<files>
    /// opens Contacts at 3s, and with "new" its editor for a new
    /// contact at 5s ("group": for a new group), or with "search:"
    /// types the text into its search
    /// then, so the directories are asked too (#307), and with
    /// "@<n>" shows only book n of the filter at 6s; "filter:"
    /// turns on its nth "Show only" filter at 5s; "multi:"
    /// selects the first n entries at 5s; "import:" imports the files,
    /// split by commas, at 5s (see `ui::contact_import`).
    pub(super) fn stage_contacts(&self, sender: &ComponentSender<AppModel>) {
        if let Ok(which) = std::env::var("HYLKI_SHOWCASE_CONTACTS") {
            input_after(sender, 3, AppMsg::OpenContacts);
            let page = self.contacts_page.sender().clone();
            if let Some(files) = which.strip_prefix("import:") {
                let paths = files.split(',').map(std::path::PathBuf::from).collect();
                input_after(sender, 5, AppMsg::ImportContacts { paths, book: None });
            } else if which == "new" || which == "group" {
                gtk::glib::timeout_add_seconds_local_once(5, move || {
                    let _ = page.send(if which == "group" {
                        ContactsPageInput::NewGroup
                    } else {
                        ContactsPageInput::NewContact
                    });
                });
            } else if let Some(filter) = which
                .strip_prefix("filter:")
                .and_then(|n| n.parse::<usize>().ok())
                .and_then(|n| crate::ui::contacts_page::ContactFilter::ALL.get(n).copied())
            {
                gtk::glib::timeout_add_seconds_local_once(5, move || {
                    let _ = page.send(ContactsPageInput::SetFilter(filter, true));
                });
            } else if let Some(n) = which
                .strip_prefix("multi:")
                .and_then(|n| n.parse::<u32>().ok())
            {
                gtk::glib::timeout_add_seconds_local_once(5, move || {
                    let _ = page.send(ContactsPageInput::SelectFirst(n));
                });
            } else if let Some(spec) = which.strip_prefix("search:") {
                let (text, book) = match spec.rsplit_once('@') {
                    Some((t, n)) if n.parse::<u32>().is_ok() => {
                        (t.to_string(), n.parse::<u32>().ok())
                    }
                    _ => (spec.to_string(), None),
                };
                let p = page.clone();
                gtk::glib::timeout_add_seconds_local_once(5, move || {
                    let _ = p.send(ContactsPageInput::SetSearch(text));
                });
                if let Some(n) = book {
                    gtk::glib::timeout_add_seconds_local_once(6, move || {
                        let _ = page.send(ContactsPageInput::PickBook(n));
                    });
                }
            }
        }
    }
}
