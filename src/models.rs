//! Core domain types shared across the UI and backend layers. Each kind of
//! thing has its own child module; every item is re-exported here, so
//! callers name them as `models::Item`.

mod attachments;
mod folders;
mod message;
mod outbox;
mod receipt_marks;
mod verdicts;

pub(crate) use attachments::*;
pub(crate) use folders::*;
pub(crate) use message::*;
pub(crate) use outbox::*;
pub(crate) use receipt_marks::*;
pub(crate) use verdicts::*;

#[cfg(test)]
pub(crate) mod test_util;
#[cfg(test)]
mod tests;

/// A configured mail account (one IMAP/SMTP identity).
#[derive(Debug, Clone)]
pub(crate) struct Account {
    pub(crate) id: u32,
    pub(crate) name: String,
    pub(crate) email: String,
    /// How the account is labelled in the UI (All Inboxes, reader chip). Defaults
    /// to the email address.
    pub(crate) label: String,
    /// Accent color used for the account dot, as a CSS color string.
    pub(crate) accent: String,
}
