//! Message formats and the text of mail: what a message is written in, carries
//! or says about itself, read and written without any GTK widget.

pub(crate) mod attribution;
pub(crate) mod direction;
pub(crate) mod invite;
mod mail_header;
pub(crate) mod markdown;
pub(crate) mod mutf7;
pub(crate) mod outlook_msg;
pub(crate) mod pgp;
pub(crate) mod quote;
pub(crate) mod reader;
pub(crate) mod sensitivity;
pub(crate) mod unsubscribe;
pub(crate) mod verify;
