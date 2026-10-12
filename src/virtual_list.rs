/**
 * @file virtual_list.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief High-performance virtualized GTK4 ListView with speculative body prefetching on hover.
 * @param model An instance of the custom MailboxModel (units: N/A).
 * @return Configured gtk::ListView instance bounded to the custom list model.
 * @throws None.
 * @warning Requires GTK4 and custom MailboxModel with RamCache.
 * @pre model must be initialized with the active mailbox configuration.
 * @post Rows recycle dynamically and hover events >300ms speculatively warm email bodies.
 */

use crate::mailbox_model::{MailboxModel, MessageObject};
use gtk::glib;
use gtk::glib::prelude::*;
use gtk::prelude::*;
use gtk::{Label, ListView, SignalListItemFactory, SingleSelection};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

/// Holds widget handles inside a recycled list item for fast direct binding
struct RowWidgets {
    root: gtk::Box,
    unread_dot: gtk::Box,
    sender: Label,
    timestamp: Label,
    subject: Label,
    snippet: Label,
    current_item: RefCell<Option<MessageObject>>,
}

pub fn build_virtualized_mailbox_view(model: &MailboxModel) -> ListView {
    let factory = SignalListItemFactory::new();
    let rows: Rc<RefCell<HashMap<usize, Rc<RowWidgets>>>> = Rc::new(RefCell::new(HashMap::new()));
    let key = |obj: &glib::Object| obj.as_ptr() as usize;

    // ─── Setup Phase: Instantiate Widget Tree Once Per Visible Slot ──────────
    {
        let rows = rows.clone();
        factory.connect_setup(move |_, obj| {
            let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else { return; };

            let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            root.add_css_class("message-row");

            let unread_dot = gtk::Box::new(gtk::Orientation::Vertical, 0);
            unread_dot.add_css_class("unread-indicator");

            let text_vbox = gtk::Box::new(gtk::Orientation::Vertical, 2);
            text_vbox.set_hexpand(true);

            let top_hbox = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let sender = Label::new(None);
            sender.add_css_class("sender");
            sender.set_halign(gtk::Align::Start);
            sender.set_hexpand(true);
            sender.set_ellipsize(gtk::pango::EllipsizeMode::End);

            let timestamp = Label::new(None);
            timestamp.add_css_class("timestamp");
            timestamp.set_halign(gtk::Align::End);

            top_hbox.append(&sender);
            top_hbox.append(&timestamp);

            let subject = Label::new(None);
            subject.add_css_class("subject");
            subject.set_halign(gtk::Align::Start);
            subject.set_ellipsize(gtk::pango::EllipsizeMode::End);

            let snippet = Label::new(None);
            snippet.add_css_class("dim-label");
            snippet.set_halign(gtk::Align::Start);
            snippet.set_ellipsize(gtk::pango::EllipsizeMode::End);

            text_vbox.append(&top_hbox);
            text_vbox.append(&subject);
            text_vbox.append(&snippet);

            root.append(&unread_dot);
            root.append(&text_vbox);

            let widgets = Rc::new(RowWidgets {
                root: root.clone(),
                unread_dot,
                sender,
                timestamp,
                subject,
                snippet,
                current_item: RefCell::new(None),
            });

            // ─── Target 4: Speculative Body Prefetch on Row Hover / Focus (>300ms) ─
            let hover_timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));

            let motion = gtk::EventControllerMotion::new();
            {
                let timer_rc = hover_timer.clone();
                let w_rc = widgets.clone();
                motion.connect_enter(move |_, _, _| {
                    if let Some(source) = timer_rc.borrow_mut().take() {
                        source.remove();
                    }
                    let w = w_rc.clone();
                    let t = timer_rc.clone();
                    let source_id = glib::timeout_add_local_once(Duration::from_millis(300), move || {
                        t.borrow_mut().take();
                        if let Some(msg) = w.current_item.borrow().as_ref() {
                            crate::async_bridge::speculative_body_prefetch(
                                msg.account_id(),
                                msg.folder_path(),
                                msg.uid(),
                            );
                        }
                    });
                    *timer_rc.borrow_mut() = Some(source_id);
                });
            }

            {
                let timer_rc = hover_timer.clone();
                motion.connect_leave(move |_| {
                    if let Some(source) = timer_rc.borrow_mut().take() {
                        source.remove();
                    }
                });
            }

            root.add_controller(motion);

            let focus = gtk::EventControllerFocus::new();
            {
                let timer_rc = hover_timer.clone();
                let w_rc = widgets.clone();
                focus.connect_enter(move |_| {
                    if let Some(source) = timer_rc.borrow_mut().take() {
                        source.remove();
                    }
                    let w = w_rc.clone();
                    let t = timer_rc.clone();
                    let source_id = glib::timeout_add_local_once(Duration::from_millis(300), move || {
                        t.borrow_mut().take();
                        if let Some(msg) = w.current_item.borrow().as_ref() {
                            crate::async_bridge::speculative_body_prefetch(
                                msg.account_id(),
                                msg.folder_path(),
                                msg.uid(),
                            );
                        }
                    });
                    *timer_rc.borrow_mut() = Some(source_id);
                });
            }
            {
                let timer_rc = hover_timer;
                focus.connect_leave(move |_| {
                    if let Some(source) = timer_rc.borrow_mut().take() {
                        source.remove();
                    }
                });
            }

            root.add_controller(focus);

            list_item.set_child(Some(&root));
            rows.borrow_mut().insert(key(obj), widgets);
        });
    }

    // ─── Bind Phase: Target 5 Strict Data Mapping to Existing Widgets ────────
    {
        let rows = rows.clone();
        factory.connect_bind(move |_, obj| {
            let Some(list_item) = obj.downcast_ref::<gtk::ListItem>() else { return; };
            let Some(widgets) = rows.borrow().get(&key(obj)).cloned() else { return; };
            let Some(msg) = list_item.item().and_downcast::<MessageObject>() else { return; };

            *widgets.current_item.borrow_mut() = Some(msg.clone());

            widgets.sender.set_text(&msg.from_name());
            widgets.timestamp.set_text(&msg.date());
            widgets.subject.set_text(&msg.subject());
            widgets.snippet.set_text(&msg.preview());

            if msg.unread() {
                widgets.unread_dot.set_visible(true);
                widgets.unread_dot.add_css_class("unread");
            } else {
                widgets.unread_dot.set_visible(false);
                widgets.unread_dot.remove_css_class("unread");
            }
        });
    }

    // ─── Teardown Phase: Clean Up Dropped Row Slots ──────────────────────────
    {
        let rows = rows;
        factory.connect_teardown(move |_, obj| {
            rows.borrow_mut().remove(&key(obj));
        });
    }

    let selection_model = SingleSelection::new(Some(model.clone()));
    ListView::new(Some(selection_model), Some(factory))
}
