//! Attachment chips and their thumbnails (#299).

use super::messages::ComposeInput;
use super::types::ComposeStep;
use super::Compose;
use super::ComposeWidgets;
use crate::config::ComposeFormat;
use crate::i18n::i18n_noop;
use crate::i18n::{i18n, i18n_f};
use crate::models::is_image_name;
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::ui::drop_zones::{DropChoice, DropContext, DropZones};
use crate::ui::fade_label::FadeLabel;
use adw::prelude::*;
use relm4::prelude::*;

/// Thumbnails of the composer's attachments by path (#299).
pub(super) type AttachThumbs = std::rc::Rc<
    std::cell::RefCell<std::collections::HashMap<std::path::PathBuf, Option<AttachThumb>>>,
>;

/// An attachment's two pictures: the hover card's, and the chip's square crop
/// at twice its size for a HiDPI screen.
#[derive(Clone)]
pub(super) struct AttachThumb {
    card: gtk::gdk::Texture,
    chip: gtk::gdk::Texture,
}

/// Edge a chip's thumbnail is drawn at, and the one its hover card uses.
const CHIP_THUMB: i32 = 32;

const CARD_THUMB: i32 = 220;

/// The most width a chip's name asks for. The columns are as wide as the
/// widest chip asks, so this is what lets three fit across a composer of
/// ordinary width; a wider composer gives the names more.
const CHIP_NAME_NATURAL: i32 = 110;

/// Files past this are not read for a thumbnail: the chip keeps its type icon.
const THUMB_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// One attached file as a chip (#299): a thumbnail for a picture or a PDF,
/// the type icon for anything else, the name and the size. Hovering shows a
/// larger picture with the type; a double click opens the file, and a right
/// click offers Open and Remove.
fn attachment_chip(
    path: &std::path::Path,
    thumbs: &AttachThumbs,
    sender: &ComponentSender<Compose>,
    index: usize,
) -> gtk::Box {
    use crate::ui::thumbnails::is_pdf_name;
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let size = std::fs::metadata(path).map(|m| m.len()).ok();
    let (content_type, _) = gtk::gio::content_type_guess(Some(path), &[]);
    let kind = gtk::gio::content_type_get_description(&content_type).to_string();

    let chip = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    chip.add_css_class("attach-chip");

    // The leading slot holds the type icon until a thumbnail is ready.
    let slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    slot.set_valign(gtk::Align::Center);
    slot.set_size_request(CHIP_THUMB, CHIP_THUMB);
    slot.set_hexpand(false);
    slot.set_homogeneous(true);
    let thumbable = is_image_name(&name) || is_pdf_name(&name);
    let cached = thumbs.borrow().get(path).cloned();
    match cached {
        Some(thumb) => fill_thumb_slot(&slot, &name, thumb.as_ref().map(|t| &t.chip)),
        None => {
            fill_thumb_slot(&slot, &name, None);
            if thumbable && size.is_some_and(|n| n <= THUMB_MAX_BYTES) {
                let weak = slot.downgrade();
                let thumbs = thumbs.clone();
                let owned = path.to_path_buf();
                let pdf = is_pdf_name(&name);
                let fill_name = name.clone();
                gtk::glib::spawn_future_local(async move {
                    let read = owned.clone();
                    let tex = gtk::gio::spawn_blocking(move || attachment_thumbnail(&read, pdf))
                        .await
                        .ok()
                        .flatten();
                    thumbs.borrow_mut().insert(owned, tex.clone());
                    let (Some(slot), Some(thumb)) = (weak.upgrade(), tex) else {
                        return;
                    };
                    fill_thumb_slot(&slot, &fill_name, Some(&thumb.chip));
                });
            }
        }
    }
    chip.append(&slot);

    // The name over the size. The name takes what the chip has left and
    // fades where it is cut off; hovering the chip slides the rest into view.
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    let lbl = FadeLabel::new(&name, CHIP_NAME_NATURAL);
    text.append(&lbl);
    let size_text = match size {
        Some(n) => crate::models::human_size(n),
        None => {
            // Moved or deleted since it was attached (#340): Send refuses it.
            chip.add_css_class("missing");
            chip.set_tooltip_text(Some(&i18n(
                "The file was moved or deleted. Attach it again from where it is now.",
            )));
            i18n("Not found")
        }
    };
    let size_lbl = gtk::Label::new(Some(&size_text));
    size_lbl.set_xalign(0.0);
    size_lbl.add_css_class("dim-label");
    size_lbl.add_css_class("caption");
    text.append(&size_lbl);
    chip.append(&text);
    slide_on_hover(&chip, &lbl);
    let rm = gtk::Button::from_icon_name("window-close-symbolic");
    rm.add_css_class("flat");
    rm.set_valign(gtk::Align::Center);
    rm.set_tooltip_text(Some(&i18n("Remove")));
    let s = sender.input_sender().clone();
    rm.connect_clicked(move |_| {
        let _ = s.send(ComposeInput::RemoveAttachment(index));
    });
    chip.append(&rm);

    add_hover_card(&chip, thumbs, path, &name, kind, size);
    add_open_and_menu(&chip, path, &name, index, sender);
    chip
}

/// Slide a chip's name to show the rest of it while the pointer is over
/// the chip, and back when it leaves.
fn slide_on_hover(chip: &gtk::Box, lbl: &FadeLabel) {
    let hover = gtk::EventControllerMotion::new();
    let l = lbl.clone();
    hover.connect_enter(move |_, _, _| l.reveal(true));
    let l = lbl.clone();
    hover.connect_leave(move |_| l.reveal(false));
    chip.add_controller(hover);
}

/// Put a chip's thumbnail in its leading slot, or the file type's icon when
/// there is none.
fn fill_thumb_slot(slot: &gtk::Box, name: &str, tex: Option<&gtk::gdk::Texture>) {
    use crate::ui::thumbnails::{icon_color_class, icon_for};
    while let Some(child) = slot.first_child() {
        slot.remove(&child);
    }
    match tex {
        Some(tex) => {
            let img = gtk::Image::from_paintable(Some(tex));
            img.set_pixel_size(CHIP_THUMB);
            img.set_overflow(gtk::Overflow::Hidden);
            img.add_css_class("attach-chip-thumb");
            slot.append(&img);
        }
        None => {
            let img = gtk::Image::from_icon_name(icon_for(name));
            img.set_pixel_size(CHIP_THUMB * 5 / 8);
            img.set_halign(gtk::Align::Center);
            img.add_css_class(icon_color_class(name));
            slot.append(&img);
        }
    }
}

/// A small picture of an attached file, made off the main thread: a picture
/// scaled down as it loads, so a large photo is never held at full size, or a
/// PDF's first page.
fn attachment_thumbnail(path: &std::path::Path, pdf: bool) -> Option<AttachThumb> {
    use gtk::gdk_pixbuf::{InterpType, Pixbuf};
    let pixbuf = if pdf {
        let data = std::fs::read(path).ok()?;
        let page = crate::ui::thumbnails::pdf_page_texture(&data, CARD_THUMB as f64)?;
        let png = page.save_to_png_bytes();
        Pixbuf::from_stream(
            &gtk::gio::MemoryInputStream::from_bytes(&png),
            gtk::gio::Cancellable::NONE,
        )
        .ok()?
    } else {
        Pixbuf::from_file_at_scale(path, CARD_THUMB, CARD_THUMB, true).ok()?
    };
    let texture = |pb: &Pixbuf| {
        let png = pb.save_to_bufferv("png", &[]).ok()?;
        gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from_owned(png)).ok()
    };
    // The chip's square: the middle of a picture, as a cover crop would, and
    // the top of a page, where its heading is.
    let (w, h) = (pixbuf.width(), pixbuf.height());
    let side = w.min(h).max(1);
    let top = if pdf { 0 } else { (h - side) / 2 };
    let square = pixbuf.new_subpixbuf((w - side) / 2, top, side, side);
    let edge = CHIP_THUMB * 2;
    let chip = square.scale_simple(edge, edge, InterpType::Bilinear)?;
    Some(AttachThumb {
        card: texture(&pixbuf)?,
        chip: texture(&chip)?,
    })
}

/// What to call an attachment change in the Undo menu: the file's own name,
/// or a count once there is more than one. `template` carries the `{name}`
/// placeholder and is translated here.
pub(super) fn attachment_label(template: &str, path: &std::path::Path, count: usize) -> String {
    let name = if count > 1 {
        crate::i18n::ni18n_f(
            "{n} attachment",
            "{n} attachments",
            count as u32,
            &[("n", &count.to_string())],
        )
    } else {
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    i18n_f(template, &[("name", &name)])
}

impl Compose {
    pub(super) fn rebuild_attachments(&self, flow: &gtk::FlowBox, sender: &ComponentSender<Self>) {
        while let Some(child) = flow.first_child() {
            flow.remove(&child);
        }
        for (i, path) in self.attachments.iter().enumerate() {
            let chip = attachment_chip(path, &self.attach_thumbs, sender, i);
            append_chip(flow, &chip);
        }
        // Cloud links (#144) sit with the attachments but read as links: a
        // cloud icon, the name, and a remove that also takes the paragraph
        // out of the body. Uploads in flight show a spinner chip.
        for (i, link) in self.cloud_links.iter().enumerate() {
            let chip = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            chip.add_css_class("attach-chip");
            chip.add_css_class("cloud-chip");
            chip.set_tooltip_text(Some(&link.url));
            chip.append(&gtk::Image::from_icon_name("cloud-symbolic"));
            let lbl = FadeLabel::new(
                &i18n_f("{name} (link)", &[("name", &link.name)]),
                CHIP_NAME_NATURAL,
            );
            lbl.set_hexpand(true);
            chip.append(&lbl);
            slide_on_hover(&chip, &lbl);
            let rm = gtk::Button::from_icon_name("window-close-symbolic");
            rm.add_css_class("flat");
            rm.set_valign(gtk::Align::Center);
            let s = sender.input_sender().clone();
            rm.connect_clicked(move |_| {
                let _ = s.send(ComposeInput::RemoveCloudLink(i));
            });
            chip.append(&rm);
            append_chip(flow, &chip);
        }
        for _ in 0..self.cloud_busy {
            let chip = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            chip.add_css_class("attach-chip");
            let spin = gtk::Spinner::new();
            spin.start();
            chip.append(&spin);
            chip.append(&gtk::Label::new(Some(&i18n("Uploading…"))));
            flow.append(&chip);
            if let Some(cell) = chip.parent().and_downcast::<gtk::FlowBoxChild>() {
                cell.set_can_focus(false);
            }
        }
        flow.set_visible(
            !self.attachments.is_empty() || !self.cloud_links.is_empty() || self.cloud_busy > 0,
        );
    }

    /// Handles [`ComposeInput::AttachFiles`].
    pub(super) fn on_attach_files(
        &mut self,
        sender: &ComponentSender<Self>,
        root: &adw::BreakpointBin,
    ) {
        let s = sender.input_sender().clone();
        pick_files(root, &i18n("Attach Files"), move |paths| {
            let _ = s.send(ComposeInput::AddAttachments(paths));
        });
    }

    /// Handles [`ComposeInput::AddAttachments`].
    pub(super) fn on_add_attachments(
        &mut self,
        paths: Vec<std::path::PathBuf>,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        self.fields_dirty = true;
        let at = self.attachments.len();
        let what = attachment_label(i18n_noop("Attach {name}"), &paths[0], paths.len());
        self.attachments.extend(paths.iter().cloned());
        self.push_history(
            what,
            ComposeStep::Remove {
                at,
                count: paths.len(),
            },
        );
        self.rebuild_attachments(&widgets.attach_box, sender);
        self.report_history(sender);
    }

    /// Handles [`ComposeInput::RemoveAttachment`].
    pub(super) fn on_remove_attachment(
        &mut self,
        i: usize,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        if i < self.attachments.len() {
            self.fields_dirty = true;
            let path = self.attachments.remove(i);
            let what = attachment_label(i18n_noop("Remove {name}"), &path, 1);
            self.push_history(
                what,
                ComposeStep::Insert {
                    at: i,
                    paths: vec![path],
                },
            );
            self.rebuild_attachments(&widgets.attach_box, sender);
            self.report_history(sender);
        }
    }

    pub(super) fn install_drop_zones(
        &mut self,
        root: &adw::BreakpointBin,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        // Files dragged over the composer bring up cards for where they go:
        // attached, in the text, or uploaded to the cloud (#293).
        let s = sender.input_sender().clone();
        let zones = DropZones::install(
            DropContext::Composer,
            root,
            &widgets.drop_overlay,
            move |choice, paths| {
                s.emit(ComposeInput::DroppedFiles(choice, paths));
            },
        );
        zones.set_allow_inline(self.format == ComposeFormat::Rich);
        zones.set_cloud_names(self.cloud_accounts.iter().map(|a| a.name.clone()).collect());
        // HYLKI_SHOWCASE_DROP_ZONES=<attach|inline|cloud|none>:<file>[:<file>…]
        // raises the surfaces two seconds after the composer opens.
        if let Ok(v) = std::env::var("HYLKI_SHOWCASE_DROP_ZONES") {
            let mut parts = std::env::split_paths(&v);
            let hover = match parts.next().as_deref().and_then(|p| p.to_str()) {
                Some("attach") => Some(DropChoice::Attach),
                Some("inline") => Some(DropChoice::Inline),
                Some("cloud") => Some(DropChoice::Cloud),
                _ => None,
            };
            let paths: Vec<_> = parts.collect();
            let zones = zones.clone();
            let host = root.clone().upcast::<gtk::Widget>();
            gtk::glib::timeout_add_seconds_local_once(2, move || {
                zones.showcase(&paths, &host, hover)
            });
        }
        self.drop_zones = Some(zones);
    }

    /// Files dropped on one of the drop zones (#293).
    pub(super) fn on_dropped_files(
        &mut self,
        choice: DropChoice,
        paths: Vec<std::path::PathBuf>,
        sender: &ComponentSender<Self>,
    ) {
        match choice {
            DropChoice::Attach => sender
                .input_sender()
                .emit(ComposeInput::AddAttachments(paths)),
            DropChoice::Inline => self.editor.insert_files(&paths),
            DropChoice::Cloud => sender.input_sender().emit(ComposeInput::CloudPicked(paths)),
        }
    }
}

/// Ask for files with the file chooser over the composer's window; `chosen`
/// gets the local paths picked, and is not called when there are none.
pub(super) fn pick_files(
    root: &adw::BreakpointBin,
    title: &str,
    chosen: impl FnOnce(Vec<std::path::PathBuf>) + 'static,
) {
    let dialog = gtk::FileDialog::new();
    dialog.set_title(title);
    let parent = root.root().and_downcast::<gtk::Window>();
    dialog.open_multiple(parent.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
        if let Ok(model) = res {
            let paths: Vec<_> = (0..model.n_items())
                .filter_map(|i| model.item(i).and_downcast::<gtk::gio::File>()?.path())
                .collect();
            if !paths.is_empty() {
                chosen(paths);
            }
        }
    });
}

/// Put a chip in the attachment flow. GtkFlowBox wraps it in a
/// FlowBoxChild, which fills its column but loses its own row
/// interactivity: the chip's click handlers are the only real targets.
fn append_chip(flow: &gtk::FlowBox, chip: &gtk::Box) {
    flow.append(chip);
    if let Some(cell) = chip.parent().and_downcast::<gtk::FlowBoxChild>() {
        cell.set_can_focus(false);
        cell.set_focusable(false);
    }
}

/// The hover card: a larger picture when there is one, the name, and what
/// kind of file it is with its size.
fn add_hover_card(
    chip: &gtk::Box,
    thumbs: &AttachThumbs,
    path: &std::path::Path,
    name: &str,
    kind: String,
    size: Option<u64>,
) {
    chip.set_has_tooltip(true);
    let thumbs = thumbs.clone();
    let owned = path.to_path_buf();
    let name = name.to_string();
    let detail = match size {
        Some(n) => format!("{kind} \u{b7} {}", crate::models::human_size(n)),
        None => kind,
    };
    chip.connect_query_tooltip(move |_, _, _, _, tooltip| {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        if let Some(Some(thumb)) = thumbs.borrow().get(&owned) {
            let tex = &thumb.card;
            let pic = gtk::Picture::for_paintable(tex);
            pic.set_content_fit(gtk::ContentFit::Contain);
            let (w, h) = (tex.width().max(1) as f64, tex.height().max(1) as f64);
            let scale = (CARD_THUMB as f64 / w.max(h)).min(1.0);
            pic.set_size_request((w * scale) as i32, (h * scale) as i32);
            card.append(&pic);
        }
        let title = gtk::Label::new(Some(&name));
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.set_max_width_chars(40);
        title.add_css_class("heading");
        card.append(&title);
        let info = gtk::Label::new(Some(&detail));
        info.set_xalign(0.0);
        card.append(&info);
        tooltip.set_custom(Some(&card));
        true
    });
}

/// Double click opens the file; right click offers Open and Remove.
fn add_open_and_menu(
    chip: &gtk::Box,
    path: &std::path::Path,
    name: &str,
    index: usize,
    sender: &ComponentSender<Compose>,
) {
    let open = {
        let owned = path.to_path_buf();
        let name = name.to_string();
        let chip = chip.downgrade();
        move || {
            let parent = chip
                .upgrade()
                .and_then(|c| c.root())
                .and_downcast::<gtk::Window>();
            let owned = owned.clone();
            let name = name.clone();
            gtk::glib::spawn_future_local(async move {
                let read = owned.clone();
                match gtk::gio::spawn_blocking(move || std::fs::read(read)).await {
                    Ok(Ok(data)) => {
                        crate::ui::attachment_io::open_bytes(&name, &data, parent.as_ref())
                    }
                    Ok(Err(e)) => tracing::warn!("could not read the attachment to open it: {e}"),
                    Err(_) => {}
                }
            });
        }
    };
    let open = std::rc::Rc::new(open);
    // The file previewer (#625) is shown a staged copy, as for mail: the
    // file itself may be where a sandbox keeps the host's previewer out.
    let preview = {
        let owned = path.to_path_buf();
        let name = name.to_string();
        let chip = chip.downgrade();
        std::rc::Rc::new(move || {
            let parent = chip
                .upgrade()
                .and_then(|c| c.root())
                .and_downcast::<gtk::Window>();
            let owned = owned.clone();
            let name = name.clone();
            gtk::glib::spawn_future_local(async move {
                let read = owned.clone();
                match gtk::gio::spawn_blocking(move || std::fs::read(read)).await {
                    Ok(Ok(data)) => crate::desktop::previewer::show(
                        vec![crate::models::Attachment { name, data }],
                        0,
                        parent.as_ref(),
                        None,
                    ),
                    Ok(Err(e)) => {
                        tracing::warn!("could not read the attachment to preview it: {e}")
                    }
                    Err(_) => {}
                }
            });
        })
    };
    let double = gtk::GestureClick::new();
    double.set_button(gtk::gdk::BUTTON_PRIMARY);
    {
        let open = open.clone();
        double.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                open();
            }
        });
    }
    chip.add_controller(double);
    let right = gtk::GestureClick::new();
    right.set_button(gtk::gdk::BUTTON_SECONDARY);
    {
        let s = sender.input_sender().clone();
        let chip_weak = chip.downgrade();
        right.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            let Some(chip) = chip_weak.upgrade() else {
                return;
            };
            let open = open.clone();
            let s = s.clone();
            let mut first = Vec::new();
            if crate::desktop::previewer::available() {
                let preview = preview.clone();
                first.push(
                    MenuEntry::new(i18n("Preview"), move || preview()).icon("view-reveal-symbolic"),
                );
            }
            first.push(MenuEntry::new(i18n("Open"), move || open()).icon("document-open-symbolic"));
            show_context_menu(
                &chip,
                x,
                y,
                vec![
                    first,
                    vec![MenuEntry::new(i18n("Remove"), move || {
                        let _ = s.send(ComposeInput::RemoveAttachment(index));
                    })
                    .icon("window-close-symbolic")],
                ],
            );
        });
    }
    chip.add_controller(right);
}
