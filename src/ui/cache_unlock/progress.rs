//! The window shown while the cache takes long to unlock: the keyring
//! waiting to be unlocked, or the one-time encryption or decryption of the
//! stored mail, which takes about a minute for every 2 GB. Nothing else is
//! on screen then, and without it the app looks as if it never started. A
//! start that takes less than a moment shows nothing.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::cache::at_rest::{self, Problem, Protection, Step};
use crate::i18n::i18n;

/// How long the unlock may take before the window appears.
const SHOW_AFTER: Duration = Duration::from_millis(400);

/// How the unlock ended.
pub(super) enum Outcome {
    Done(Result<Protection, Problem>),
    /// The user closed the window. Whatever was under way is left
    /// unfinished, and the cache is as it was: a copy being written is
    /// thrown away at the next start.
    Quit,
}

/// Run [`at_rest::prepare_reporting`] on a thread of its own, and show what
/// it is doing once it takes longer than [`SHOW_AFTER`].
pub(super) fn prepare(db: &Path, encrypt: bool) -> Outcome {
    let step: Arc<Mutex<Option<Step>>> = Arc::default();
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("cache-unlock".into())
        .spawn({
            let db = db.to_path_buf();
            let step = step.clone();
            move || {
                let report = |now: Step| {
                    if let Ok(mut current) = step.lock() {
                        *current = Some(now);
                    }
                };
                let _ = tx.send(at_rest::prepare_reporting(
                    &db,
                    encrypt,
                    &super::Keyring,
                    &report,
                ));
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cache: no thread for the unlock, so no progress window: {e}");
        return Outcome::Done(at_rest::prepare_reporting(
            db,
            encrypt,
            &super::Keyring,
            &|_| {},
        ));
    }
    match rx.recv_timeout(SHOW_AFTER) {
        Ok(result) => return Outcome::Done(result),
        Err(RecvTimeoutError::Disconnected) => return Outcome::Done(Err(stopped())),
        Err(RecvTimeoutError::Timeout) => {}
    }
    if !super::init_gtk() {
        // No display: wait as before, without a window.
        return Outcome::Done(rx.recv().unwrap_or_else(|_| Err(stopped())));
    }
    show_until_done(rx, step)
}

fn stopped() -> Problem {
    Problem::Storage("the unlock stopped without an answer".into())
}

/// Show the window and follow `step` until `rx` answers or the user quits.
fn show_until_done(
    rx: mpsc::Receiver<Result<Protection, Problem>>,
    step: Arc<Mutex<Option<Step>>>,
) -> Outcome {
    let view = View::new();
    let main_loop = glib::MainLoop::new(None, false);
    let outcome: Rc<RefCell<Option<Outcome>>> = Rc::default();

    let quit = {
        let main_loop = main_loop.clone();
        let outcome = outcome.clone();
        move || {
            outcome.borrow_mut().get_or_insert(Outcome::Quit);
            main_loop.quit();
        }
    };
    view.window.connect_close_request({
        let quit = quit.clone();
        move |_| {
            quit();
            glib::Propagation::Stop
        }
    });
    view.quit.connect_clicked(move |_| quit());

    // Never returns Break, so the source is still there to remove below
    // however the loop ended.
    let finished = Rc::new(Cell::new(false));
    let poll = glib::timeout_add_local(Duration::from_millis(100), {
        let view = view.clone();
        let main_loop = main_loop.clone();
        let outcome = outcome.clone();
        let step = step.clone();
        move || {
            if finished.get() {
                return glib::ControlFlow::Continue;
            }
            let answer = match rx.try_recv() {
                Ok(answer) => answer,
                Err(TryRecvError::Empty) => {
                    let now = step.lock().ok().and_then(|s| s.clone());
                    view.show(now.as_ref());
                    return glib::ControlFlow::Continue;
                }
                Err(TryRecvError::Disconnected) => Err(stopped()),
            };
            finished.set(true);
            outcome.borrow_mut().get_or_insert(Outcome::Done(answer));
            main_loop.quit();
            glib::ControlFlow::Continue
        }
    });

    view.show(step.lock().ok().and_then(|s| s.clone()).as_ref());
    view.window.present();
    main_loop.run();
    poll.remove();
    view.window.destroy();
    // Let the window's unmap reach the compositor before a dialog, or the
    // app's own window, takes its place.
    while glib::MainContext::default().iteration(false) {}
    outcome.take().unwrap_or(Outcome::Quit)
}

/// The window's parts that change with the step.
#[derive(Clone)]
struct View {
    window: adw::Window,
    heading: gtk::Label,
    body: gtk::Label,
    /// The spinner while waiting, the bar while converting, in one place
    /// so the window keeps its size.
    busy: gtk::Stack,
    spinner: adw::Spinner,
    bar: gtk::ProgressBar,
    quit: gtk::Button,
}

impl View {
    fn new() -> View {
        // The app's own icon theme, as the main window will have it, so the
        // app icon below is found by name.
        crate::desktop::icon_theme::register_icons(crate::config::load_privacy().desktop_icons);

        let icon = gtk::Image::from_icon_name(crate::APP_ID);
        icon.set_pixel_size(96);
        icon.set_margin_bottom(6);
        let heading = gtk::Label::new(None);
        heading.add_css_class("title-2");
        heading.set_wrap(true);
        heading.set_justify(gtk::Justification::Center);
        let body = gtk::Label::new(None);
        body.set_wrap(true);
        body.set_justify(gtk::Justification::Center);
        body.set_max_width_chars(40);
        let spinner = adw::Spinner::new();
        spinner.set_size_request(32, 32);
        spinner.set_halign(gtk::Align::Center);
        let bar = gtk::ProgressBar::new();
        bar.set_show_text(true);
        bar.set_pulse_step(0.08);
        bar.set_valign(gtk::Align::Center);
        let busy = gtk::Stack::new();
        busy.set_margin_top(6);
        busy.add_child(&spinner);
        busy.add_child(&bar);
        let quit = gtk::Button::with_label(&i18n("Quit"));
        quit.add_css_class("pill");
        quit.set_halign(gtk::Align::Center);
        quit.set_margin_top(12);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_start(36);
        content.set_margin_end(36);
        content.set_margin_bottom(30);
        content.append(&icon);
        content.append(&heading);
        content.append(&body);
        content.append(&busy);
        content.append(&quit);

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        header.set_show_title(false);
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));

        let window = adw::Window::new();
        window.set_title(Some("Hylki"));
        window.set_default_size(440, -1);
        window.set_resizable(false);
        window.set_content(Some(&toolbar));
        View {
            window,
            heading,
            body,
            busy,
            spinner,
            bar,
            quit,
        }
    }

    /// Say what `step` is doing. Before the first report the keyring is
    /// what is being waited for.
    fn show(&self, step: Option<&Step>) {
        let (heading, body) = match step {
            None | Some(Step::Keyring) => (
                i18n("Waiting for the Keyring"),
                i18n("Mail stored on this computer is encrypted with a key stored in the keyring. Unlock the keyring if it asks."),
            ),
            Some(Step::Copying { encrypt, .. } | Step::Checking { encrypt }) => (
                converting(*encrypt),
                i18n("This happens once and can take a few minutes. If you quit, your mail is left as it was."),
            ),
        };
        if self.heading.text() != heading.as_str() {
            self.heading.set_text(&heading);
        }
        if self.body.text() != body.as_str() {
            self.body.set_text(&body);
        }
        match step {
            None | Some(Step::Keyring) => self.busy.set_visible_child(&self.spinner),
            Some(Step::Copying {
                staging, expected, ..
            }) => {
                self.busy.set_visible_child(&self.bar);
                self.bar.set_fraction(copied(staging, *expected));
                self.bar.set_text(None);
            }
            Some(Step::Checking { .. }) => {
                self.busy.set_visible_child(&self.bar);
                self.bar.set_text(Some(&i18n("Checking the copy")));
                self.bar.pulse();
            }
        }
    }
}

fn converting(encrypt: bool) -> String {
    if encrypt {
        i18n("Encrypting Stored Mail")
    } else {
        i18n("Decrypting Stored Mail")
    }
}

/// How far the copy at `staging` has grown towards `expected` bytes, held
/// short of the end: the copy is checked after it is written.
fn copied(staging: &Path, expected: u64) -> f64 {
    let written = std::fs::metadata(staging).map_or(0, |m| m.len());
    if expected == 0 {
        return 0.0;
    }
    (written as f64 / expected as f64).clamp(0.0, 0.99)
}
