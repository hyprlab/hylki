//! Building one row's widgets, once, for the factory to bind over and over.

use super::row::RowWidgets;
use super::{SwipeSurface, ACCOUNT_COLUMN_PX, PEOPLE_COLUMN_PX, SENDER_COLUMN_PX, TAGS_COLUMN_PX};
use crate::i18n::i18n;
use crate::ui::column_bin::ColumnBin;
use adw::prelude::*;
use relm4::prelude::*;

/// The row's widget tree. The host is what the view wraps in its item
/// widget; everything else hangs off it.
pub(super) fn build_widgets() -> RowWidgets {
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.add_css_class("message-item");

    // Replies slide open and shut instead of the list jumping to a new row
    // count the instant a conversation toggles. (PR #79)
    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
    revealer.set_transition_duration(200);
    revealer.set_reveal_child(true);
    host.append(&revealer);

    // The swipe surface: the fixed action strip underneath, the row's real
    // content on top, slid away from it under a drag (#swipe).
    let surface = SwipeSurface::default();
    revealer.set_child(Some(&surface));

    let swipe_bg = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    swipe_bg.set_overflow(gtk::Overflow::Hidden);
    let swipe_inner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    swipe_inner.set_valign(gtk::Align::Center);
    swipe_inner.set_hexpand(true);
    swipe_inner.set_margin_start(16);
    swipe_inner.set_margin_end(16);
    let swipe_icon = gtk::Image::new();
    let swipe_label = gtk::Label::new(None);
    swipe_inner.append(&swipe_icon);
    swipe_inner.append(&swipe_label);
    swipe_bg.append(&swipe_inner);
    surface.set_background(&swipe_bg);

    let overlay = gtk::Overlay::new();
    surface.set_foreground(&overlay);

    // The content: avatar, unread dot and the three lines of text.
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    content.add_css_class("message-row");
    // Anything wider than the row is clipped here rather than painted
    // across the divider into the reader.
    content.set_overflow(gtk::Overflow::Hidden);
    overlay.set_child(Some(&content));

    // The circle sits in a revealer so Focus Mode can slide it away (and
    // back). SlideRight: folding, the circle moves off past the left edge.
    let avatar_revealer = gtk::Revealer::new();
    avatar_revealer.set_transition_type(gtk::RevealerTransitionType::SlideRight);
    avatar_revealer.set_transition_duration(crate::ui::FOCUS_ANIM_MS);
    avatar_revealer.add_css_class("focus-fade");
    avatar_revealer.set_reveal_child(true);
    let avatar = adw::Avatar::new(38, None, true);
    avatar.set_valign(gtk::Align::Center);
    avatar_revealer.set_child(Some(&avatar));
    content.append(&avatar_revealer);

    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("unread-dot");
    content.append(&dot);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    content.append(&text);

    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let name = gtk::Label::new(None);
    name.set_halign(gtk::Align::Start);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("message-sender");
    top.append(&name);
    let answer = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let replied = gtk::Image::from_icon_name("mail-reply-sender-symbolic");
    let forwarded = gtk::Image::from_icon_name("mail-forward-symbolic");
    for arrow in [&replied, &forwarded] {
        arrow.add_css_class("dim-icon");
        answer.append(arrow);
    }
    top.append(&answer);
    let clip = gtk::Image::from_icon_name("mail-attachment-symbolic");
    clip.add_css_class("dim-icon");
    top.append(&clip);
    // How important the sender marked it (#563), and a column of its own on
    // one line (#334).
    let importance = gtk::Image::from_icon_name("emblem-important-symbolic");
    importance.set_visible(false);
    top.append(&importance);
    let star = gtk::Image::from_icon_name("starred-symbolic");
    star.add_css_class("star-icon");
    top.append(&star);
    // Sent mail's read receipt (#520), beside the date it was sent.
    let receipt = gtk::Image::from_icon_name("hylki-read-receipt-symbolic");
    receipt.set_visible(false);
    top.append(&receipt);
    let date = gtk::Label::new(None);
    date.set_halign(gtk::Align::End);
    // Ellipsized so it stops being the row's floor: it is the one item on
    // this line with no give (#29).
    date.set_ellipsize(gtk::pango::EllipsizeMode::End);
    date.add_css_class("message-date");
    top.append(&date);
    // The conversation chip (thread heads only): the count and the
    // expand/collapse caret merged into one grey pill.
    let chip = gtk::Button::new();
    chip.set_tooltip_text(Some(i18n("Show conversation").as_str()));
    chip.add_css_class("flat");
    chip.add_css_class("thread-chip");
    chip.set_valign(gtk::Align::Center);
    let chip_box = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    // Centred: with the caret hidden the bare count must not sit against
    // the chip's left edge.
    chip_box.set_halign(gtk::Align::Center);
    let chip_count = gtk::Label::new(None);
    chip_box.append(&chip_count);
    // One caret; the "open" class rotates it via a CSS transition. (PR #79)
    let chip_caret = gtk::Image::from_icon_name("pan-end-symbolic");
    chip_caret.add_css_class("thread-toggle-icon");
    chip_box.append(&chip_caret);
    chip.set_child(Some(&chip_box));
    top.append(&chip);
    text.append(&top);

    // The subject and the tag chips at its end, where a long subject gives
    // way before the sender's name would. Hidden whole by Focus Mode.
    let subject_line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let subject = gtk::Label::new(None);
    subject.set_halign(gtk::Align::Start);
    subject.set_hexpand(true);
    subject.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subject.add_css_class("message-subject");
    subject_line.append(&subject);
    let tags_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tags_box.set_valign(gtk::Align::Center);
    subject_line.append(&tags_box);
    text.append(&subject_line);

    // The message's own text, at full width.
    let preview_line = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    // An encrypted message (#133) shows a lock where its text would be, in
    // the preview's own dimmed color.
    let lock = gtk::Image::from_icon_name("channel-secure-symbolic");
    lock.set_pixel_size(12);
    lock.set_valign(gtk::Align::Center);
    lock.add_css_class("message-preview");
    preview_line.append(&lock);
    let preview = gtk::Label::new(None);
    // Fill (not Start): the layout width then matches the allocation, so
    // the ellipsis lands right where the text is cut.
    preview.set_halign(gtk::Align::Fill);
    preview.set_hexpand(true);
    preview.set_xalign(0.0);
    preview.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
    preview.add_css_class("message-preview");
    preview_line.append(&preview);
    text.append(&preview_line);

    // The pills' own line, under the text, when the setting puts them
    // there (#374).
    let pills_line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    pills_line.set_visible(false);
    text.append(&pills_line);

    // The single-line layout's row (#334): empty and hidden until a row is
    // laid out that way, when the widgets above move into it.
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.set_hexpand(true);
    line.set_visible(false);
    content.append(&line);
    // The columns a card has no place for, which live on that line, each in
    // a bin that keeps it the same width on every row.
    let column_label = |px: i32| {
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let bin = ColumnBin::new(px);
        bin.set_child(Some(&label));
        bin.set_visible(false);
        line.append(&bin);
        (label, bin)
    };
    let (recipients, recipients_col) = column_label(SENDER_COLUMN_PX);
    let (people, people_col) = column_label(PEOPLE_COLUMN_PX);
    let (account, account_col) = column_label(ACCOUNT_COLUMN_PX);
    let name_col = ColumnBin::new(SENDER_COLUMN_PX);
    let subject_col = ColumnBin::new(0);
    let date_col = ColumnBin::new(-1);
    let tags_col = ColumnBin::new(TAGS_COLUMN_PX);
    tags_col.set_visible(false);
    // The subject takes whatever room the other columns leave.
    subject_col.set_hexpand(true);
    account.add_css_class("message-account");
    let due = gtk::Label::new(None);
    due.set_width_chars(9);
    due.set_xalign(1.0);
    due.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let due_col = ColumnBin::new(-1);
    due_col.set_child(Some(&due));
    due_col.set_visible(false);
    line.append(&due_col);

    // The last reply's rail: a real dotted border on a widget spanning
    // exactly the row's top half, so it ends at the node dot. Added before
    // the node so the dot draws over where they meet.
    let rail_stub = gtk::Box::new(gtk::Orientation::Vertical, 0);
    rail_stub.set_halign(gtk::Align::Start);
    rail_stub.set_homogeneous(true);
    let stub = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    stub.add_css_class("thread-rail-stub");
    rail_stub.append(&stub);
    rail_stub.append(&gtk::Box::new(gtk::Orientation::Horizontal, 0));
    overlay.add_overlay(&rail_stub);

    // The node dot where a reply meets the conversation's rail.
    let node = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    node.add_css_class("thread-node");
    node.set_halign(gtk::Align::Start);
    node.set_valign(gtk::Align::Center);
    overlay.add_overlay(&node);

    RowWidgets {
        host,
        revealer,
        surface,
        swipe_bg,
        swipe_inner,
        swipe_icon,
        swipe_label,
        overlay,
        rail_stub,
        node,
        content,
        recipients,
        recipients_col,
        people,
        people_col,
        importance,
        account,
        account_col,
        tags_col,
        due,
        due_col,
        name_col,
        subject_col,
        date_col,
        avatar_revealer,
        avatar,
        dot,
        text,
        top,
        line,
        name,
        answer,
        replied,
        forwarded,
        clip,
        star,
        receipt,
        date,
        chip,
        chip_count,
        chip_caret,
        subject_line,
        pills_line,
        subject,
        tags_box,
        preview_line,
        lock,
        preview,
    }
}
