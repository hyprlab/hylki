//! Test fixtures for the shared data types, used by the tests of every
//! layer above them.

use super::Message;

/// A message with only its ids set, every other field empty.
pub(crate) fn msg(account_id: u32, folder_id: u32, uid: u32) -> Message {
    Message {
        id: uid,
        account_id,
        folder_id,
        uid,
        from_name: String::new(),
        from_addr: String::new(),
        reply_to: String::new(),
        reply_to_name: String::new(),
        to: String::new(),
        cc: String::new(),
        bcc: String::new(),
        subject: String::new(),
        preview: String::new(),
        body: String::new(),
        date: String::new(),
        timestamp: 0,
        unread: false,
        starred: false,
        answered: false,
        forwarded: false,
        keywords: Vec::new(),
        has_attachment: false,
        message_id: String::new(),
        references: String::new(),
        importance: Default::default(),
        due: 0,
        labels: Vec::new(),
    }
}
