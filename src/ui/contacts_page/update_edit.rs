//! Messages about one contact: composing to it, the editors and saving
//! them, and deleting.

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::{GroupEdit, GroupFormat};
use crate::i18n::{i18n, i18n_f};
use crate::ui::context_menu::show_context_menu;

use super::detail::member_for;
use super::member_editor::collect_edit;
use super::menus::{choose_contact_files, confirm};
use super::types::{
    ContactScope, ContactsPage, ContactsPageInput, ContactsPageOutput, Editor, DIRECTORY,
};
use super::{ContactsPageWidgets, Flow};

impl ContactsPage {
    pub(super) fn on_compose(&mut self, sender: ComponentSender<Self>, email: String) -> Flow {
        let _ = sender.output(ContactsPageOutput::Compose(email));
        Flow::Continue(())
    }

    pub(super) fn on_open_url(&mut self, url: String) -> Flow {
        // Bare "example.org" URLs are common in vCards.
        let full = if url.contains("://") {
            url
        } else {
            format!("https://{url}")
        };
        crate::ui::launch::open_link(&full, None);
        Flow::Continue(())
    }

    pub(super) fn on_edit(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
    ) -> Flow {
        if let Some(idx) = self.selected.filter(|&i| i < DIRECTORY) {
            self.edit(widgets, &sender, idx);
        }
        Flow::Continue(())
    }

    pub(super) fn on_edit_index(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
        idx: usize,
    ) -> Flow {
        if idx < self.contacts.len() {
            self.selected = Some(idx);
            self.select_row_for(idx);
            self.edit(widgets, &sender, idx);
        }
        Flow::Continue(())
    }

    pub(super) fn on_new_contact(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
    ) -> Flow {
        self.new_form(widgets);
        self.render_editor(widgets, &sender);
        Flow::Continue(())
    }

    pub(super) fn on_new_group(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
    ) -> Flow {
        self.new_form(widgets);
        self.render_group_editor(widgets, &sender);
        Flow::Continue(())
    }

    pub(super) fn on_compose_group(&mut self, sender: ComponentSender<Self>, idx: usize) -> Flow {
        let Some(g) = self
            .contacts
            .get(idx)
            .filter(|g| g.reachable().next().is_some())
        else {
            return Flow::Break(());
        };
        let field = g.members_field();
        let _ = sender.output(if g.hide_addresses {
            ContactsPageOutput::ComposeBcc(field)
        } else {
            ContactsPageOutput::Compose(field)
        });
        Flow::Continue(())
    }

    pub(super) fn on_add_to_group(
        &mut self,
        sender: ComponentSender<Self>,
        group: usize,
        person: usize,
    ) -> Flow {
        let (Some(g), Some(p)) = (self.contacts.get(group), self.contacts.get(person)) else {
            return Flow::Break(());
        };
        let Some(member) = member_for(g, p, true) else {
            return Flow::Break(());
        };
        let mut members = g.members.clone();
        members.push(member);
        let edit = GroupEdit {
            name: g.name.clone(),
            members,
            hide_addresses: g.hide_addresses,
            kept: g.hidden_members.clone(),
        };
        let Some(format) = g.group else {
            return Flow::Break(());
        };
        let _ = sender.output(ContactsPageOutput::SaveGroup {
            book_uid: g.book_uid.clone(),
            format,
            original: Some(g.raw_vcard.clone()),
            edit,
        });
        Flow::Continue(())
    }

    pub(super) fn on_import_contacts(&mut self, sender: ComponentSender<Self>) -> Flow {
        let parent = relm4::main_application().active_window();
        let s = sender.clone();
        let book = self.import_book();
        choose_contact_files(parent.as_ref(), move |paths| {
            let _ = s.output(ContactsPageOutput::ImportContacts {
                paths,
                book: book.clone(),
            });
        });
        Flow::Continue(())
    }

    /// The book being looked at, for an import to offer first.
    pub(super) fn import_book(&self) -> Option<String> {
        match &self.scope {
            ContactScope::Book(uid) | ContactScope::Group { book: uid, .. } => Some(uid.clone()),
            ContactScope::All | ContactScope::Account(_) => None,
        }
    }

    pub(super) fn on_cancel_edit(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
    ) -> Flow {
        self.editor = None;
        self.editing_target = None;
        if let Some(contacts) = self.pending.take() {
            sender.input(ContactsPageInput::SetContacts(contacts));
        }
        if self.contacts.is_empty() && self.directories.is_empty() {
            widgets.page_stack.set_visible_child_name("none");
        }
        self.render_detail(widgets, &sender);
        Flow::Continue(())
    }

    pub(super) fn on_save_edit(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
    ) -> Flow {
        match &self.editor {
            Some(Editor::Contact(_)) => self.save_contact_form(widgets, sender),
            Some(Editor::Group(_)) => self.save_group_form(widgets, sender),
            None => Flow::Break(()),
        }
    }

    /// Save the group form: the group as it was edited, in its own book or,
    /// when another book was picked, made there and removed from its own.
    fn save_group_form(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
    ) -> Flow {
        let Some(Editor::Group(form)) = &self.editor else {
            return Flow::Break(());
        };
        let mut edit = GroupEdit {
            name: form.name.text().trim().to_string(),
            members: form.members.borrow().clone(),
            hide_addresses: form.hide.is_active(),
            ..GroupEdit::default()
        };
        // A group is known by its name alone.
        if edit.name.is_empty() {
            form.name.add_css_class("error");
            form.name.grab_focus();
            return Flow::Break(());
        }
        let picked = form
            .book
            .as_ref()
            .and_then(|row| form.books.get(row.selected() as usize))
            .cloned();
        let output = match self.editing_target.and_then(|i| self.contacts.get(i)) {
            // Saved in another book: made there, then removed
            // from its own.
            Some(g) if picked.as_ref().is_some_and(|b| b.uid != g.book_uid) => {
                let to = picked.unwrap_or_else(crate::contacts::local_book);
                let format = to.groups.unwrap_or(GroupFormat::Evolution);
                edit.members = self.members_for(edit.members, Some(&g.book_uid), &to.uid, format);
                let key = (g.book_uid.clone(), g.eds_uid.clone());
                self.forget(std::slice::from_ref(&key));
                let _ = sender.output(ContactsPageOutput::MoveGroup {
                    from: key.0,
                    uid: key.1,
                    to: to.uid,
                    format,
                    edit,
                });
                self.editor = None;
                self.editing_target = None;
                self.rebuild(widgets, &sender);
                return Flow::Break(());
            }
            Some(g) => {
                let format = g.group.unwrap_or(GroupFormat::Evolution);
                edit.members =
                    self.members_for(edit.members, Some(&g.book_uid), &g.book_uid, format);
                edit.kept = g.hidden_members.clone();
                ContactsPageOutput::SaveGroup {
                    book_uid: g.book_uid.clone(),
                    format,
                    original: Some(g.raw_vcard.clone()),
                    edit,
                }
            }
            None => {
                let book = picked.unwrap_or_else(crate::contacts::local_book);
                let format = book.groups.unwrap_or(GroupFormat::Evolution);
                edit.members = self.members_for(edit.members, None, &book.uid, format);
                ContactsPageOutput::SaveGroup {
                    book_uid: book.uid,
                    format,
                    original: None,
                    edit,
                }
            }
        };
        let _ = sender.output(output);
        self.editor = None;
        self.editing_target = None;
        self.render_detail(widgets, &sender);
        Flow::Break(())
    }

    /// Save the contact form: an edit, a new contact, or (another book
    /// picked) a move once it is confirmed. An empty form saves nothing.
    fn save_contact_form(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
    ) -> Flow {
        let Some(Editor::Contact(editor)) = &self.editor else {
            return Flow::Break(());
        };
        let edit = collect_edit(editor);
        // Anything at all is a contact; an empty form is nothing.
        let empty = [
            &edit.name,
            &edit.nickname,
            &edit.org,
            &edit.title,
            &edit.note,
            &edit.birthday,
        ]
        .iter()
        .all(|v| v.is_empty())
            && edit.emails.is_empty()
            && edit.phones.is_empty()
            && edit.urls.is_empty()
            && edit.addresses.is_empty()
            && !matches!(edit.photo, crate::contacts::PhotoEdit::Set(_));
        if empty {
            return Flow::Break(());
        }
        let picked = editor
            .book
            .as_ref()
            .and_then(|row| self.books.get(row.selected() as usize));
        match self.editing_target.and_then(|i| self.contacts.get(i)) {
            // Saved in another book: moved there, with the edits,
            // once the move is confirmed. The form stays till then.
            Some(c) if picked.is_some_and(|b| b.uid != c.book_uid) => {
                let to = picked.map(|b| b.uid.clone()).unwrap_or_default();
                if !self.can_move(c, &to) {
                    return Flow::Break(());
                }
                let mut moved = c.clone();
                moved.raw_vcard = crate::contacts::patched_vcard(&c.raw_vcard, &edit);
                moved.photo = match &edit.photo {
                    crate::contacts::PhotoEdit::Keep => c.photo_bytes(),
                    crate::contacts::PhotoEdit::Remove => None,
                    crate::contacts::PhotoEdit::Set(bytes) => Some(bytes.clone()),
                };
                moved.photo_key = moved.photo.as_deref().map_or(0, crate::contacts::photo_key);
                let vcard = crate::contacts::moved_vcard(&moved);
                let key = (c.book_uid.clone(), c.eds_uid.clone());
                self.ask_move(widgets, &sender, vec![key], to, Some(vcard));
                return Flow::Break(());
            }
            Some(c) => {
                let vcard = crate::contacts::patched_vcard(&c.raw_vcard, &edit);
                let _ = sender.output(ContactsPageOutput::SaveContact {
                    book_uid: c.book_uid.clone(),
                    vcard,
                });
            }
            None => {
                let vcard = crate::contacts::new_vcard(&edit);
                let book_uid = editor
                    .book
                    .as_ref()
                    .and_then(|row| self.books.get(row.selected() as usize))
                    .map(|b| b.uid.clone());
                let _ = sender.output(ContactsPageOutput::CreateContact { book_uid, vcard });
            }
        }
        // The app re-reads EDS and pushes SetContacts; until then the
        // card shows the pre-edit state rather than a half-saved one.
        self.editor = None;
        self.editing_target = None;
        self.render_detail(widgets, &sender);
        Flow::Continue(())
    }

    pub(super) fn on_open_in_gnome(&mut self, idx: usize) -> Flow {
        if let Some(c) = self.contacts.get(idx) {
            crate::ui::contacts_browser::launch_gnome_contacts_for(&c.eds_uid);
        }
        Flow::Continue(())
    }

    pub(super) fn on_delete_selected(&mut self, sender: ComponentSender<Self>) -> Flow {
        // Not while an editor is open: that contact is being changed.
        if self.editor.is_some() {
            return Flow::Break(());
        }
        if !self.multi.is_empty() {
            sender.input(ContactsPageInput::BulkDeleteRequest);
        } else if let Some(idx) = self.selected {
            sender.input(ContactsPageInput::DeleteRequest(idx));
        }
        Flow::Continue(())
    }

    pub(super) fn on_delete_request(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
        idx: usize,
    ) -> Flow {
        let Some(c) = self.contacts.get(idx) else {
            return Flow::Break(());
        };
        let synced = self.synced(&c.book_uid);
        let name = [("name", c.name.as_str())];
        let (heading, body) = if c.is_group() {
            let body = if synced {
                i18n_f(
                    "This will remove the group {name} from your address book locally and on the server. The contacts are not removed.",
                    &name,
                )
            } else {
                i18n_f("This will remove the group {name} from your address book. The contacts are not removed.", &name)
            };
            (i18n("Delete Group?"), body)
        } else {
            let body = if synced {
                i18n_f(
                    "This will remove the contact {name} from your address book locally and on the server.",
                    &name,
                )
            } else {
                i18n_f(
                    "This will remove the contact {name} from your address book.",
                    &name,
                )
            };
            (i18n("Delete Contact?"), body)
        };
        let s = sender.input_sender().clone();
        let keys = vec![(c.book_uid.clone(), c.eds_uid.clone())];
        confirm(widgets, &heading, &body, &i18n("Delete"), true, move || {
            let _ = s.send(ContactsPageInput::DeleteConfirmed(keys.clone()));
        });
        Flow::Continue(())
    }

    pub(super) fn on_row_menu(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
        idx: usize,
        x: f64,
        y: f64,
    ) -> Flow {
        let entries = self.row_menu(idx, &sender);
        if !entries.is_empty() {
            show_context_menu(&widgets.list, x, y, entries);
        }
        Flow::Continue(())
    }

    pub(super) fn on_open_photo(
        &mut self,
        sender: ComponentSender<Self>,
        name: String,
        data: Vec<u8>,
    ) -> Flow {
        let _ = sender.output(ContactsPageOutput::ShowPhoto { name, data });
        Flow::Continue(())
    }

    pub(super) fn on_delete_confirmed(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
        keys: Vec<(String, String)>,
    ) -> Flow {
        let gone = self.forget(&keys);
        if !gone.is_empty() {
            let _ = sender.output(ContactsPageOutput::DeleteContacts(gone));
        }
        // The entries leave the list right now; the EDS delete and
        // the reconciling re-read follow behind.
        self.editor = None;
        self.editing_target = None;
        self.rebuild(widgets, &sender);
        Flow::Continue(())
    }
}
