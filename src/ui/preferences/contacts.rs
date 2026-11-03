//! Settings → Contacts (#408), hosted in the window's stack.

use super::*;

impl Preferences {
    pub(super) fn on_set_contact_books(&mut self, books: Vec<crate::contacts::Book>) {
        if let Some(c) = &self.contacts {
            c.emit(crate::ui::contact_sources::ContactSourcesInput::SetBooks(
                books,
            ));
        }
    }

    pub(super) fn on_set_local_contact_count(&mut self, n: usize) {
        if let Some(c) = &self.contacts {
            c.emit(crate::ui::contact_sources::ContactSourcesInput::SetLocalCount(n));
        }
    }

    pub(super) fn on_contacts(
        &mut self,
        out: crate::ui::contact_sources::ContactSourcesOutput,
        sender: ComponentSender<Self>,
    ) {
        use crate::ui::contact_sources::ContactSourcesOutput as Out;
        match out {
            Out::PageOpen { open, form } => {
                self.contacts_open = open;
                self.editor_open = form;
                self.editor_page = "contacts";
                if let Some(header) = &self.host_header {
                    header.set_visible(!open);
                }
            }
            Out::Changed => {
                let _ = sender.output(PrefOutput::ContactSourcesChanged);
            }
            Out::ImportLocal => {
                let _ = sender.output(PrefOutput::ImportLocalContacts);
            }
            Out::ExportLocal => {
                let _ = sender.output(PrefOutput::ExportLocalContacts);
            }
            Out::DeleteAllLocal => {
                let _ = sender.output(PrefOutput::DeleteAllLocalContacts);
            }
            Out::SetSearchOpen(on) => {
                let _ = sender.output(PrefOutput::SetContactsSearchOpen(on));
            }
            Out::SetContactNames(on) => {
                let _ = sender.output(PrefOutput::SetContactNames(on));
            }
            // Through the Sidebar page's row, which tells the app.
            Out::SetPlacement(placement) => {
                if let Some(row) = &self.contacts_placement_row {
                    let idx = contacts_placement_index(placement);
                    if row.selected() != idx {
                        row.set_selected(idx);
                    }
                }
            }
            Out::SetContactBook(uid) => {
                let _ = sender.output(PrefOutput::SetContactBook(uid));
            }
            Out::LoadBooks => {
                let _ = sender.output(PrefOutput::LoadContactBooks);
            }
        }
    }
}
