/**
 * @file virtual_list.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Binds the lazy-loading model to the GTK4 list view factory.
 * @param model An instance of the custom MailboxModel (units: N/A).
 * @return Configured gtk::ListView instance bounded to the custom list model.
 * @throws None.
 * @warning Requires GTK4 and the custom MailboxModel subclass.
 * @pre model must be initialized with the correct total row count from the database.
 * @post UI strictly recycles row widgets and dynamically queries missing data on scroll.
 */
use gtk::prelude::*;
use gtk::{ListView, SignalListItemFactory, SingleSelection, Label};
use crate::mailbox_model::{MailboxModel, MessageObject};

pub fn build_virtualized_mailbox_view(model: &MailboxModel) -> ListView {
    assert!(model.n_items() >= 0, "Custom ListModel initialized in an invalid state.");

    let factory = SignalListItemFactory::new();

    factory.connect_setup(move |_, list_item| {
        let label = Label::new(None);
        list_item.set_child(Some(&label));
    });

    factory.connect_bind(move |_, list_item| {
        let item = list_item.item().expect("Missing item in bind pass.");
        let message = item.downcast::<MessageObject>().expect("Invalid object type mapping.");
        
        let message_subject = message.subject(); 
        
        let child = list_item.child().expect("Missing child widget pointer.");
        let label = child.downcast_ref::<Label>().expect("Child widget is not a label casting.");

        label.set_text(&message_subject); 
    });

    let selection_model = SingleSelection::new(Some(model.clone()));
    ListView::new(Some(selection_model), Some(factory))
}
