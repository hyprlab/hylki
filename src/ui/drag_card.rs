//! What a dragged message looks like under the pointer, and what it carries:
//! shared by the message list's rows and the reader's cards, so a message
//! dragged from either looks the same and lands on the same folders.

use crate::i18n::i18n;
use adw::prelude::*;

/// How many cards a dragged selection draws at most; the count says the rest.
pub(crate) const DRAG_STACK: usize = 5;

/// How far each card behind sits from the one in front, down and right.
const DRAG_STACK_STEP: i32 = 5;

/// The width of every card in a dragged selection's stack.
const DRAG_CARD_WIDTH: i32 = 300;

/// One message as a drag card: its circle, the subject over who it is from
/// (or to).
pub(crate) fn card(
    subject: &str,
    name: &str,
    email: &str,
    image: Option<&gtk::gdk::Paintable>,
) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.add_css_class("message-drag-card");
    let avatar = adw::Avatar::new(34, Some(name), true);
    let initials = crate::ui::initials::InitialsPaintable::for_name(name);
    avatar.set_custom_image(image.or(initials.as_ref().map(|p| p.upcast_ref())));
    avatar.set_valign(gtk::Align::Center);
    card.append(&avatar);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.set_valign(gtk::Align::Center);
    text.set_hexpand(true);
    let subject = if subject.trim().is_empty() {
        i18n("(no subject)")
    } else {
        subject.to_string()
    };
    let title = gtk::Label::new(Some(&subject));
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(40);
    let who = match (name.trim(), email.trim()) {
        (n, e) if n.is_empty() || n.eq_ignore_ascii_case(e) => e.to_string(),
        (n, "") => n.to_string(),
        (n, e) => format!("{n} <{e}>"),
    };
    let subtitle = gtk::Label::new(Some(&who));
    subtitle.add_css_class("dim-label");
    subtitle.add_css_class("caption");
    subtitle.set_xalign(0.0);
    subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subtitle.set_max_width_chars(40);
    text.append(&title);
    text.append(&subtitle);
    card.append(&text);
    card
}

/// A dragged selection: the front card wearing the full `count`, and up to
/// [`DRAG_STACK`] - 1 of the others stacked behind it.
pub(crate) fn stack(front: gtk::Box, behind: Vec<gtk::Box>, count: usize) -> gtk::Widget {
    let badge = gtk::Label::new(Some(&count.to_string()));
    badge.add_css_class("message-drag-count");
    badge.set_valign(gtk::Align::Center);
    front.append(&badge);
    // One width for every card, so the edges behind run parallel.
    let fixed = gtk::Fixed::new();
    for (depth, card) in behind.iter().take(DRAG_STACK - 1).enumerate().rev() {
        card.set_size_request(DRAG_CARD_WIDTH, -1);
        let at = ((depth + 1) as i32 * DRAG_STACK_STEP) as f64;
        fixed.put(card, at, at);
    }
    front.set_size_request(DRAG_CARD_WIDTH, -1);
    fixed.put(&front, 0.0, 0.0);
    fixed.upcast()
}

/// The payload a sidebar folder takes as a move: the marker "vireo-move" and
/// one (account, folder, uid, id) group per message (see
/// `sidebar::dnd::parse_move_payload`).
pub(crate) fn move_payload(items: &[(u32, u32, u32, u32)]) -> gtk::gdk::ContentProvider {
    let mut payload = String::from("vireo-move");
    for (a, f, u, i) in items {
        payload.push_str(&format!("\t{a}\t{f}\t{u}\t{i}"));
    }
    gtk::gdk::ContentProvider::for_value(&payload.to_value())
}
