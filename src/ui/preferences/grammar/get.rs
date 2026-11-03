//! A downloadable checker's row: what it is, a Download button when it is
//! the one chosen, the download's progress and a Cancel button while it
//! runs, and Remove once it is on this computer, whichever is chosen then,
//! so a download can always be undone.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use super::Save;
use crate::i18n::{i18n, i18n_f};
use crate::services::grammar::install::{megabytes, Progress};
use crate::services::grammar::{harper, local, Engine};

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Download,
    Cancel,
    Remove,
}

struct Inner {
    row: adw::ActionRow,
    bar: gtk::ProgressBar,
    button: gtk::Button,
    action: Cell<Action>,
    engine: Engine,
    running: RefCell<Option<Arc<Progress>>>,
    error: RefCell<Option<String>>,
    changed: RefCell<Option<Rc<dyn Fn()>>>,
    save: Save,
}

#[derive(Clone)]
pub(super) struct Get(Rc<Inner>);

impl Get {
    /// The row for `engine`, Harper or LanguageTool.
    pub(super) fn new(engine: Engine, save: Save) -> Self {
        let row = adw::ActionRow::new();
        row.set_subtitle_lines(3);
        let bar = gtk::ProgressBar::new();
        bar.set_valign(gtk::Align::Center);
        bar.set_width_request(120);
        bar.set_visible(false);
        row.add_suffix(&bar);
        let button = gtk::Button::new();
        button.set_valign(gtk::Align::Center);
        row.add_suffix(&button);
        let get = Get(Rc::new(Inner {
            row,
            bar,
            button: button.clone(),
            action: Cell::new(Action::Download),
            engine,
            running: RefCell::new(None),
            error: RefCell::new(None),
            changed: RefCell::new(None),
            save,
        }));
        let weak = Rc::downgrade(&get.0);
        button.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                Get(inner).clicked();
            }
        });
        get
    }

    pub(super) fn widget(&self) -> &adw::ActionRow {
        &self.0.row
    }

    /// Called when what the row shows changes by itself (a download ends).
    pub(super) fn on_change(&self, f: Rc<dyn Fn()>) {
        *self.0.changed.borrow_mut() = Some(f);
    }

    /// Whether a download is running.
    pub(super) fn busy(&self) -> bool {
        self.0.running.borrow().is_some()
    }

    /// Show the state of the download, with `chosen` the checker chosen
    /// and `on` when checking is. Once downloaded the row stays, to remove
    /// it, even when checking is off or another checker is chosen.
    pub(super) fn show(&self, chosen: Engine, on: bool) {
        let s = &self.0;
        let engine = s.engine;
        let (name, available, installed, size) = match engine {
            Engine::Harper => (
                "Harper",
                harper::available(),
                harper::installed(),
                harper::download_size(),
            ),
            _ => (
                "LanguageTool",
                local::available(),
                local::installed(),
                local::download_size(),
            ),
        };
        let running = s.running.borrow().clone();
        s.row
            .set_visible(chosen == engine || installed.is_some() || running.is_some());
        s.row
            .set_sensitive(on || installed.is_some() || running.is_some());
        s.row.set_title(name);
        s.button.remove_css_class("suggested-action");
        s.bar.set_visible(false);
        s.button.set_visible(true);
        if let Some(progress) = running {
            let done = progress.done.load(Ordering::Relaxed);
            let total = progress.total.load(Ordering::Relaxed).max(1);
            s.bar.set_visible(true);
            s.bar.set_fraction(done as f64 / total as f64);
            s.row.set_subtitle(&i18n_f(
                "Downloading: {done} of {total}",
                &[("done", &megabytes(done)), ("total", &megabytes(total))],
            ));
            self.set_action(Action::Cancel);
        } else if !available {
            s.row
                .set_subtitle(&i18n("Not available for this computer."));
            s.button.set_visible(false);
        } else if let Some(version) = installed {
            s.row.set_subtitle(&i18n_f(
                "Version {version}, on this computer.",
                &[("version", &version)],
            ));
            self.set_action(Action::Remove);
        } else {
            let error = s.error.borrow().clone();
            s.row.set_subtitle(&error.unwrap_or_else(|| {
                i18n_f(
                    "Not downloaded yet. The download is {size}.",
                    &[("size", &megabytes(size))],
                )
            }));
            s.button.add_css_class("suggested-action");
            self.set_action(Action::Download);
        }
    }

    fn set_action(&self, action: Action) {
        self.0.action.set(action);
        self.0.button.set_label(&match action {
            Action::Download => i18n("Download"),
            Action::Cancel => i18n("Cancel"),
            Action::Remove => i18n("Remove"),
        });
    }

    fn changed(&self) {
        let f = self.0.changed.borrow().clone();
        if let Some(f) = f {
            f();
        }
    }

    fn clicked(&self) {
        let engine = self.0.engine;
        match self.0.action.get() {
            Action::Cancel => {
                if let Some(p) = self.0.running.borrow().as_ref() {
                    p.cancel.store(true, Ordering::Relaxed);
                }
            }
            Action::Remove => {
                self.0.button.set_sensitive(false);
                let get = self.clone();
                glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || match engine {
                        Engine::Harper => harper::remove(),
                        _ => local::remove(),
                    })
                    .await
                    .unwrap_or_else(|_| Err(i18n("The files could not be removed.")));
                    if let Err(why) = result {
                        *get.0.error.borrow_mut() = Some(why);
                    }
                    get.0.button.set_sensitive(true);
                    get.changed();
                });
            }
            Action::Download => self.download(engine),
        }
    }

    fn download(&self, engine: Engine) {
        let progress = Arc::new(Progress::default());
        *self.0.running.borrow_mut() = Some(progress.clone());
        self.0.error.borrow_mut().take();
        // The checker being downloaded is the one used, from now on.
        (self.0.save)(&|s| s.engine = Some(engine));
        self.changed();
        let ticking = Rc::new(Cell::new(true));
        {
            let (get, ticking) = (self.clone(), ticking.clone());
            glib::timeout_add_local(Duration::from_millis(250), move || {
                if !ticking.get() {
                    return glib::ControlFlow::Break;
                }
                get.show(engine, true);
                glib::ControlFlow::Continue
            });
        }
        let get = self.clone();
        glib::spawn_future_local(async move {
            let p = progress.clone();
            let result = gtk::gio::spawn_blocking(move || match engine {
                Engine::Harper => harper::download(&p),
                _ => local::download(&p),
            })
            .await
            .unwrap_or_else(|_| Err(i18n("The download stopped unexpectedly.")));
            ticking.set(false);
            get.0.running.borrow_mut().take();
            match result {
                Ok(()) => {
                    if engine == Engine::Local {
                        local::warm();
                    }
                }
                Err(_) if progress.cancel.load(Ordering::Relaxed) => {}
                Err(why) => {
                    tracing::warn!("grammar checker download failed: {why}");
                    *get.0.error.borrow_mut() = Some(why);
                }
            }
            get.changed();
        });
    }
}
