//! Printing and the print preview (issues #16, #359).
//!
//! Everything printed goes through a PDF. WebKit lays the message out on the
//! chosen paper and writes it to a file; Hylki then draws the page footer
//! (page numbers, the date printed) onto every page and hands that file to the
//! printer. The footer cannot come from the document: WebKit has no CSS page
//! margin boxes, and a `position: fixed` element prints once, not on every
//! page.
//!
//! The preview is that same PDF, shown page by page in a window of Hylki's
//! own. The print dialog's own Preview button belongs to the portal and, in a
//! sandbox, produces nothing; handing a PDF to an external viewer was tried
//! and is a chain of things that can each fail quietly (a URI, the document
//! portal, whatever the desktop opens `application/pdf` with). None of that is
//! needed to answer the only question a preview is asked: *what will come out
//! of the printer?*

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use webkit6::prelude::WebViewExt;
use gtk::{cairo, gdk, gio, glib, pango};

use crate::config::PrintOptions;
use crate::i18n::{i18n, i18n_f};

/// The margin on every side of a printed page, in millimetres. The paper's
/// own default (a quarter of an inch) puts text where a printer may not reach.
const MARGIN_MM: f64 = 15.0;
/// The left margin with Settings → Reading → Printing → wider left margin:
/// room for a hole punch, as DIN 5008 leaves it.
const PUNCH_MARGIN_MM: f64 = 25.0;
/// The bottom margin when there is a footer to fit under the text.
const FOOTER_MARGIN_MM: f64 = 20.0;
/// How far the footer's baseline sits above the bottom edge.
const FOOTER_BASELINE_MM: f64 = 10.0;
const POINTS_PER_MM: f64 = 72.0 / 25.4;

thread_local! {
    static OPTIONS: Cell<PrintOptions> = Cell::new(PrintOptions::default());
    /// The printer and paper chosen last, so the next print dialog opens on
    /// them and the preview is laid out on that paper.
    static LAST: RefCell<Option<(String, gtk::PageSetup)>> = const { RefCell::new(None) };
    /// The views laying out a print. Nothing else holds them, and a dropped
    /// view never finishes loading.
    static PENDING: RefCell<Vec<webkit6::WebView>> = const { RefCell::new(Vec::new()) };
    /// The PDF writer's name, once found: looking blocks until every print
    /// backend has answered.
    static FILE_PRINTER: RefCell<Option<String>> = const { RefCell::new(None) };
    static SERIAL: Cell<u32> = const { Cell::new(0) };
}

/// What the app's Settings say a printed page carries.
pub fn set_options(options: PrintOptions) {
    OPTIONS.with(|o| o.set(options));
}

fn options() -> PrintOptions {
    OPTIONS.with(Cell::get)
}

/// Print `html`: ask for the printer and the paper, lay the message out on
/// that paper, add the footer, and send it.
///
/// The settings come through `GtkPrintDialog`'s callback rather than WebKit's
/// own `run_dialog`, which spins a nested main loop; polling a glib future
/// inside one aborts the process.
pub fn print_html(html: &str, job_name: &str, parent: Option<gtk::Window>) {
    let dialog = gtk::PrintDialog::new();
    dialog.set_title(&i18n("Print Message"));
    let settings = gtk::PrintSettings::new();
    // Names the job in the queue and seeds the filename when printing to a
    // file, which is otherwise "unknown".
    settings.set(gtk::PRINT_SETTINGS_OUTPUT_BASENAME, Some(job_name));
    if let Some((printer, page)) = LAST.with(|l| l.borrow().clone()) {
        settings.set_printer(&printer);
        dialog.set_page_setup(&page);
    }
    dialog.set_print_settings(&settings);

    let html = html.to_string();
    let job = job_name.to_string();
    let chooser = dialog.clone();
    let owner = parent.clone();
    chooser.setup(owner.as_ref(), gio::Cancellable::NONE, move |result| {
        let setup = match result {
            Ok(setup) => setup,
            // Dismissing the dialog arrives here as an error; it is the
            // ordinary way to change your mind, not a failure.
            Err(e) => {
                tracing::debug!("print dialog dismissed: {e}");
                return;
            }
        };
        let page = setup.page_setup();
        if let Some(printer) = setup.print_settings().printer() {
            LAST.with(|l| *l.borrow_mut() = Some((printer.to_string(), page.clone())));
        }
        if file_printer().is_none() {
            // Without a PDF writer there is no file to add a footer to, so
            // WebKit prints straight to the printer instead.
            tracing::warn!("no PDF writer: printing without the page footer");
            print_direct(&html, &setup);
            return;
        }
        render_pdf(&html, &page, &job, move |result| match result {
            Ok(path) => {
                let file = gio::File::for_path(&path);
                dialog.print_file(parent.as_ref(), Some(&setup), &file, gio::Cancellable::NONE, move |r| {
                    if let Err(e) = r {
                        tracing::warn!("printing failed: {e}");
                    }
                    let _ = std::fs::remove_file(&path);
                });
            }
            Err(e) => tracing::warn!("printing failed: {e}"),
        });
    });
}

/// WebKit prints `html` itself, without the footer: the fallback for a
/// system with no PDF writer.
fn print_direct(html: &str, setup: &gtk::PrintSetup) {
    let page = laid_out(&setup.page_setup(), PrintOptions { page_numbers: false, date: false, ..options() });
    let settings = setup.print_settings();
    load_for_print(html, move |view, finished| {
        let print = webkit6::PrintOperation::new(view);
        print.set_print_settings(&settings);
        print.set_page_setup(&page);
        print.connect_failed(|_, error| tracing::warn!("printing failed: {error}"));
        // Keep the operation alive until WebKit says it is done; dropping it
        // would cancel the job. `finished` follows `failed` too.
        let keep = RefCell::new(Some((print.clone(), finished)));
        print.connect_finished(move |_| {
            if let Some((_, finished)) = keep.borrow_mut().take() {
                finished();
            }
        });
        print.print();
    });
}

/// Load `html` into a view of its own and call `ready` once it has loaded,
/// with the view and a function to call when the view's work is over.
///
/// Not the reader's own view: the reader puts each message in an iframe,
/// which a print engine draws at its on-screen size, scrollbars and all,
/// clipping the rest.
fn load_for_print(html: &str, ready: impl FnOnce(&webkit6::WebView, Box<dyn FnOnce()>) + 'static) {
    // WebKit sizes a printed page from `gtk-xft-dpi`, which is -1 where no
    // desktop sets it (no settings portal, no XSettings): a one-page message
    // then printed as thirty thousand pages. GTK reads -1 as 96 dpi, so
    // saying 96 outright changes nothing on screen.
    if let Some(settings) = gtk::Settings::default() {
        if settings.gtk_xft_dpi() <= 0 {
            settings.set_gtk_xft_dpi(96 * 1024);
        }
    }
    let webview = crate::ui::message_view::new_preview_webview();
    PENDING.with(|p| p.borrow_mut().push(webview.clone()));
    let ready = RefCell::new(Some(ready));
    webview.connect_load_changed(move |view, event| {
        if event != webkit6::LoadEvent::Finished {
            return;
        }
        let Some(ready) = ready.borrow_mut().take() else { return };
        let done = view.clone();
        ready(
            view,
            Box::new(move || {
                PENDING.with(|p| p.borrow_mut().retain(|v| v != &done));
                // Its web process must not outlive it (#221).
                crate::memory_report::release_web_view(&done);
            }),
        );
    });
    webview.load_html(html, Some("https://hylki.localhost/print"));
}

/// The paper of `base` with Hylki's margins, never narrower than the
/// paper's own: those can carry the printer's limits.
fn laid_out(base: &gtk::PageSetup, options: PrintOptions) -> gtk::PageSetup {
    let page = base.copy();
    let mm = gtk::Unit::Mm;
    let (top, right, bottom, left) = margins_mm(options);
    page.set_top_margin(page.top_margin(mm).max(top), mm);
    page.set_right_margin(page.right_margin(mm).max(right), mm);
    page.set_bottom_margin(page.bottom_margin(mm).max(bottom), mm);
    page.set_left_margin(page.left_margin(mm).max(left), mm);
    page
}

/// Top, right, bottom and left margins for `options`, in millimetres.
fn margins_mm(options: PrintOptions) -> (f64, f64, f64, f64) {
    let bottom = if has_footer(options) { FOOTER_MARGIN_MM } else { MARGIN_MM };
    let left = if options.punch_margin { PUNCH_MARGIN_MM } else { MARGIN_MM };
    (MARGIN_MM, MARGIN_MM, bottom, left)
}

fn has_footer(options: PrintOptions) -> bool {
    options.page_numbers || options.date
}

/// Lay `html` out on `page`'s paper and write it as a PDF with the footer,
/// then call `done` with the file, which the caller deletes.
fn render_pdf(
    html: &str,
    page: &gtk::PageSetup,
    title: &str,
    done: impl FnOnce(Result<PathBuf, String>) + 'static,
) {
    let Some(printer) = file_printer() else {
        done(Err(i18n("No PDF writer is available on this system")));
        return;
    };
    let dir = std::env::temp_dir().join(format!("hylki-print-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        done(Err(e.to_string()));
        return;
    }
    let n = SERIAL.with(|s| {
        s.set(s.get() + 1);
        s.get()
    });
    let raw = dir.join(format!("{n}-webkit.pdf"));
    let out = dir.join(format!("{n}.pdf"));
    let options = options();
    let page = laid_out(page, options);
    let footer = Footer {
        options,
        printed: crate::datefmt::date_time(crate::datefmt::now()),
        left: page.left_margin(gtk::Unit::Points),
        right: page.right_margin(gtk::Unit::Points),
    };
    let title = title.to_string();

    load_for_print(html, move |view, finished| {
        // Only the printer and the file: the copies, page ranges and the
        // rest of what the dialog chose apply when the PDF is printed, and
        // must not apply twice.
        let settings = gtk::PrintSettings::new();
        settings.set_printer(&printer);
        settings.set(gtk::PRINT_SETTINGS_OUTPUT_URI, Some(&gio::File::for_path(&raw).uri()));
        settings.set(gtk::PRINT_SETTINGS_OUTPUT_FILE_FORMAT, Some("pdf"));

        let print = webkit6::PrintOperation::new(view);
        print.set_print_settings(&settings);
        print.set_page_setup(&page);
        // `finished` fires whether or not the job worked, and after `failed`,
        // so a failure has to be remembered rather than assumed away.
        let failed = Rc::new(RefCell::new(None::<String>));
        let mark = failed.clone();
        print.connect_failed(move |_, error| {
            *mark.borrow_mut() = Some(error.to_string());
        });
        let keep = RefCell::new(Some(print.clone()));
        let done = RefCell::new(Some((done, finished)));
        print.connect_finished(move |_| {
            keep.borrow_mut().take();
            let Some((done, finished)) = done.borrow_mut().take() else { return };
            finished();
            let result = match failed.borrow_mut().take() {
                Some(e) => Err(e),
                None => stamp(&raw, &out, &footer, &title).map(|()| out.clone()),
            };
            let _ = std::fs::remove_file(&raw);
            if result.is_err() {
                let _ = std::fs::remove_file(&out);
            }
            done(result);
        });
        print.print();
    });
}

/// What goes at the foot of every page, and where its two ends are.
struct Footer {
    options: PrintOptions,
    /// When it was printed, in the app's date and time style.
    printed: String,
    /// The page's left and right margins, in points.
    left: f64,
    right: f64,
}

/// Copy the PDF at `raw` to `out` page by page, drawing the footer on each.
fn stamp(raw: &Path, out: &Path, footer: &Footer, title: &str) -> Result<(), String> {
    let _guard = crate::ui::attachments_gallery::pdf_render_lock();
    let doc = poppler::Document::from_file(&gio::File::for_path(raw).uri(), None)
        .map_err(|e| e.to_string())?;
    let pages = doc.n_pages();
    // Every page is sized as it is drawn; this first size is never used.
    let surface = cairo::PdfSurface::new(595.0, 842.0, out).map_err(|e| e.to_string())?;
    let _ = surface.set_metadata(cairo::PdfMetadata::Title, title);
    let _ = surface.set_metadata(cairo::PdfMetadata::Creator, "Hylki");
    let cr = cairo::Context::new(&surface).map_err(|e| e.to_string())?;
    for n in 0..pages {
        let Some(page) = doc.page(n) else { continue };
        let (width, height) = page.size();
        surface.set_size(width, height).map_err(|e| e.to_string())?;
        cr.save().map_err(|e| e.to_string())?;
        page.render_for_printing(&cr);
        cr.restore().map_err(|e| e.to_string())?;
        draw_footer(&cr, footer, width, height, n as u32 + 1, pages as u32);
        cr.show_page().map_err(|e| e.to_string())?;
    }
    drop(cr);
    surface.finish();
    surface.status().map_err(|e| e.to_string())
}

/// The footer's two texts: the date at the left, the page number at the right.
fn footer_texts(footer: &Footer, page: u32, pages: u32) -> (Option<String>, Option<String>) {
    let date = footer.options.date.then(|| footer.printed.clone());
    let number = footer.options.page_numbers.then(|| {
        i18n_f("Page {page} of {pages}", &[("page", &page.to_string()), ("pages", &pages.to_string())])
    });
    (date, number)
}

fn draw_footer(cr: &cairo::Context, footer: &Footer, width: f64, height: f64, page: u32, pages: u32) {
    let (date, number) = footer_texts(footer, page, pages);
    if date.is_none() && number.is_none() {
        return;
    }
    let layout = pangocairo::functions::create_layout(cr);
    let mut font = pango::FontDescription::from_string("Sans");
    // In points, which are the PDF's own units; a plain size would be read
    // at the screen's 96 dpi and come out a third larger.
    font.set_absolute_size(8.0 * f64::from(pango::SCALE));
    layout.set_font_description(Some(&font));
    let baseline = height - FOOTER_BASELINE_MM * POINTS_PER_MM;
    cr.set_source_rgb(0.33, 0.33, 0.33);
    let show = |text: &str, right_aligned: bool| {
        layout.set_text(text);
        let (w, _) = layout.pixel_size();
        let ascent = f64::from(layout.baseline()) / f64::from(pango::SCALE);
        let x = if right_aligned { width - footer.right - f64::from(w) } else { footer.left };
        cr.move_to(x, baseline - ascent);
        pangocairo::functions::show_layout(cr, &layout);
    };
    if let Some(date) = date {
        show(&date, false);
    }
    if let Some(number) = number {
        show(&number, true);
    }
}

/// The name of a printer that writes to a file, for making the PDF.
///
/// Asks GTK rather than assuming: the file printer's name is translated, and
/// enumeration is asynchronous. `wait = true` blocks until the backends have
/// answered, which is why a literal "Print to File" can fail even when the
/// printer exists.
fn file_printer() -> Option<String> {
    if let Some(name) = FILE_PRINTER.with(|f| f.borrow().clone()) {
        return Some(name);
    }
    let found = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let collector = found.clone();
    gtk::enumerate_printers(
        move |printer| {
            if printer.is_virtual() && printer.accepts_pdf() {
                if let Ok(mut slot) = collector.lock() {
                    *slot = Some(printer.name().to_string());
                }
                return true; // stop at the first one
            }
            false
        },
        true,
    );
    let name = found.lock().ok()?.clone()?;
    FILE_PRINTER.with(|f| *f.borrow_mut() = Some(name.clone()));
    Some(name)
}

/// Show `html` as it will print, page by page, with a Print button that
/// prints it and a button that saves it as a PDF.
pub fn open(parent: &gtk::Window, html: &str, job_name: &str) {
    let win = adw::Window::builder()
        .transient_for(parent)
        .modal(false)
        .title(&i18n("Print Preview"))
        .default_width(900)
        .default_height(900)
        .build();

    // The PDF on screen, which Save as PDF copies; deleted with the window.
    let pdf: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let closed = Rc::new(Cell::new(false));
    {
        let pdf = pdf.clone();
        let closed = closed.clone();
        win.connect_close_request(move |_| {
            closed.set(true);
            if let Some(path) = pdf.borrow_mut().take() {
                let _ = std::fs::remove_file(path);
            }
            glib::Propagation::Proceed
        });
    }

    let stack = gtk::Stack::new();
    let spinner = adw::Spinner::builder()
        .width_request(32)
        .height_request(32)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    stack.add_named(&spinner, Some("loading"));
    let failure = adw::StatusPage::builder().icon_name("printer-symbolic").build();
    stack.add_named(&failure, Some("failed"));
    let pages = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(16)
        .margin_end(16)
        .build();
    let scroller = gtk::ScrolledWindow::builder().child(&pages).build();
    scroller.add_css_class("print-desk");
    stack.add_named(&scroller, Some("pages"));
    stack.set_visible_child_name("loading");

    // Toasts confirm a save without stealing focus from the preview.
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));

    let header = adw::HeaderBar::new();
    let print_btn = gtk::Button::builder()
        .label(&i18n("Print…"))
        .css_classes(["suggested-action"])
        .build();
    {
        let html = html.to_string();
        let job = job_name.to_string();
        let win = win.clone();
        print_btn.connect_clicked(move |_| {
            print_html(&html, &job, Some(win.clone().upcast()));
        });
    }
    header.pack_end(&print_btn);

    let save_btn = gtk::Button::builder().label(&i18n("Save as PDF…")).sensitive(false).build();
    {
        let pdf = pdf.clone();
        let job = job_name.to_string();
        let win = win.clone();
        let toasts = toasts.clone();
        save_btn.connect_clicked(move |_| {
            if let Some(path) = pdf.borrow().clone() {
                save_as_pdf(&path, &job, &win, &toasts);
            }
        });
    }
    header.pack_start(&save_btn);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));
    win.set_content(Some(&toolbar));

    // Escape closes it, as it does the shortcuts window.
    let keys = gtk::EventControllerKey::new();
    let closer = win.clone();
    keys.connect_key_pressed(move |_, keyval, _, _| {
        if keyval == gdk::Key::Escape {
            closer.close();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    win.add_controller(keys);
    win.present();

    // On the paper last printed on, or the locale's own.
    let paper = LAST
        .with(|l| l.borrow().as_ref().map(|(_, page)| page.clone()))
        .unwrap_or_else(gtk::PageSetup::new);
    let scale = f64::from(win.scale_factor().max(1));
    render_pdf(html, &paper, job_name, move |result| {
        if closed.get() {
            if let Ok(path) = result {
                let _ = std::fs::remove_file(path);
            }
            return;
        }
        let shown = result.and_then(|path| {
            let textures = page_textures(&path, scale);
            *pdf.borrow_mut() = Some(path);
            textures
        });
        match shown {
            Ok(textures) => {
                for (texture, width, height) in textures {
                    // The paper's own size on screen. The texture has more
                    // pixels than that on a high-density display, so the
                    // picture may shrink it, and its size is set outright: a
                    // picture in a scroller is otherwise given its minimum
                    // height and draws the page smaller.
                    let picture = gtk::Picture::for_paintable(&texture);
                    picture.set_can_shrink(true);
                    picture.set_size_request(width.round() as i32, height.round() as i32);
                    picture.set_halign(gtk::Align::Center);
                    picture.add_css_class("print-page");
                    pages.append(&picture);
                }
                save_btn.set_sensitive(true);
                stack.set_visible_child_name("pages");
            }
            Err(e) => {
                tracing::warn!("print preview failed: {e}");
                failure.set_title(&i18n("No Preview"));
                failure.set_description(Some(&e));
                stack.set_visible_child_name("failed");
            }
        }
    });
}

/// Every page of the PDF at `path` as a texture, with the page's width and
/// height on screen in logical pixels. Drawn at `scale` device pixels per logical one,
/// so text is sharp on a high-density display.
fn page_textures(path: &Path, scale: f64) -> Result<Vec<(gdk::Texture, f64, f64)>, String> {
    let _guard = crate::ui::attachments_gallery::pdf_render_lock();
    let doc = poppler::Document::from_file(&gio::File::for_path(path).uri(), None)
        .map_err(|e| e.to_string())?;
    // CSS pixels per point: a page shows at its printed size.
    let px = 96.0 / 72.0;
    let mut textures = Vec::new();
    for n in 0..doc.n_pages() {
        let Some(page) = doc.page(n) else { continue };
        let (w, h) = page.size();
        let zoom = px * scale;
        let (pw, ph) = ((w * zoom).round() as i32, (h * zoom).round() as i32);
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, pw, ph)
            .map_err(|e| e.to_string())?;
        {
            let cr = cairo::Context::new(&surface).map_err(|e| e.to_string())?;
            // A PDF page is transparent; paper is white.
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.paint().map_err(|e| e.to_string())?;
            cr.scale(zoom, zoom);
            page.render_for_printing(&cr);
        }
        surface.flush();
        let stride = surface.stride() as usize;
        let data = surface.data().map_err(|e| e.to_string())?.to_vec();
        // Cairo's ARGB32 is premultiplied BGRA in memory on a little-endian
        // machine, and the native-endian equivalent on any other.
        let format = if cfg!(target_endian = "little") {
            gdk::MemoryFormat::B8g8r8a8Premultiplied
        } else {
            gdk::MemoryFormat::A8r8g8b8Premultiplied
        };
        let texture = gdk::MemoryTexture::new(pw, ph, format, &glib::Bytes::from_owned(data), stride);
        textures.push((texture.upcast(), w * px, h * px));
    }
    Ok(textures)
}

/// Save the preview's PDF where the user picks.
///
/// The file comes from the portal's file chooser, so the place handed back
/// is one the sandbox may write to.
fn save_as_pdf(pdf: &Path, suggested_name: &str, parent: &adw::Window, toasts: &adw::ToastOverlay) {
    let chooser = gtk::FileDialog::new();
    chooser.set_title(&i18n("Save as PDF"));
    chooser.set_initial_name(Some(&format!("{suggested_name}.pdf")));
    let source = gio::File::for_path(pdf);
    let toasts = toasts.clone();
    chooser.save(Some(parent), gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else {
            // Cancelled: the ordinary way to change one's mind.
            return;
        };
        let name = file
            .basename()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "PDF".to_string());
        match source.copy(&file, gio::FileCopyFlags::OVERWRITE, gio::Cancellable::NONE, None) {
            Ok(()) => {
                tracing::info!(uri = %file.uri(), "saved a PDF");
                toasts.add_toast(adw::Toast::new(&i18n_f("Saved {name}", &[("name", &name)])));
            }
            Err(e) => {
                tracing::warn!("saving the PDF failed: {e}");
                toasts.add_toast(adw::Toast::new(&i18n_f(
                    "Could not save the PDF: {error}",
                    &[("error", &e.to_string())],
                )));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn footer(options: PrintOptions) -> Footer {
        Footer { options, printed: "6 October 2026, 09:00".into(), left: 42.5, right: 42.5 }
    }

    #[test]
    fn the_footer_says_what_settings_ask_for() {
        let both = PrintOptions { page_numbers: true, date: true, punch_margin: false };
        let (date, number) = footer_texts(&footer(both), 2, 5);
        assert_eq!(date.as_deref(), Some("6 October 2026, 09:00"));
        assert_eq!(number.as_deref(), Some("Page 2 of 5"));

        let none = PrintOptions { page_numbers: false, date: false, punch_margin: false };
        assert_eq!(footer_texts(&footer(none), 1, 1), (None, None));
    }

    #[test]
    fn margins_make_room_for_the_footer_and_the_punch() {
        let plain = PrintOptions { page_numbers: false, date: false, punch_margin: false };
        assert_eq!(margins_mm(plain), (MARGIN_MM, MARGIN_MM, MARGIN_MM, MARGIN_MM));
        let punched = PrintOptions { punch_margin: true, page_numbers: true, ..plain };
        let (_, right, bottom, left) = margins_mm(punched);
        assert_eq!(left, PUNCH_MARGIN_MM);
        assert_eq!(right, MARGIN_MM);
        // The footer's baseline sits inside the bottom margin, clear of the text.
        assert!(bottom > FOOTER_BASELINE_MM + 4.0);
    }

    #[test]
    fn a_page_with_a_footer_survives_the_stamp() {
        // A two-page PDF drawn by cairo stands in for WebKit's.
        let dir = std::env::temp_dir().join(format!("hylki-stamp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("raw.pdf");
        let out = dir.join("out.pdf");
        {
            let surface = cairo::PdfSurface::new(595.0, 842.0, &raw).unwrap();
            let cr = cairo::Context::new(&surface).unwrap();
            for _ in 0..2 {
                cr.rectangle(50.0, 50.0, 100.0, 100.0);
                cr.fill().unwrap();
                cr.show_page().unwrap();
            }
            drop(cr);
            surface.finish();
        }
        let options = PrintOptions { page_numbers: true, date: true, punch_margin: false };
        stamp(&raw, &out, &footer(options), "A subject").unwrap();
        let doc = poppler::Document::from_file(&gio::File::for_path(&out).uri(), None).unwrap();
        assert_eq!(doc.n_pages(), 2);
        let text = doc.page(1).unwrap().text().map(|t| t.to_string()).unwrap_or_default();
        assert!(text.contains("Page 2 of 2"), "{text}");
        assert!(text.contains("6 October 2026"), "{text}");
        assert_eq!(doc.title().as_deref(), Some("A subject"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
