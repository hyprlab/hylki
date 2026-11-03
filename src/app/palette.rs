//! The keyboard help: the command palette's key, menu entries that show their
//! keys, and the shortcut tables the shortcuts window lists.

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::{i18n, i18n_noop};
use crate::models::FolderKind;
use crate::ui::preferences::PrefOutput;
use crate::ui::sidebar::CtxAction;

use super::types::UnifiedView;
use super::{AppModel, AppMsg};

/// Ctrl+K opens the command palette (#399) wherever the keyboard is, a
/// composer included: in the capture phase, ahead of a focused editor that
/// keeps the other Ctrl keys for its text. Ctrl+Shift+K opens it with `>`
/// typed, on the actions only.
pub(super) fn palette_key(sender: &ComponentSender<AppModel>) -> gtk::EventControllerKey {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let s = sender.input_sender().clone();
    keys.connect_key_pressed(move |keys, keyval, _, state| {
        use gtk::gdk::ModifierType as M;
        let mods = state & (M::CONTROL_MASK | M::SHIFT_MASK | M::ALT_MASK | M::SUPER_MASK);
        let ctrl = mods == M::CONTROL_MASK || mods == M::CONTROL_MASK | M::SHIFT_MASK;
        if ctrl && matches!(keyval, gtk::gdk::Key::k | gtk::gdk::Key::K) {
            // Not over a dialog already up, the palette itself among them.
            let dialog = keys.widget().is_some_and(|w| {
                w.downcast_ref::<adw::ApplicationWindow>()
                    .map(|w| w.visible_dialog().is_some())
                    .or_else(|| {
                        w.downcast_ref::<adw::Window>()
                            .map(|w| w.visible_dialog().is_some())
                    })
                    .unwrap_or(false)
            });
            if dialog {
                return gtk::glib::Propagation::Proceed;
            }
            let _ = s.send(AppMsg::CommandPalette(mods.contains(M::SHIFT_MASK)));
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    keys
}

/// A menu entry that shows its keys even though they belong to another
/// action (see `accounts-key`).
pub(super) fn keyed_item(label: &str, action: &str, accel: &str) -> gtk::gio::MenuItem {
    let item = gtk::gio::MenuItem::new(Some(label), Some(action));
    item.set_attribute_value("accel", Some(&accel.to_variant()));
    item
}

/// Every shortcut with its key and description, for the reference window.
const SHORTCUT_HELP: &[(&str, &[(&str, &str)])] = &[
    (
        i18n_noop("Move around"),
        &[
            ("j  or  ↓", i18n_noop("Next message")),
            ("k  or  ↑", i18n_noop("Previous message")),
            ("l  or  →", i18n_noop("Open the selected message")),
            ("h  or  ←  or  u", i18n_noop("Back to the message list")),
            (
                "→  and  ←",
                i18n_noop("Open or fold a conversation in the list"),
            ),
            ("w", i18n_noop("Next message in the conversation")),
            ("b", i18n_noop("Previous message in the conversation")),
            ("/  or  Ctrl+F", i18n_noop("Search")),
        ],
    ),
    (
        i18n_noop("Act on a message"),
        &[
            ("r  or  Ctrl+R", i18n_noop("Reply")),
            ("R  or  Ctrl+Shift+R", i18n_noop("Reply to all")),
            ("f", i18n_noop("Forward")),
            ("a", i18n_noop("Archive")),
            ("z", i18n_noop("Snooze")),
            ("d  or  Delete", i18n_noop("Delete")),
            (
                "D  or  Shift+Delete",
                i18n_noop("Delete permanently, without going to Trash"),
            ),
            ("!", i18n_noop("Mark as spam")),
            ("s", i18n_noop("Star or unstar")),
            ("m", i18n_noop("Mark read or unread")),
            ("x", i18n_noop("Select this row (for a bulk action)")),
            (
                "1 … 9",
                i18n_noop("Add or remove a tag (the first nine, in Settings order)"),
            ),
            ("0", i18n_noop("Remove every tag")),
            ("Ctrl+U", i18n_noop("View Source")),
            ("Ctrl+P", i18n_noop("Print the message you are reading")),
            ("Ctrl+Shift+P", i18n_noop("Preview it as a PDF first")),
        ],
    ),
    (
        i18n_noop("Writing"),
        &[
            ("c  or  Ctrl+N", i18n_noop("Compose")),
            ("Ctrl+Shift+N", i18n_noop("New message from a template")),
            ("Ctrl+Enter", i18n_noop("Send the message you are writing")),
            ("Ctrl+Shift+A", i18n_noop("Attach files")),
            (
                "Ctrl+Shift+T  /  C  /  B",
                i18n_noop("Go to To, Cc or Bcc, showing the row"),
            ),
            (
                "Esc",
                i18n_noop("Back out of a reply and return to the list"),
            ),
            (
                "Ctrl+.  or  Ctrl+;",
                i18n_noop("Emoji chooser, at the cursor"),
            ),
            (
                "Shift+Return",
                i18n_noop("New paragraph (Return starts a new line, unless Settings swaps them)"),
            ),
            (
                "Ctrl+B  /  Ctrl+I  /  Ctrl+U",
                i18n_noop("Bold, italic, underline"),
            ),
        ],
    ),
    (
        i18n_noop("Attachments"),
        &[
            (
                "Space  or  Enter",
                i18n_noop("Preview the highlighted attachment"),
            ),
            (
                "←  /  →",
                i18n_noop("Previous or next in the gallery's preview"),
            ),
            ("Esc", i18n_noop("Close the gallery's preview")),
        ],
    ),
    (
        i18n_noop("Everything else"),
        &[
            (
                "Ctrl+Z",
                i18n_noop("Undo the last action, or the last edit while you are writing"),
            ),
            ("Ctrl+Shift+Z", i18n_noop("Redo it (Ctrl+Y does the same)")),
            (
                "Ctrl+Shift+S",
                i18n_noop("Reveal the status bar (also: long-press Refresh)"),
            ),
            (
                "Ctrl+Shift+A",
                i18n_noop("Show or hide the accounts in the sidebar"),
            ),
            ("Ctrl+Shift+F", i18n_noop("Focus Mode on or off")),
            ("F11", i18n_noop("Fullscreen on or off")),
            (
                "Ctrl+Alt+1  /  2  /  3",
                i18n_noop("Reading pane right of the list, below it, or off"),
            ),
            ("Ctrl++  /  Ctrl+-", i18n_noop("Message zoom in or out")),
            ("Ctrl+0", i18n_noop("Message zoom back to the default")),
            ("Ctrl+,", i18n_noop("Settings")),
            (
                "Ctrl+F",
                i18n_noop("Search the Settings window, when it is open"),
            ),
            (
                "Ctrl+Shift+C",
                i18n_noop("Console mode (when enabled in Settings)"),
            ),
            (
                "Ctrl+W",
                i18n_noop("Close the window (background sync keeps running)"),
            ),
            ("Ctrl+Q", i18n_noop("Quit Hylki entirely")),
            (
                "Ctrl+K",
                i18n_noop("Command palette: search actions, folders and tags"),
            ),
            ("Ctrl+Shift+K", i18n_noop("Command palette, actions only")),
            ("?  or  Ctrl+?  or  F1", i18n_noop("This list")),
        ],
    ),
];

/// Gmail's keys (#392), shown in place of the first two sections of
/// `SHORTCUT_HELP` when Settings turns them on.
const GMAIL_HELP: &[(&str, &[(&str, &str)])] = &[
    (
        i18n_noop("Move around"),
        &[
            ("j  /  k", i18n_noop("Move down or up the list without opening; while reading, open the next or previous conversation")),
            ("o  or  Enter", i18n_noop("Open the message the cursor is on")),
            ("u  or  Esc", i18n_noop("Close the conversation, back to the list")),
            ("n  /  p", i18n_noop("Next or previous message in the conversation")),
            (";  /  :", i18n_noop("Expand or collapse every message of the conversation")),
            ("g  i", i18n_noop("Go to Inbox")),
            ("g  s", i18n_noop("Go to Starred")),
            ("g  t", i18n_noop("Go to Sent")),
            ("g  d", i18n_noop("Go to Drafts")),
            ("g  a", i18n_noop("Go to Archive")),
            ("g  c", i18n_noop("Go to Contacts")),
            ("/", i18n_noop("Search")),
        ],
    ),
    (
        i18n_noop("Act on a message"),
        &[
            ("r", i18n_noop("Reply")),
            ("a", i18n_noop("Reply to all")),
            ("f", i18n_noop("Forward")),
            ("e", i18n_noop("Archive the conversation")),
            ("]  or  }", i18n_noop("Archive the conversation and open the older one")),
            ("[  or  {", i18n_noop("Archive the conversation and open the newer one")),
            ("#", i18n_noop("Delete")),
            ("Shift+Delete", i18n_noop("Delete permanently, without going to Trash")),
            ("!", i18n_noop("Mark as spam")),
            ("s", i18n_noop("Star or unstar")),
            ("Shift+I", i18n_noop("Mark the conversation as read")),
            ("Shift+U", i18n_noop("Mark the conversation as unread and close it")),
            ("_", i18n_noop("Mark as unread from this message on and close it")),
            ("x", i18n_noop("Select this row without opening it")),
            ("*  a  /  *  n", i18n_noop("Select every row, or none")),
            ("*  r  /  *  u", i18n_noop("Select the read or the unread conversations")),
            ("*  s  /  *  t", i18n_noop("Select the starred or the unstarred conversations")),
            ("v", i18n_noop("Move to a folder")),
            ("b", i18n_noop("Snooze")),
            ("l", i18n_noop("Tags")),
            ("z", i18n_noop("Undo the last action")),
            ("1 … 9", i18n_noop("Add or remove a tag (the first nine, in Settings order)")),
            ("Shift+R  /  Shift+A", i18n_noop("Reply, or reply to all, in a window")),
            ("Shift+F", i18n_noop("Forward in a window")),
            ("c", i18n_noop("Compose")),
            ("d", i18n_noop("Compose in a window")),
            ("Shift+N", i18n_noop("Check for new mail")),
        ],
    ),
];

impl AppModel {
    /// What the command palette offers now (#399): the actions that apply
    /// to what is open or selected, then the views, every account's folders
    /// and the tags.
    pub(super) fn command_palette_entries(
        &self,
        sender: &ComponentSender<Self>,
    ) -> Vec<crate::ui::command_palette::Entry> {
        use crate::ui::command_palette::Entry;
        let mut out = Vec::new();
        let action = i18n("Action");
        let mut add = |title: String, detail: &str, key: &str, msg: fn() -> AppMsg| {
            let s = sender.input_sender().clone();
            out.push(Entry {
                title,
                detail: detail.to_string(),
                key: key.to_string(),
                action: true,
                run: Box::new(move || {
                    let _ = s.send(msg());
                }),
            });
        };
        add(i18n("New Message"), &action, "Ctrl+N", || AppMsg::Compose);
        add(
            i18n("New Message from Template"),
            &action,
            "Ctrl+Shift+N",
            || AppMsg::NewFromTemplate,
        );
        if self.reply_target().is_some() {
            add(i18n("Reply"), &action, "Ctrl+R", || AppMsg::Reply);
            add(i18n("Reply to All"), &action, "Ctrl+Shift+R", || {
                AppMsg::ReplyAll
            });
            add(i18n("Forward"), &action, "", || AppMsg::Forward);
            add(i18n("Edit as New Message"), &action, "", || {
                AppMsg::EditAsNewCurrent
            });
            add(i18n("View Source"), &action, "Ctrl+U", || {
                AppMsg::ViewSource
            });
            add(i18n("Print"), &action, "Ctrl+P", || AppMsg::PrintMessage);
            add(i18n("Print Preview"), &action, "Ctrl+Shift+P", || {
                AppMsg::PrintPreview
            });
            add(i18n("Find in Message"), &action, "", || {
                AppMsg::OpenReaderFind
            });
        }
        if self.acts_on_selection() {
            add(i18n("Archive"), &action, "", || AppMsg::Archive);
            if self.snooze_offered() {
                add(i18n("Snooze"), &action, "", || AppMsg::SnoozeMenu);
            }
            add(i18n("Delete"), &action, "Delete", || AppMsg::Delete);
            add(i18n("Delete Permanently"), &action, "Shift+Delete", || {
                AppMsg::DeleteForever
            });
            add(i18n("Mark as Spam"), &action, "", || AppMsg::MarkSpam);
            add(i18n("Star or Remove Star"), &action, "", || {
                AppMsg::ToggleStar
            });
            add(i18n("Mark as Read or Unread"), &action, "", || {
                AppMsg::ToggleReadCurrent
            });
            add(i18n("Move to Folder"), &action, "", || AppMsg::MoveToMenu);
            add(i18n("Tags"), &action, "", || AppMsg::ReaderTagMenu);
        }
        add(i18n("Open Message File…"), &action, "", || {
            AppMsg::PickMessageFile
        });
        add(i18n("Undo"), &action, "Ctrl+Z", || AppMsg::Undo);
        add(i18n("Redo"), &action, "Ctrl+Shift+Z", || AppMsg::Redo);
        add(i18n("Check for New Mail"), &action, "", || AppMsg::Refresh);
        add(i18n("Search Messages"), &action, "Ctrl+F", || {
            AppMsg::OpenListSearch
        });
        add(i18n("Show or Hide the Sidebar"), &action, "", || {
            AppMsg::ToggleSidebar
        });
        add(i18n("Reading Pane Right"), &action, "Ctrl+Alt+1", || {
            AppMsg::SetReadingPane(crate::config::ReadingPane::Right)
        });
        add(i18n("Reading Pane Below"), &action, "Ctrl+Alt+2", || {
            AppMsg::SetReadingPane(crate::config::ReadingPane::Below)
        });
        add(i18n("Reading Pane Off"), &action, "Ctrl+Alt+3", || {
            AppMsg::SetReadingPane(crate::config::ReadingPane::Off)
        });
        add(i18n("Settings"), &action, "Ctrl+,", || AppMsg::OpenSettings);
        add(i18n("Mail Accounts"), &action, "", || AppMsg::OpenAccounts);
        add(i18n("New Smart Folder…"), &action, "", || {
            AppMsg::SidebarContext(CtxAction::NewSmartFolder)
        });
        add(i18n("Keyboard Shortcuts"), &action, "Ctrl+?", || {
            AppMsg::ShowShortcuts
        });
        add(i18n("About Hylki"), &action, "", || AppMsg::OpenAbout);
        {
            let mut focus = self.focus;
            focus.enabled = !focus.enabled;
            let s = sender.input_sender().clone();
            out.push(Entry {
                title: if focus.enabled {
                    i18n("Turn Focus Mode On")
                } else {
                    i18n("Turn Focus Mode Off")
                },
                detail: action.clone(),
                key: "Ctrl+Shift+F".into(),
                action: true,
                run: Box::new(move || {
                    let _ = s.send(AppMsg::Pref(PrefOutput::SetFocusMode(focus)));
                }),
            });
        }

        let view = i18n("View");
        let mut go = |title: String, detail: String, msg: Box<dyn Fn() -> AppMsg>| {
            let s = sender.input_sender().clone();
            out.push(Entry {
                title,
                detail,
                key: String::new(),
                action: false,
                run: Box::new(move || {
                    let _ = s.send(msg());
                }),
            });
        };
        // The merged views the sidebar lists, under the sidebar's names.
        let every = i18n("All accounts");
        for kind in [
            FolderKind::Inbox,
            FolderKind::Starred,
            FolderKind::Sent,
            FolderKind::Drafts,
            FolderKind::Archive,
            FolderKind::Junk,
            FolderKind::Trash,
            FolderKind::Snoozed,
        ] {
            let listed = if kind == FolderKind::Inbox {
                self.unified_inboxes_shown()
            } else {
                self.unified_kinds_listed().has(kind)
            };
            if listed {
                go(
                    crate::ui::sidebar::kind_label(kind),
                    every.clone(),
                    Box::new(move || AppMsg::UnifiedSelected(UnifiedView::Kind(kind))),
                );
            }
        }
        go(
            i18n("Contacts"),
            view.clone(),
            Box::new(|| AppMsg::OpenContacts),
        );
        go(
            i18n("Attachments"),
            view.clone(),
            Box::new(|| AppMsg::ShowAttachments),
        );
        go(
            i18n("Outbox"),
            view.clone(),
            Box::new(|| AppMsg::ShowOutbox),
        );
        for email in self.ordered_emails() {
            let Some(account) = self.accounts.iter().find(|a| a.email == email) else {
                continue;
            };
            for f in self.folders.get(&account.id).into_iter().flatten() {
                let (account_id, folder_id, name, path) =
                    (account.id, f.id, f.name.clone(), f.path.clone());
                go(
                    f.name.clone(),
                    format!("{} · {}", account.label, f.path),
                    Box::new(move || AppMsg::FolderSelected {
                        account_id,
                        folder_id,
                        name: name.clone(),
                        path: path.clone(),
                    }),
                );
            }
        }
        let tag = i18n("Tag");
        for t in &self.tags {
            let keyword = t.keyword.clone();
            go(
                t.name.clone(),
                tag.clone(),
                Box::new(move || AppMsg::TagSelected {
                    keyword: Some(keyword.clone()),
                    account: None,
                }),
            );
        }
        let smart = i18n("Smart Folder");
        for f in &self.smart_folders {
            let id = f.id.clone();
            go(
                f.name.clone(),
                smart.clone(),
                Box::new(move || AppMsg::SmartSelected(id.clone())),
            );
        }
        out
    }

    /// A plain window listing every keyboard shortcut.
    pub(super) fn build_shortcuts_window(&self) -> adw::Window {
        // Wide enough that the longest keys ("Ctrl+B  /  Ctrl+I  /  Ctrl+U")
        // leave the description room for a line or two (#392).
        let win = adw::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title(i18n("Keyboard Shortcuts"))
            .default_width(600)
            .default_height(640)
            .build();

        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.set_margin_top(12);
        page.set_margin_bottom(18);
        page.set_margin_start(18);
        page.set_margin_end(18);

        if !self.single_key.get() {
            let text = if self.gmail_keys.get() {
                i18n("Single-key shortcuts are switched off, and Gmail keys with them. Turn them on in Settings → System.")
            } else {
                i18n("Single-key shortcuts are switched off. Turn them on in Settings → System.")
            };
            let off = gtk::Label::new(Some(text.as_str()));
            off.add_css_class("dim-label");
            off.set_wrap(true);
            off.set_xalign(0.0);
            off.set_margin_bottom(12);
            page.append(&off);
        }

        let sections: Vec<&(&str, &[(&str, &str)])> = if self.gmail_keys.get() {
            GMAIL_HELP
                .iter()
                .chain(SHORTCUT_HELP.iter().skip(2))
                .collect()
        } else {
            SHORTCUT_HELP.iter().collect()
        };
        for (section, keys) in sections {
            let title = gtk::Label::new(Some(i18n(section).as_str()));
            title.add_css_class("heading");
            title.set_halign(gtk::Align::Start);
            title.set_margin_top(14);
            title.set_margin_bottom(6);
            page.append(&title);

            let list = gtk::ListBox::new();
            list.add_css_class("boxed-list");
            list.set_selection_mode(gtk::SelectionMode::None);
            for (key, what) in *keys {
                let row = adw::ActionRow::builder().title(i18n(what)).build();
                let label = gtk::Label::new(Some(key));
                label.add_css_class("shortcut-key");
                label.set_valign(gtk::Align::Center);
                row.add_suffix(&label);
                list.append(&row);
            }
            page.append(&list);
        }

        // Which number is which tag (#157): the first nine, in Settings
        // order, each with its swatch. Only when there are tags to list.
        if !self.tags.is_empty() {
            let title = gtk::Label::new(Some(i18n("Your tags").as_str()));
            title.add_css_class("heading");
            title.set_halign(gtk::Align::Start);
            title.set_margin_top(14);
            title.set_margin_bottom(6);
            page.append(&title);
            let list = gtk::ListBox::new();
            list.add_css_class("boxed-list");
            list.set_selection_mode(gtk::SelectionMode::None);
            for (i, tag) in self.tags.iter().take(9).enumerate() {
                let row = adw::ActionRow::builder()
                    .title(gtk::glib::markup_escape_text(&tag.name))
                    .build();
                row.add_prefix(&crate::ui::context_menu::swatch_widget(&tag.color, true));
                let label = gtk::Label::new(Some(&(i + 1).to_string()));
                label.add_css_class("shortcut-key");
                label.set_valign(gtk::Align::Center);
                row.add_suffix(&label);
                list.append(&row);
            }
            page.append(&list);
        }

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&page)
            .build();
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::new());
        tv.set_content(Some(&scroller));
        win.set_content(Some(&tv));

        // Escape closes it, as does the accelerator that opened it. A reference
        // you can't dismiss with the key you opened it with is a nuisance.
        let keys = gtk::EventControllerKey::new();
        let closer = win.clone();
        keys.connect_key_pressed(move |_, keyval, _, state| {
            let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let toggles = ctrl && matches!(keyval, gtk::gdk::Key::question | gtk::gdk::Key::slash);
            if keyval == gtk::gdk::Key::Escape || toggles || keyval == gtk::gdk::Key::F1 {
                closer.close();
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        win.add_controller(keys);

        win.present();
        win
    }

    /// (Re)build the burger menu's help section: the Console entry appears
    /// only while Console mode is enabled in Settings.
    pub(super) fn rebuild_help_menu(&self) {
        self.help_menu.remove_all();
        let status_bar = if self.status_bar_shown {
            i18n("Hide Status Bar")
        } else {
            i18n("Reveal Status Bar")
        };
        self.help_menu
            .append(Some(status_bar.as_str()), Some("win.status-bar"));
        if self.console_mode {
            self.help_menu.append_item(&keyed_item(
                &i18n("Console"),
                "win.console",
                "<Ctrl><Shift>c",
            ));
        }
        self.help_menu.append(
            Some(i18n("Keyboard Shortcuts").as_str()),
            Some("win.shortcuts"),
        );
        self.help_menu.append(
            Some(format!("{} {}", i18n("About"), crate::APP_NAME).as_str()),
            Some("win.about"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shortcut_is_documented() {
        // The reference window is the only place the keys are written down, so a
        // new shortcut without a line there would be invisible.
        let documented: Vec<&str> = SHORTCUT_HELP
            .iter()
            .flat_map(|(_, keys)| keys.iter().map(|(key, _)| *key))
            .collect();
        for key in [
            "j  or  ↓",
            "r  or  Ctrl+R",
            "R  or  Ctrl+Shift+R",
            "c  or  Ctrl+N",
            "Ctrl+U",
            "a",
            "d  or  Delete",
            "w",
            "b",
            "x",
            "?  or  Ctrl+?  or  F1",
            "1 … 9",
            "0",
            "/  or  Ctrl+F",
            "Ctrl+.  or  Ctrl+;",
        ] {
            assert!(documented.contains(&key), "{key} is not in the reference");
        }
        // Every documented line has a description.
        for (_, keys) in SHORTCUT_HELP {
            for (key, what) in *keys {
                assert!(!key.trim().is_empty() && !what.trim().is_empty());
            }
        }
    }
}
