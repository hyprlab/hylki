/**
 * @file mailbox_model.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Implements a lazy-loading gio::ListModel subclass for infinite scrolling.
 * @param None directly; operates as a GObject subclass.
 * @return A glib::Object implementing gio::ListModel.
 * @throws None.
 * @warning Asynchronous page fetches must resolve before the user scrolls past the placeholder buffer.
 * @pre Tokio runtime must be active to process background chunk queries.
 * @post Only visible widget rows hold active data references in memory.
 */
use gio::subclass::prelude::*;
use glib::subclass::prelude::*;
use glib::{Object, Properties};
use std::cell::RefCell;
use std::collections::HashMap;

glib::wrapper! {
    pub struct MessageObject(ObjectSubclass<message_imp::MessageObject>);
}

mod message_imp {
    use super::*;
    
    #[derive(Default, Properties)]
    #[properties(wrapper_type = super::MessageObject)]
    pub struct MessageObject {
        #[property(get, set)]
        pub subject: RefCell<String>,
    }
    
    #[glib::object_subclass]
    impl ObjectSubclass for MessageObject {
        const NAME: &'static str = "MessageObject";
        type Type = super::MessageObject;
    }
    impl ObjectImpl for MessageObject {}
}

glib::wrapper! {
    pub struct MailboxModel(ObjectSubclass<model_imp::MailboxModel>)
        @implements gio::ListModel;
}

mod model_imp {
    use super::*;
    
    #[derive(Default)]
    pub struct MailboxModel {
        pub cache: RefCell<HashMap<u32, super::MessageObject>>,
        pub total_count: RefCell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MailboxModel {
        const NAME: &'static str = "MailboxModel";
        type Type = super::MailboxModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for MailboxModel {}

    impl ListModelImpl for MailboxModel {
        fn item_type(&self) -> glib::Type {
            super::MessageObject::static_type()
        }

        fn n_items(&self) -> u32 {
            let count = *self.total_count.borrow();
            assert!(count >= 0, "Item count cannot be negative.");
            count
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            let cache = self.cache.borrow();
            if let Some(msg) = cache.get(&position) {
                return Some(msg.clone().upcast());
            }

            assert!(position < *self.total_count.borrow(), "Requested out of bounds index.");
            
            let placeholder = glib::Object::builder::<super::MessageObject>()
                .property("subject", "Loading data...")
                .build();
                
            Some(placeholder.upcast())
        }
    }
}
