//! Messages about selection, groups and moves: drag and drop, the sidebar
//! tree's actions, and the bulk actions on several contacts.

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::{ContactDetails, GroupEdit, Member};
use crate::i18n::{i18n, ni18n_f};

use super::menus::confirm;
use super::rows::is_member;
use super::types::{
    ContactScope, ContactsPage, ContactsPageInput, ContactsPageOutput, DropOn, TreeAction,
};
use super::{ContactsPageWidgets, Flow};

impl ContactsPage {
    pub(super) fn on_drop_contacts(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
        keys: Vec<(String, String)>,
        on: DropOn,
        moving: bool,
    ) -> Flow {
        let people: Vec<usize> = keys
            .iter()
            .filter_map(|(book, uid)| {
                self.contacts
                    .iter()
                    .position(|c| !c.is_group() && c.book_uid == *book && c.eds_uid == *uid)
            })
            .collect();
        match on {
            DropOn::Book(to) if moving => {
                let keys: Vec<(String, String)> = people
                    .iter()
                    .map(|&i| &self.contacts[i])
                    .filter(|c| self.can_move(c, &to))
                    .map(|c| (c.book_uid.clone(), c.eds_uid.clone()))
                    .collect();
                self.ask_move(widgets, &sender, keys, to, None);
            }
            // A copy leaves the contact where it was: no question.
            DropOn::Book(to) => {
                let writable = self.books.iter().any(|b| b.uid == to);
                let cards: Vec<String> = people
                    .iter()
                    .map(|&i| &self.contacts[i])
                    .filter(|c| writable && c.book_uid != to)
                    .map(crate::contacts::moved_vcard)
                    .collect();
                if !cards.is_empty() {
                    let _ = sender.output(ContactsPageOutput::CopyContacts { to, cards });
                }
            }
            DropOn::Group { book, uid } => {
                let Some(g) = self
                    .contacts
                    .iter()
                    .position(|c| c.is_group() && c.book_uid == book && c.eds_uid == uid)
                else {
                    return Flow::Break(());
                };
                self.add_to_group(&sender, g, &people);
            }
        }
        Flow::Continue(())
    }

    pub(super) fn on_tree(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
        action: TreeAction,
    ) -> Flow {
        let group = |book: &str, uid: &str| {
            self.contacts
                .iter()
                .position(|c| c.is_group() && c.book_uid == book && c.eds_uid == uid)
        };
        match action {
            TreeAction::NewContact(book) => {
                sender.input(ContactsPageInput::SetScope(ContactScope::Book(book)));
                sender.input(ContactsPageInput::NewContact);
            }
            TreeAction::NewGroup(book) => {
                sender.input(ContactsPageInput::SetScope(ContactScope::Book(book)));
                sender.input(ContactsPageInput::NewGroup);
            }
            TreeAction::EditGroup { book, uid } => {
                if let Some(idx) = group(&book, &uid) {
                    sender.input(ContactsPageInput::SetScope(ContactScope::Group {
                        book,
                        uid,
                    }));
                    sender.input(ContactsPageInput::EditIndex(idx));
                }
            }
            TreeAction::DeleteGroup { book, uid } => {
                if let Some(idx) = group(&book, &uid) {
                    sender.input(ContactsPageInput::DeleteRequest(idx));
                }
            }
            TreeAction::RenameBook(uid) => self.ask_book_name(widgets, &sender, uid),
            TreeAction::ComposeGroup { book, uid } => {
                if let Some(idx) = group(&book, &uid) {
                    sender.input(ContactsPageInput::ComposeGroup(idx));
                }
            }
        }
        Flow::Continue(())
    }

    pub(super) fn on_remove_from_group(
        &mut self,
        sender: ComponentSender<Self>,
        people: Vec<usize>,
    ) -> Flow {
        let ContactScope::Group { book, uid } = &self.scope else {
            return Flow::Break(());
        };
        let Some(g) = self
            .contacts
            .iter()
            .find(|c| c.is_group() && c.book_uid == *book && c.eds_uid == *uid)
        else {
            return Flow::Break(());
        };
        let Some(format) = g.group else {
            return Flow::Break(());
        };
        let gone: Vec<&ContactDetails> = people
            .iter()
            .filter_map(|&i| self.contacts.get(i))
            .collect();
        let members: Vec<Member> = g
            .members
            .iter()
            .filter(|m| !gone.iter().any(|p| is_member(g, m, p)))
            .cloned()
            .collect();
        if members.len() == g.members.len() {
            return Flow::Break(());
        }
        let edit = GroupEdit {
            name: g.name.clone(),
            members,
            hide_addresses: g.hide_addresses,
            kept: g.hidden_members.clone(),
        };
        let _ = sender.output(ContactsPageOutput::SaveGroup {
            book_uid: g.book_uid.clone(),
            format,
            original: Some(g.raw_vcard.clone()),
            edit,
        });
        Flow::Continue(())
    }

    pub(super) fn on_selection_changed(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
    ) -> Flow {
        // Read as the selection is now: the change that sent this
        // may have been followed by others.
        let picked = self.picked_entries();
        match picked.as_slice() {
            [] => {}
            [idx] => {
                let idx = *idx;
                if self.multi.is_empty()
                    && self.selected == Some(idx)
                    && (self.editor.is_none() || self.editing_target == Some(idx))
                {
                    return Flow::Break(());
                }
                // Moving away abandons an open editor.
                self.multi.clear();
                self.editor = None;
                self.editing_target = None;
                self.selected = Some(idx);
                self.render_detail(widgets, &sender);
            }
            many => {
                // A directory's entry is not a contact to act on.
                let keys: Vec<usize> = many
                    .iter()
                    .copied()
                    .filter(|&i| i < self.contacts.len())
                    .collect();
                if keys == self.multi && self.editor.is_none() {
                    return Flow::Break(());
                }
                self.multi = keys;
                self.editor = None;
                self.editing_target = None;
                self.selected = None;
                self.render_detail(widgets, &sender);
            }
        }
        Flow::Continue(())
    }

    pub(super) fn on_select_first(&mut self, n: u32) -> Flow {
        let entries: Vec<u32> = self
            .row_map
            .iter()
            .enumerate()
            .filter(|(_, i)| i.is_some())
            .map(|(pos, _)| pos as u32)
            .take(n as usize)
            .collect();
        self.selection.unselect_all();
        for pos in entries {
            self.selection.select_item(pos, false);
        }
        Flow::Continue(())
    }

    pub(super) fn on_bulk_add_to_group(&mut self, sender: ComponentSender<Self>, g: usize) -> Flow {
        let chosen = self.chosen();
        self.add_to_group(&sender, g, &chosen);
        Flow::Continue(())
    }

    pub(super) fn on_edit_move_confirmed(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
        to: String,
        key: (String, String),
        vcard: String,
    ) -> Flow {
        self.forget(std::slice::from_ref(&key));
        let _ = sender.output(ContactsPageOutput::MoveContacts {
            to,
            cards: vec![(key.0, key.1, vcard)],
        });
        self.editor = None;
        self.editing_target = None;
        self.rebuild(widgets, &sender);
        Flow::Continue(())
    }

    pub(super) fn on_bulk_move_confirmed(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
        to: String,
        keys: Vec<(String, String)>,
    ) -> Flow {
        let cards: Vec<(String, String, String)> = keys
            .iter()
            .filter_map(|(book, uid)| {
                self.contacts
                    .iter()
                    .find(|c| c.book_uid == *book && c.eds_uid == *uid)
            })
            .map(|c| {
                (
                    c.book_uid.clone(),
                    c.eds_uid.clone(),
                    crate::contacts::moved_vcard(c),
                )
            })
            .collect();
        if cards.is_empty() {
            return Flow::Break(());
        }
        // They leave their old books' lists now; the re-read after
        // the move shows them in the new one.
        self.forget(&keys);
        let _ = sender.output(ContactsPageOutput::MoveContacts { to, cards });
        self.rebuild(widgets, &sender);
        Flow::Continue(())
    }

    pub(super) fn on_write_failed(&mut self) -> Flow {
        self.deleted.clear();
        Flow::Continue(())
    }

    pub(super) fn on_bulk_delete_request(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
    ) -> Flow {
        let chosen = self.chosen();
        if chosen.is_empty() {
            return Flow::Break(());
        }
        let keys: Vec<(String, String)> = chosen
            .iter()
            .map(|&i| {
                (
                    self.contacts[i].book_uid.clone(),
                    self.contacts[i].eds_uid.clone(),
                )
            })
            .collect();
        let synced = chosen
            .iter()
            .any(|&i| self.synced(&self.contacts[i].book_uid));
        let groups = chosen.iter().all(|&i| self.contacts[i].is_group());
        let n = chosen.len() as u32;
        let count = n.to_string();
        let args = [("n", count.as_str())];
        let (heading, body) = match (groups, synced) {
            (true, true) => (
                ni18n_f("Delete {n} Group?", "Delete {n} Groups?", n, &args),
                ni18n_f(
                    "This will remove {n} group from your address books locally and on the server. The contacts are not removed.",
                    "This will remove {n} groups from your address books locally and on the server. The contacts are not removed.",
                    n,
                    &args,
                ),
            ),
            (true, false) => (
                ni18n_f("Delete {n} Group?", "Delete {n} Groups?", n, &args),
                ni18n_f(
                    "This will remove {n} group from your address book. The contacts are not removed.",
                    "This will remove {n} groups from your address books. The contacts are not removed.",
                    n,
                    &args,
                ),
            ),
            (false, true) => (
                ni18n_f("Delete {n} Contact?", "Delete {n} Contacts?", n, &args),
                ni18n_f(
                    "This will remove {n} contact from your address books locally and on the server.",
                    "This will remove {n} contacts from your address books locally and on the server.",
                    n,
                    &args,
                ),
            ),
            (false, false) => (
                ni18n_f("Delete {n} Contact?", "Delete {n} Contacts?", n, &args),
                ni18n_f(
                    "This will remove {n} contact from your address book.",
                    "This will remove {n} contacts from your address books.",
                    n,
                    &args,
                ),
            ),
        };
        let s = sender.input_sender().clone();
        confirm(widgets, &heading, &body, &i18n("Delete"), true, move || {
            let _ = s.send(ContactsPageInput::DeleteConfirmed(keys.clone()));
        });
        Flow::Continue(())
    }
}
