//! WebKit hanging on the host's fonts (#296).
//!
//! Inside the Flatpak, fontconfig offers WebKit the runtime's fonts and the
//! host's, mounted under `/run/host`. On some hosts that set sends WebKit's
//! web process into a loop in its first font lookup (Skia's fontconfig font
//! manager walking the family names, at full CPU, for good): no document
//! ever finishes loading, so the reader shows its spinner forever and the
//! composer body never takes a key, while the mail side works normally.
//! Nothing in the host's set points at the culprit in advance, and the
//! runtime's own fonts are known to load.
//!
//! So a document that has not finished loading after a while is checked
//! against a probe: a tiny page in a web process of its own. A probe that
//! loads means the hold-up is the document, and nothing changes. A probe
//! that hangs too means WebKit cannot get past the fonts: web processes are
//! then started with `FONTCONFIG_FILE` naming a config that hides the
//! host's fonts, a second probe confirms that this is what fixes it, and
//! the stuck views are restarted on it. The choice is kept for the runtime
//! commit it was made under, so a runtime update tries every font again.
//!
//! Only web processes get the smaller set: the app's own fontconfig has
//! loaded before the variable is set, so the interface keeps every font.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use webkit6::prelude::*;

/// The config the Flatpak ships (see the manifest): the runtime's own
/// fonts and settings, with nothing from the host, not even its font
/// folders or caches (see the file itself).
const FALLBACK_CONF: &str = "/app/share/hylki/fontconfig/fallback.conf";

/// How long a document may take before the fonts are suspected. Well past a
/// normal render, which is milliseconds; a message waiting on a slow remote
/// image can pass it, and only costs a probe.
const STALL: Duration = Duration::from_secs(8);

/// How long the probe gets: a fresh web process and a page of five lines.
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// A page that cannot finish loading without its fonts: the script lays the
/// text out before the load can complete, whether or not the view is shown.
const PROBE_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\"></head><body>\
<p style=\"font-family:sans-serif\">Hylki Aa 0123</p>\
<p style=\"font-family:serif\">Aa</p>\
<p style=\"font-family:monospace\">Aa</p>\
<p style=\"font-family:system-ui,-apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif\">Aa</p>\
<script>document.body.getBoundingClientRect();</script>\
</body></html>";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Nothing known yet: slow documents are watched.
    Watching,
    /// A probe is out.
    Probing,
    /// Settled for this session, one way or the other: nothing is watched.
    Settled,
    /// Not available here (not the Flatpak, or the user set the fonts up).
    Off,
}

/// A load that has not finished yet, kept so it can be issued again on a
/// fresh web process.
struct Pending {
    view: glib::WeakRef<webkit6::WebView>,
    html: String,
    base: Option<String>,
    generation: u64,
}

thread_local! {
    static STATE: Cell<State> = const { Cell::new(State::Watching) };
    static CONF: RefCell<Option<String>> = const { RefCell::new(None) };
    static PENDING: RefCell<Vec<Pending>> = const { RefCell::new(Vec::new()) };
    static WATCHED: RefCell<Vec<glib::WeakRef<webkit6::WebView>>> = const { RefCell::new(Vec::new()) };
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// The fallback config, when this build has one. `HYLKI_FONT_FALLBACK_CONF`
/// names another file, for trying the whole path outside the Flatpak.
fn fallback_conf() -> Option<String> {
    if let Ok(path) = std::env::var("HYLKI_FONT_FALLBACK_CONF") {
        return std::path::Path::new(&path).is_file().then_some(path);
    }
    (crate::platform::is_flatpak() && std::path::Path::new(FALLBACK_CONF).is_file())
        .then(|| FALLBACK_CONF.to_string())
}

/// The runtime the choice is tied to: its commit in the Flatpak.
fn runtime_key() -> String {
    std::fs::read_to_string("/.flatpak-info")
        .ok()
        .and_then(|info| {
            info.lines()
                .find_map(|l| l.strip_prefix("runtime-commit=").map(|c| c.trim().to_string()))
        })
        .unwrap_or_else(|| "host".to_string())
}

/// Debug hook: `HYLKI_TEST_FONT_HANG=1` makes every watched document and the
/// probe hang the way the fonts do, until the fallback is in use, so the
/// whole detection and recovery can be run on a machine without the fault.
/// A watched document is simply not loaded (the reader's CSP would refuse a
/// looping script); the probe runs one, so its process really spins.
fn simulated_hang() -> bool {
    std::env::var_os("HYLKI_TEST_FONT_HANG").is_some() && !fallback_active()
}

fn fallback_active() -> bool {
    let Some(conf) = CONF.with(|c| c.borrow().clone()) else { return false };
    std::env::var("FONTCONFIG_FILE").is_ok_and(|v| v == conf)
}

fn probe_html() -> String {
    if simulated_hang() {
        PROBE_HTML.replace("<script>", "<script>for(;;){}")
    } else {
        PROBE_HTML.to_string()
    }
}

/// At startup, before any web view loads: put a fallback settled under this
/// runtime back in place. `widget` is any widget of the app, whose font map
/// is loaded first so the interface keeps the host's fonts.
pub fn startup(widget: &impl IsA<gtk::Widget>) {
    let Some(conf) = fallback_conf() else {
        STATE.with(|s| s.set(State::Off));
        return;
    };
    // Someone who set the fonts up by hand (the workaround in #296 was
    // exactly this) is not overridden.
    if let Ok(own) = std::env::var("FONTCONFIG_FILE") {
        tracing::info!("web fonts: FONTCONFIG_FILE is set ({own}); leaving WebKit's fonts alone");
        STATE.with(|s| s.set(State::Off));
        return;
    }
    CONF.with(|c| *c.borrow_mut() = Some(conf.clone()));
    let runtime = runtime_key();
    match crate::config::load_web_font_fallback() {
        Some(saved) if saved == runtime => {
            let _ = widget.pango_context().list_families();
            std::env::set_var("FONTCONFIG_FILE", &conf);
            STATE.with(|s| s.set(State::Settled));
            tracing::info!("web fonts: WebKit uses the runtime's fonts only (it hung on the host's under this runtime)");
        }
        Some(_) => {
            crate::config::save_web_font_fallback(None);
            tracing::info!("web fonts: the runtime has changed since WebKit hung on the host's fonts; trying them again");
        }
        None => {}
    }
}

/// `view.load_html`, watched: a document that does not finish in time has
/// the fonts checked, and is loaded again on a fresh web process if they
/// were what held it up.
pub fn load_html(view: &webkit6::WebView, html: &str, base: Option<&str>) {
    if !simulated_hang() {
        view.load_html(html, base);
    }
    if STATE.with(Cell::get) == State::Off || STATE.with(Cell::get) == State::Settled {
        return;
    }
    watch(view);
    let generation = GENERATION.with(|g| {
        g.set(g.get() + 1);
        g.get()
    });
    PENDING.with(|p| {
        let mut p = p.borrow_mut();
        p.retain(|e| e.view.upgrade().is_some_and(|v| v != *view));
        p.push(Pending {
            view: view.downgrade(),
            html: html.to_string(),
            base: base.map(str::to_string),
            generation,
        });
    });
    if STATE.with(Cell::get) != State::Watching {
        return;
    }
    let weak = view.downgrade();
    glib::timeout_add_local_once(STALL, move || {
        let Some(view) = weak.upgrade() else { return };
        let still = PENDING.with(|p| {
            p.borrow().iter().any(|e| e.generation == generation && e.view.upgrade().is_some_and(|v| v == view))
        });
        if still && STATE.with(Cell::get) == State::Watching {
            suspect();
        }
    });
}

/// Clear a view's pending load once it finishes.
fn watch(view: &webkit6::WebView) {
    let known = WATCHED.with(|w| {
        let mut w = w.borrow_mut();
        w.retain(|v| v.upgrade().is_some());
        if w.iter().any(|v| v.upgrade().is_some_and(|v| v == *view)) {
            return true;
        }
        w.push(view.downgrade());
        false
    });
    if known {
        return;
    }
    view.connect_load_changed(|view, event| {
        if event != webkit6::LoadEvent::Finished {
            return;
        }
        // A load cancelled by a newer one finishes too; only the document
        // that is actually in the view ends the watch.
        let uri = view.uri();
        PENDING.with(|p| {
            p.borrow_mut().retain(|e| {
                let mine = e.view.upgrade().is_some_and(|v| v == *view);
                !(mine && (e.base.is_none() || e.base.as_deref() == uri.as_deref()))
            })
        });
    });
}

/// A document is late: probe with the fonts as they are, and if that hangs,
/// with the fallback.
fn suspect() {
    STATE.with(|s| s.set(State::Probing));
    tracing::warn!("web fonts: a document has not loaded in {} s; checking whether WebKit can load its fonts", STALL.as_secs());
    probe(|loaded| {
        if loaded {
            tracing::info!("web fonts: the probe loaded, so the late document is slow on its own");
            settle();
            return;
        }
        let Some(conf) = CONF.with(|c| c.borrow().clone()) else { return settle() };
        tracing::warn!("web fonts: WebKit hangs on the fonts it is given; trying the runtime's fonts only");
        std::env::set_var("FONTCONFIG_FILE", &conf);
        probe(move |loaded| {
            if !loaded {
                std::env::remove_var("FONTCONFIG_FILE");
                tracing::warn!("web fonts: WebKit hangs with the runtime's fonts too, so the fonts are not the cause");
                settle();
                return;
            }
            crate::config::save_web_font_fallback(Some(&runtime_key()));
            tracing::warn!("web fonts: WebKit loads with the runtime's fonts only; restarting the stuck views on them");
            let stuck = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
            STATE.with(|s| s.set(State::Settled));
            for e in stuck {
                let Some(view) = e.view.upgrade() else { continue };
                // A closed composer's view outlives it (#221); it must not
                // be handed a new web process.
                if view.root().is_none() {
                    continue;
                }
                // The process is spinning, not dead: it has to be ended for
                // the view to get a new one, started with the new setting.
                view.terminate_web_process();
                view.load_html(&e.html, e.base.as_deref());
            }
        });
    });
}

fn settle() {
    STATE.with(|s| s.set(State::Settled));
    PENDING.with(|p| p.borrow_mut().clear());
}

/// Load the probe page in a web process of its own (a separate context
/// shares nothing with the stuck ones) and report whether it finished in
/// time. The process is ended either way.
fn probe(done: impl FnOnce(bool) + 'static) {
    let settings = webkit6::Settings::new();
    settings.set_enable_javascript(true);
    let view = webkit6::WebView::builder()
        .web_context(&webkit6::WebContext::new())
        .settings(&settings)
        .build();
    let done: Rc<RefCell<Option<Box<dyn FnOnce(bool)>>>> = Rc::new(RefCell::new(Some(Box::new(done))));
    {
        let done = done.clone();
        view.connect_load_changed(move |view, event| {
            if event != webkit6::LoadEvent::Finished {
                return;
            }
            let Some(f) = done.borrow_mut().take() else { return };
            let view = view.clone();
            glib::idle_add_local_once(move || view.terminate_web_process());
            f(true);
        });
    }
    view.load_html(&probe_html(), Some("https://hylki-font-probe.localhost/"));
    glib::timeout_add_local_once(PROBE_TIMEOUT, move || {
        let f = done.borrow_mut().take();
        view.terminate_web_process();
        if let Some(f) = f {
            f(false);
        }
    });
}
