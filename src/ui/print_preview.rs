//! Print preview, shown inside Hylki (issue #16).
//!
//! The print dialog's own Preview button belongs to the portal and, in a
//! sandbox, produces nothing. Writing a PDF and handing it to an external viewer
//! was tried and is a chain of things that can each fail quietly — a temporary
//! file, a URI, the document portal, and whatever the desktop has registered for
//! `application/pdf`. None of that is needed to answer the only question a
//! preview is asked: *what will come out of the printer?*
//!
//! So the preview is a window of Hylki's own, showing the message with its print
//! styling applied — the header block, light colours — laid out on a page-shaped
//! sheet. Printing from that window prints the very thing being looked at.

use adw::prelude::*;
use webkit6::prelude::WebViewExt;
use crate::i18n::{i18n, i18n_f};

/// Open the print dialog for a web view and print it.
///
/// Split in two deliberately: `GtkPrintDialog` collects the settings through a
/// callback, and WebKit prints with them. WebKit's own `run_dialog` spins a
/// nested main loop, and polling a glib future inside one aborts the process.
pub fn print_webview(webview: &webkit6::WebView, job_name: &str, parent: Option<gtk::Window>) {
    print_webview_then(webview, job_name, parent, || {});
}

/// [`print_webview`], with `done` called once the job has finished, failed,
/// or the dialog was dismissed — for a caller whose view exists only for
/// this print (see [`print_html`]).
fn print_webview_then(
    webview: &webkit6::WebView,
    job_name: &str,
    parent: Option<gtk::Window>,
    done: impl FnOnce() + 'static,
) {
    let print = webkit6::PrintOperation::new(webview);
    let dialog = gtk::PrintDialog::new();
    dialog.set_title(&i18n("Print Message"));

    let settings = gtk::PrintSettings::new();
    // Names the job in the queue and seeds the filename when printing to a file,
    // which is otherwise "unknown".
    settings.set(gtk::PRINT_SETTINGS_OUTPUT_BASENAME, Some(job_name));
    dialog.set_print_settings(&settings);

    let done = std::rc::Rc::new(std::cell::RefCell::new(Some(Box::new(done) as Box<dyn FnOnce()>)));
    dialog.setup(
        parent.as_ref(),
        gtk::gio::Cancellable::NONE,
        move |result| match result {
            Ok(setup) => {
                print.set_print_settings(&setup.print_settings());
                print.set_page_setup(&setup.page_setup());
                print.connect_failed(|_, error| {
                    tracing::warn!("printing failed: {error}");
                });
                // Keep the operation alive until WebKit says it is done; dropping
                // it here would cancel the job. `finished` follows `failed` too.
                let keep = std::cell::RefCell::new(Some(print.clone()));
                print.connect_finished(move |_| {
                    keep.borrow_mut().take();
                    if let Some(f) = done.borrow_mut().take() {
                        f();
                    }
                });
                print.print();
            }
            // Dismissing the dialog arrives here as an error; it is the ordinary
            // way to change your mind, not a failure.
            Err(e) => {
                tracing::debug!("print dialog dismissed: {e}");
                if let Some(f) = done.borrow_mut().take() {
                    f();
                }
            }
        },
    );
}

/// Print `html` without showing it: render it in a view of its own, then print
/// that once it has loaded.
///
/// Printing the reader's own view is what a print command would naturally do,
/// and it is wrong here — the reader puts each message in an iframe, which a
/// print engine draws at its on-screen size, scrollbars and all, clipping the
/// rest. So Ctrl+P renders the same page-shaped document the preview shows.
pub fn print_html(html: &str, job_name: &str, parent: Option<gtk::Window>) {
    let webview = crate::ui::message_view::new_preview_webview();
    // Nothing else holds this view, and a dropped view never finishes loading.
    // One at a time is enough: starting another print replaces it.
    PENDING.with(|slot| *slot.borrow_mut() = Some(webview.clone()));

    let job = job_name.to_string();
    let done = std::rc::Rc::new(std::cell::Cell::new(false));
    webview.connect_load_changed(move |view, event| {
        if event != webkit6::LoadEvent::Finished || done.replace(true) {
            return;
        }
        let view_done = view.clone();
        print_webview_then(view, &job, parent.clone(), move || {
            // The job is over: this view has nothing left to do, and its
            // web process must not outlive it (#221).
            PENDING.with(|slot| {
                let mut slot = slot.borrow_mut();
                if slot.as_ref() == Some(&view_done) {
                    slot.take();
                }
            });
            crate::memory_report::release_web_view(&view_done);
        });
    });
    webview.load_html(html, Some("https://hylki.localhost/print"));
}

thread_local! {
    /// The view of a print that has been asked for but has not loaded yet.
    static PENDING: std::cell::RefCell<Option<webkit6::WebView>> =
        const { std::cell::RefCell::new(None) };
}

/// The name of a printer that writes to a file, for saving a PDF.
///
/// Asks GTK rather than assuming: the file printer's name is translated, and
/// enumeration is asynchronous — `wait = true` blocks until the backends have
/// answered, which is why a literal "Print to File" can fail even when the
/// printer exists.
fn file_printer() -> Option<String> {
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
    let name = found.lock().ok()?.clone();
    name
}

/// Write the view to a PDF the user picks, with no print dialog in the way.
///
/// The file comes from the portal's file chooser, so the path handed back is one
/// the sandbox may write to; its URI comes from GIO rather than being built by
/// hand, because a mail subject makes a filename full of spaces.
fn save_as_pdf(
    webview: &webkit6::WebView,
    suggested_name: &str,
    parent: &adw::Window,
    toasts: &adw::ToastOverlay,
) {
    let Some(printer) = file_printer() else {
        toasts.add_toast(adw::Toast::new("No PDF writer is available on this system"));
        tracing::warn!("no printer available that can write a PDF");
        return;
    };

    let chooser = gtk::FileDialog::new();
    chooser.set_title(&i18n("Save as PDF"));
    chooser.set_initial_name(Some(&format!("{suggested_name}.pdf")));

    let webview = webview.clone();
    let toasts = toasts.clone();
    chooser.save(Some(parent), gtk::gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else {
            // Cancelled: the ordinary way to change one's mind.
            return;
        };
        let uri = file.uri().to_string();
        let name = file
            .basename()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "PDF".to_string());

        let settings = gtk::PrintSettings::new();
        settings.set_printer(&printer);
        settings.set(gtk::PRINT_SETTINGS_OUTPUT_URI, Some(&uri));
        settings.set(gtk::PRINT_SETTINGS_OUTPUT_FILE_FORMAT, Some("pdf"));

        let print = webkit6::PrintOperation::new(&webview);
        print.set_print_settings(&settings);
        // `finished` fires whether or not the job worked, and after `failed`, so
        // success has to be remembered rather than assumed — otherwise a failed
        // save still says "Saved", which is worse than saying nothing.
        let failed = std::rc::Rc::new(std::cell::Cell::new(false));
        let failed_toasts = toasts.clone();
        let mark = failed.clone();
        print.connect_failed(move |_, error| {
            mark.set(true);
            tracing::warn!("saving the PDF failed: {error}");
            failed_toasts.add_toast(adw::Toast::new(&i18n_f("Could not save the PDF: {error}", &[("error", &error.to_string())])));
        });
        let keep = std::cell::RefCell::new(Some(print.clone()));
        let done_toasts = toasts.clone();
        print.connect_finished(move |_| {
            keep.borrow_mut().take();
            if failed.get() {
                return;
            }
            tracing::info!(%uri, "saved a PDF");
            done_toasts.add_toast(adw::Toast::new(&format!("Saved {name}")));
        });
        print.print();
    });
}

/// Show `html` as a print preview, with a Print button that prints it.
pub fn open(parent: &adw::ApplicationWindow, html: &str, job_name: &str) {
    let win = adw::Window::builder()
        .transient_for(parent)
        .modal(false)
        .title(&i18n("Print Preview"))
        .default_width(820)
        .default_height(900)
        .build();

    let webview = crate::ui::message_view::new_preview_webview();
    webview.set_vexpand(true);
    webview.load_html(html, Some("https://hylki.localhost/print-preview"));
    {
        // The preview's view goes with its window (#221).
        let webview = webview.clone();
        win.connect_close_request(move |_| {
            crate::memory_report::release_web_view(&webview);
            gtk::glib::Propagation::Proceed
        });
    }

    // Toasts confirm a save without stealing focus from the preview.
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&webview));

    let header = adw::HeaderBar::new();
    let print_btn = gtk::Button::builder()
        .label(&i18n("Print…"))
        .css_classes(["suggested-action"])
        .build();
    {
        let webview = webview.clone();
        let job = job_name.to_string();
        let win = win.clone();
        print_btn.connect_clicked(move |_| {
            // Print the preview itself, so what comes out is what is on screen.
            print_webview(&webview, &job, Some(win.clone().upcast()));
        });
    }
    header.pack_end(&print_btn);

    let save_btn = gtk::Button::builder().label(&i18n("Save as PDF…")).build();
    {
        let webview = webview.clone();
        let job = job_name.to_string();
        let win = win.clone();
        let toasts = toasts.clone();
        save_btn.connect_clicked(move |_| {
            save_as_pdf(&webview, &job, &win, &toasts);
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
        if keyval == gtk::gdk::Key::Escape {
            closer.close();
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    win.add_controller(keys);

    win.present();
}

/// Styling that turns the reader's document into something page-shaped: the
/// print-only header revealed, light colours, and a sheet with margins on a grey
/// desk. Applied on screen only — printing uses the document's own `@media
/// print` rules, which say the same thing.
pub const PREVIEW_STYLES: &str = "\
    html{background:#e0e0e0;}\
    body{background:#e0e0e0 !important;color:#000 !important;padding:24px 0;}\
    .vireo-print-sheet{background:#fff;color:#000;width:21cm;max-width:calc(100% - 32px);\
      min-height:29.7cm;margin:0 auto;padding:1.6cm 1.5cm;box-sizing:border-box;\
      box-shadow:0 2px 12px rgba(0,0,0,0.25);}\
    .vireo-print-hdr{display:block !important;padding:0 0 10pt;margin:0 0 12pt;\
      border-bottom:1pt solid #999;font:10pt/1.45 system-ui,sans-serif;color:#000;}\
    .vireo-print-subject{font-size:14pt;font-weight:700;margin:0 0 8pt;}\
    .vireo-print-row{margin:0 0 2pt;}\
    .vireo-print-label{font-weight:700;}\
    @media print{\
      html,body{background:#fff !important;padding:0;}\
      .vireo-print-sheet{width:auto;min-height:0;margin:0;padding:0;box-shadow:none;}\
    }";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preview_shows_what_printing_hides() {
        // The header exists in the document but is hidden on screen; the preview
        // is the one place it must be visible without printing.
        assert!(PREVIEW_STYLES.contains(".vireo-print-hdr{display:block !important"));
        // Light, on a page-shaped sheet.
        assert!(PREVIEW_STYLES.contains("color:#000 !important"));
        assert!(PREVIEW_STYLES.contains("width:21cm"));
        // Printing the preview must not print the grey desk or the shadow.
        assert!(PREVIEW_STYLES.contains("@media print"));
        assert!(PREVIEW_STYLES.contains("box-shadow:none"));
    }
}
