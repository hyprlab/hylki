//! The Contacts page's helpers that work out what is in scope and
//! selected, ask before moving, and keep the sidebar's tree and the book
//! picker in step.

use std::collections::HashMap;

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::{ContactDetails, GroupEdit, GroupFormat, Member};
use crate::i18n::{i18n, i18n_f, ni18n_f};

use super::detail::member_for;
use super::menus::{confirm, matches_query, names_groups};
use super::rows::{is_member, split_book_name, Line};
use super::types::{
    ContactFilter, ContactScope, ContactTree, ContactsPage, ContactsPageInput, ContactsPageOutput,
    TreeBook, TreeGroup, DIRECTORY,
};
use super::ContactsPageWidgets;

impl ContactsPage {
    /// The entries held, their bytes (the floor: strings, vCards and photos)
    /// and how much of that is photos, for the memory report.
    pub(crate) fn memory_stats(&self) -> (usize, usize, usize) {
        let entries = self
            .contacts
            .iter()
            .chain(self.pending.iter().flatten())
            .chain(&self.directory_hits);
        let (mut n, mut bytes, mut photos) = (0, 0, 0);
        for c in entries {
            let photo = c.photo.as_ref().map_or(0, Vec::len);
            let labeled = |l: &[crate::contacts::Labeled]| {
                l.iter()
                    .map(|x| 48 + x.label.len() + x.value.len())
                    .sum::<usize>()
            };
            n += 1;
            photos += photo;
            bytes += std::mem::size_of::<ContactDetails>()
                + photo
                + c.raw_vcard.len()
                + c.name.len()
                + c.note.len()
                + labeled(&c.emails)
                + labeled(&c.phones)
                + labeled(&c.addresses);
        }
        (n, bytes, photos)
    }

    /// Whether a contact matches the active search.
    pub(super) fn matches(&self, c: &ContactDetails) -> bool {
        self.in_scope(c) && matches_query(c, &self.query)
    }

    /// Whether the list's scope takes `c`, the search aside.
    pub(super) fn in_scope(&self, c: &ContactDetails) -> bool {
        match &self.scope {
            // Every book's groups are listed only when asked for, by the
            // Groups filter or a search for them.
            ContactScope::All => {
                !c.is_group() || self.groups_only() || names_groups(&self.query)
            }
            // A book lists its groups as well as its people, the groups
            // first; so does an account of several books.
            ContactScope::Book(book) => c.book_uid == *book,
            ContactScope::Account(key) => self.book_key(&c.book_uid) == *key,
            // The group itself, first, then its people.
            ContactScope::Group { book, uid } => match self.scope_group() {
                Some(g) if c.is_group() => {
                    c.book_uid == *book && c.eds_uid == *uid && std::ptr::eq(g, c)
                }
                Some(g) => g.members.iter().any(|m| is_member(g, m, c)),
                None => false,
            },
        }
    }

    /// The key the books of `uid`'s account share ([`TreeBook::key`]).
    pub(super) fn book_key(&self, uid: &str) -> String {
        let directory = self.directories.iter().any(|(d, _)| d == uid);
        self.filter_books
            .iter()
            .find(|(u, _)| u == uid)
            .map(|(_, name)| split_book_name(name, directory).0)
            .unwrap_or_default()
    }

    /// The group the list shows, in group scope.
    pub(super) fn scope_group(&self) -> Option<&ContactDetails> {
        let ContactScope::Group { book, uid } = &self.scope else {
            return None;
        };
        self.contacts
            .iter()
            .find(|c| c.is_group() && c.book_uid == *book && c.eds_uid == *uid)
    }

    /// The book a new contact or group starts in: the one the list shows,
    /// when it can take it, else the default (first).
    pub(super) fn scope_book(&self, books: &[crate::contacts::Book]) -> usize {
        let shown = match &self.scope {
            ContactScope::All | ContactScope::Account(_) => None,
            ContactScope::Book(book) | ContactScope::Group { book, .. } => Some(book),
        };
        shown
            .and_then(|b| books.iter().position(|x| x.uid == *b))
            .unwrap_or(0)
    }

    /// Put these people (indexes into `contacts`) in the group at `g`,
    /// those it can take and does not have.
    pub(super) fn add_to_group(&self, sender: &ComponentSender<Self>, g: usize, people: &[usize]) {
        let Some(group) = self.contacts.get(g) else {
            return;
        };
        let Some(format) = group.group else { return };
        let mut members = group.members.clone();
        for &p in people {
            if let Some(m) = self
                .contacts
                .get(p)
                .and_then(|p| member_for(group, p, true))
            {
                if !members.iter().any(|x| x.same(&m)) {
                    members.push(m);
                }
            }
        }
        if members.len() == group.members.len() {
            return;
        }
        let edit = GroupEdit {
            name: group.name.clone(),
            members,
            hide_addresses: group.hide_addresses,
            kept: group.hidden_members.clone(),
        };
        let _ = sender.output(ContactsPageOutput::SaveGroup {
            book_uid: group.book_uid.clone(),
            format,
            original: Some(group.raw_vcard.clone()),
            edit,
        });
    }

    /// Ask for a name for the book `uid`, Hylki's own: GNOME and Evolution
    /// keep the book's. One given before can be dropped again.
    pub(super) fn ask_book_name(
        &self,
        widgets: &ContactsPageWidgets,
        sender: &ComponentSender<Self>,
        uid: String,
    ) {
        let Some((_, name)) = self.filter_books.iter().find(|(u, _)| *u == uid) else {
            return;
        };
        let own = name
            .split_once(" · ")
            .map_or(name.as_str(), |(_, own)| own)
            .to_string();
        let given = crate::contacts::contact_sources::names().contains_key(&uid);
        let entry = gtk::Entry::new();
        entry.set_text(&own);
        entry.set_activates_default(true);
        let dialog = adw::AlertDialog::new(
            Some(&i18n("Rename Address Book")),
            Some(&i18n(
                "The name is used in Hylki only; GNOME Contacts and Evolution keep the book's own.",
            )),
        );
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", &i18n("Cancel"));
        if given {
            dialog.add_response("reset", &i18n("Use Its Own Name"));
        }
        dialog.add_response("rename", &i18n("Rename"));
        dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("rename"));
        dialog.set_close_response("cancel");
        let s = sender.output_sender().clone();
        let field = entry.clone();
        dialog.connect_response(None, move |_, response| {
            let name = match response {
                "rename" => Some(field.text().trim().to_string()).filter(|n| !n.is_empty()),
                "reset" => None,
                _ => return,
            };
            let _ = s.send(ContactsPageOutput::RenameBook {
                uid: uid.clone(),
                name,
            });
        });
        dialog.present(widgets.page_stack.root().as_ref());
        entry.grab_focus();
    }

    /// Put the address book dropdown in step with the books and the
    /// scope. Hidden while there is only one book to show.
    pub(super) fn sync_book_picker(&mut self, widgets: &ContactsPageWidgets) {
        widgets
            .book_filter
            .set_visible(self.book_picker && self.filter_books.len() > 1);
        if !self.book_picker {
            return;
        }
        let mut labels = vec![i18n("All Address Books")];
        labels.extend(self.filter_books.iter().map(|(_, name)| name.clone()));
        // A group shows as its book; an account's books as every book.
        let at = match &self.scope {
            ContactScope::Book(b) | ContactScope::Group { book: b, .. } => self
                .filter_books
                .iter()
                .position(|(u, _)| u == b)
                .map_or(0, |i| i + 1),
            ContactScope::All | ContactScope::Account(_) => 0,
        } as u32;
        self.picker_guard.set(true);
        if labels != self.picker_labels {
            let strs: Vec<&str> = labels.iter().map(String::as_str).collect();
            widgets
                .book_filter
                .set_model(Some(&gtk::StringList::new(&strs)));
            self.picker_labels = labels;
        }
        if widgets.book_filter.selected() != at {
            widgets.book_filter.set_selected(at);
        }
        self.picker_guard.set(false);
    }

    /// The books and groups for the sidebar, handed over when they change.
    pub(super) fn publish_tree(&mut self, sender: &ComponentSender<Self>) {
        let directories: std::collections::HashSet<&String> =
            self.directories.iter().map(|(uid, _)| uid).collect();
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for c in self.contacts.iter().filter(|c| !c.is_group()) {
            *counts.entry(c.book_uid.as_str()).or_default() += 1;
        }
        let books = self
            .filter_books
            .iter()
            .map(|(uid, name)| {
                let directory = directories.contains(uid);
                let (key, account, own) = split_book_name(name, directory);
                let book = self.books.iter().find(|b| b.uid == *uid);
                let mut groups: Vec<TreeGroup> = self
                    .contacts
                    .iter()
                    .filter(|c| c.is_group() && c.book_uid == *uid)
                    .map(|g| TreeGroup {
                        book: uid.clone(),
                        uid: g.eds_uid.clone(),
                        name: g.name.clone(),
                        count: g.members.len(),
                        editable: g.group_editable() && book.is_some(),
                    })
                    .collect();
                groups.sort_by_key(|g| g.name.to_lowercase());
                TreeBook {
                    uid: uid.clone(),
                    account,
                    key,
                    name: own,
                    count: counts.get(uid.as_str()).copied().unwrap_or(0),
                    writable: book.is_some(),
                    takes_groups: book.is_some_and(|b| b.groups.is_some()),
                    directory,
                    groups,
                }
            })
            .collect();
        let tree = ContactTree {
            total: self.contacts.iter().filter(|c| !c.is_group()).count(),
            books,
            scope: self.scope.clone(),
        };
        if self.tree.as_ref() != Some(&tree) {
            self.tree = Some(tree.clone());
            let _ = sender.output(ContactsPageOutput::Tree(tree));
        }
    }

    /// Whether `c` passes the "Show only" filters on; groups always do, and
    /// with Groups on nobody else does.
    pub(super) fn passes_filters(
        &self,
        c: &ContactDetails,
        grouped: &std::collections::HashSet<String>,
    ) -> bool {
        if self.groups_only() {
            return c.is_group();
        }
        c.is_group()
            || ContactFilter::ALL
                .iter()
                .all(|f| self.filters & f.bit() == 0 || f.passes(c, grouped))
    }

    pub(super) fn groups_only(&self) -> bool {
        self.filters & ContactFilter::Groups.bit() != 0
    }

    /// The addresses of every group's people, lower case, for "Not in a
    /// Group"; empty unless that filter is on.
    pub(super) fn grouped_addresses(&self) -> std::collections::HashSet<String> {
        if self.filters & ContactFilter::NoGroup.bit() == 0 {
            return std::collections::HashSet::new();
        }
        self.contacts
            .iter()
            .filter(|c| c.is_group())
            .flat_map(|c| {
                c.members
                    .iter()
                    .flat_map(|m| [Some(m.email.to_lowercase()), m.card_key(&c.book_uid)])
            })
            .flatten()
            .collect()
    }

    pub(super) fn sync_filter_menu(&self) {
        if let Some(menu) = &self.filter_menu {
            menu.sync(self.sort, self.filters);
        }
    }

    /// The list positions selected, as entries (contacts and directory
    /// hits), in list order.
    pub(super) fn picked_entries(&self) -> Vec<usize> {
        let bits = self.selection.selection();
        (0..bits.size())
            .filter_map(|n| {
                self.row_map
                    .get(bits.nth(n as u32) as usize)
                    .copied()
                    .flatten()
            })
            .collect()
    }

    /// A group's people as the group in book `to`, of `format`, keeps
    /// them; `from` is the book it is in now, if any. A member it already
    /// names there stays as written. Anyone else is found in Contacts: by
    /// the contact they were picked as, their card in `from`, or their
    /// address, a contact in `to` first. An Apple group names that contact
    /// in `to`, copied there when it is in another book (someone known only
    /// by an address gets a card at the save); an Evolution list holds the
    /// address, so someone with none is left out.
    pub(super) fn members_for(
        &self,
        members: Vec<Member>,
        from: Option<&str>,
        to: &str,
        format: GroupFormat,
    ) -> Vec<Member> {
        let same = from == Some(to);
        let people = || self.contacts.iter().filter(|c| !c.is_group());
        members
            .into_iter()
            .filter_map(|m| {
                if same && !m.line.is_empty() {
                    return Some(m);
                }
                if format == GroupFormat::Evolution {
                    return (!m.email.is_empty()).then(|| Member {
                        name: m.name,
                        email: m.email,
                        ..Member::default()
                    });
                }
                let person = m
                    .source
                    .as_ref()
                    .and_then(|(book, uid)| {
                        people().find(|c| c.book_uid == *book && c.eds_uid == *uid)
                    })
                    .or_else(|| {
                        let from = from?;
                        people()
                            .find(|c| !m.uid.is_empty() && c.book_uid == from && c.eds_uid == m.uid)
                    })
                    .or_else(|| {
                        people()
                            .filter(|c| {
                                !m.email.is_empty()
                                    && c.emails
                                        .iter()
                                        .any(|e| e.value.eq_ignore_ascii_case(&m.email))
                            })
                            .min_by_key(|c| c.book_uid != to)
                    });
                let base = Member {
                    name: m.name,
                    email: m.email,
                    ..Member::default()
                };
                match person {
                    Some(p) if p.book_uid == to && !p.eds_uid.is_empty() => Some(Member {
                        uid: p.eds_uid.clone(),
                        ..base
                    }),
                    Some(p) => Some(Member {
                        card: crate::contacts::moved_vcard(p),
                        ..base
                    }),
                    None => (!base.email.is_empty() || !m.card.is_empty()).then_some(Member {
                        card: m.card,
                        ..base
                    }),
                }
            })
            .collect()
    }

    /// The contacts selected together, as indexes into `contacts`.
    pub(super) fn chosen(&self) -> Vec<usize> {
        self.multi
            .iter()
            .copied()
            .filter(|&i| i < self.contacts.len())
            .collect()
    }

    /// The people of the selection that can move to the book `to`: those
    /// in another book Hylki can write to, and delete from.
    pub(super) fn movable(&self, to: &str) -> Vec<usize> {
        self.chosen()
            .into_iter()
            .filter(|&i| self.can_move(&self.contacts[i], to))
            .collect()
    }

    /// Whether a person can move to the book `to`: from another book Hylki
    /// can write to, and delete from.
    pub(super) fn can_move(&self, c: &ContactDetails, to: &str) -> bool {
        let writable = |uid: &str| self.books.iter().any(|b| b.uid == uid);
        !c.is_group()
            && c.book_uid != to
            && !c.eds_uid.is_empty()
            && writable(to)
            && writable(&c.book_uid)
    }

    /// Ask before moving these contacts, by book and UID, to the book `to`:
    /// out of the books they are in, into that one. `edited` is the card
    /// of a contact being edited, saved with the move.
    pub(super) fn ask_move(
        &self,
        widgets: &ContactsPageWidgets,
        sender: &ComponentSender<Self>,
        keys: Vec<(String, String)>,
        to: String,
        edited: Option<String>,
    ) {
        let Some(book) = self.books.iter().find(|b| b.uid == to) else {
            return;
        };
        let one = match keys.as_slice() {
            [] => return,
            [(b, u)] => self
                .contacts
                .iter()
                .find(|c| c.book_uid == *b && c.eds_uid == *u),
            _ => None,
        };
        let (heading, body) = match one {
            Some(c) => (
                i18n("Move Contact?"),
                i18n_f(
                    "This will move {name} out of {from} and into {to}, locally and on the server. A group in {from} that holds the contact will no longer have it.",
                    &[("name", &c.name), ("from", &c.book_name), ("to", &book.name)],
                ),
            ),
            None => {
                let n = keys.len() as u32;
                let count = n.to_string();
                (
                    ni18n_f("Move {n} Contact?", "Move {n} Contacts?", n, &[("n", &count)]),
                    ni18n_f(
                        "This will move {n} contact out of the address book it is in and into {to}, locally and on the server. A group there that holds it will no longer have it.",
                        "This will move {n} contacts out of the address books they are in and into {to}, locally and on the server. Groups there that hold them will no longer have them.",
                        n,
                        &[("n", &count), ("to", &book.name)],
                    ),
                )
            }
        };
        let s = sender.input_sender().clone();
        confirm(widgets, &heading, &body, &i18n("Move"), false, move || {
            let _ = s.send(match (&edited, keys.first()) {
                (Some(vcard), Some(key)) => ContactsPageInput::EditMoveConfirmed {
                    to: to.clone(),
                    key: key.clone(),
                    vcard: vcard.clone(),
                },
                _ => ContactsPageInput::BulkMoveConfirmed {
                    to: to.clone(),
                    keys: keys.clone(),
                },
            });
        });
    }

    /// Whether a book syncs with a server. Books on this computer keep
    /// groups as Evolution lists; the rest are a server's.
    pub(super) fn synced(&self, book_uid: &str) -> bool {
        !crate::contacts::is_local_book(book_uid)
            && self
                .books
                .iter()
                .find(|b| b.uid == book_uid)
                .is_none_or(|b| b.groups != Some(GroupFormat::Evolution))
    }

    /// Take these entries (by book and UID) off the list, for good: a
    /// re-read that still has them leaves them out until EDS lets go.
    /// Returns those that are EDS's to delete.
    pub(super) fn forget(&mut self, keys: &[(String, String)]) -> Vec<(String, String)> {
        let mut gone = Vec::new();
        for (book, uid) in keys {
            let Some(idx) = self
                .contacts
                .iter()
                .position(|c| c.book_uid == *book && c.eds_uid == *uid)
            else {
                continue;
            };
            self.contacts.remove(idx);
            self.items.remove(idx);
            if !uid.is_empty() {
                self.deleted.insert((book.clone(), uid.clone()));
                gone.push((book.clone(), uid.clone()));
            }
        }
        for (i, item) in self.items.iter().enumerate() {
            item.borrow::<Line>().idx.set(Some(i));
        }
        self.selected = None;
        self.multi.clear();
        gone
    }

    /// The entry at `idx`, a directory entry's index included.
    pub(super) fn entry(&self, idx: usize) -> Option<&ContactDetails> {
        match idx.checked_sub(DIRECTORY) {
            Some(i) => self.directory_hits.get(i),
            None => self.contacts.get(idx),
        }
    }

    /// Whether one of `c`'s addresses is already a contact's, in any book.
    pub(super) fn in_contacts(&self, c: &ContactDetails) -> bool {
        self.contacts.iter().any(|k| {
            k.emails.iter().any(|e| {
                c.emails
                    .iter()
                    .any(|h| h.value.eq_ignore_ascii_case(&e.value))
            })
        })
    }

    /// Fill the filter popover's books from those known: the writable ones
    /// (the Hylki book always among them), any others the contacts come
    /// from, and the directories. A book unticked stays so while it is
    /// there.
    pub(super) fn refresh_book_filter(&mut self) {
        let mut books: Vec<(String, String)> = self
            .books
            .iter()
            .map(|b| (b.uid.clone(), b.name.clone()))
            .collect();
        for c in &self.contacts {
            if !c.book_uid.is_empty() && !books.iter().any(|(uid, _)| *uid == c.book_uid) {
                books.push((c.book_uid.clone(), c.book_name.clone()));
            }
        }
        // A directory lists the people a search finds in it (#307).
        for (uid, name) in &self.directories {
            if !books.iter().any(|(u, _)| u == uid) {
                books.push((uid.clone(), format!("LDAP — {name}")));
            }
        }
        if books == self.filter_books {
            return;
        }
        self.filter_books = books;
        // A book that went is no longer shown alone.
        let gone = match &self.scope {
            ContactScope::All => false,
            ContactScope::Book(b) | ContactScope::Group { book: b, .. } => {
                !self.filter_books.iter().any(|(u, _)| u == b)
            }
            ContactScope::Account(key) => !self
                .filter_books
                .iter()
                .any(|(u, _)| self.book_key(u) == *key),
        };
        if gone {
            self.scope = ContactScope::All;
        }
    }

    /// Ready the pane for a new contact or group: nothing selected, so the
    /// form is what it shows.
    pub(super) fn new_form(&mut self, widgets: &ContactsPageWidgets) {
        self.editing_target = None;
        widgets.page_stack.set_visible_child_name("browser");
        self.selected = None;
        self.multi.clear();
        self.selection.unselect_all();
    }

    /// Move the list selection to the row showing contact `idx` (if visible).
    pub(super) fn select_row_for(&self, idx: usize) {
        if let Some(pos) = self.row_map.iter().position(|&i| i == Some(idx)) {
            self.selection.select_item(pos as u32, true);
        }
    }
}
