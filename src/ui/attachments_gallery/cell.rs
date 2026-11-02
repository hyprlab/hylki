//! One grid cell, and the "Deleting…" treatment a file gets while it is
//! removed from the server (shared with the composer's attachment drawer).

use adw::prelude::*;
use gtk::gdk;
use relm4::ComponentSender;

use crate::i18n::i18n;
use crate::models::GalleryItem;

use super::labels::folder_label;
use super::{
    icon_color_class, icon_for, spawn_thumbnail_render, thumbnail_spinner, thumbnail_texture,
    AttachmentsGallery, GalleryInput, RatioBox, Thumbnail,
};

/// One grid cell: a thumbnail (image or PDF first page) or type icon, plus name
/// + size.
pub(super) fn build_cell(
    index: usize,
    item: &GalleryItem,
    width: i32,
    deleting: bool,
    sender: &ComponentSender<AttachmentsGallery>,
) -> gtk::Widget {
    let cell = gtk::Box::new(gtk::Orientation::Vertical, 6);
    cell.add_css_class("gallery-cell");
    cell.set_hexpand(true);
    cell.set_halign(gtk::Align::Fill);
    cell.set_tooltip_text(Some(&format!("{} — {}", item.name, item.human_size())));

    // Lock the thumbnail section to a 4:3 aspect ratio; its width tracks the
    // (responsive) column width and the height follows, filling the cell.
    let aspect = RatioBox::new(&thumbnail(item));
    // Preferred column width (the footer slider's value) — the FlowBox packs at
    // least 3 per row and adds more as the window widens.
    aspect.set_width_request(width);
    aspect.set_hexpand(true);

    // Overlay quick-action buttons at the thumbnail's bottom-right corner; they
    // fade in on hover (via CSS). Download and Open need the file's bytes cached;
    // "Go to Message" always works, so it shows even for uncached attachments.
    let thumb_overlay = gtk::Overlay::new();
    thumb_overlay.set_child(Some(&aspect));

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    actions.set_valign(gtk::Align::End);
    actions.set_margin_end(6);
    actions.set_margin_bottom(6);
    add_quick_actions(&actions, item, index, sender, |icon, tip| {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("gallery-open");
        b.add_css_class("circular");
        b.add_css_class("osd");
        b.set_tooltip_text(Some(tip));
        b
    });
    if deleting {
        cell.add_css_class("attachment-deleting");
        thumb_overlay.add_overlay(&deleting_veil(true));
    } else {
        thumb_overlay.add_overlay(&actions);
    }

    cell.append(&thumb_overlay);
    append_cell_labels(&cell, item);

    let child = gtk::FlowBoxChild::new();
    child.set_child(Some(&cell));
    // Nothing more to do with a file on its way off the server.
    if deleting {
        child.set_can_target(false);
        return child.upcast();
    }
    // Single click keeps the FlowBox's built-in activation (which opens the
    // preview).
    add_item_gestures(&child, index, sender);
    child.upcast()
}

/// A cell's picture: the thumbnail when it is ready, a spinner while a
/// worker renders it (replaced in place when it lands), else the type icon.
fn thumbnail(item: &GalleryItem) -> gtk::Box {
    let thumb_holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    thumb_holder.add_css_class("gallery-thumb");
    thumb_holder.set_halign(gtk::Align::Fill);
    thumb_holder.set_valign(gtk::Align::Fill);

    let thumb = match item.data.as_ref() {
        Some(d) => thumbnail_texture(&item.name, d),
        None => Thumbnail::Fallback,
    };
    match thumb {
        Thumbnail::Ready(tex) => thumb_holder.append(&gallery_picture(&tex)),
        Thumbnail::Fallback => thumb_holder.append(&gallery_icon(&item.name)),
        Thumbnail::Pending => {
            thumb_holder.append(&thumbnail_spinner());
            let holder = thumb_holder.downgrade();
            let name = item.name.clone();
            let data = item.data.clone().unwrap_or_default();
            spawn_thumbnail_render(&item.name, data, move |tex| {
                // The cell may be gone by now (search narrowed, list rebuilt);
                // the render still landed in the cache for the next build.
                let Some(holder) = holder.upgrade() else {
                    return;
                };
                while let Some(child) = holder.first_child() {
                    holder.remove(&child);
                }
                match tex {
                    Some(tex) => holder.append(&gallery_picture(&tex)),
                    None => holder.append(&gallery_icon(&name)),
                }
            });
        }
    }
    thumb_holder
}

/// The text under a cell's picture: name, sender, subject, then folder,
/// date and size on one line.
fn append_cell_labels(cell: &gtk::Box, item: &GalleryItem) {
    let name = gtk::Label::new(Some(&item.name));
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    name.set_max_width_chars(18);
    name.add_css_class("gallery-name");
    cell.append(&name);

    let from = item.from_name.trim();
    if !from.is_empty() {
        let sender = gtk::Label::new(Some(from));
        sender.set_ellipsize(gtk::pango::EllipsizeMode::End);
        sender.set_max_width_chars(18);
        sender.add_css_class("gallery-from");
        cell.append(&sender);
    }

    let subject = item.subject.trim();
    if !subject.is_empty() {
        let subj = gtk::Label::new(Some(subject));
        subj.set_ellipsize(gtk::pango::EllipsizeMode::End);
        subj.set_max_width_chars(18);
        subj.add_css_class("gallery-subject");
        subj.add_css_class("dim-label");
        cell.append(&subj);
    }

    let mut meta = vec![folder_label(item)];
    let date = item.date_label();
    if !date.is_empty() {
        meta.push(date);
    }
    meta.push(item.human_size());
    let sub = gtk::Label::new(Some(&meta.join(" · ")));
    sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
    sub.set_max_width_chars(18);
    sub.add_css_class("gallery-size");
    sub.add_css_class("dim-label");
    cell.append(&sub);
}

/// The quick actions a cell or a table row carries, in `actions`: Download
/// and Open when the file's bytes are in hand, Go to Message always.
/// `button` makes each one in the caller's style.
pub(super) fn add_quick_actions(
    actions: &gtk::Box,
    item: &GalleryItem,
    index: usize,
    sender: &ComponentSender<AttachmentsGallery>,
    button: impl Fn(&str, &str) -> gtk::Button,
) {
    if item.data.is_some() {
        let download = button("folder-download-symbolic", &i18n("Download"));
        let s = sender.clone();
        download.connect_clicked(move |_| s.input(GalleryInput::DownloadItem(index)));
        actions.append(&download);

        let open = button("document-open-symbolic", &i18n("Open"));
        let s = sender.clone();
        open.connect_clicked(move |_| s.input(GalleryInput::OpenItem(index)));
        actions.append(&open);
    }
    let goto = button("mail-unread-symbolic", &i18n("Go to Message"));
    let s = sender.clone();
    goto.connect_clicked(move |_| s.input(GalleryInput::GoToItem(index)));
    actions.append(&goto);
}

/// The gestures a cell or a table row carries: right-click for the item's
/// menu at the click point, double-click to open it in its own application.
pub(super) fn add_item_gestures(
    widget: &impl IsA<gtk::Widget>,
    index: usize,
    sender: &ComponentSender<AttachmentsGallery>,
) {
    let right = gtk::GestureClick::new();
    right.set_button(gtk::gdk::BUTTON_SECONDARY);
    let s = sender.clone();
    right.connect_pressed(move |_, _, x, y| {
        s.input(GalleryInput::ContextMenu { index, x, y });
    });
    widget.add_controller(right);

    let dbl = gtk::GestureClick::new();
    dbl.set_button(gtk::gdk::BUTTON_PRIMARY);
    let s = sender.clone();
    dbl.connect_pressed(move |_, n, _, _| {
        if n == 2 {
            s.input(GalleryInput::OpenExternal(index));
        }
    });
    widget.add_controller(dbl);
}

/// The gallery's cover-cropped thumbnail picture for a ready texture.
fn gallery_picture(tex: &gdk::Texture) -> gtk::Picture {
    let pic = gtk::Picture::for_paintable(tex);
    pic.set_content_fit(gtk::ContentFit::Cover);
    pic.set_hexpand(true);
    pic.set_vexpand(true);
    pic.add_css_class("gallery-thumb-image");
    pic
}

/// The gallery's centred type icon for anything without a thumbnail.
fn gallery_icon(name: &str) -> gtk::Image {
    let img = gtk::Image::from_icon_name(icon_for(name));
    img.set_pixel_size(56);
    img.set_hexpand(true);
    img.add_css_class("gallery-file-icon");
    img.add_css_class(icon_color_class(name));
    img
}

/// How long a file deleted from the server takes to fade out (#289).
const DELETE_FADE_MS: u32 = 420;

/// "Deleting…", in place of a file's actions while it is removed from the
/// server (#289).
pub(crate) fn deleting_label() -> gtk::Label {
    let l = gtk::Label::new(Some(&i18n("Deleting…")));
    l.add_css_class("attachment-deleting-label");
    l.add_css_class("caption");
    l.set_valign(gtk::Align::Center);
    l
}

/// A row's actions while its file is removed from the server: still there,
/// unseen and out of reach, so the row keeps its size, with "Deleting…"
/// over them.
pub(crate) fn deleting_over(actions: &impl IsA<gtk::Widget>) -> gtk::Overlay {
    actions.set_opacity(0.0);
    actions.set_can_target(false);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(actions));
    let label = deleting_label();
    label.set_halign(gtk::Align::End);
    overlay.add_overlay(&label);
    overlay
}

/// A thumbnail's cover while its file is removed from the server: a pale
/// red veil, with "Deleting…" across the middle when `labelled` (a
/// thumbnail too small for it says so beneath instead).
pub(crate) fn deleting_veil(labelled: bool) -> gtk::Widget {
    let veil = gtk::Box::new(gtk::Orientation::Vertical, 0);
    veil.add_css_class("attachment-deleting-veil");
    veil.set_hexpand(true);
    veil.set_vexpand(true);
    veil.set_overflow(gtk::Overflow::Hidden);
    if labelled {
        let label = deleting_label();
        label.set_halign(gtk::Align::Center);
        label.set_vexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        veil.append(&label);
    }
    veil.upcast()
}

/// Fade a deleted file's cell or row out, then call `done` to take it away.
/// Libadwaita skips the fade when animations are off or the widget is not
/// on screen, and `done` comes at once.
pub(crate) fn fade_out(widget: &impl IsA<gtk::Widget>, done: impl Fn() + 'static) {
    let w = widget.clone().upcast::<gtk::Widget>();
    let target = adw::CallbackAnimationTarget::new({
        let w = w.clone();
        move |v| w.set_opacity(v)
    });
    let anim = adw::TimedAnimation::new(&w, 1.0, 0.0, DELETE_FADE_MS, target);
    anim.set_easing(adw::Easing::EaseOutCubic);
    anim.connect_done(move |_| done());
    anim.play();
}
