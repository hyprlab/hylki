//! The grid's cells and the list's rows.

use super::{AttachmentDrawer, AttachmentDrawerInput};
use crate::i18n::i18n;
use crate::models::{is_image_name, Attachment};
use crate::ui::attachments_gallery::{deleting_label, deleting_over, deleting_veil};
use crate::ui::thumbnails::{
    icon_color_class, icon_for, is_pdf_name, spawn_thumbnail_render, texture_from,
    thumbnail_texture, Thumbnail,
};
use adw::prelude::*;
use gtk::glib;
use relm4::prelude::*;

/// Whether an attachment can be shown in the drawer's lightbox: a decodable
/// image, or a PDF (whose first page renders on demand).
pub(crate) fn previewable(att: &Attachment) -> bool {
    (is_image_name(&att.name) && texture_from(&att.data).is_some()) || is_pdf_name(&att.name)
}

/// Whether Space shows an attachment in the GNOME file previewer (#625):
/// one the lightbox does not show, nor an attached message, which reads
/// best in a window of Hylki's own (#622), and only with a previewer there.
/// Anything else Space opens as a click would.
pub(crate) fn for_previewer(att: &Attachment) -> bool {
    !previewable(att)
        && !crate::mail::message_file::is_message(&att.name, &att.data)
        && crate::desktop::previewer::available()
}

/// The drawer's cover-cropped picture for a ready thumbnail texture.
fn drawer_picture(tex: &gtk::gdk::Texture) -> gtk::Widget {
    let pic = gtk::Picture::for_paintable(tex);
    pic.set_content_fit(gtk::ContentFit::Cover);
    pic.set_hexpand(true);
    pic.set_vexpand(true);
    pic.upcast()
}

/// The drawer's centred type icon, sized to the current thumbnail scale.
fn drawer_icon(name: &str, thumb: i32) -> gtk::Widget {
    let img = gtk::Image::from_icon_name(icon_for(name));
    img.set_pixel_size((thumb / 2).max(24));
    img.set_halign(gtk::Align::Center);
    img.set_valign(gtk::Align::Center);
    img.set_hexpand(true);
    img.set_vexpand(true);
    img.add_css_class("gallery-file-icon");
    img.add_css_class(icon_color_class(name));
    img.upcast()
}

/// Whether an attachment is an OpenPGP public key: by its armour, or by a
/// key file's usual name.
fn is_key_attachment(att: &Attachment) -> bool {
    let head = &att.data[..att.data.len().min(64)];
    if head.starts_with(b"-----BEGIN PGP PUBLIC KEY BLOCK-----") {
        return true;
    }
    let name = att.name.to_ascii_lowercase();
    (name.ends_with(".asc")
        || name.ends_with(".gpg")
        || name.ends_with(".pgp")
        || name.ends_with(".key"))
        && !head.starts_with(b"-----BEGIN PGP MESSAGE-----")
        && !head.starts_with(b"-----BEGIN PGP SIGNATURE-----")
}

/// Build one grid cell: a square thumbnail (image/PDF texture or type icon)
/// with hover Download/Open actions and the filename beneath it.
pub(super) fn build_cell(
    index: usize,
    att: &Attachment,
    thumb: i32,
    deleting: bool,
    sender: &ComponentSender<AttachmentDrawer>,
) -> gtk::FlowBoxChild {
    let cell = gtk::Box::new(gtk::Orientation::Vertical, 4);
    cell.add_css_class("drawer-cell");
    cell.set_halign(gtk::Align::Center);

    // The thumbnail content: a cover-cropped image/PDF-page render, or a
    // centred type icon. Anything not yet in the render cache gets a spinner
    // while a worker decodes it — never a frozen window.
    let content: gtk::Widget = match thumbnail_texture(&att.name, &att.data) {
        Thumbnail::Ready(tex) => drawer_picture(&tex),
        Thumbnail::Fallback => drawer_icon(&att.name, thumb),
        Thumbnail::Pending => {
            let holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            holder.set_hexpand(true);
            holder.set_vexpand(true);
            holder.append(&crate::ui::thumbnails::thumbnail_spinner());
            let weak = holder.downgrade();
            let name = att.name.clone();
            spawn_thumbnail_render(&att.name, att.data.clone(), move |tex| {
                let Some(holder) = weak.upgrade() else { return };
                while let Some(child) = holder.first_child() {
                    holder.remove(&child);
                }
                match tex {
                    Some(tex) => holder.append(&drawer_picture(&tex)),
                    None => holder.append(&drawer_icon(&name, thumb)),
                }
            });
            holder.upcast()
        }
    };

    // Force a fixed 1:1 square regardless of the image's intrinsic size, so cells
    // stay uniform and the grid flows left-to-right, wrapping into rows.
    let square = SquareBox::new(&content, thumb);
    square.set_overflow(gtk::Overflow::Hidden);
    square.add_css_class("drawer-thumb");

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&square));

    // Hover quick actions (Download, Open) — same style as the gallery.
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    actions.set_halign(gtk::Align::End);
    actions.set_valign(gtk::Align::End);
    actions.set_margin_end(6);
    actions.set_margin_bottom(6);
    let action_btn = |icon: &str, tip: &str| {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("gallery-open");
        b.add_css_class("drawer-action");
        b.add_css_class("circular");
        b.add_css_class("osd");
        b.set_tooltip_text(Some(tip));
        b
    };
    let download = action_btn("folder-download-symbolic", &i18n("Download"));
    let s = sender.clone();
    download.connect_clicked(move |_| {
        s.input_sender()
            .emit(AttachmentDrawerInput::Download(index))
    });
    actions.append(&download);
    let open = action_btn("document-open-symbolic", &i18n("Open"));
    let s = sender.clone();
    open.connect_clicked(move |_| s.input_sender().emit(AttachmentDrawerInput::Open(index)));
    actions.append(&open);
    // An attached public key (#133): one click puts it in the keyring.
    if is_key_attachment(att) {
        let import = action_btn("channel-secure-symbolic", &i18n("Import OpenPGP key"));
        let s = sender.clone();
        import.connect_clicked(move |_| {
            s.input_sender()
                .emit(AttachmentDrawerInput::ImportKey(index))
        });
        actions.append(&import);
    }
    if deleting {
        cell.add_css_class("attachment-deleting");
        overlay.add_overlay(&deleting_veil(false));
    } else {
        overlay.add_overlay(&actions);
    }

    cell.append(&overlay);

    // The name, or "Deleting…" while the file is deleted from the server:
    // whole, even where it is wider than the smallest thumbnail.
    let name = if deleting {
        deleting_label()
    } else {
        gtk::Label::new(Some(&att.name))
    };
    if !deleting {
        name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        name.set_max_width_chars(1);
    }
    name.set_width_request(thumb);
    name.set_justify(gtk::Justification::Center);
    name.set_tooltip_text(Some(&att.name));
    name.add_css_class("drawer-filename");
    name.add_css_class("caption");
    cell.append(&name);

    let child = gtk::FlowBoxChild::new();
    child.set_child(Some(&cell));
    // The full filename on hover anywhere in the cell — the label below the
    // thumbnail is ellipsized, so the thumbnail itself must answer too.
    child.set_tooltip_text(Some(&att.name));
    // Nothing more to do with a file on its way off the server.
    if deleting {
        child.set_can_target(false);
        return child;
    }

    // Right-click → context menu at the click point.
    let right = gtk::GestureClick::new();
    right.set_button(gtk::gdk::BUTTON_SECONDARY);
    let s = sender.clone();
    right.connect_pressed(move |_, _, x, y| {
        s.input_sender()
            .emit(AttachmentDrawerInput::ContextMenu { index, x, y });
    });
    child.add_controller(right);
    add_double_click_open(&child, index, sender);

    child
}

/// Double-click (primary): the drawer's activation gesture — previewable
/// types open the lightbox, everything else its default application.
fn add_double_click_open(
    child: &gtk::FlowBoxChild,
    index: usize,
    sender: &ComponentSender<AttachmentDrawer>,
) {
    let dbl = gtk::GestureClick::new();
    dbl.set_button(gtk::gdk::BUTTON_PRIMARY);
    let s = sender.clone();
    dbl.connect_pressed(move |_, n, _, _| {
        if n == 2 {
            s.input_sender()
                .emit(AttachmentDrawerInput::Activate(index));
        }
    });
    child.add_controller(dbl);
}

/// One row of the drawer's list view: type icon, filename, size, and the same
/// Download/Open quick actions the grid shows on hover. Activation (click) and
/// the right-click context menu match the grid's behaviour.
pub(super) fn build_list_row(
    index: usize,
    att: &Attachment,
    deleting: bool,
    sender: &ComponentSender<AttachmentDrawer>,
) -> gtk::FlowBoxChild {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("drawer-list-row");

    let icon = gtk::Image::from_icon_name(icon_for(&att.name));
    icon.set_pixel_size(20);
    icon.add_css_class(icon_color_class(&att.name));
    row.append(&icon);

    let name = gtk::Label::new(Some(&att.name));
    name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    name.set_halign(gtk::Align::Start);
    name.set_hexpand(true);
    name.set_xalign(0.0);
    row.append(&name);

    let size = gtk::Label::new(Some(&att.human_size()));
    size.add_css_class("dim-label");
    size.add_css_class("caption");
    size.set_valign(gtk::Align::Center);
    row.append(&size);

    let action_btn = |icon: &str, tip: &str| {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("flat");
        b.set_valign(gtk::Align::Center);
        b.set_tooltip_text(Some(tip));
        b
    };
    // Spaced as the row spaces its other parts.
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let download = action_btn("folder-download-symbolic", &i18n("Download"));
    let s = sender.clone();
    download.connect_clicked(move |_| {
        s.input_sender()
            .emit(AttachmentDrawerInput::Download(index))
    });
    actions.append(&download);
    let open = action_btn("document-open-symbolic", &i18n("Open"));
    let s = sender.clone();
    open.connect_clicked(move |_| s.input_sender().emit(AttachmentDrawerInput::Open(index)));
    actions.append(&open);
    if deleting {
        row.append(&deleting_over(&actions));
    } else {
        row.append(&actions);
    }

    let child = gtk::FlowBoxChild::new();
    child.set_child(Some(&row));
    child.set_tooltip_text(Some(&att.name));
    if deleting {
        // The ground on the cell itself, where the hover and selection
        // highlights are drawn, so it has their shape.
        child.add_css_class("attachment-deleting");
        child.set_can_target(false);
        return child;
    }

    let right = gtk::GestureClick::new();
    right.set_button(gtk::gdk::BUTTON_SECONDARY);
    let s = sender.clone();
    right.connect_pressed(move |_, _, x, y| {
        s.input_sender()
            .emit(AttachmentDrawerInput::ContextMenu { index, x, y });
    });
    child.add_controller(right);
    add_double_click_open(&child, index, sender);

    child
}

glib::wrapper! {
    /// A single-child container that always measures to a fixed N×N square,
    /// ignoring the child's intrinsic size. This keeps every thumbnail uniformly
    /// sized (so images don't blow the cell out to their native pixel width) and
    /// lets the grid pack many per row, flowing left-to-right.
    struct SquareBox(ObjectSubclass<imp::SquareBox>) @extends gtk::Widget;
}

impl SquareBox {
    pub(super) fn new(child: &impl IsA<gtk::Widget>, size: i32) -> Self {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let obj: Self = glib::Object::new();
        obj.imp().size.set(size);
        child.set_parent(&obj);
        obj
    }
}

mod imp {
    use std::cell::Cell;

    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub(super) struct SquareBox {
        pub(super) size: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SquareBox {
        const NAME: &'static str = "HylkiSquareBox";
        type Type = super::SquareBox;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for SquareBox {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for SquareBox {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::ConstantSize
        }

        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let s = self.size.get().max(1);
            (s, s, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.obj().first_child() {
                child.allocate(width, height, baseline, None);
            }
        }
    }
}
