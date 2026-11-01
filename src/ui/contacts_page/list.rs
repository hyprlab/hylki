//! The contact list: rebuilding it, decoding photos, a row's menu and the
//! view of several selected contacts.

use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::ContactDetails;
use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::ui::context_menu::{show_context_menu, MenuEntry};

use super::detail::{group_byline, member_for};
use super::menus::{names_groups, BulkChoices};
use super::rows::{avatar_for, group, is_member, line_photo_key, Line, LineKind};
use super::types::{ContactScope, ContactSort, ContactsPage, ContactsPageInput, DIRECTORY};
use super::ContactsPageWidgets;

impl ContactsPage {
    /// Rebuild the list from `contacts` + `query` + `sort`, keep (or default)
    /// the selection, and render its detail card.
    pub(super) fn rebuild(
        &mut self,
        widgets: &ContactsPageWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let started = std::time::Instant::now();
        self.publish_tree(sender);
        self.sync_book_picker(widgets);
        // What a row dragged from the list carries.
        self.drag.borrow_mut().keys = self
            .contacts
            .iter()
            .map(|c| {
                (!c.is_group() && !c.eds_uid.is_empty())
                    .then(|| (c.book_uid.clone(), c.eds_uid.clone()))
            })
            .collect();
        let empty = self.contacts.is_empty() && self.directories.is_empty();
        widgets
            .page_stack
            .set_visible_child_name(if empty { "none" } else { "browser" });

        let grouped = self.grouped_addresses();
        let people = self.listed_people(&grouped);
        self.show_count(widgets, &people);
        let lines = self.directory_lines(&grouped, &people);
        let missing = self.fill_store(&people, lines);

        // Several selected together stay so while two of them are listed.
        let multi_pos: Vec<u32> = self
            .multi
            .iter()
            .filter_map(|&idx| self.row_map.iter().position(|&i| i == Some(idx)))
            .map(|pos| pos as u32)
            .collect();
        if multi_pos.len() > 1 {
            for &pos in &multi_pos {
                self.selection.select_item(pos, false);
            }
            self.multi.retain(|idx| self.row_map.contains(&Some(*idx)));
            self.selected = None;
            self.render_detail(widgets, sender);
            self.decode_photos(missing, sender);
            return;
        }
        self.multi.clear();

        // Selection: keep it if it survived the filter, else the first match.
        let shown_pos = self
            .selected
            .and_then(|sel| self.row_map.iter().position(|&i| i == Some(sel)))
            .or_else(|| self.row_map.iter().position(Option::is_some));
        match shown_pos {
            Some(pos) => {
                self.selected = self.row_map[pos];
                self.selection.select_item(pos as u32, true);
            }
            None => self.selected = None,
        }
        self.render_detail(widgets, sender);
        self.decode_photos(missing, sender);
        tracing::debug!(
            "contacts: listed {} in {:?}",
            self.row_map.len(),
            started.elapsed()
        );
    }

    /// The contacts the list shows, in its order: in scope, matching the
    /// search and the filters, groups first.
    fn listed_people(&self, grouped: &std::collections::HashSet<String>) -> Vec<usize> {
        let mut people: Vec<usize> = (0..self.contacts.len())
            .filter(|&i| {
                self.matches(&self.contacts[i]) && self.passes_filters(&self.contacts[i], grouped)
            })
            .collect();
        people.sort_by_key(|&i| {
            let c = &self.contacts[i];
            let name = c.name.to_lowercase();
            // Groups head the people, by name: a group shown with its
            // people, or a book's groups.
            let key = match self.sort {
                _ if c.is_group() => name,
                ContactSort::FirstName => name,
                // Sort by the final word of the name (then the whole name for
                // ties) — how an address book orders by surname.
                ContactSort::LastName => {
                    format!("{} {name}", name.rsplit(' ').next().unwrap_or(&name))
                }
                ContactSort::Email => c.primary_email().map(str::to_lowercase).unwrap_or(name),
            };
            (!c.is_group(), key)
        });
        people
    }

    /// The count over the list: everyone in scope, or so many of them while
    /// a search or a filter narrows it. Asked for groups, by the Groups
    /// filter or a search for them, it counts the groups.
    fn show_count(&self, widgets: &ContactsPageWidgets, people: &[usize]) {
        // People in what the list shows, and of them those it lists.
        let groups = self.groups_only() || names_groups(&self.query);
        let total = self
            .contacts
            .iter()
            .filter(|c| c.is_group() == groups && self.in_scope(c))
            .count();
        let shown = people
            .iter()
            .filter(|&&i| self.contacts[i].is_group() == groups)
            .count();
        let narrowed = !self.query.is_empty() || self.filters != 0;
        widgets.count_label.set_label(&if !narrowed {
            total.to_string()
        } else {
            i18n_f(
                "{shown} of {total}",
                &[("shown", &shown.to_string()), ("total", &total.to_string())],
            )
        });
    }

    /// The lines after the people: the directories' answers, the
    /// directories that did not answer, and a note for a directory picked
    /// but not searched.
    fn directory_lines(
        &self,
        grouped: &std::collections::HashSet<String>,
        people: &[usize],
    ) -> Vec<Line> {
        let mut lines: Vec<Line> = Vec::new();
        // Directories are searched with every book, or alone; a group has
        // none of their people.
        let groups = matches!(self.scope, ContactScope::Group { .. });

        // The directories' answers, after the contacts and by name whatever
        // the sort, under a heading of their own so nobody takes them for
        // contacts. A book picked keeps those of the directory it is.
        let in_filter = |uid: &str| match &self.scope {
            ContactScope::Book(b) => b == uid,
            ContactScope::Account(key) => self.book_key(uid) == *key,
            _ => true,
        };
        // Someone the list already shows as a contact is not listed again,
        // nor one found in two directories. Only those shown count: with a
        // directory picked, a person kept in another book is still listed
        // from the directory, or that view would lose them (#307).
        let mut listed: std::collections::HashSet<String> = people
            .iter()
            .flat_map(|&i| self.contacts[i].emails.iter())
            .map(|e| e.value.to_lowercase())
            .collect();
        let mut hits = Vec::new();
        for (i, c) in self.directory_hits.iter().enumerate() {
            if groups || !in_filter(&c.book_uid) || !self.passes_filters(c, grouped) {
                continue;
            }
            let emails: Vec<String> = c.emails.iter().map(|e| e.value.to_lowercase()).collect();
            if emails.iter().any(|e| listed.contains(e)) {
                continue;
            }
            listed.extend(emails);
            // Which directory it is in, after the address.
            let subtitle = match c.primary_email() {
                Some(email) => format!("{email} · {}", c.book_name),
                None => c.book_name.clone(),
            };
            hits.push(Line::entry(DIRECTORY + i, c, subtitle));
        }
        if !hits.is_empty() {
            lines.push(Line::other(
                LineKind::Heading,
                i18n("In Directories"),
                String::new(),
            ));
            lines.extend(hits);
        }

        // A directory that did not answer, last and not selectable, so a
        // search that finds nobody in it says why.
        for f in self
            .directory_failures
            .iter()
            .filter(|f| !groups && in_filter(&f.uid))
        {
            lines.push(Line::other(
                LineKind::Failure,
                f.name.clone(),
                f.message.clone(),
            ));
        }

        // A directory picked in the sidebar is empty until searched.
        let directory_picked = match &self.scope {
            ContactScope::Book(b) => self.directories.iter().any(|(d, _)| d == b),
            _ => false,
        };
        if directory_picked && !groups && people.is_empty() && lines.is_empty() {
            let text = if self.query.trim().chars().count() < crate::contacts::directory::MIN_QUERY
            {
                i18n("Type three letters or more to search the directory.")
            } else {
                i18n("Nobody in the directory matches.")
            };
            lines.push(Line::other(LineKind::Note, text, String::new()));
        }
        lines
    }

    /// Put `people` and then `lines` in the list, and answer the photos the
    /// new lines show that are not decoded yet.
    fn fill_store(&mut self, people: &[usize], lines: Vec<Line>) -> std::collections::HashSet<u64> {
        let items: Vec<gtk::glib::BoxedAnyObject> = people
            .iter()
            .filter_map(|&idx| self.items.get(idx).cloned())
            .chain(lines.into_iter().map(gtk::glib::BoxedAnyObject::new))
            .collect();
        self.row_map = items
            .iter()
            .map(|item| item.borrow::<Line>().idx.get())
            .collect();
        let missing: std::collections::HashSet<u64> = items
            .iter()
            .filter_map(|item| match item.borrow::<Line>().kind {
                LineKind::Entry { photo_key, .. } if photo_key != 0 => Some(photo_key),
                _ => None,
            })
            .filter(|key| {
                !self.decoding.contains(key)
                    && crate::services::avatar::photo_thumbnail(*key).is_none()
            })
            .collect();
        // Unselected first: the selection would otherwise follow the old
        // lines through the swap and report a position in the new ones.
        self.selection.unselect_all();
        self.store.splice(0, self.store.n_items(), &items);
        missing
    }

    /// The list items for a fresh read: an unchanged line keeps the item it
    /// had, and with it its row.
    pub(super) fn items_for(&self, contacts: &[ContactDetails]) -> Vec<gtk::glib::BoxedAnyObject> {
        let mut had: HashMap<(&str, &str), &gtk::glib::BoxedAnyObject> = self
            .contacts
            .iter()
            .zip(&self.items)
            .map(|(c, item)| ((c.book_uid.as_str(), c.eds_uid.as_str()), item))
            .collect();
        contacts
            .iter()
            .enumerate()
            .map(|(idx, c)| {
                let subtitle = if c.is_group() {
                    group_byline(c)
                } else {
                    // An address, else a phone number, else the company:
                    // whatever tells the card apart, as an address book does.
                    c.primary_email()
                        .or_else(|| c.phones.first().map(|p| p.value.as_str()))
                        .or_else(|| (!c.org.is_empty()).then_some(c.org.as_str()))
                        .filter(|v| *v != c.name)
                        .unwrap_or_default()
                        .to_string()
                };
                let line = Line::entry(idx, c, subtitle);
                match had.remove(&(c.book_uid.as_str(), c.eds_uid.as_str())) {
                    Some(item) if *item.borrow::<Line>() == line => {
                        item.borrow::<Line>().idx.set(Some(idx));
                        item.clone()
                    }
                    _ => gtk::glib::BoxedAnyObject::new(line),
                }
            })
            .collect()
    }

    /// Decode these photos on other threads; the rows waiting for them show
    /// each as it lands.
    pub(super) fn decode_photos(
        &mut self,
        mut keys: std::collections::HashSet<u64>,
        sender: &ComponentSender<Self>,
    ) {
        // Each photo once, however many people share it. The address books'
        // photos go to the thread, or are let go when not wanted now: the
        // list draws thumbnails, and the rest read a photo again from its
        // card (`photo_bytes`) when they need it (#504).
        let mut photos: Vec<(u64, Vec<u8>)> = Vec::new();
        for c in &mut self.contacts {
            let key = line_photo_key(c);
            c.photo_key = key;
            if let Some(bytes) = c.photo.take() {
                if keys.remove(&key) {
                    photos.push((key, bytes));
                }
            }
        }
        for c in &self.directory_hits {
            let key = line_photo_key(c);
            if keys.remove(&key) {
                photos.extend(c.photo.clone().map(|bytes| (key, bytes)));
            }
        }
        // A photo let go earlier and wanted now (a search or filter brought
        // its contact in) is read from its card on the thread.
        let cards: Vec<ContactDetails> = self
            .contacts
            .iter()
            .filter(|c| keys.remove(&c.photo_key))
            .map(|c| ContactDetails {
                raw_vcard: c.raw_vcard.clone(),
                book_uid: c.book_uid.clone(),
                photo_key: c.photo_key,
                ..Default::default()
            })
            .collect();
        if photos.is_empty() && cards.is_empty() {
            return;
        }
        self.decoding.extend(photos.iter().map(|(key, _)| *key));
        self.decoding.extend(cards.iter().map(|c| c.photo_key));
        let s = sender.input_sender().clone();
        std::thread::spawn(move || {
            photos.extend(
                cards
                    .iter()
                    .filter_map(|c| Some((c.photo_key, c.photo_bytes()?))),
            );
            crate::services::avatar::decode_photo_thumbnails(photos, || {
                let _ = s.send(ContactsPageInput::PhotosDecoded);
            });
        });
    }

    /// The right-click menu for an entry of the list.
    pub(super) fn row_menu(
        &self,
        idx: usize,
        sender: &ComponentSender<Self>,
    ) -> Vec<Vec<MenuEntry>> {
        let s = sender.input_sender();
        if idx >= DIRECTORY {
            return self.directory_hit_menu(idx, s);
        }
        let Some(c) = self.contacts.get(idx) else {
            return Vec::new();
        };
        // On one of several selected: what can be done to them all.
        if self.multi.contains(&idx) {
            return self.selection_menu(s);
        }
        self.contact_menu(idx, c, s)
    }

    /// The menu of a person found in a directory.
    fn directory_hit_menu(
        &self,
        idx: usize,
        s: &relm4::Sender<ContactsPageInput>,
    ) -> Vec<Vec<MenuEntry>> {
        let Some(c) = self.entry(idx) else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        if let Some(email) = c.primary_email().map(str::to_string) {
            let sc = s.clone();
            entries.push(
                MenuEntry::new(i18n("New message"), move || {
                    let _ = sc.send(ContactsPageInput::Compose(email.clone()));
                })
                .icon("mail-message-new-symbolic"),
            );
        }
        if !self.in_contacts(c) {
            let sa = s.clone();
            entries.push(
                MenuEntry::new(i18n("Add to Contacts"), move || {
                    let _ = sa.send(ContactsPageInput::AddDirectoryHit(idx));
                })
                .icon("contact-new-symbolic"),
            );
        }
        vec![entries]
    }

    /// The menu of several selected contacts: what can be done to them all.
    fn selection_menu(&self, s: &relm4::Sender<ContactsPageInput>) -> Vec<Vec<MenuEntry>> {
        let choices = self.bulk_choices();
        let mut first = Vec::new();
        if let Some(g) = self.scope_group().filter(|g| g.group_editable()) {
            let chosen = self.chosen();
            if chosen
                .iter()
                .any(|&i| g.members.iter().any(|m| is_member(g, m, &self.contacts[i])))
            {
                let sr = s.clone();
                first.push(
                    MenuEntry::new(i18n("Remove from Group"), move || {
                        let _ = sr.send(ContactsPageInput::RemoveFromGroup(chosen.clone()));
                    })
                    .icon("list-remove-symbolic"),
                );
            }
        }
        if !choices.groups.is_empty() {
            first.push(
                MenuEntry::submenu(i18n("Add to Group"), vec![choices.group_entries(s)])
                    .icon("system-users-symbolic"),
            );
        }
        if !choices.books.is_empty() {
            first.push(
                MenuEntry::submenu(i18n("Move to Address Book"), vec![choices.book_entries(s)])
                    .icon("x-office-address-book-symbolic"),
            );
        }
        let sd = s.clone();
        let delete = vec![MenuEntry::new(i18n("Delete…"), move || {
            let _ = sd.send(ContactsPageInput::BulkDeleteRequest);
        })
        .icon("user-trash-symbolic")];
        if first.is_empty() {
            vec![delete]
        } else {
            vec![first, delete]
        }
    }

    /// The menu of one contact or group, `c` at `idx`.
    fn contact_menu(
        &self,
        idx: usize,
        c: &ContactDetails,
        s: &relm4::Sender<ContactsPageInput>,
    ) -> Vec<Vec<MenuEntry>> {
        let local = crate::contacts::is_local_book(&c.book_uid);
        let group = c.is_group();
        let (se, sd, sg) = (s.clone(), s.clone(), s.clone());
        let mut first = vec![MenuEntry::new(i18n("Edit"), move || {
            let _ = se.send(ContactsPageInput::EditIndex(idx));
        })
        .icon("document-edit-symbolic")
        .enabled(!group || c.group_editable())];
        if group {
            let sc = s.clone();
            first.push(
                MenuEntry::new(i18n("New message"), move || {
                    let _ = sc.send(ContactsPageInput::ComposeGroup(idx));
                })
                .icon("mail-message-new-symbolic")
                .enabled(c.reachable().next().is_some()),
            );
        }
        // A person can go in a group they are not in, by name.
        if !group {
            let mut groups: Vec<(usize, &str)> = (0..self.contacts.len())
                .filter(|&g| member_for(&self.contacts[g], c, false).is_some())
                .map(|g| (g, self.contacts[g].name.as_str()))
                .collect();
            groups.sort_by_key(|(_, name)| name.to_lowercase());
            if !groups.is_empty() {
                let entries = groups
                    .into_iter()
                    .map(|(g, name)| {
                        let sa = s.clone();
                        MenuEntry::new(name.to_string(), move || {
                            let _ = sa.send(ContactsPageInput::AddToGroup {
                                group: g,
                                person: idx,
                            });
                        })
                        .icon("system-users-symbolic")
                    })
                    .collect();
                first.push(
                    MenuEntry::submenu(i18n("Add to Group"), vec![entries])
                        .icon("system-users-symbolic"),
                );
            }
        }
        // Out of the group the list shows.
        if let Some(g) = self.scope_group().filter(|g| !group && g.group_editable()) {
            if g.members.iter().any(|m| is_member(g, m, c)) {
                let sr = s.clone();
                first.push(
                    MenuEntry::new(i18n("Remove from Group"), move || {
                        let _ = sr.send(ContactsPageInput::RemoveFromGroup(vec![idx]));
                    })
                    .icon("list-remove-symbolic"),
                );
            }
        }
        // Another book, with a question first.
        let books: Vec<(String, String)> = self
            .books
            .iter()
            .filter(|b| self.can_move(c, &b.uid))
            .map(|b| (b.uid.clone(), b.name.clone()))
            .collect();
        if !books.is_empty() {
            let entries = books
                .into_iter()
                .map(|(uid, name)| {
                    let sm = s.clone();
                    MenuEntry::new(name, move || {
                        let _ = sm.send(ContactsPageInput::MoveRequest {
                            idx,
                            to: uid.clone(),
                        });
                    })
                    .icon("x-office-address-book-symbolic")
                })
                .collect();
            first.push(
                MenuEntry::submenu(i18n("Move to Address Book"), vec![entries])
                    .icon("x-office-address-book-symbolic"),
            );
        }
        // GNOME Contacts knows nothing of the Hylki book, nor of groups.
        if !local && !group {
            first.push(
                MenuEntry::new(i18n("Open in GNOME Contacts"), move || {
                    let _ = sg.send(ContactsPageInput::OpenInGnome(idx));
                })
                .icon("adw-external-link-symbolic"),
            );
        }
        vec![
            first,
            vec![MenuEntry::new(i18n("Delete…"), move || {
                let _ = sd.send(ContactsPageInput::DeleteRequest(idx));
            })
            .icon("user-trash-symbolic")],
        ]
    }

    /// What the entries selected together can be given: the groups any of
    /// them can join, and the books any of them can move to.
    fn bulk_choices(&self) -> BulkChoices {
        let chosen = self.chosen();
        let mut groups: Vec<(usize, String)> = (0..self.contacts.len())
            .filter(|&g| {
                chosen
                    .iter()
                    .any(|&p| member_for(&self.contacts[g], &self.contacts[p], false).is_some())
            })
            .map(|g| (g, self.contacts[g].name.clone()))
            .collect();
        groups.sort_by_key(|(_, name)| name.to_lowercase());
        let books = self
            .books
            .iter()
            .filter(|b| !self.movable(&b.uid).is_empty())
            .map(|b| (b.uid.clone(), b.name.clone()))
            .collect();
        BulkChoices { groups, books }
    }

    /// The pane for several entries selected together: how many, what
    /// can be done to them all, and who they are.
    pub(super) fn render_multi(
        &self,
        widgets: &ContactsPageWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let detail = &widgets.detail_box;
        widgets.edit_btn.set_sensitive(false);
        let chosen = self.chosen();
        let n = chosen.len() as u32;
        let count = n.to_string();
        let groups = !chosen.is_empty() && chosen.iter().all(|&i| self.contacts[i].is_group());

        let icon = gtk::Image::from_icon_name(if groups {
            "system-users-symbolic"
        } else {
            "edit-select-all-symbolic"
        });
        icon.set_pixel_size(64);
        icon.add_css_class("dim-label");
        icon.set_margin_top(24);
        detail.append(&icon);
        let title = gtk::Label::new(Some(&if groups {
            ni18n_f(
                "{n} Group Selected",
                "{n} Groups Selected",
                n,
                &[("n", &count)],
            )
        } else {
            ni18n_f(
                "{n} Contact Selected",
                "{n} Contacts Selected",
                n,
                &[("n", &count)],
            )
        }));
        title.add_css_class("title-1");
        title.set_wrap(true);
        title.set_justify(gtk::Justification::Center);
        title.set_margin_top(12);
        detail.append(&title);

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        actions.set_halign(gtk::Align::Center);
        actions.set_margin_top(18);
        let choices = Rc::new(self.bulk_choices());
        let pill = |label: &str| {
            let b = gtk::Button::with_label(label);
            b.add_css_class("pill");
            b
        };
        if let Some(g) = self.scope_group().filter(|g| !groups && g.group_editable()) {
            let remove = pill(&i18n("Remove from Group"));
            remove.set_sensitive(
                chosen
                    .iter()
                    .any(|&i| g.members.iter().any(|m| is_member(g, m, &self.contacts[i]))),
            );
            let (s, people) = (sender.input_sender().clone(), chosen.clone());
            remove.connect_clicked(move |_| {
                let _ = s.send(ContactsPageInput::RemoveFromGroup(people.clone()));
            });
            actions.append(&remove);
        }
        if !groups {
            let add = pill(&i18n("Add to Group"));
            add.set_sensitive(!choices.groups.is_empty());
            let (c, s) = (choices.clone(), sender.input_sender().clone());
            add.connect_clicked(move |b| {
                show_context_menu(
                    b,
                    (b.width() / 2) as f64,
                    b.height() as f64,
                    vec![c.group_entries(&s)],
                );
            });
            actions.append(&add);
            let move_btn = pill(&i18n("Move to…"));
            move_btn.set_tooltip_text(Some(&i18n("Move to another address book")));
            move_btn.set_sensitive(!choices.books.is_empty());
            let (c, s) = (choices.clone(), sender.input_sender().clone());
            move_btn.connect_clicked(move |b| {
                show_context_menu(
                    b,
                    (b.width() / 2) as f64,
                    b.height() as f64,
                    vec![c.book_entries(&s)],
                );
            });
            actions.append(&move_btn);
        }
        let delete = pill(&i18n("Delete…"));
        delete.add_css_class("destructive-action");
        delete.set_sensitive(!chosen.is_empty());
        let s = sender.input_sender().clone();
        delete.connect_clicked(move |_| {
            let _ = s.send(ContactsPageInput::BulkDeleteRequest);
        });
        actions.append(&delete);
        detail.append(&actions);

        let list = group(detail, &i18n("Selected"));
        for &i in &chosen {
            let c = &self.contacts[i];
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(&c.name));
            row.set_subtitle(&gtk::glib::markup_escape_text(&c.book_name));
            row.add_prefix(&avatar_for(c, 32));
            list.append(&row);
        }
    }
}
