//! Outgoing mail: recipient parsing, building the message, OpenPGP/MIME, and
//! taking queued mail apart again for editing.

pub(super) mod build;
pub(super) mod editable;
mod pgp_mime;
pub(super) mod recipients;
mod report;
