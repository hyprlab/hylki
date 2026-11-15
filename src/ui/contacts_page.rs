//! In-app Contacts view, shown in the main content area (like the attachments
//! gallery): a searchable, sortable list of every GNOME Contacts entry beside
//! the selected contact's full card, with editing, creation and deletion done
//! right here (writes go through EDS, so GNOME Contacts and CardDAV stay in
//! sync). Anything Hylki doesn't do — linking, photos — is one click away via
//! a contact's "Open in GNOME Contacts".

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::i18n;

mod actions;
mod detail;
mod forms;
mod list;
mod member_editor;
mod menus;
mod rows;
mod types;

use menus::{new_menu, FilterMenu};
use rows::{row_factory, DragKeys};

pub(crate) use menus::{book_label_factory, choose_vcf_files};
pub(crate) use rows::dragged_contacts;
pub(crate) use types::*;

mod update_edit;
mod update_groups;
mod update_list;

#[cfg(test)]
mod tests;

/// What a message handler leaves to `update_with_view`: `Continue` runs
/// `update_view` after it, `Break` (a handler that returned early) skips it,
/// as the early returns of the original single `match` did.
type Flow = std::ops::ControlFlow<()>;

#[relm4::component(pub(crate))]
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
                        set_icon_name: "sidebar-show-symbolic",
                        set_tooltip_text: Some(i18n("Toggle sidebar").as_str()),
                        add_css_class: "flat",
                        connect_clicked[sender] => move |_| {
                            let _ = sender.output(ContactsPageOutput::ToggleSidebar);
                        },
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "document-open-symbolic",
                        set_tooltip_text: Some(i18n("Import vCard files into the Hylki address book").as_str()),
                        add_css_class: "flat",
                        connect_clicked => ContactsPageInput::ImportContacts,
                    },
                    pack_end = &gtk::Button {
                        set_icon_name: "list-add-symbolic",
                        set_tooltip_text: Some(i18n("New contact or group").as_str()),
                        add_css_class: "flat",
                        connect_clicked[sender] => move |b| new_menu(b, &sender),
                    },
                },

                #[wrap(Some)]
                set_content = &adw::StatusPage {
                    set_icon_name: Some("x-office-address-book-symbolic"),
                    #[watch]
                    set_title: &if model.loading { i18n("Loading Contacts…") } else { i18n("No Contacts") },
                    #[watch]
                    set_description: Some(if model.loading {
                        String::new()
                    } else {
                        i18n("Add a contact with the + button, or import vCard files. Contacts are kept in Hylki's own address book, and the address books of GNOME Contacts are listed here too.")
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
                            set_icon_name: "sidebar-show-symbolic",
                            set_tooltip_text: Some(i18n("Toggle sidebar").as_str()),
                            add_css_class: "flat",
                            connect_clicked[sender] => move |_| {
                                let _ = sender.output(ContactsPageOutput::ToggleSidebar);
                            },
                        },
                        // Beside it, as in the message list's header.
                        pack_start = &gtk::Button {
                            set_icon_name: "system-search-symbolic",
                            set_tooltip_text: Some(i18n("Search contacts (Ctrl+F)").as_str()),
                            add_css_class: "flat",
                            connect_clicked => ContactsPageInput::FocusSearch,
                        },
                        // pack_end packs right-to-left: sort and filter at the
                        // far right (matching the message list), then the
                        // count, then import, then "+", first on the right.
                        #[name = "filter_btn"]
                        pack_end = &gtk::MenuButton {
                            set_icon_name: "filter-symbolic",
                            set_tooltip_text: Some(i18n("Sort and filter contacts").as_str()),
                            set_valign: gtk::Align::Center,
                            add_css_class: "flat",
                        },
                        #[name = "count_label"]
                        pack_end = &gtk::Label {
                            set_valign: gtk::Align::Center,
                            add_css_class: "list-count",
                        },
                        pack_end = &gtk::Button {
                            set_icon_name: "document-open-symbolic",
                            set_tooltip_text: Some(i18n("Import vCard files into the Hylki address book").as_str()),
                            add_css_class: "flat",
                            connect_clicked => ContactsPageInput::ImportContacts,
                        },
                        pack_end = &gtk::Button {
                            set_icon_name: "list-add-symbolic",
                            set_tooltip_text: Some(i18n("New contact or group").as_str()),
                            add_css_class: "flat",
                            connect_clicked[sender] => move |b| new_menu(b, &sender),
                        },
                    },

                    #[wrap(Some)]
                    set_content = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_width_request: 280,

                        // The search, shown by the header's search button:
                        // the message list's search bar.
                        gtk::Revealer {
                            set_transition_type: gtk::RevealerTransitionType::SlideDown,
                            #[watch]
                            set_reveal_child: model.search_open || model.search_pinned,

                            gtk::Box {
                                set_spacing: 6,
                                add_css_class: "list-search-row",

                                #[name = "search"]
                                gtk::SearchEntry {
                                    set_hexpand: true,
                                    set_width_chars: 3,
                                    set_placeholder_text: Some(i18n("Search contacts").as_str()),
                                    connect_search_changed[sender] => move |e| {
                                        sender.input(ContactsPageInput::Query(e.text().to_string()));
                                    },
                                    connect_stop_search[sender] => move |_| {
                                        sender.input(ContactsPageInput::CloseSearch);
                                    },
                                    // Leaving it empty closes it too.
                                    add_controller = gtk::EventControllerFocus {
                                        connect_leave[sender] => move |ctl| {
                                            let Some(entry) = ctl.widget().and_downcast::<gtk::SearchEntry>() else { return };
                                            let sender = sender.clone();
                                            crate::ui::close_if_focus_left_row(&entry, move || {
                                                sender.input(ContactsPageInput::CloseSearch);
                                            });
                                        },
                                    },
                                },

                            },
                        },

                        // Which address book the list shows, while the
                        // sidebar does not list them: every one, or one.
                        #[name = "book_filter"]
                        gtk::DropDown {
                            set_margin_top: 6,
                            set_margin_bottom: 6,
                            set_margin_start: 10,
                            set_margin_end: 10,
                            set_visible: false,
                            set_tooltip_text: Some(i18n("Address book").as_str()),
                        },

                        gtk::ScrolledWindow {
                            set_vexpand: true,
                            set_hscrollbar_policy: gtk::PolicyType::Never,

                            #[name = "list"]
                            gtk::ListView {
                                add_css_class: "navigation-sidebar",
                                add_css_class: "contacts-listbox",
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
                        #[name = "edit_btn"]
                        pack_start = &gtk::Button {
                            set_icon_name: "document-edit-symbolic",
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
        let mut model = ContactsPage {
            contacts: Vec::new(),
            query: String::new(),
            sort: ContactSort::FirstName,
            scope: ContactScope::All,
            tree: None,
            drag: Rc::new(RefCell::new(DragKeys::default())),
            filters: 0,
            filter_menu: None,
            book_picker: false,
            picker_labels: Vec::new(),
            picker_guard: Rc::new(std::cell::Cell::new(false)),
            search_open: false,
            search_pinned: false,
            search_closed_at: None,
            selected: None,
            multi: Vec::new(),
            row_map: Vec::new(),
            store: gtk::gio::ListStore::new::<gtk::glib::BoxedAnyObject>(),
            selection: gtk::MultiSelection::new(None::<gtk::gio::ListStore>),
            rows: Rc::new(RefCell::new(HashMap::new())),
            items: Vec::new(),
            loading: true,
            editor: None,
            editing_target: None,
            deleted: std::collections::HashSet::new(),
            books: Vec::new(),
            filter_books: Vec::new(),
            directory_hits: Vec::new(),
            directory_generation: 0,
            directories: Vec::new(),
            directory_failures: Vec::new(),
            suggestions: Rc::new(RefCell::new(Vec::new())),
            pending: None,
            decoding: std::collections::HashSet::new(),
        };
        let widgets = view_output!();
        widgets.detail_overlay.add_overlay(&compose_slot);

        // Delete on the list asks to remove the contact being looked at. The
        // controller sits on the list, not the page, so Delete inside the
        // search box or the editor still edits text.
        {
            let keys = gtk::EventControllerKey::new();
            let s = sender.input_sender().clone();
            keys.connect_key_pressed(move |_, keyval, _, _| {
                if !matches!(keyval, gtk::gdk::Key::Delete | gtk::gdk::Key::KP_Delete) {
                    return gtk::glib::Propagation::Proceed;
                }
                let _ = s.send(ContactsPageInput::DeleteSelected);
                gtk::glib::Propagation::Stop
            });
            widgets.list.add_controller(keys);
        }

        model.filter_menu = Some(FilterMenu::new(&widgets.filter_btn, sender.input_sender()));

        // The dropdown's own label gives way in a narrow pane; its list
        // shows each name whole.
        widgets
            .book_filter
            .set_factory(Some(&book_label_factory(true)));
        widgets
            .book_filter
            .set_list_factory(Some(&book_label_factory(false)));
        {
            let s = sender.input_sender().clone();
            let guard = model.picker_guard.clone();
            widgets.book_filter.connect_selected_notify(move |d| {
                if !guard.get() {
                    let _ = s.send(ContactsPageInput::PickBook(d.selected()));
                }
            });
        }

        model.selection.set_model(Some(&model.store));
        {
            let s = sender.input_sender().clone();
            model.selection.connect_selection_changed(move |_, _, _| {
                let _ = s.send(ContactsPageInput::SelectionChanged);
            });
        }
        widgets.list.set_model(Some(&model.selection));
        widgets.list.set_factory(Some(&row_factory(
            &model.rows,
            sender.input_sender(),
            &model.drag,
        )));

        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        let flow = match message {
            ContactsPageInput::SetLoading => self.on_set_loading(widgets, sender.clone()),
            // A read that comes in while a form is open waits in `pending`
            // until the form closes.
            ContactsPageInput::SetContacts(contacts) if self.editor.is_some() => {
                self.pending = Some(contacts);
                Flow::Continue(())
            }
            ContactsPageInput::SetContacts(contacts) => {
                self.on_set_contacts(widgets, sender.clone(), contacts)
            }
            ContactsPageInput::Query(q) if q.to_lowercase() == self.query => Flow::Continue(()),
            ContactsPageInput::Query(q) => self.on_query(widgets, sender.clone(), q),
            ContactsPageInput::SetSearch(text) => self.on_set_search(widgets, text),
            ContactsPageInput::FocusSearch => self.on_focus_search(widgets, sender.clone()),
            ContactsPageInput::CloseSearch => self.on_close_search(widgets, sender.clone()),
            ContactsPageInput::SetBookPicker(on) => self.on_set_book_picker(widgets, on),
            ContactsPageInput::SetSearchPinned(on) => self.on_set_search_pinned(on),
            ContactsPageInput::SetFilter(filter, on) => {
                self.on_set_filter(widgets, sender.clone(), filter, on)
            }
            ContactsPageInput::ClearFilters => self.on_clear_filters(widgets, sender.clone()),
            ContactsPageInput::SuggestionsLoaded(suggestions) => {
                self.on_suggestions_loaded(suggestions)
            }
            ContactsPageInput::PickBook(n) => self.on_pick_book(sender.clone(), n),
            ContactsPageInput::DirectoriesKnown(directories) => {
                self.on_directories_known(widgets, sender.clone(), directories)
            }
            ContactsPageInput::DirectoryLookup { generation, query } => {
                self.on_directory_lookup(sender.clone(), generation, query)
            }
            ContactsPageInput::DirectoryResults {
                generation,
                hits,
                failed,
            } => self.on_directory_results(widgets, sender.clone(), generation, hits, failed),
            ContactsPageInput::AddDirectoryHit(idx) => {
                self.on_add_directory_hit(sender.clone(), idx)
            }
            ContactsPageInput::SetSort(sort) => self.on_set_sort(widgets, sender.clone(), sort),
            ContactsPageInput::SetScope(scope) => self.on_set_scope(widgets, sender.clone(), scope),
            ContactsPageInput::DropContacts { keys, on, moving } => {
                self.on_drop_contacts(widgets, sender.clone(), keys, on, moving)
            }
            ContactsPageInput::Tree(action) => self.on_tree(widgets, sender.clone(), action),
            ContactsPageInput::RemoveFromGroup(people) => {
                self.on_remove_from_group(sender.clone(), people)
            }
            ContactsPageInput::SelectionChanged => {
                self.on_selection_changed(widgets, sender.clone())
            }
            ContactsPageInput::SelectFirst(n) => self.on_select_first(n),
            ContactsPageInput::BulkAddToGroup(g) => self.on_bulk_add_to_group(sender.clone(), g),
            ContactsPageInput::BulkMoveRequest(to) => {
                self.on_bulk_move_request(widgets, sender.clone(), to)
            }
            ContactsPageInput::MoveRequest { idx, to } => {
                self.on_move_request(widgets, sender.clone(), idx, to)
            }
            ContactsPageInput::EditMoveConfirmed { to, key, vcard } => {
                self.on_edit_move_confirmed(widgets, sender.clone(), to, key, vcard)
            }
            ContactsPageInput::BulkMoveConfirmed { to, keys } => {
                self.on_bulk_move_confirmed(widgets, sender.clone(), to, keys)
            }
            ContactsPageInput::WriteFailed => self.on_write_failed(),
            ContactsPageInput::BulkDeleteRequest => {
                self.on_bulk_delete_request(widgets, sender.clone())
            }
            ContactsPageInput::Compose(email) => self.on_compose(sender.clone(), email),
            ContactsPageInput::OpenUrl(url) => self.on_open_url(url),
            ContactsPageInput::Edit => self.on_edit(widgets, sender.clone()),
            ContactsPageInput::EditIndex(idx) => self.on_edit_index(widgets, sender.clone(), idx),
            ContactsPageInput::NewContact => self.on_new_contact(widgets, sender.clone()),
            ContactsPageInput::NewGroup => self.on_new_group(widgets, sender.clone()),
            ContactsPageInput::ComposeGroup(idx) => self.on_compose_group(sender.clone(), idx),
            ContactsPageInput::AddToGroup { group, person } => {
                self.on_add_to_group(sender.clone(), group, person)
            }
            ContactsPageInput::ImportContacts => self.on_import_contacts(sender.clone()),
            ContactsPageInput::CancelEdit => self.on_cancel_edit(widgets, sender.clone()),
            ContactsPageInput::SaveEdit => self.on_save_edit(widgets, sender.clone()),
            ContactsPageInput::OpenInGnome(idx) => self.on_open_in_gnome(idx),
            ContactsPageInput::DeleteSelected => self.on_delete_selected(sender.clone()),
            ContactsPageInput::DeleteRequest(idx) => {
                self.on_delete_request(widgets, sender.clone(), idx)
            }
            ContactsPageInput::SetBooks(books) => self.on_set_books(sender.clone(), books),
            ContactsPageInput::PhotosDecoded => self.on_photos_decoded(),
            ContactsPageInput::RowMenu { idx, x, y } => {
                self.on_row_menu(widgets, sender.clone(), idx, x, y)
            }
            ContactsPageInput::OpenPhoto { name, data } => {
                self.on_open_photo(sender.clone(), name, data)
            }
            ContactsPageInput::DeleteConfirmed(keys) => {
                self.on_delete_confirmed(widgets, sender.clone(), keys)
            }
        };
        if flow.is_continue() {
            // Overriding `update_with_view` replaces the generated body that ran
            // `update_view` after every message; without this the `#[watch]`
            // bindings above (the "Loading Contacts…" / "No Contacts" face) only
            // ever hold their `init` values, and an empty address book looked
            // like a load that never finished.
            self.update_view(widgets, sender);
        }
    }
}
