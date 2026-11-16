//! Moving contacts to another address book from a dialog: the books they
//! can go to, one of them picked, and the move confirmed in the same step.
//! It reaches every book from the list's menus, so a move needs neither the
//! sidebar's address books to drag onto nor a drag.

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::{i18n, i18n_f, ni18n_f};

use super::rows::split_book_name;
use super::types::{ContactsPage, ContactsPageInput};
use super::{ContactsPageWidgets, Flow};

/// More books than this scroll in the dialog.
const SCROLL_AFTER: usize = 6;

/// A book the contacts can go to, and those of them that can.
struct Target {
    uid: String,
    name: String,
    keys: Vec<(String, String)>,
}

impl ContactsPage {
    /// Whether any of the people at `people` can move to another book.
    pub(super) fn can_move_any(&self, people: &[usize]) -> bool {
        !self.move_targets(people).is_empty()
    }

    /// The books the people at `people` can move to, in the order of the
    /// book list (the default first), each with the people that can.
    fn move_targets(&self, people: &[usize]) -> Vec<Target> {
        self.books
            .iter()
            .filter_map(|b| {
                let keys: Vec<(String, String)> = people
                    .iter()
                    .filter_map(|&i| self.contacts.get(i))
                    .filter(|c| self.can_move(c, &b.uid))
                    .map(|c| (c.book_uid.clone(), c.eds_uid.clone()))
                    .collect();
                (!keys.is_empty()).then(|| Target {
                    uid: b.uid.clone(),
                    name: b.name.clone(),
                    keys,
                })
            })
            .collect()
    }

    /// Ask which book the people at `people` go to, and move them there
    /// once one is picked and Move pressed.
    #[allow(deprecated)] // adw::MessageDialog, as the page's other questions
    pub(super) fn on_move_dialog(
        &mut self,
        widgets: &mut ContactsPageWidgets,
        sender: ComponentSender<Self>,
        people: Vec<usize>,
    ) -> Flow {
        let targets = self.move_targets(&people);
        if targets.is_empty() {
            return Flow::Break(());
        }
        let persons: Vec<_> = people
            .iter()
            .filter_map(|&i| self.contacts.get(i))
            .filter(|c| !c.is_group())
            .collect();
        let body = match persons.as_slice() {
            [c] => i18n_f(
                "{name} leaves {from}, locally and on the server, and any group there that holds the contact.",
                &[("name", &c.name), ("from", &split_book_name(&c.book_name, false).2)],
            ),
            _ => {
                let n = persons.len() as u32;
                ni18n_f(
                    "{n} contact leaves its address book, locally and on the server, and any group there that holds it.",
                    "{n} contacts leave their address books, locally and on the server, and any group there that holds them.",
                    n,
                    &[("n", &n.to_string())],
                )
            }
        };
        let win = widgets.page_stack.root().and_downcast::<gtk::Window>();
        let dialog = adw::MessageDialog::new(
            win.as_ref(),
            Some(&i18n("Move to Address Book")),
            Some(&body),
        );

        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        let picked = std::rc::Rc::new(std::cell::Cell::new(None::<usize>));
        let mut first: Option<gtk::CheckButton> = None;
        for (i, t) in targets.iter().enumerate() {
            let (_, account, own) = split_book_name(&t.name, false);
            let row = adw::ActionRow::new();
            row.set_title(&gtk::glib::markup_escape_text(&own));
            if !account.is_empty() && account != own {
                row.set_subtitle(&gtk::glib::markup_escape_text(&account));
            }
            let check = gtk::CheckButton::new();
            check.set_valign(gtk::Align::Center);
            match &first {
                Some(f) => check.set_group(Some(f)),
                None => first = Some(check.clone()),
            }
            let (picked, d) = (picked.clone(), dialog.clone());
            check.connect_toggled(move |c| {
                if c.is_active() {
                    picked.set(Some(i));
                    d.set_response_enabled("move", true);
                }
            });
            row.add_prefix(&check);
            row.set_activatable_widget(Some(&check));
            // The radio takes the keyboard; a focused row drew its ring
            // across the list as the dialog opened.
            row.set_focusable(false);
            list.append(&row);
        }
        // A scroller clips the list's shadow, so only a long list has one,
        // with room around the list for the shadow inside it.
        if targets.len() > SCROLL_AFTER {
            list.set_margin_top(6);
            list.set_margin_bottom(6);
            list.set_margin_start(6);
            list.set_margin_end(6);
            let scroll = gtk::ScrolledWindow::new();
            scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
            scroll.set_propagate_natural_height(true);
            scroll.set_max_content_height(360);
            scroll.set_child(Some(&list));
            dialog.set_extra_child(Some(&scroll));
        } else {
            dialog.set_extra_child(Some(&list));
        }

        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("move", &i18n("Move"));
        dialog.set_response_appearance("move", adw::ResponseAppearance::Suggested);
        // Nothing is picked for the user: Move waits for a book.
        dialog.set_response_enabled("move", false);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let s = sender.input_sender().clone();
        dialog.connect_response(None, move |_, resp| {
            if resp != "move" {
                return;
            }
            let Some(t) = picked.get().and_then(|i| targets.get(i)) else {
                return;
            };
            let _ = s.send(ContactsPageInput::BulkMoveConfirmed {
                to: t.uid.clone(),
                keys: t.keys.clone(),
            });
        });
        dialog.present();
        // Opened from a menu, nothing in it has the keyboard yet.
        gtk::prelude::GtkWindowExt::set_focus(&dialog, None::<&gtk::Widget>);
        Flow::Continue(())
    }
}
