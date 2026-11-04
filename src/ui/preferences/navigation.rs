//! The category sidebar (#141): its pages, showing one, and asking before
//! an open editor is left.

use super::*;

/// One sidebar entry (#141): the stack child it shows, and whether that
/// child lives in the accounts component (whose own stack then switches).
pub(super) struct SidePage {
    pub(super) id: &'static str,
    pub(super) title: &'static str,
    pub(super) icon: &'static str,
    pub(super) accounts: bool,
}

/// The sidebar, section by section.
pub(super) const SIDE_PAGES: &[(&str, &[SidePage])] = &[
    (
        i18n_noop("Accounts"),
        &[
            SidePage {
                id: "accounts",
                title: i18n_noop("Mail Accounts"),
                icon: "avatar-default-symbolic",
                accounts: true,
            },
            SidePage {
                id: "tags",
                title: i18n_noop("Tags"),
                icon: "tag-outline-symbolic",
                accounts: true,
            },
            SidePage {
                id: "filters",
                title: i18n_noop("Filters"),
                icon: "filter-folder-symbolic",
                accounts: true,
            },
            SidePage {
                id: "smart",
                title: i18n_noop("Smart Folders"),
                icon: "folder-saved-search-symbolic",
                accounts: true,
            },
            SidePage {
                id: "senders",
                title: i18n_noop("Senders"),
                icon: "contact-new-symbolic",
                accounts: true,
            },
            SidePage {
                id: "openpgp",
                title: i18n_noop("OpenPGP"),
                icon: "channel-secure-symbolic",
                accounts: false,
            },
            SidePage {
                id: "cloud",
                title: i18n_noop("Cloud Storage"),
                icon: "cloud-symbolic",
                accounts: false,
            },
            SidePage {
                id: "translation",
                title: i18n_noop("Translation"),
                icon: "translate-symbolic",
                accounts: false,
            },
            SidePage {
                id: "contacts",
                title: i18n_noop("Contacts"),
                icon: "x-office-address-book-symbolic",
                accounts: false,
            },
        ],
    ),
    (
        i18n_noop("Settings"),
        &[
            SidePage {
                id: "general",
                title: i18n_noop("General"),
                icon: "puzzle-piece-symbolic",
                accounts: false,
            },
            SidePage {
                id: "appearance",
                title: i18n_noop("Appearance"),
                icon: "preferences-desktop-appearance-symbolic",
                accounts: false,
            },
            SidePage {
                id: "sidebar",
                title: i18n_noop("Sidebar"),
                icon: "sidebar-show-symbolic",
                accounts: false,
            },
            SidePage {
                id: "list",
                title: i18n_noop("Message List"),
                icon: "view-list-bullet-symbolic",
                accounts: false,
            },
            SidePage {
                id: "conversations",
                title: i18n_noop("Conversations"),
                icon: "chat-bubbles-text-symbolic",
                accounts: false,
            },
            SidePage {
                id: "reading",
                title: i18n_noop("Reading"),
                icon: "hylki-mail-read-symbolic",
                accounts: false,
            },
            SidePage {
                id: "composing",
                title: i18n_noop("Composing"),
                icon: "document-edit-symbolic",
                accounts: false,
            },
            SidePage {
                id: "privacy",
                title: i18n_noop("Privacy"),
                icon: "security-high-symbolic",
                accounts: false,
            },
            SidePage {
                id: "assistants",
                title: i18n_noop("AI Assistants"),
                icon: "network-workgroup-symbolic",
                accounts: false,
            },
            SidePage {
                id: "datetime",
                title: i18n_noop("Date and Time"),
                icon: "x-office-calendar-symbolic",
                accounts: false,
            },
            SidePage {
                id: "system",
                title: i18n_noop("System"),
                icon: "applications-system-symbolic",
                accounts: false,
            },
            SidePage {
                id: "backup",
                title: i18n_noop("Backup"),
                icon: "document-save-symbolic",
                accounts: false,
            },
        ],
    ),
];

pub(super) fn side_page(id: &str) -> Option<&'static SidePage> {
    SIDE_PAGES
        .iter()
        .flat_map(|(_, pages)| pages.iter())
        .find(|p| p.id == id)
}

impl Preferences {
    /// Select a sidebar row by page id; the selection handler shows it.
    pub(super) fn select_row(&self, id: &str) {
        let Some(list) = &self.side_list else { return };
        let name = format!("page:{id}");
        let mut i = 0;
        while let Some(row) = list.row_at_index(i) {
            if row.widget_name() == name {
                list.select_row(Some(&row));
                return;
            }
            i += 1;
        }
    }

    /// The sidebar moved away from an open account editor: save, discard,
    /// or stay. Staying puts the selection back on Mail Accounts.
    #[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
    pub(super) fn ask_to_leave_editor(&self, id: &str, sender: &ComponentSender<Self>) {
        let parent = relm4::main_application().active_window();
        let (heading, body) = match self.editor_page {
            "filters" => (
                i18n("Save the filter?"),
                i18n("Your changes are lost if you discard them."),
            ),
            "smart" => (
                i18n("Save the smart folder?"),
                i18n("Your changes are lost if you discard them."),
            ),
            "tags" => (
                i18n("Save the tag?"),
                i18n("Your changes are lost if you discard them."),
            ),
            "contacts" => (
                i18n("Save the address book?"),
                i18n("Your changes are lost if you discard them."),
            ),
            _ => (
                i18n("Save the account?"),
                i18n("Your changes are lost if you discard them."),
            ),
        };
        let dialog = adw::MessageDialog::new(parent.as_ref(), Some(&heading), Some(&body));
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("discard", &i18n("Discard"));
        dialog.add_response("save", &i18n("Save"));
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let s = sender.clone();
        let accounts = self.accounts_sender.clone();
        let cloud = self.cloud.as_ref().map(|c| c.sender().clone());
        let contacts = self.contacts.as_ref().map(|c| c.sender().clone());
        let editor_page = self.editor_page;
        let id = id.to_string();
        dialog.connect_response(None, move |_, resp| {
            let cloud_editor = editor_page == "cloud";
            let contacts_editor = editor_page == "contacts";
            match resp {
                "save" => {
                    if contacts_editor {
                        if let Some(c) = &contacts {
                            let _ = c
                                .send(crate::ui::contact_sources::ContactSourcesInput::SaveClicked);
                        }
                    } else if cloud_editor {
                        if let Some(c) = &cloud {
                            let _ =
                                c.send(crate::ui::cloud_accounts::CloudAccountsInput::SaveClicked);
                        }
                    } else {
                        let _ = accounts.send(crate::ui::accounts::AccountsInput::SaveOpenPage);
                    }
                    s.input(PrefInput::LeaveEditorTo(id.clone()));
                }
                "discard" => {
                    if contacts_editor {
                        if let Some(c) = &contacts {
                            let _ = c
                                .send(crate::ui::contact_sources::ContactSourcesInput::CloseEditor);
                        }
                    } else if cloud_editor {
                        if let Some(c) = &cloud {
                            let _ =
                                c.send(crate::ui::cloud_accounts::CloudAccountsInput::CloseEditor);
                        }
                    } else {
                        let _ = accounts.send(crate::ui::accounts::AccountsInput::CloseEditor);
                    }
                    s.input(PrefInput::LeaveEditorTo(id.clone()));
                }
                // Staying: put the selection back on the editor's own page.
                _ => s.input(PrefInput::ShowPageById(editor_page.into())),
            }
        });
        dialog.present();
    }

    /// Return the deferred pages to the stack (see `deferred_pages`).
    pub(super) fn mount_pages(&self) {
        let pages = std::mem::take(&mut *self.deferred_pages.borrow_mut());
        if let Some(stack) = &self.panels_stack {
            for (name, child) in pages {
                stack.add_named(&child, Some(&name));
            }
        }
    }

    /// Show a category: the accounts component's page, or one of ours.
    fn show_page(&self, id: &str) {
        let Some(page) = side_page(id) else { return };
        self.mount_pages();
        if let Some(stack) = &self.panels_stack {
            if page.accounts {
                stack.set_visible_child_name("accounts");
                let _ = self
                    .accounts_sender
                    .send(crate::ui::accounts::AccountsInput::ShowPage(id.to_string()));
            } else {
                stack.set_visible_child_name(id);
            }
        }
        if let Some(cp) = &self.content_page {
            cp.set_title(&i18n(page.title));
        }
        if let Some(split) = &self.split {
            if split.is_collapsed() {
                split.set_show_content(true);
            }
        }
    }
}

impl Preferences {
    pub(super) fn on_set_accounts_panel(
        &mut self,
        panel: gtk::Widget,
        accounts: relm4::Sender<crate::ui::accounts::AccountsInput>,
    ) {
        self.accounts_sender = accounts;
        if let Some(slot) = &self.accounts_slot {
            slot.set_child(Some(&panel));
        }
    }

    pub(super) fn on_select_page(&mut self, id: String, sender: ComponentSender<Self>) {
        if self.contacts_open && !self.editor_open && id != "contacts" {
            // A Contacts page with nothing to save closes on its
            // own, so the window's header comes back for the next.
            if let Some(c) = &self.contacts {
                c.emit(crate::ui::contact_sources::ContactSourcesInput::CloseEditor);
            }
        }
        if self.editor_open && id != self.editor_page {
            // The accounts panel's editors can tell whether anything
            // was actually changed, and answer with either
            // LeaveEditorTo or LeaveEditorPrompt. The cloud and
            // contacts editors keep no such record, so they are
            // always asked about.
            if self.editor_page == "cloud" || self.editor_page == "contacts" {
                self.ask_to_leave_editor(&id, &sender);
            } else {
                let _ = self
                    .accounts_sender
                    .send(crate::ui::accounts::AccountsInput::LeaveRequest(id));
            }
        } else {
            self.show_page(&id);
            if id == "system" {
                sender.input(PrefInput::NautilusRefresh);
                sender.input(PrefInput::MeasureStorage);
            }
            let _ = sender.output(PrefOutput::PageShown(id));
        }
    }

    pub(super) fn on_reveal_columns(&mut self) {
        self.select_row("list");
        // Once the page is shown and laid out, which the selection
        // sets going, can the group be scrolled to.
        if let Some(group) = self.columns_group.clone() {
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
                scroll_to(&group)
            });
        }
    }

    pub(super) fn on_leave_editor_to(&mut self, id: String, sender: ComponentSender<Self>) {
        // The row the user clicked is already the selected one — the
        // click selected it before any of this — so re-selecting it
        // emits nothing and the page has to be shown outright.
        self.select_row(&id);
        self.show_page(&id);
        if id == "system" {
            sender.input(PrefInput::NautilusRefresh);
        }
        let _ = sender.output(PrefOutput::PageShown(id));
    }

    pub(super) fn on_editor_open(&mut self, page: Option<&'static str>) {
        self.editor_open = page.is_some();
        self.editor_page = page.unwrap_or("accounts");
        if let Some(header) = &self.host_header {
            header.set_visible(page.is_none());
        }
    }

    pub(super) fn on_cloud_editor_open(&mut self, open: bool) {
        self.editor_open = open;
        self.editor_page = "cloud";
        if let Some(header) = &self.host_header {
            header.set_visible(!open);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_pages_have_unique_ids_and_are_found_by_id() {
        let mut ids: Vec<&str> = SIDE_PAGES
            .iter()
            .flat_map(|(_, pages)| pages.iter().map(|p| p.id))
            .collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "side page ids must be unique");
        for id in ids {
            assert_eq!(side_page(id).map(|p| p.id), Some(id));
        }
        assert!(side_page("nope").is_none());
    }
}
