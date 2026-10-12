/**
 * @file mailbox_model.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Predictive lazy-loading gio::ListModel with bounded 1,000-row LRU RamCache.
 * @param None directly; operates as a GObject subclass.
 * @return A glib::Object implementing gio::ListModel with two-way lookahead.
 * @throws None.
 * @warning Bounds resident memory strictly to 1,000 message items.
 * @pre Database initialized and Tokio async bridge active.
 * @post Viewport lookahead ensures rows are in RAM before scrolling completes.
 */

use gtk::gio;
use gtk::gio::subclass::prelude::*;
use gtk::glib;
use gtk::glib::prelude::*;
use gtk::glib::subclass::prelude::*;
use gtk::glib::{Object, Properties};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;

// ─── GObject wrapper for Message item ────────────────────────────────────────

glib::wrapper! {
    pub struct MessageObject(ObjectSubclass<message_imp::MessageObject>);
}

mod message_imp {
    use super::*;

    #[derive(Default, Properties)]
    #[properties(wrapper_type = super::MessageObject)]
    pub struct MessageObject {
        #[property(get, set)]
        pub id: Cell<u32>,
        #[property(get, set)]
        pub account_id: Cell<u32>,
        #[property(get, set)]
        pub folder_id: Cell<u32>,
        #[property(get, set)]
        pub uid: Cell<u32>,
        #[property(get, set)]
        pub from_name: RefCell<String>,
        #[property(get, set)]
        pub subject: RefCell<String>,
        #[property(get, set)]
        pub preview: RefCell<String>,
        #[property(get, set)]
        pub date: RefCell<String>,
        #[property(get, set)]
        pub unread: Cell<bool>,
        #[property(get, set)]
        pub folder_path: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MessageObject {
        const NAME: &'static str = "HylkiZeroLatencyMessageObject";
        type Type = super::MessageObject;
    }
    impl ObjectImpl for MessageObject {}
}

/// Weigh implementation for Hylki's RamCache: each message object weighs 1 unit
impl crate::ram_cache::Weigh for MessageObject {
    fn weight(&self) -> usize {
        1
    }
}

// ─── MailboxModel GObject Subclass ──────────────────────────────────────────

glib::wrapper! {
    pub struct MailboxModel(ObjectSubclass<model_imp::MailboxModel>)
        @implements gio::ListModel;
}

mod model_imp {
    use super::*;

    pub struct MailboxModel {
        // Target 3: Capped sliding window of 1,000 active rows using Hylki's RamCache
        pub cache: RefCell<crate::ram_cache::RamCache<u32, super::MessageObject>>,
        pub total_count: RefCell<u32>,
        pub account_id: Cell<u32>,
        pub folder_id: Cell<u32>,
        pub folder_path: RefCell<String>,
        pub inflight: RefCell<HashSet<u32>>,
    }

    impl Default for MailboxModel {
        fn default() -> Self {
            Self {
                cache: RefCell::new(crate::ram_cache::RamCache::new(1000)),
                total_count: RefCell::new(0),
                account_id: Cell::new(0),
                folder_id: Cell::new(0),
                folder_path: RefCell::new(String::new()),
                inflight: RefCell::new(HashSet::new()),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MailboxModel {
        const NAME: &'static str = "HylkiZeroLatencyMailboxModel";
        type Type = super::MailboxModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for MailboxModel {}

    impl ListModelImpl for MailboxModel {
        fn item_type(&self) -> glib::Type {
            super::MessageObject::static_type()
        }

        fn n_items(&self) -> u32 {
            *self.total_count.borrow()
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            let total = *self.total_count.borrow();
            if position >= total {
                return None;
            }

            let cached_hit = self.cache.borrow().get(&position).cloned();

            // Target 2: Predictive Viewport Lookahead (X - 50 through X + 50)
            let lookahead_start = position.saturating_sub(50);
            let lookahead_end = (position + 50).min(total);

            let chunk_start = (lookahead_start / 50) * 50;
            let chunk_end = ((lookahead_end + 49) / 50) * 50;

            let mut chunks_to_fetch = Vec::new();
            {
                let cache = self.cache.borrow();
                let mut inflight = self.inflight.borrow_mut();

                let mut offset = chunk_start;
                while offset < chunk_end && offset < total {
                    if !inflight.contains(&offset) {
                        let chunk_len = 50.min(total - offset);
                        let missing = (offset..offset + chunk_len).any(|i| !cache.contains_key(&i));
                        if missing {
                            inflight.insert(offset);
                            chunks_to_fetch.push(offset);
                        }
                    }
                    offset += 50;
                }
            }

            // Proactively trigger async Tokio prefetch tasks
            if !chunks_to_fetch.is_empty() {
                let account_id = self.account_id.get();
                let folder_id = self.folder_id.get();
                let folder_path = self.folder_path.borrow().clone();
                let model = self.obj();

                for offset in chunks_to_fetch {
                    crate::async_bridge::dispatch_prefetch(
                        account_id,
                        folder_path.clone(),
                        folder_id,
                        offset,
                        50,
                        model.clone(),
                    );
                }
            }

            if let Some(msg) = cached_hit {
                Some(msg.upcast())
            } else {
                // Return immediate placeholder while background prefetch lands
                let placeholder = glib::Object::builder::<super::MessageObject>()
                    .property("subject", "Loading data...")
                    .property("from-name", "")
                    .property("preview", "")
                    .property("date", "")
                    .property("unread", false)
                    .build();
                Some(placeholder.upcast())
            }
        }
    }
}

impl MailboxModel {
    pub fn new(account_id: u32, folder_id: u32, folder_path: String, total_count: u32) -> Self {
        let model: Self = glib::Object::builder().build();
        model.imp().account_id.set(account_id);
        model.imp().folder_id.set(folder_id);
        *model.imp().folder_path.borrow_mut() = folder_path;
        *model.imp().total_count.borrow_mut() = total_count;
        model
    }

    /// Receives fetched chunk from Tokio worker, updates RamCache, and signals GTK
    pub fn receive_page(&self, start: u32, items: Vec<MessageObject>) {
        let count = items.len() as u32;
        {
            let mut cache = self.imp().cache.borrow_mut();
            let mut inflight = self.imp().inflight.borrow_mut();
            inflight.remove(&start);

            for (idx, item) in items.into_iter().enumerate() {
                // RamCache enforces sliding window of 1,000 rows automatically
                cache.insert(start + idx as u32, item);
            }
        }
        // Seamlessly rebind visible row widgets without flicker!
        self.items_changed(start, count, count);
    }
}
