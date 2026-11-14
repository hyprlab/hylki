//! The detail pane: a contact's card, a group's card, and the editors.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::{ContactDetails, GroupFormat, Labeled, Member};
use crate::i18n::{i18n, i18n_f};

use super::forms::{
    address_editor, editable_values, entry_row, photo_editor, BirthdayForm, EMAIL_LABELS,
    PHONE_LABELS,
};
use super::member_editor::member_editor;
use super::menus::{book_label_factory, editor_bar};
use super::rows::{avatar_for, avatar_with, copy_button, flat_button, group, labeled_row};
use super::types::{
    ContactForm, ContactsPage, ContactsPageInput, ContactsPageOutput, Editor, GroupForm, DIRECTORY,
};
use super::ContactsPageWidgets;

/// The top of a contact's card: the photo (opening the lightbox when there
/// is one), the name, who they are, and for a person found in a directory
/// the button that keeps them as a contact.
fn identity_header(
    detail: &gtk::Box,
    sender: &ComponentSender<ContactsPage>,
    c: &ContactDetails,
    hit: Option<usize>,
) {
    let photo = c.photo_bytes();
    let avatar = avatar_with(c, 96, photo.as_deref());
    if let Some(photo) = &photo {
        // A real photo expands to the lightbox on click.
        let btn = gtk::Button::new();
        btn.set_child(Some(&avatar));
        btn.add_css_class("flat");
        btn.add_css_class("circular");
        btn.set_halign(gtk::Align::Center);
        btn.set_tooltip_text(Some(i18n("View photo").as_str()));
        let s = sender.input_sender().clone();
        let name = c.name.clone();
        let data = photo.clone();
        btn.connect_clicked(move |_| {
            let _ = s.send(ContactsPageInput::OpenPhoto {
                name: name.clone(),
                data: data.clone(),
            });
        });
        detail.append(&btn);
    } else {
        avatar.set_halign(gtk::Align::Center);
        detail.append(&avatar);
    }

    let name = gtk::Label::new(Some(&c.name));
    name.add_css_class("title-1");
    name.set_wrap(true);
    name.set_justify(gtk::Justification::Center);
    name.set_margin_top(12);
    detail.append(&name);

    let mut byline = Vec::new();
    if !c.nickname.is_empty() && c.nickname != c.name {
        byline.push(format!("“{}”", c.nickname));
    }
    match (!c.title.is_empty(), !c.org.is_empty()) {
        (true, true) => byline.push(format!("{} · {}", c.title, c.org)),
        (true, false) => byline.push(c.title.clone()),
        (false, true) => byline.push(c.org.clone()),
        (false, false) => {}
    }
    if !byline.is_empty() {
        let sub = gtk::Label::new(Some(&byline.join("  —  ")));
        sub.add_css_class("dim-label");
        sub.set_wrap(true);
        sub.set_justify(gtk::Justification::Center);
        sub.set_margin_top(4);
        detail.append(&sub);
    }

    // From a directory: it can be kept as a contact, not edited here.
    if let Some(idx) = hit {
        let add = gtk::Button::with_label(&i18n("Add to Contacts"));
        add.add_css_class("pill");
        add.set_halign(gtk::Align::Center);
        add.set_margin_top(12);
        let s = sender.input_sender().clone();
        add.connect_clicked(move |b| {
            b.set_sensitive(false);
            let _ = s.send(ContactsPageInput::AddDirectoryHit(idx));
        });
        detail.append(&add);
    }
}

/// The card's groups of fields: addresses with a button to write to them,
/// phones, postal addresses, websites, and the details below.
fn field_groups(
    detail: &gtk::Box,
    sender: &ComponentSender<ContactsPage>,
    c: &ContactDetails,
    hit: Option<usize>,
) {
    if !c.emails.is_empty() {
        let list = group(detail, &i18n("Email"));
        for e in &c.emails {
            let row = labeled_row(e);
            let compose = flat_button("mail-message-new-symbolic", &i18n("New message"));
            let s = sender.input_sender().clone();
            let addr = e.value.clone();
            compose.connect_clicked(move |_| {
                let _ = s.send(ContactsPageInput::Compose(addr.clone()));
            });
            row.add_suffix(&copy_button(&e.value));
            row.add_suffix(&compose);
            row.set_activatable(true);
            let s = sender.input_sender().clone();
            let addr = e.value.clone();
            row.connect_activated(move |_| {
                let _ = s.send(ContactsPageInput::Compose(addr.clone()));
            });
            list.append(&row);
        }
    }

    if !c.phones.is_empty() {
        let list = group(detail, &i18n("Phone"));
        for p in &c.phones {
            let row = labeled_row(p);
            row.add_suffix(&copy_button(&p.value));
            list.append(&row);
        }
    }

    if !c.addresses.is_empty() {
        let list = group(detail, &i18n("Address"));
        for a in &c.addresses {
            let row = labeled_row(a);
            row.add_suffix(&copy_button(&a.value));
            list.append(&row);
        }
    }

    if !c.urls.is_empty() {
        let list = group(detail, &i18n("Website"));
        for url in &c.urls {
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(url));
            row.set_activatable(true);
            let icon = gtk::Image::from_icon_name("adw-external-link-symbolic");
            icon.add_css_class("dim-label");
            row.add_suffix(&icon);
            let s = sender.input_sender().clone();
            let url = url.clone();
            row.connect_activated(move |_| {
                let _ = s.send(ContactsPageInput::OpenUrl(url.clone()));
            });
            list.append(&row);
        }
    }

    let mut extras: Vec<Labeled> = Vec::new();
    if !c.birthday.is_empty() {
        extras.push(Labeled {
            label: i18n("Birthday"),
            value: c.birthday.clone(),
        });
    }
    if !c.note.is_empty() {
        extras.push(Labeled {
            label: i18n("Note"),
            value: c.note.clone(),
        });
    }
    // Where this entry lives: the local book, a synced account, or the
    // directory it was found in.
    if hit.is_some() {
        extras.push(Labeled {
            label: i18n("Directory"),
            value: c.book_name.clone(),
        });
    } else if !c.book_name.is_empty() {
        extras.push(Labeled {
            label: i18n("Address Book"),
            value: c.book_name.clone(),
        });
    }
    if !extras.is_empty() {
        let list = group(detail, &i18n("Details"));
        for x in &extras {
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(&x.label));
            row.set_subtitle(&gtk::glib::markup_escape_text(&x.value));
            row.add_css_class("property");
            list.append(&row);
        }
    }
}

/// A group's card: who is in it, and a message to them all.
fn render_group_card(
    detail: &gtk::Box,
    sender: &ComponentSender<ContactsPage>,
    c: &ContactDetails,
    idx: usize,
) {
    let avatar = avatar_for(c, 96);
    avatar.set_halign(gtk::Align::Center);
    detail.append(&avatar);

    let name = gtk::Label::new(Some(&c.name));
    name.add_css_class("title-1");
    name.set_wrap(true);
    name.set_justify(gtk::Justification::Center);
    name.set_margin_top(12);
    detail.append(&name);

    let sub = gtk::Label::new(Some(&group_byline(c)));
    sub.add_css_class("dim-label");
    sub.set_wrap(true);
    sub.set_justify(gtk::Justification::Center);
    sub.set_margin_top(4);
    detail.append(&sub);

    if c.reachable().next().is_some() {
        let write = gtk::Button::with_label(&i18n("New Message"));
        write.add_css_class("pill");
        write.add_css_class("suggested-action");
        write.set_halign(gtk::Align::Center);
        write.set_margin_top(12);
        let s = sender.input_sender().clone();
        write.connect_clicked(move |_| {
            let _ = s.send(ContactsPageInput::ComposeGroup(idx));
        });
        detail.append(&write);
    }

    let list = group(detail, &i18n("Members"));
    for m in &c.members {
        let row = adw::ActionRow::new();
        // Someone known by their phone number only: in the group, but
        // not written to.
        if m.email.is_empty() {
            row.set_title(&gtk::glib::markup_escape_text(&m.name));
            row.set_subtitle(&i18n("No email address"));
            list.append(&row);
            continue;
        }
        let named = !m.name.trim().is_empty() && m.name != m.email;
        row.set_title(&gtk::glib::markup_escape_text(if named {
            &m.name
        } else {
            &m.email
        }));
        if named {
            row.set_subtitle(&gtk::glib::markup_escape_text(&m.email));
        }
        let recipient = crate::worker::format_recipient(&m.name, &m.email);
        let compose = flat_button("mail-message-new-symbolic", &i18n("New message"));
        let s = sender.input_sender().clone();
        let to = recipient.clone();
        compose.connect_clicked(move |_| {
            let _ = s.send(ContactsPageInput::Compose(to.clone()));
        });
        row.add_suffix(&copy_button(&m.email));
        row.add_suffix(&compose);
        row.set_activatable(true);
        let s = sender.input_sender().clone();
        row.connect_activated(move |_| {
            let _ = s.send(ContactsPageInput::Compose(recipient.clone()));
        });
        list.append(&row);
    }
    if c.members.is_empty() {
        let row = adw::ActionRow::new();
        row.set_title(&i18n("No one is in this group yet."));
        row.add_css_class("dim-label");
        list.append(&row);
    }

    let details = group(detail, &i18n("Details"));
    if !c.book_name.is_empty() {
        let row = adw::ActionRow::new();
        row.set_title(&i18n("Address Book"));
        row.set_subtitle(&gtk::glib::markup_escape_text(&c.book_name));
        row.add_css_class("property");
        details.append(&row);
    }
    if !c.group_editable() {
        let row = adw::ActionRow::new();
        row.set_title(&i18n("Changed in the app it came from"));
        row.set_subtitle(&i18n(
            "This group names its members by contact, which Hylki does not edit.",
        ));
        row.add_css_class("property");
        details.append(&row);
    }
}

/// `person` as a new member of `group`, when the group can take them and
/// does not have them. An Apple group or a category names its members by
/// their card in its own book, so someone of another book is copied there, whole, when
/// they join (unless the book has them by address already); an Evolution
/// list holds addresses, so it takes only someone with one. The copy, with
/// its photo, is made only `with_card`, for the save: a menu only asks.
pub(super) fn member_for(
    group: &ContactDetails,
    person: &ContactDetails,
    with_card: bool,
) -> Option<Member> {
    if !group.group_editable() || person.is_group() {
        return None;
    }
    let apple = group.group.is_some_and(GroupFormat::holds_cards);
    let own_card = apple && person.book_uid == group.book_uid && !person.eds_uid.is_empty();
    let member = Member {
        name: person.name.clone(),
        email: person.primary_email().unwrap_or_default().to_string(),
        uid: if own_card {
            person.eds_uid.clone()
        } else {
            String::new()
        },
        card: if apple && !own_card && with_card {
            crate::contacts::moved_vcard(person)
        } else {
            String::new()
        },
        ..Member::default()
    };
    if member.email.is_empty() && member.uid.is_empty() && !(apple && !own_card) {
        return None;
    }
    let present = group.members.iter().any(|m| m.same(&member))
        || person.emails.iter().any(|e| {
            group
                .members
                .iter()
                .any(|m| m.email.eq_ignore_ascii_case(&e.value))
        })
        || (member.email.is_empty()
            && !own_card
            && group
                .members
                .iter()
                .any(|m| m.email.is_empty() && m.name.eq_ignore_ascii_case(&member.name)));
    (!present).then_some(member)
}

pub(super) fn group_byline(c: &ContactDetails) -> String {
    crate::contacts::group_reach(c.members.len(), c.hide_addresses)
}

/// Follow the group form's book choice: the note on Apple groups, and what
/// saving in another book than the group's own (`own`, named `old_name`)
/// does.
fn follow_book_choice(
    row: &adw::ComboRow,
    books: &[crate::contacts::Book],
    note: &gtk::Label,
    move_note: &gtk::Label,
    cards: Rc<Cell<bool>>,
    own: Option<usize>,
    old_name: String,
) {
    let formats: Vec<Option<GroupFormat>> = books.iter().map(|b| b.groups).collect();
    let (note, move_note) = (note.downgrade(), move_note.downgrade());
    row.connect_selected_notify(move |row| {
        let at = row.selected() as usize;
        let format = formats.get(at).copied().flatten();
        let holds_cards = format.is_some_and(GroupFormat::holds_cards);
        cards.set(holds_cards);
        if let Some(note) = note.upgrade() {
            note.set_visible(holds_cards);
        }
        let Some(move_note) = move_note.upgrade() else { return };
        let moving = own.is_some_and(|own| own != at);
        move_note.set_visible(moving);
        if moving {
            let mut text = i18n_f(
                "Saving moves the group to this address book, with its people, and removes it from {book}.",
                &[("book", &old_name)],
            );
            if format == Some(GroupFormat::Evolution) {
                text.push(' ');
                text.push_str(&i18n("Groups here hold addresses, so people with no email address are left out."));
            }
            move_note.set_label(&text);
        }
    });
}

impl ContactsPage {
    /// Fill the right-hand pane with the selected contact's card.
    pub(super) fn render_detail(
        &mut self,
        widgets: &ContactsPageWidgets,
        sender: &ComponentSender<Self>,
    ) {
        self.drag.borrow_mut().multi = self.multi.clone();
        self.editor = None;
        let detail = &widgets.detail_box;
        while let Some(child) = detail.first_child() {
            detail.remove(&child);
        }
        if !self.multi.is_empty() {
            self.render_multi(widgets, sender);
            return;
        }
        let Some(c) = self.selected.and_then(|i| self.entry(i)) else {
            return;
        };
        let hit = self.selected.filter(|&i| i >= DIRECTORY);
        widgets
            .edit_btn
            .set_sensitive(hit.is_none() && (!c.is_group() || c.group_editable()));
        if let Some(idx) = self.selected.filter(|_| c.is_group()) {
            render_group_card(detail, sender, c, idx);
            return;
        }

        // Someone already kept as a contact is not offered again.
        identity_header(detail, sender, c, hit.filter(|_| !self.in_contacts(c)));
        field_groups(detail, sender, c, hit);
    }

    /// Open the form for the entry at `idx`: a group's, or a contact's. A
    /// group Hylki cannot write stays as it is.
    pub(super) fn edit(
        &mut self,
        widgets: &ContactsPageWidgets,
        sender: &ComponentSender<Self>,
        idx: usize,
    ) {
        let Some(c) = self.contacts.get(idx) else {
            return;
        };
        if c.is_group() && !c.group_editable() {
            return;
        }
        let group = c.is_group();
        self.editing_target = Some(idx);
        if group {
            self.render_group_editor(widgets, sender);
        } else {
            self.render_editor(widgets, sender);
        }
    }

    /// Replace the detail pane with the group form for `editing_target`
    /// (blank for a new group, in a book picked from those that keep
    /// groups).
    pub(super) fn render_group_editor(
        &mut self,
        widgets: &ContactsPageWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let detail = &widgets.detail_box;
        while let Some(child) = detail.first_child() {
            detail.remove(&child);
        }
        let blank = ContactDetails::default();
        let c = self
            .editing_target
            .and_then(|i| self.contacts.get(i))
            .unwrap_or(&blank);
        let heading = if self.editing_target.is_some() {
            i18n("Edit Group")
        } else {
            i18n("New Group")
        };
        editor_bar(detail, sender, &heading);

        let identity = group(detail, &i18n("Group"));
        let editing = self.editing_target.is_some();
        let books = self.group_books(c, editing);
        let row = adw::ComboRow::new();
        row.set_title(&i18n("Address book"));
        let labels: Vec<&str> = books.iter().map(|b| b.name.as_str()).collect();
        row.set_model(Some(&gtk::StringList::new(&labels)));
        // The name whole as a tooltip, like the settings pages' book
        // dropdown: two books of one CardDAV account share a long prefix.
        row.set_factory(Some(&book_label_factory(true)));
        row.set_list_factory(Some(&book_label_factory(false)));
        // Its own book, or for a new one the first book the list shows,
        // else the default (first).
        let at = if editing {
            books.iter().position(|b| b.uid == c.book_uid).unwrap_or(0)
        } else {
            self.scope_book(&books)
        };
        row.set_selected(at as u32);
        row.set_sensitive(books.len() > 1);
        let apple = books[at].groups.is_some_and(GroupFormat::holds_cards);
        identity.append(&row);
        // What saving in another book does, said while one is picked.
        let move_note = gtk::Label::new(None);
        move_note.add_css_class("dim-label");
        move_note.add_css_class("caption");
        move_note.set_wrap(true);
        move_note.set_xalign(0.0);
        move_note.set_margin_top(6);
        move_note.set_visible(false);
        detail.append(&move_note);
        let book_row = Some(row);
        let name = entry_row(&i18n("Name"), &c.name);
        name.connect_changed(|row| row.remove_css_class("error"));
        identity.append(&name);
        let hide = adw::SwitchRow::new();
        hide.set_title(&i18n("Send as Bcc"));
        hide.set_subtitle(&i18n("Members do not see each other's addresses."));
        hide.set_active(c.hide_addresses);
        identity.append(&hide);

        self.seed_suggestions();
        let _ = sender.output(ContactsPageOutput::WantSuggestions);
        let no_address = self.members_without_address(c);
        let cards = Rc::new(Cell::new(apple));
        let members = member_editor(
            detail,
            &c.members,
            self.suggestions.clone(),
            Rc::new(no_address),
            cards.clone(),
        );
        // An Apple group (iCloud's) or a category (Nextcloud's) holds
        // cards of its own book.
        let note = gtk::Label::new(Some(&i18n(
            "Groups in this address book hold its own contacts: anyone added who is not in it yet is added to it.",
        )));
        note.add_css_class("dim-label");
        note.add_css_class("caption");
        note.set_wrap(true);
        note.set_xalign(0.0);
        note.set_margin_top(6);
        note.set_visible(apple);
        detail.append(&note);
        if let Some(row) = &book_row {
            follow_book_choice(
                row,
                &books,
                &note,
                &move_note,
                cards,
                editing.then_some(at),
                c.book_name.clone(),
            );
        }
        if self.editing_target.is_none() {
            name.grab_focus();
        }
        self.editor = Some(Editor::Group(GroupForm {
            name,
            hide,
            members,
            book: book_row,
            books,
        }));
    }

    /// The books a group can be saved in: any that keeps groups, with the
    /// group's own first when it is not one of them, else the Hylki book.
    fn group_books(&self, c: &ContactDetails, editing: bool) -> Vec<crate::contacts::Book> {
        // Any book that keeps groups: a new group goes in one, a group
        // being edited can move to another. The Hylki book when none is
        // listed yet.
        let mut books: Vec<crate::contacts::Book> = self
            .books
            .iter()
            .filter(|b| b.groups.is_some())
            .cloned()
            .collect();
        if editing && !books.iter().any(|b| b.uid == c.book_uid) {
            books.insert(
                0,
                crate::contacts::Book {
                    uid: c.book_uid.clone(),
                    name: c.book_name.clone(),
                    groups: c.group,
                },
            );
        }
        if books.is_empty() {
            books.push(crate::contacts::local_book());
        }
        books
    }

    /// Offer the contacts here as people to add until the composer's
    /// suggestions are read.
    fn seed_suggestions(&self) {
        // People to add are offered as the composer offers recipients: from
        // Contacts and from the mail sent and received. Until that is read,
        // the contacts here stand in.
        if self.suggestions.borrow().is_empty() {
            let contacts = self
                .contacts
                .iter()
                .filter(|p| !p.is_group())
                .flat_map(|p| {
                    p.emails.iter().map(|e| crate::contacts::Suggestion {
                        name: p.name.clone(),
                        email: e.value.clone(),
                        from_contacts: true,
                        ..Default::default()
                    })
                });
            self.suggestions.borrow_mut().extend(contacts);
        }
    }

    /// The contacts with no address, by their book's name, who can join a
    /// group that names its members by contact.
    fn members_without_address(&self, c: &ContactDetails) -> Vec<(Member, String)> {
        // The contacts with no address, who can join a group that names
        // its members by contact.
        self.contacts
            .iter()
            .filter(|p| !p.is_group() && p.emails.is_empty() && !p.eds_uid.is_empty())
            .map(|p| {
                let m = Member {
                    name: p.name.clone(),
                    source: Some((p.book_uid.clone(), p.eds_uid.clone())),
                    // Their card, when it is in the group's book already.
                    uid: if p.book_uid == c.book_uid {
                        p.eds_uid.clone()
                    } else {
                        String::new()
                    },
                    ..Member::default()
                };
                (m, p.book_name.clone())
            })
            .collect()
    }

    /// Replace the detail pane with the editor form for `editing_target`
    /// (blank for a new contact).
    pub(super) fn render_editor(
        &mut self,
        widgets: &ContactsPageWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let detail = &widgets.detail_box;
        while let Some(child) = detail.first_child() {
            detail.remove(&child);
        }
        let blank = ContactDetails::default();
        let c = self
            .editing_target
            .and_then(|i| self.contacts.get(i))
            .unwrap_or(&blank);

        let heading = if self.editing_target.is_some() {
            i18n("Edit Contact")
        } else {
            i18n("New Contact")
        };
        editor_bar(detail, sender, &heading);

        let photo = Rc::new(RefCell::new(crate::contacts::PhotoEdit::Keep));
        photo_editor(detail, c, photo.clone());

        // ---- identity fields ----
        let identity = group(detail, &i18n("Identity"));
        let name = entry_row(&i18n("Name"), &c.name);
        let nickname = entry_row(&i18n("Nickname"), &c.nickname);
        let org = entry_row(&i18n("Organization"), &c.org);
        let title = entry_row(&i18n("Title"), &c.title);
        identity.append(&name);
        identity.append(&nickname);
        identity.append(&org);
        identity.append(&title);
        let (birthday_row, birthday) = BirthdayForm::new(&c.birthday_raw);
        identity.append(&birthday_row);

        // ---- the address book: chosen for a new contact; for one being
        // edited, its own, or another to move it to (when Hylki can write
        // to its book, and so delete from it) ----
        let mut book = None;
        let editing = self.editing_target.is_some();
        let own = self
            .books
            .iter()
            .position(|b| b.uid == c.book_uid)
            .filter(|_| !c.eds_uid.is_empty());
        if (!editing || own.is_some()) && !self.books.is_empty() {
            let row = adw::ComboRow::new();
            row.set_title(&i18n("Address book"));
            let labels: Vec<&str> = self.books.iter().map(|b| b.name.as_str()).collect();
            row.set_model(Some(&gtk::StringList::new(&labels)));
            // The name whole as a tooltip, like the settings pages' book
            // dropdown: two books of one CardDAV account share a long
            // prefix.
            row.set_factory(Some(&book_label_factory(true)));
            row.set_list_factory(Some(&book_label_factory(false)));
            // Its own book, or for a new one the first book the list shows,
            // else the default (first).
            let at = own
                .filter(|_| editing)
                .unwrap_or_else(|| self.scope_book(&self.books));
            row.set_selected(at as u32);
            row.set_sensitive(self.books.len() > 1);
            if editing {
                row.set_subtitle(&i18n("Another book moves the contact there when saved."));
            }
            identity.prepend(&row);
            book = Some(row);
        } else if !c.book_name.is_empty() {
            let row = adw::ActionRow::new();
            row.set_title(&i18n("Address book"));
            row.set_subtitle(&gtk::glib::markup_escape_text(&c.book_name));
            row.add_css_class("property");
            identity.prepend(&row);
        }

        // ---- repeating value groups ----
        let emails = editable_values(
            detail,
            &i18n("Email"),
            &i18n("Add email"),
            &c.emails
                .iter()
                .map(|l| (l.value.clone(), l.label.clone()))
                .collect::<Vec<_>>(),
            EMAIL_LABELS,
        );
        let phones = editable_values(
            detail,
            &i18n("Phone"),
            &i18n("Add phone"),
            &c.phones
                .iter()
                .map(|l| (l.value.clone(), l.label.clone()))
                .collect::<Vec<_>>(),
            PHONE_LABELS,
        );
        let addresses = address_editor(detail, &c.postal);
        let urls = editable_values(
            detail,
            &i18n("Website"),
            &i18n("Add website"),
            &c.urls
                .iter()
                .map(|u| (u.clone(), String::new()))
                .collect::<Vec<_>>(),
            &[],
        );

        // ---- note ----
        let note_list = group(detail, &i18n("Note"));
        let note = gtk::TextView::new();
        note.set_wrap_mode(gtk::WrapMode::WordChar);
        note.buffer().set_text(&c.note);
        note.set_top_margin(8);
        note.set_bottom_margin(8);
        note.set_left_margin(10);
        note.set_right_margin(10);
        note.set_height_request(72);
        let note_row = gtk::ListBoxRow::new();
        note_row.set_activatable(false);
        note_row.set_child(Some(&note));
        note_list.append(&note_row);

        self.editor = Some(Editor::Contact(ContactForm {
            name,
            nickname,
            org,
            title,
            note,
            emails,
            phones,
            urls,
            addresses,
            birthday,
            photo,
            book,
        }));
    }
}
