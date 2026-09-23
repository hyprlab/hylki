//! In-app Contacts view, shown in the main content area (like the attachments
//! gallery): a searchable, sortable list of every GNOME Contacts entry beside
//! the selected contact's full card, with editing, creation and deletion done
//! right here (writes go through EDS, so GNOME Contacts and CardDAV stay in
//! sync). Anything Hylki doesn't do — linking, photos, address books — is one
//! click away via "Open GNOME Contacts".

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::contacts::{ContactDetails, ContactEdit, Labeled};
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::i18n::{i18n, i18n_f, i18n_noop};

/// How the contact list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactSort {
    FirstName,
    LastName,
    Email,
}

/// Live handles into the open editor form.
struct Editor {
    name: adw::EntryRow,
    nickname: adw::EntryRow,
    org: adw::EntryRow,
    title: adw::EntryRow,
    note: gtk::TextView,
    emails: Rc<RefCell<Vec<(adw::EntryRow, String)>>>,
    phones: Rc<RefCell<Vec<(adw::EntryRow, String)>>>,
    urls: Rc<RefCell<Vec<(adw::EntryRow, String)>>>,
}

pub struct ContactsPage {
    contacts: Vec<ContactDetails>,
    query: String,
    sort: ContactSort,
    /// Index into `contacts` of the contact shown in the detail pane.
    selected: Option<usize>,
    /// Filtered + sorted list-row order → index into `contacts`.
    row_map: Vec<usize>,
    /// A load is underway (the page was just opened).
    loading: bool,
    /// The editor form, while open. `editing_target` is the contact being
    /// edited, `None` for a brand-new one.
    editor: Option<Editor>,
    editing_target: Option<usize>,
    /// EDS UIDs deleted here but possibly still in EDS's local cache — a
    /// re-read races the CardDAV flush, and without these tombstones a
    /// deleted contact pops right back into the list. A tombstone lives
    /// until a fresh read no longer contains its contact.
    deleted: std::collections::HashSet<String>,
}

#[derive(Debug)]
pub enum ContactsPageInput {
    /// Fresh EDS read: replace everything (keeps the selection when possible).
    SetContacts(Vec<ContactDetails>),
    /// The page was just opened; a read is on its way.
    SetLoading,
    Query(String),
    SetSort(ContactSort),
    RowSelected(i32),
    Compose(String),
    OpenUrl(String),
    /// Edit the selected contact / a specific one (context menu).
    Edit,
    EditIndex(usize),
    /// The header's "+": a blank editor for a new contact.
    NewContact,
    CancelEdit,
    SaveEdit,
    OpenInGnome(usize),
    DeleteRequest(usize),
    DeleteConfirmed(usize),
    /// The card's photo was clicked: expand it in the app lightbox.
    OpenPhoto { name: String, data: Vec<u8> },
}

#[derive(Debug)]
pub enum ContactsPageOutput {
    /// Start a message to this address.
    Compose(String),
    /// The header's sidebar button (same spot as the message list's).
    ToggleSidebar,
    /// Persist an edit: the patched vCard back to its book.
    SaveContact { book_uid: String, vcard: String },
    /// Create a brand-new contact (the app picks the book).
    CreateContact { vcard: String },
    /// Delete, already confirmed by the user.
    DeleteContact { book_uid: String, uid: String },
    /// Show the contact's photo in the app lightbox.
    ShowPhoto { name: String, data: Vec<u8> },
}

#[relm4::component(pub)]
impl Component for ContactsPage {
    /// The app's compose revealer, mounted over the detail pane.
    type Init = gtk::Revealer;
    type Input = ContactsPageInput;
    type Output = ContactsPageOutput;
    type CommandOutput = ();

    view! {
        #[name = "page_stack"]
        gtk::Stack {
            set_transition_type: gtk::StackTransitionType::Crossfade,

            // No contacts at all (or still loading the first read).
            add_named[Some("none")] = &adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
                    add_css_class: "flat",
                    #[wrap(Some)]
                    set_title_widget = &gtk::Label {
                        set_label: &i18n("Contacts"),
                        add_css_class: "pane-title",
                    },
                    pack_start = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-sidebar-show-symbolic",
                        set_tooltip_text: Some(i18n("Toggle sidebar").as_str()),
                        add_css_class: "flat",
                        connect_clicked[sender] => move |_| {
                            let _ = sender.output(ContactsPageOutput::ToggleSidebar);
                        },
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-x-office-address-book-symbolic",
                        set_tooltip_text: Some(i18n("Open GNOME Contacts").as_str()),
                        add_css_class: "flat",
                        connect_clicked => move |_| {
                            crate::ui::contacts_browser::launch_gnome_contacts();
                        },
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "co.hyprlab.Hylki-list-add-symbolic",
                        set_tooltip_text: Some(i18n("New contact").as_str()),
                        add_css_class: "flat",
                        connect_clicked => ContactsPageInput::NewContact,
                    },
                },

                #[wrap(Some)]
                set_content = &adw::StatusPage {
                    set_icon_name: Some("co.hyprlab.Hylki-x-office-address-book-symbolic"),
                    #[watch]
                    set_title: &if model.loading { i18n("Loading Contacts…") } else { i18n("No Contacts") },
                    #[watch]
                    set_description: Some(if model.loading {
                        String::new()
                    } else {
                        i18n("Contacts you add in GNOME Contacts appear here.")
                    }.as_str()),
                },
            },

            // Each pane carries its own header (like the mail view), so the
            // divider between them runs the full height of the window; the
            // paned handle lets the list grow, with its launch width as the
            // floor.
            add_named[Some("browser")] = &gtk::Paned {
                set_orientation: gtk::Orientation::Horizontal,
                set_wide_handle: false,
                set_position: crate::config::load_contacts_pane_width(),
                set_resize_start_child: false,
                set_shrink_start_child: false,
                set_resize_end_child: true,
                set_shrink_end_child: false,
                // Remember the width the user drags to. Debounced: one write
                // once the drag settles (see the mail view's list pane).
                connect_position_notify[
                    pending = std::rc::Rc::new(std::cell::RefCell::new(
                        None::<gtk::glib::SourceId>,
                    ))
                ] => move |p| {
                    let pos = p.position();
                    if let Some(id) = pending.borrow_mut().take() {
                        id.remove();
                    }
                    let armed = pending.clone();
                    *pending.borrow_mut() = Some(gtk::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(600),
                        move || {
                            *armed.borrow_mut() = None;
                            crate::config::save_contacts_pane_width(pos);
                        },
                    ));
                },

                #[wrap(Some)]
                set_start_child = &adw::ToolbarView {
                    add_top_bar = &adw::HeaderBar {
                        add_css_class: "flat",
                        // Left pane: no window controls (the detail pane's
                        // header carries the window's close button).
                        set_show_start_title_buttons: false,
                        set_show_end_title_buttons: false,
                        #[wrap(Some)]
                        set_title_widget = &gtk::Label {
                            set_label: "",
                        },
                        // Leftmost, mirroring the pane it acts on — same spot
                        // as in the message list's header.
                        pack_start = &gtk::Button {
                            set_icon_name: "co.hyprlab.Hylki-sidebar-show-symbolic",
                            set_tooltip_text: Some(i18n("Toggle sidebar").as_str()),
                            add_css_class: "flat",
                            connect_clicked[sender] => move |_| {
                                let _ = sender.output(ContactsPageOutput::ToggleSidebar);
                            },
                        },
                        // pack_end packs right-to-left: sort at the far right
                        // (matching the message list), then the count, then
                        // the GNOME Contacts launcher, then "+".
                        #[name = "sort_btn"]
                        pack_end = &gtk::MenuButton {
                            set_icon_name: "co.hyprlab.Hylki-view-sort-descending-symbolic",
                            set_tooltip_text: Some(i18n("Sort contacts").as_str()),
                            set_valign: gtk::Align::Center,
                            add_css_class: "flat",
                        },
                        #[name = "count_label"]
                        pack_end = &gtk::Label {
                            set_valign: gtk::Align::Center,
                            add_css_class: "list-count",
                        },
                        pack_end = &gtk::Button {
                            set_icon_name: "co.hyprlab.Hylki-x-office-address-book-symbolic",
                            set_tooltip_text: Some(i18n("Open GNOME Contacts").as_str()),
                            add_css_class: "flat",
                            connect_clicked => move |_| {
                                crate::ui::contacts_browser::launch_gnome_contacts();
                            },
                        },
                        pack_end = &gtk::Button {
                            set_icon_name: "co.hyprlab.Hylki-list-add-symbolic",
                            set_tooltip_text: Some(i18n("New contact").as_str()),
                            add_css_class: "flat",
                            connect_clicked => ContactsPageInput::NewContact,
                        },
                    },

                    #[wrap(Some)]
                    set_content = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_width_request: 280,

                        #[name = "search"]
                        gtk::SearchEntry {
                            set_placeholder_text: Some(i18n("Search contacts").as_str()),
                            set_margin_top: 8,
                            set_margin_bottom: 8,
                            set_margin_start: 10,
                            set_margin_end: 10,
                            connect_search_changed[sender] => move |e| {
                                sender.input(ContactsPageInput::Query(e.text().to_string()));
                            },
                        },

                        gtk::ScrolledWindow {
                            set_vexpand: true,
                            set_hscrollbar_policy: gtk::PolicyType::Never,

                            #[name = "list"]
                            gtk::ListBox {
                                add_css_class: "navigation-sidebar",
                                add_css_class: "contacts-listbox",
                                set_selection_mode: gtk::SelectionMode::Single,
                                connect_row_selected[sender] => move |_, row| {
                                    if let Some(row) = row {
                                        sender.input(ContactsPageInput::RowSelected(row.index()));
                                    }
                                },
                            },
                        },
                    },
                },

                #[wrap(Some)]
                #[name = "detail_overlay"]
                set_end_child = &gtk::Overlay {
                    // The app mounts its contacts compose slot over this in
                    // init, so "New message" slides down right here.
                    #[wrap(Some)]
                    set_child = &adw::ToolbarView {
                    set_hexpand: true,

                    add_top_bar = &adw::HeaderBar {
                        add_css_class: "flat",
                        set_show_start_title_buttons: false,
                        // Top-left of the card pane: edit the shown contact.
                        pack_start = &gtk::Button {
                            set_icon_name: "co.hyprlab.Hylki-document-edit-symbolic",
                            set_tooltip_text: Some(i18n("Edit contact").as_str()),
                            add_css_class: "flat",
                            connect_clicked => ContactsPageInput::Edit,
                        },
                        #[wrap(Some)]
                        set_title_widget = &gtk::Label {
                            set_label: &i18n("Contacts"),
                            add_css_class: "pane-title",
                        },
                    },

                    #[wrap(Some)]
                    set_content = &gtk::ScrolledWindow {
                        set_hexpand: true,
                        set_vexpand: true,
                        set_hscrollbar_policy: gtk::PolicyType::Never,
                        add_css_class: "contact-detail-pane",

                        adw::Clamp {
                            set_maximum_size: 560,
                            set_tightening_threshold: 420,

                            #[name = "detail_box"]
                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_margin_top: 24,
                                set_margin_bottom: 32,
                                set_margin_start: 18,
                                set_margin_end: 18,
                            },
                        },
                    },
                    },
                },
            },
        }
    }

    fn init(
        compose_slot: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let model = ContactsPage {
            contacts: Vec::new(),
            query: String::new(),
            sort: ContactSort::FirstName,
            selected: None,
            row_map: Vec::new(),
            loading: true,
            editor: None,
            editing_target: None,
            deleted: std::collections::HashSet::new(),
        };
        let widgets = view_output!();
        widgets.detail_overlay.add_overlay(&compose_slot);

        // The sort menu: a radio per order, mirroring the message list's
        // sort button.
        {
            let pop = gtk::Popover::new();
            let vbox = gtk::Box::new(gtk::Orientation::Vertical, 2);
            vbox.set_margin_top(6);
            vbox.set_margin_bottom(6);
            vbox.set_margin_start(6);
            vbox.set_margin_end(6);
            let mut first: Option<gtk::CheckButton> = None;
            for (label, sort) in [
                (i18n_noop("First name"), ContactSort::FirstName),
                (i18n_noop("Last name"), ContactSort::LastName),
                (i18n_noop("Email"), ContactSort::Email),
            ] {
                let check = gtk::CheckButton::with_label(&i18n(label));
                if let Some(f) = &first {
                    check.set_group(Some(f));
                } else {
                    check.set_active(true);
                    first = Some(check.clone());
                }
                let s = sender.input_sender().clone();
                let p = pop.clone();
                check.connect_toggled(move |c| {
                    if c.is_active() {
                        let _ = s.send(ContactsPageInput::SetSort(sort));
                        p.popdown();
                    }
                });
                vbox.append(&check);
            }
            pop.set_child(Some(&vbox));
            widgets.sort_btn.set_popover(Some(&pop));
        }

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match message {
            ContactsPageInput::SetLoading => {
                self.loading = true;
                if self.contacts.is_empty() {
                    widgets.page_stack.set_visible_child_name("none");
                }
            }

            ContactsPageInput::SetContacts(mut contacts) => {
                // Drop tombstoned (locally deleted) entries the read still
                // carries; a tombstone retires once a read comes back clean.
                let incoming: std::collections::HashSet<String> =
                    contacts.iter().map(|c| c.eds_uid.clone()).collect();
                self.deleted.retain(|uid| incoming.contains(uid));
                contacts.retain(|c| !self.deleted.contains(&c.eds_uid));
                // Keep the same person selected across a refresh, by identity.
                let keep = self
                    .selected
                    .and_then(|i| self.contacts.get(i))
                    .map(|c| (c.name.clone(), c.primary_email().map(str::to_string)));
                self.contacts = contacts;
                self.loading = false;
                self.editor = None;
                self.editing_target = None;
                self.selected = keep.and_then(|(name, email)| {
                    self.contacts.iter().position(|c| {
                        c.name == name && c.primary_email().map(str::to_string) == email
                    })
                });
                self.rebuild(widgets, &sender);
            }

            ContactsPageInput::Query(q) => {
                self.query = q.to_lowercase();
                self.rebuild(widgets, &sender);
            }

            ContactsPageInput::SetSort(sort) => {
                if self.sort != sort {
                    self.sort = sort;
                    self.rebuild(widgets, &sender);
                }
            }

            ContactsPageInput::RowSelected(row) => {
                let Some(&idx) = self.row_map.get(row as usize) else { return };
                if self.selected == Some(idx) && self.editor.is_none() {
                    return;
                }
                // Moving away abandons an open editor.
                self.editor = None;
                self.editing_target = None;
                self.selected = Some(idx);
                self.render_detail(widgets, &sender);
            }

            ContactsPageInput::Compose(email) => {
                let _ = sender.output(ContactsPageOutput::Compose(email));
            }

            ContactsPageInput::OpenUrl(url) => {
                // Bare "example.org" URLs are common in vCards.
                let full = if url.contains("://") { url } else { format!("https://{url}") };
                crate::ui::launch::open_link(&full, None);
            }

            ContactsPageInput::Edit => {
                if self.selected.is_some() {
                    self.editing_target = self.selected;
                    self.render_editor(widgets, &sender);
                }
            }

            ContactsPageInput::EditIndex(idx) => {
                if idx < self.contacts.len() {
                    self.selected = Some(idx);
                    self.select_row_for(widgets, idx);
                    self.editing_target = Some(idx);
                    self.render_editor(widgets, &sender);
                }
            }

            ContactsPageInput::NewContact => {
                self.editing_target = None;
                widgets.page_stack.set_visible_child_name("browser");
                self.render_editor(widgets, &sender);
            }

            ContactsPageInput::CancelEdit => {
                self.editor = None;
                self.editing_target = None;
                if self.contacts.is_empty() {
                    widgets.page_stack.set_visible_child_name("none");
                }
                self.render_detail(widgets, &sender);
            }

            ContactsPageInput::SaveEdit => {
                let Some(editor) = &self.editor else { return };
                let edit = collect_edit(editor);
                if edit.name.is_empty() && edit.emails.is_empty() {
                    return; // nothing worth saving
                }
                match self.editing_target.and_then(|i| self.contacts.get(i)) {
                    Some(c) => {
                        let vcard = crate::contacts::patched_vcard(&c.raw_vcard, &edit);
                        let _ = sender.output(ContactsPageOutput::SaveContact {
                            book_uid: c.book_uid.clone(),
                            vcard,
                        });
                    }
                    None => {
                        let vcard = crate::contacts::new_vcard(&edit);
                        let _ = sender.output(ContactsPageOutput::CreateContact { vcard });
                    }
                }
                // The app re-reads EDS and pushes SetContacts; until then the
                // card shows the pre-edit state rather than a half-saved one.
                self.editor = None;
                self.editing_target = None;
                self.render_detail(widgets, &sender);
            }

            ContactsPageInput::OpenInGnome(idx) => {
                if let Some(c) = self.contacts.get(idx) {
                    crate::ui::contacts_browser::launch_gnome_contacts_for(&c.eds_uid);
                }
            }

            ContactsPageInput::DeleteRequest(idx) => {
                let Some(c) = self.contacts.get(idx) else { return };
                let win = widgets.page_stack.root().and_downcast::<gtk::Window>();
                let dialog = adw::MessageDialog::new(
                    win.as_ref(),
                    Some(i18n("Delete Contact?").as_str()),
                    Some(&format!(
                        "{} is removed from your address book — and, for a synced \
                         book, from the server too.",
                        c.name
                    )),
                );
                dialog.add_response("cancel", &i18n("Cancel"));
                dialog.add_response("delete", &i18n("Delete"));
                dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                let s = sender.input_sender().clone();
                dialog.connect_response(None, move |_, resp| {
                    if resp == "delete" {
                        let _ = s.send(ContactsPageInput::DeleteConfirmed(idx));
                    }
                });
                dialog.present();
            }

            ContactsPageInput::OpenPhoto { name, data } => {
                let _ = sender.output(ContactsPageOutput::ShowPhoto { name, data });
            }

            ContactsPageInput::DeleteConfirmed(idx) => {
                if idx >= self.contacts.len() {
                    return;
                }
                let c = self.contacts.remove(idx);
                if !c.eds_uid.is_empty() {
                    self.deleted.insert(c.eds_uid.clone());
                    let _ = sender.output(ContactsPageOutput::DeleteContact {
                        book_uid: c.book_uid,
                        uid: c.eds_uid,
                    });
                }
                // The entry leaves the list right now; the EDS delete and the
                // reconciling re-read follow behind.
                self.selected = None;
                self.editor = None;
                self.editing_target = None;
                self.rebuild(widgets, &sender);
            }
        }
        // Overriding `update_with_view` replaces the generated body that ran
        // `update_view` after every message; without this the `#[watch]`
        // bindings above (the "Loading Contacts…" / "No Contacts" face) only
        // ever hold their `init` values, and an empty address book looked
        // like a load that never finished.
        self.update_view(widgets, sender);
    }
}

impl ContactsPage {
    /// Whether a contact matches the active search.
    fn matches(&self, c: &ContactDetails) -> bool {
        if self.query.is_empty() {
            return true;
        }
        let q = self.query.as_str();
        c.name.to_lowercase().contains(q)
            || c.nickname.to_lowercase().contains(q)
            || c.org.to_lowercase().contains(q)
            || c.emails.iter().any(|e| e.value.to_lowercase().contains(q))
            || c.phones.iter().any(|p| p.value.to_lowercase().contains(q))
    }

    /// Move the list selection to the row showing contact `idx` (if visible).
    fn select_row_for(&self, widgets: &ContactsPageWidgets, idx: usize) {
        if let Some(pos) = self.row_map.iter().position(|&i| i == idx) {
            if let Some(row) = widgets.list.row_at_index(pos as i32) {
                widgets.list.select_row(Some(&row));
            }
        }
    }

    /// Rebuild the list from `contacts` + `query` + `sort`, keep (or default)
    /// the selection, and render its detail card.
    fn rebuild(&mut self, widgets: &ContactsPageWidgets, sender: &ComponentSender<Self>) {
        widgets
            .page_stack
            .set_visible_child_name(if self.contacts.is_empty() { "none" } else { "browser" });

        while let Some(child) = widgets.list.first_child() {
            widgets.list.remove(&child);
        }
        self.row_map = (0..self.contacts.len())
            .filter(|&i| self.matches(&self.contacts[i]))
            .collect();
        self.row_map.sort_by_key(|&i| {
            let c = &self.contacts[i];
            let name = c.name.to_lowercase();
            match self.sort {
                ContactSort::FirstName => name,
                // Sort by the final word of the name (then the whole name for
                // ties) — how an address book orders by surname.
                ContactSort::LastName => {
                    format!("{} {name}", name.rsplit(' ').next().unwrap_or(&name))
                }
                ContactSort::Email => {
                    c.primary_email().map(str::to_lowercase).unwrap_or(name)
                }
            }
        });
        widgets.count_label.set_label(&if self.query.is_empty() {
            self.contacts.len().to_string()
        } else {
            i18n_f(
                "{shown} of {total}",
                &[
                    ("shown", &self.row_map.len().to_string()),
                    ("total", &self.contacts.len().to_string()),
                ],
            )
        });
        for &idx in &self.row_map {
            let c = &self.contacts[idx];
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(&c.name));
            if let Some(email) = c.primary_email() {
                if email != c.name {
                    row.set_subtitle(&gtk::glib::markup_escape_text(email));
                }
            }
            row.set_activatable(true);
            row.add_prefix(&avatar_for(c, 34));

            // Right-click: the quick actions on this entry.
            let right_click = gtk::GestureClick::new();
            right_click.set_button(3);
            let s = sender.input_sender().clone();
            right_click.connect_pressed(move |gesture, _, x, y| {
                let Some(widget) = gesture.widget() else { return };
                let (se, sd, sg) = (s.clone(), s.clone(), s.clone());
                show_context_menu(
                    &widget,
                    x,
                    y,
                    vec![
                        vec![
                            MenuEntry::new(i18n("Edit"), move || {
                                let _ = se.send(ContactsPageInput::EditIndex(idx));
                            })
                            .icon("co.hyprlab.Hylki-document-edit-symbolic"),
                            MenuEntry::new(i18n("Open in GNOME Contacts"), move || {
                                let _ = sg.send(ContactsPageInput::OpenInGnome(idx));
                            })
                            .icon("co.hyprlab.Hylki-adw-external-link-symbolic"),
                        ],
                        vec![MenuEntry::new(i18n("Delete…"), move || {
                            let _ = sd.send(ContactsPageInput::DeleteRequest(idx));
                        })
                        .icon("co.hyprlab.Hylki-user-trash-symbolic")],
                    ],
                );
            });
            row.add_controller(right_click);
            widgets.list.append(&row);
        }

        // Selection: keep it if it survived the filter, else the first match.
        let shown_pos = self
            .selected
            .and_then(|sel| self.row_map.iter().position(|&i| i == sel))
            .or((!self.row_map.is_empty()).then_some(0));
        match shown_pos {
            Some(pos) => {
                self.selected = Some(self.row_map[pos]);
                if let Some(row) = widgets.list.row_at_index(pos as i32) {
                    widgets.list.select_row(Some(&row));
                }
            }
            None => self.selected = None,
        }
        self.render_detail(widgets, sender);
    }

    /// Fill the right-hand pane with the selected contact's card.
    fn render_detail(&mut self, widgets: &ContactsPageWidgets, sender: &ComponentSender<Self>) {
        self.editor = None;
        let detail = &widgets.detail_box;
        while let Some(child) = detail.first_child() {
            detail.remove(&child);
        }
        let Some(c) = self.selected.and_then(|i| self.contacts.get(i)) else {
            return;
        };

        // ---- identity header: photo, name, who they are ----
        let avatar = avatar_for(c, 96);
        if let Some(photo) = &c.photo {
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

        // ---- field groups ----
        if !c.emails.is_empty() {
            let list = group(detail, "Email");
            for e in &c.emails {
                let row = labeled_row(e);
                let compose = flat_button(
                    "co.hyprlab.Hylki-mail-message-new-symbolic",
                    &i18n("New message"),
                );
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
            let list = group(detail, "Phone");
            for p in &c.phones {
                let row = labeled_row(p);
                row.add_suffix(&copy_button(&p.value));
                list.append(&row);
            }
        }

        if !c.addresses.is_empty() {
            let list = group(detail, "Address");
            for a in &c.addresses {
                let row = labeled_row(a);
                row.add_suffix(&copy_button(&a.value));
                list.append(&row);
            }
        }

        if !c.urls.is_empty() {
            let list = group(detail, "Website");
            for url in &c.urls {
                let row = adw::ActionRow::new();
                row.set_title(&gtk::glib::markup_escape_text(url));
                row.set_activatable(true);
                let icon =
                    gtk::Image::from_icon_name("co.hyprlab.Hylki-adw-external-link-symbolic");
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
            extras.push(Labeled { label: "Birthday".into(), value: c.birthday.clone() });
        }
        if !c.note.is_empty() {
            extras.push(Labeled { label: "Note".into(), value: c.note.clone() });
        }
        // Where this entry lives — the local book or a synced account.
        if !c.book_name.is_empty() {
            extras.push(Labeled { label: "Address Book".into(), value: c.book_name.clone() });
        }
        if !extras.is_empty() {
            let list = group(detail, "Details");
            for x in &extras {
                let row = adw::ActionRow::new();
                row.set_title(&gtk::glib::markup_escape_text(&x.label));
                row.set_subtitle(&gtk::glib::markup_escape_text(&x.value));
                row.add_css_class("property");
                list.append(&row);
            }
        }
    }

    /// Replace the detail pane with the editor form for `editing_target`
    /// (blank for a new contact).
    fn render_editor(&mut self, widgets: &ContactsPageWidgets, sender: &ComponentSender<Self>) {
        let detail = &widgets.detail_box;
        while let Some(child) = detail.first_child() {
            detail.remove(&child);
        }
        let blank = ContactDetails::default();
        let c = self
            .editing_target
            .and_then(|i| self.contacts.get(i))
            .unwrap_or(&blank);

        // ---- Cancel · title · Save bar (dialog-header layout) ----
        let bar = gtk::CenterBox::new();
        let cancel = gtk::Button::with_label(&i18n("Cancel"));
        let s = sender.input_sender().clone();
        cancel.connect_clicked(move |_| {
            let _ = s.send(ContactsPageInput::CancelEdit);
        });
        let save = gtk::Button::with_label(&i18n("Save"));
        save.add_css_class("suggested-action");
        let s = sender.input_sender().clone();
        save.connect_clicked(move |_| {
            let _ = s.send(ContactsPageInput::SaveEdit);
        });
        let heading = gtk::Label::new(Some(if self.editing_target.is_some() { i18n("Edit Contact") } else { i18n("New Contact") }.as_str()));
        heading.add_css_class("title-4");
        bar.set_start_widget(Some(&cancel));
        bar.set_center_widget(Some(&heading));
        bar.set_end_widget(Some(&save));
        detail.append(&bar);

        // ---- identity fields ----
        let identity = group(detail, "Identity");
        let name = entry_row("Name", &c.name);
        let nickname = entry_row("Nickname", &c.nickname);
        let org = entry_row("Organization", &c.org);
        let title = entry_row("Title", &c.title);
        identity.append(&name);
        identity.append(&nickname);
        identity.append(&org);
        identity.append(&title);

        // ---- repeating value groups ----
        let emails = editable_values(
            detail,
            &i18n("Email"),
            &i18n("Add email"),
            &c.emails.iter().map(|l| (l.value.clone(), l.label.clone())).collect::<Vec<_>>(),
        );
        let phones = editable_values(
            detail,
            &i18n("Phone"),
            &i18n("Add phone"),
            &c.phones.iter().map(|l| (l.value.clone(), l.label.clone())).collect::<Vec<_>>(),
        );
        let urls = editable_values(
            detail,
            &i18n("Website"),
            &i18n("Add website"),
            &c.urls.iter().map(|u| (u.clone(), String::new())).collect::<Vec<_>>(),
        );

        // ---- note ----
        let note_list = group(detail, "Note");
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

        self.editor = Some(Editor { name, nickname, org, title, note, emails, phones, urls });
    }
}

/// Read the editor form back into a `ContactEdit`.
fn collect_edit(e: &Editor) -> ContactEdit {
    let values = |rows: &Rc<RefCell<Vec<(adw::EntryRow, String)>>>| -> Vec<Labeled> {
        rows.borrow()
            .iter()
            .filter_map(|(entry, label)| {
                let value = entry.text().trim().to_string();
                (!value.is_empty()).then(|| Labeled { label: label.clone(), value })
            })
            .collect()
    };
    let buffer = e.note.buffer();
    ContactEdit {
        name: e.name.text().trim().to_string(),
        nickname: e.nickname.text().trim().to_string(),
        org: e.org.text().trim().to_string(),
        title: e.title.text().trim().to_string(),
        note: buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .trim()
            .to_string(),
        emails: values(&e.emails),
        phones: values(&e.phones),
        urls: values(&e.urls).into_iter().map(|l| l.value).collect(),
    }
}

/// A titled group of editable value rows with a remove button each and an
/// "add" row at the bottom. Returns the live handle used at save time.
fn editable_values(
    detail: &gtk::Box,
    title: &str,
    add_label: &str,
    initial: &[(String, String)],
) -> Rc<RefCell<Vec<(adw::EntryRow, String)>>> {
    let list = group(detail, title);
    let rows: Rc<RefCell<Vec<(adw::EntryRow, String)>>> = Rc::new(RefCell::new(Vec::new()));

    // The "add" row sits last; value rows are inserted above it.
    let add_row = gtk::ListBoxRow::new();
    add_row.set_activatable(true);
    let add_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    add_box.set_margin_top(10);
    add_box.set_margin_bottom(10);
    add_box.set_margin_start(12);
    let plus = gtk::Image::from_icon_name("co.hyprlab.Hylki-list-add-symbolic");
    plus.add_css_class("dim-label");
    let label = gtk::Label::new(Some(add_label));
    label.add_css_class("dim-label");
    add_box.append(&plus);
    add_box.append(&label);
    add_row.set_child(Some(&add_box));

    let field = title.to_string();
    let add_value = {
        let list = list.clone();
        let rows = rows.clone();
        let add_row = add_row.clone();
        move |value: &str, label: String, grab: bool| {
            let entry = adw::EntryRow::new();
            entry.set_title(&if label.is_empty() { field.clone() } else { label.clone() });
            entry.set_text(value);
            let remove = gtk::Button::from_icon_name("co.hyprlab.Hylki-user-trash-symbolic");
            remove.set_tooltip_text(Some(i18n("Remove").as_str()));
            remove.set_valign(gtk::Align::Center);
            remove.add_css_class("flat");
            {
                let list = list.clone();
                let rows = rows.clone();
                let entry = entry.clone();
                remove.connect_clicked(move |_| {
                    rows.borrow_mut().retain(|(e, _)| e != &entry);
                    list.remove(&entry);
                });
            }
            entry.add_suffix(&remove);
            // Insert above the trailing "add" row.
            list.insert(&entry, add_row.index());
            rows.borrow_mut().push((entry.clone(), label));
            if grab {
                entry.grab_focus();
            }
        }
    };

    for (value, label) in initial {
        add_value(value, label.clone(), false);
    }
    {
        let add_value = add_value.clone();
        let add_row = add_row.clone();
        list.connect_row_activated(move |_, row| {
            if row == &add_row {
                add_value("", String::new(), true);
            }
        });
    }
    list.append(&add_row);
    rows
}

fn entry_row(title: &str, value: &str) -> adw::EntryRow {
    let row = adw::EntryRow::new();
    row.set_title(title);
    row.set_text(value);
    row
}

/// The contact's avatar: their GNOME Contacts photo when they have one, the
/// usual initials disc when not.
fn avatar_for(c: &ContactDetails, size: i32) -> adw::Avatar {
    let avatar = adw::Avatar::new(size, Some(&c.name), true);
    if let Some(photo) = &c.photo {
        let bytes = gtk::glib::Bytes::from(photo.as_slice());
        if let Ok(texture) = gtk::gdk::Texture::from_bytes(&bytes) {
            avatar.set_custom_image(Some(&texture));
        }
    }
    avatar
}

/// A titled boxed list appended to the detail column; returns the list.
fn group(detail: &gtk::Box, title: &str) -> gtk::ListBox {
    let heading = gtk::Label::new(Some(title));
    heading.add_css_class("heading");
    heading.set_halign(gtk::Align::Start);
    heading.set_margin_top(20);
    heading.set_margin_bottom(6);
    detail.append(&heading);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    detail.append(&list);
    list
}

/// A value row with its (optional) "Home"/"Work" label as the subtitle.
fn labeled_row(l: &Labeled) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(&gtk::glib::markup_escape_text(&l.value));
    if !l.label.is_empty() {
        row.set_subtitle(&gtk::glib::markup_escape_text(&l.label));
    }
    row
}

fn flat_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tooltip));
    b.set_valign(gtk::Align::Center);
    b.add_css_class("flat");
    b
}

/// A suffix button that puts `text` on the clipboard, confirming by briefly
/// swapping its icon for a checkmark.
fn copy_button(text: &str) -> gtk::Button {
    let b = flat_button("co.hyprlab.Hylki-edit-copy-symbolic", "Copy");
    let text = text.to_string();
    b.connect_clicked(move |b| {
        b.clipboard().set_text(&text);
        b.set_icon_name("co.hyprlab.Hylki-verified-checkmark-symbolic");
        let b = b.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
            b.set_icon_name("co.hyprlab.Hylki-edit-copy-symbolic");
        });
    });
    b
}
