//! Printing the attachment the lightbox shows (#632): a PDF as it is, an
//! image fitted to one page. With the lightbox open, Ctrl+P means the
//! document on screen, not the message it came with.

use std::path::{Path, PathBuf};

use gtk::prelude::*;
use gtk::{cairo, gio};

use super::{LAST, MARGIN_MM, POINTS_PER_MM};
use crate::i18n::i18n;

/// Ask for the printer and the paper, then print `data`, the attachment
/// called `name`. Only PDFs and images reach here: what the lightbox shows.
pub(crate) fn print_attachment(name: &str, data: &[u8], parent: Option<gtk::Window>) {
    let image = crate::models::is_image_name(name);
    let texture = if image {
        match crate::ui::thumbnails::texture_from(data) {
            Some(t) => Some(t),
            None => {
                tracing::warn!("printing an attachment: the image does not decode");
                return;
            }
        }
    } else {
        None
    };

    let dialog = gtk::PrintDialog::new();
    dialog.set_title(&i18n("Print"));
    let settings = gtk::PrintSettings::new();
    // Names the job in the queue, and the file when printing to one.
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("attachment");
    settings.set(gtk::PRINT_SETTINGS_OUTPUT_BASENAME, Some(stem));
    if let Some((printer, page)) = LAST.with(|l| l.borrow().clone()) {
        settings.set_printer(&printer);
        dialog.set_page_setup(&page);
    }
    dialog.set_print_settings(&settings);

    let data = data.to_vec();
    let name = name.to_string();
    let chooser = dialog.clone();
    let owner = parent.clone();
    chooser.setup(owner.as_ref(), gio::Cancellable::NONE, move |result| {
        let setup = match result {
            Ok(setup) => setup,
            // Dismissing the dialog is not a failure.
            Err(e) => {
                tracing::debug!("print dialog dismissed: {e}");
                return;
            }
        };
        let page = setup.page_setup();
        if let Some(printer) = setup.print_settings().printer() {
            LAST.with(|l| *l.borrow_mut() = Some((printer.to_string(), page.clone())));
        }
        let path = match write_printable(&name, &data, texture.as_ref(), &page) {
            Ok(path) => path,
            Err(e) => {
                tracing::warn!("printing an attachment failed: {e}");
                return;
            }
        };
        let file = gio::File::for_path(&path);
        dialog.print_file(
            parent.as_ref(),
            Some(&setup),
            &file,
            gio::Cancellable::NONE,
            move |r| {
                if let Err(e) = r {
                    tracing::warn!("printing an attachment failed: {e}");
                }
                let _ = std::fs::remove_file(&path);
            },
        );
    });
}

/// The file the printer is handed: a PDF copied as it is, or the image laid
/// out on `page`. A private copy, since it holds someone's mail; the caller
/// deletes it.
fn write_printable(
    name: &str,
    data: &[u8],
    texture: Option<&gtk::gdk::Texture>,
    page: &gtk::PageSetup,
) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("hylki-print-{}", std::process::id()));
    crate::config::private_dir(&dir).map_err(|e| e.to_string())?;
    let n = super::SERIAL.with(|s| {
        s.set(s.get() + 1);
        s.get()
    });
    let out = dir.join(format!("{n}-attachment.pdf"));
    let written = match texture {
        Some(texture) => {
            let paper = (
                page.paper_width(gtk::Unit::Points),
                page.paper_height(gtk::Unit::Points),
            );
            image_pdf(texture, paper, name, &out)
        }
        None => crate::config::write_private_bytes(&out, data).map_err(|e| e.to_string()),
    };
    if written.is_err() {
        let _ = std::fs::remove_file(&out);
    }
    written.map(|()| out)
}

/// Where an image of `image` pixels goes on a page of `paper` points: its
/// scale and top-left corner. Centred inside the margins, shrunk to fit,
/// and never drawn bigger than on a 96 dpi screen, which would only print
/// a small picture blurred across the sheet.
fn image_placement(image: (f64, f64), paper: (f64, f64)) -> (f64, f64, f64) {
    let margin = MARGIN_MM * POINTS_PER_MM;
    let area = (
        (paper.0 - 2.0 * margin).max(1.0),
        (paper.1 - 2.0 * margin).max(1.0),
    );
    let scale = (area.0 / image.0).min(area.1 / image.1).min(72.0 / 96.0);
    let x = (paper.0 - image.0 * scale) / 2.0;
    let y = (paper.1 - image.1 * scale) / 2.0;
    (scale, x, y)
}

/// One page of `paper` points, the image on it, written to `out` as a PDF.
fn image_pdf(
    texture: &gtk::gdk::Texture,
    paper: (f64, f64),
    title: &str,
    out: &Path,
) -> Result<(), String> {
    let (w, h) = (texture.width(), texture.height());
    let stride = cairo::Format::ARgb32
        .stride_for_width(w as u32)
        .map_err(|e| e.to_string())?;
    let mut pixels = vec![0u8; stride as usize * h as usize];
    // The default download is premultiplied BGRA in native byte order,
    // which is cairo's ARGB32.
    texture.download(&mut pixels, stride as usize);
    let image = cairo::ImageSurface::create_for_data(pixels, cairo::Format::ARgb32, w, h, stride)
        .map_err(|e| e.to_string())?;

    let surface = cairo::PdfSurface::new(paper.0, paper.1, out).map_err(|e| e.to_string())?;
    let _ = surface.set_metadata(cairo::PdfMetadata::Title, title);
    let _ = surface.set_metadata(cairo::PdfMetadata::Creator, "Hylki");
    let cr = cairo::Context::new(&surface).map_err(|e| e.to_string())?;
    let (scale, x, y) = image_placement((f64::from(w), f64::from(h)), paper);
    cr.translate(x, y);
    cr.scale(scale, scale);
    cr.set_source_surface(&image, 0.0, 0.0)
        .map_err(|e| e.to_string())?;
    cr.paint().map_err(|e| e.to_string())?;
    cr.show_page().map_err(|e| e.to_string())?;
    drop(cr);
    surface.finish();
    surface.status().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A4: (f64, f64) = (595.0, 842.0);

    #[test]
    fn a_photo_shrinks_to_the_page_inside_the_margins() {
        let (scale, x, y) = image_placement((4000.0, 3000.0), A4);
        let margin = MARGIN_MM * POINTS_PER_MM;
        assert!((x - margin).abs() < 0.01, "{x}");
        assert!(4000.0 * scale <= A4.0 - 2.0 * margin + 0.01);
        // Centred down the page.
        assert!((y - (A4.1 - 3000.0 * scale) / 2.0).abs() < 0.01);
    }

    #[test]
    fn a_small_picture_is_not_blown_up() {
        let (scale, x, _) = image_placement((64.0, 64.0), A4);
        assert_eq!(scale, 0.75);
        assert!((x - (A4.0 - 48.0) / 2.0).abs() < 0.01);
    }

    #[test]
    fn an_image_prints_as_one_page_on_the_paper() {
        gtk::init().ok();
        let dir =
            std::env::temp_dir().join(format!("hylki-print-image-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("image.pdf");
        let bytes = gtk::glib::Bytes::from_owned([0u8, 0, 0, 255].repeat(8 * 4));
        let texture: gtk::gdk::Texture =
            gtk::gdk::MemoryTexture::new(8, 4, gtk::gdk::MemoryFormat::R8g8b8a8, &bytes, 8 * 4)
                .upcast();
        image_pdf(&texture, A4, "photo.png", &out).unwrap();
        let doc = poppler::Document::from_file(&gio::File::for_path(&out).uri(), None).unwrap();
        assert_eq!(doc.n_pages(), 1);
        let (w, h) = doc.page(0).unwrap().size();
        assert!((w - A4.0).abs() < 0.5 && (h - A4.1).abs() < 0.5, "{w}x{h}");
        // The black picture is in the middle of the page, not lost.
        let mut page = cairo::ImageSurface::create(cairo::Format::Rgb24, 595, 842).unwrap();
        {
            let cr = cairo::Context::new(&page).unwrap();
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.paint().unwrap();
            doc.page(0).unwrap().render(&cr);
        }
        let stride = page.stride() as usize;
        let pixels = page.data().unwrap();
        let middle = pixels[421 * stride + 297 * 4];
        assert!(middle < 30, "{middle}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
