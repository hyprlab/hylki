//! A change to mail an assistant asks for, checked against what Settings
//! allows before it goes to the app, which makes it the way its own menus do:
//! on screen at once, on the server after, and in the Undo history.

use std::collections::HashMap;

use crate::models::Message;

/// One message to change.
#[derive(Debug)]
pub(crate) struct Target {
    /// The id the assistant named it by, for the answer.
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) message: Message,
}

/// What to do to the targets.
#[derive(Debug)]
pub(crate) enum Change {
    Read(bool),
    Star(bool),
    Tag {
        keyword: String,
        add: bool,
    },
    /// To this folder of each account, by account id.
    MoveTo(HashMap<u32, String>),
    Archive,
    Trash,
}

/// What came of it.
#[derive(Debug, Default)]
pub(crate) struct Organized {
    /// The ids changed.
    pub(crate) changed: Vec<String>,
    /// The ids left alone, each with why.
    pub(crate) skipped: Vec<(String, String)>,
}
