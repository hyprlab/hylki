//! A message popped out into its own top-level window: a standalone reader with
//! the main window's reader toolbar, in the layout Settings gives it, and the
//! same attachment drawer and lightbox.
//!
//! The window owns its own [`MessageView`] and attachment state, but defers the
//! real work (reply, move, load attachments, …) back to the app via outputs so
//! behaviour stays identical to the main window.

use self::toolbar::Toolbar;

use relm4::prelude::*;

use crate::config::{ReaderToolbar, Tag};
use crate::models::{Attachment, Message};
use crate::ui::attachment_drawer::AttachmentDrawer;
use crate::ui::lightbox::Lightbox;
use crate::ui::message_view::MessageView;

mod component;
mod content;
mod keys;
mod on_inputs;
mod toolbar;
mod types;

pub(crate) use types::{MessageWindowInit, MessageWindowInput, MessageWindowOutput};

pub(crate) struct MessageWindow {
    msg: Message,
    /// What the reader renders: the whole conversation, or just `[msg]`.
    thread: Vec<Message>,
    view: Controller<MessageView>,
    account_name: Option<String>,
    account_color: Option<String>,
    allow_remote: bool,
    loading: bool,
    /// Each member's files, in the order they arrived.
    attachments: Vec<((u32, u32), Vec<Attachment>)>,
    /// The members whose files are on their way.
    attachments_loading: std::collections::HashSet<(u32, u32)>,
    drawer_enabled: bool,
    drawer: Controller<AttachmentDrawer>,
    lightbox: Controller<Lightbox>,
    tags: Vec<Tag>,
    toolbar: ReaderToolbar,
    in_junk: bool,
    restorable: bool,
    snooze: Option<bool>,
    /// The right group is folded into the ⋯ menu: the window is too narrow
    /// for the whole row.
    collapsed: bool,
    /// The right group stays folded at every width (Settings).
    fold_right: bool,
    /// The window and what fills it, for keeping the window's minimum
    /// width at its content's (see [`MessageWindow::fit_min_width`]).
    window: adw::Window,
    content: gtk::Overlay,
    bar: Toolbar,
    gmail_keys: std::rc::Rc<std::cell::Cell<bool>>,
}

impl MessageWindow {}
