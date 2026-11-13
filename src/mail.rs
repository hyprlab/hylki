//! Message formats and the text of mail: what a message is written in, carries
//! or says about itself, read and written without any GTK widget.

pub(crate) mod at_link;
pub(crate) mod attachment_reminder;
pub(crate) mod attribution;
pub(crate) mod direction;
pub(crate) mod flowed;
pub(crate) mod invite;
pub(crate) mod link_clean;
mod mail_header;
pub(crate) mod markdown;
pub(crate) mod merge;
pub(crate) mod message_file;
pub(crate) mod mutf7;
pub(crate) mod outlook_msg;
pub(crate) mod pgp;
pub(crate) mod quote;
pub(crate) mod reader;
pub(crate) mod receipt;
pub(crate) mod sensitivity;
pub(crate) mod unsubscribe;
pub(crate) mod verify;
