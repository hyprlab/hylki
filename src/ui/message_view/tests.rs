//! The reader's tests, by concern. `msg_for_print` is the shared fixture.

use super::*;
use super::{
    dark::*, frame::*, html::*, invite::*, links::*, menu::*, print::*, remote::*, snippets::*,
    style::*,
};

mod characterization;
mod dark_style;
mod document;
mod print_links;
mod scan_perf;
mod snippets;
mod wrapper;

pub(super) fn msg_for_print() -> Message {
    Message {
        id: 1,
        account_id: 1,
        folder_id: 1,
        uid: 1,
        from_name: "Ada Lovelace".into(),
        from_addr: "ada@example.com".into(),
        reply_to: String::new(),
        reply_to_name: String::new(),
        to: "me@example.com".into(),
        cc: "carol@example.com".into(),
        bcc: String::new(),
        subject: "Quarterly numbers".into(),
        preview: String::new(),
        body: "<p>hi</p>".into(),
        date: "09:14".into(),
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
