//! Building the reader's WebView and the WebContext it shares with print
//! preview and the composer.

use super::links::{decode_data_uri, default_image_name, is_launchable_uri};
use super::menu::strip_navigation_items;
use super::MessageView;
use crate::i18n::i18n;
use adw::prelude::*;
use relm4::prelude::*;
use webkit6::prelude::{PolicyDecisionExt, WebViewExt};

/// A web view for the print preview: same sandboxing as the reader's, since it
/// shows the same message.
pub(crate) fn new_preview_webview() -> webkit6::WebView {
    build_webview("print preview")
}

thread_local! {
    /// One web context for every view the app creates (reader, pop-outs, print
    /// preview, rich editors), configured for showing documents rather than
    /// browsing (issue #106). The default context runs WebKit's browser cache
    /// model — big in-memory resource cache, back/forward page cache — which
    /// for a mail reader only hoards dead documents: every render loads a fresh
    /// unique URI, so nothing cached is ever revisited, and the web process
    /// grew by hundreds of megabytes per reading session and never shrank.
    /// `DocumentViewer` turns those caches off, and the memory-pressure limit
    /// makes the web process actively release memory instead of waiting for
    /// system-wide pressure that arrives only after the damage is done.
    static WEB_CONTEXT: webkit6::WebContext = {
        let mut pressure = webkit6::MemoryPressureSettings::new();
        // In MB. Trimming starts at a fraction of this (~1/3), so the web
        // process settles in the low hundreds of MB instead of the gigabytes
        // reported in #106.
        pressure.set_memory_limit(512);
        let ctx = webkit6::WebContext::builder()
            .memory_pressure_settings(&pressure)
            .build();
        ctx.set_cache_model(webkit6::CacheModel::DocumentViewer);
        ctx
    };
}

/// The shared document-viewer web context (see [`WEB_CONTEXT`]).
pub(crate) fn shared_web_context() -> webkit6::WebContext {
    WEB_CONTEXT.with(|c| c.clone())
}

/// Keep Tab and Shift+Tab moving through the window after they leave a
/// page. WebKitGTK parents a hidden GtkTextView inside every view for its
/// key-binding translator, and it can take focus: when Tab ran off the
/// end of the document, GTK's focus chain went into the view and gave
/// that text view the keyboard, which then kept every Tab for itself.
/// The translator forwards keys to it directly, so it never needs focus.
pub(crate) fn keep_tab_out_of_key_translator(webview: &webkit6::WebView) {
    fn unfocusable(view: &gtk::Widget) {
        let mut child = view.first_child();
        while let Some(c) = child {
            if c.is::<gtk::TextView>() {
                c.set_can_focus(false);
                c.set_focusable(false);
            }
            child = c.next_sibling();
        }
    }
    unfocusable(webview.upcast_ref());
    // In case a WebKit release builds the translator later.
    webview.connect_realize(|v| unfocusable(v.upcast_ref()));
}

pub(super) fn new_webview() -> webkit6::WebView {
    build_webview("message view")
}

/// Create a sandboxed WebView: no JavaScript or dev tools, smooth scrolling, and
/// links routed to the external browser.
fn build_webview(role: &'static str) -> webkit6::WebView {
    // A user-content manager with a script message handler lets the wrapper
    // document notify us (e.g. a double-clicked conversation header).
    let ucm = webkit6::UserContentManager::new();
    ucm.register_script_message_handler("hylki", None);
    let webview = webkit6::WebView::builder()
        .web_context(&shared_web_context())
        .user_content_manager(&ucm)
        .build();
    crate::desktop::memory_report::register_web_view(&webview, role);
    keep_tab_out_of_key_translator(&webview);

    let settings = webkit6::Settings::new();
    // JavaScript runs only in our own (trusted) wrapper document — it sizes each
    // message's iframe to its content. Every email body is embedded in a
    // `sandbox`ed iframe WITHOUT `allow-scripts`, so message scripts never run.
    settings.set_enable_javascript(true);
    settings.set_enable_developer_extras(false);
    // JS console output lands on stdout, where the tracing/console-mode
    // pipeline can pick it up — a silent script error in the wrapper document
    // otherwise kills card interactions with no trace at all.
    settings.set_enable_write_console_messages_to_stdout(true);
    webview.set_settings(&settings);

    offer_save_image(&webview);
    launch_links(&webview);
    show_link_tooltips(&webview);
    webview
}

/// "Save Image As…" on a right-clicked image. WebKit's own item hands the
/// image to a download, which needs a `WebKitNetworkSession` destination
/// handler we don't have — so it silently did nothing. Every image the reader
/// shows inline (attached photos, and `cid:` images since 1.7.1) is a `data:`
/// URI whose bytes are already in the document, so swap in our own item that
/// decodes them and opens a real save dialog.
fn offer_save_image(webview: &webkit6::WebView) {
    webview.connect_context_menu(|view, menu, hit| {
        strip_navigation_items(menu);
        if !hit.context_is_image() {
            return false; // not an image — leave the default menu alone
        }
        let Some(uri) = hit.image_uri() else {
            return false;
        };
        let Some((mime, data)) = decode_data_uri(&uri) else {
            // A remote image: WebKit's own download-backed item is the only way
            // to save it, so leave the menu untouched.
            return false;
        };

        // Replace the stock item in place so the menu keeps its familiar order.
        let Some(stock) = menu
            .items()
            .into_iter()
            .find(|i| i.stock_action() == webkit6::ContextMenuAction::DownloadImageToDisk)
        else {
            return false;
        };
        let position = menu.items().iter().position(|i| i == &stock).unwrap_or(0);

        let action = gtk::gio::SimpleAction::new("vireo-save-image", None);
        let window = view.root().and_downcast::<gtk::Window>();
        action.connect_activate(move |_, _| {
            let dialog = gtk::FileDialog::builder()
                .initial_name(default_image_name(&mime))
                .title(i18n("Save Image"))
                .build();
            let data = data.clone();
            dialog.save(window.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
                if let Ok(file) = res {
                    if let Some(path) = file.path() {
                        let _ = std::fs::write(path, &data);
                    }
                }
            });
        });
        menu.remove(&stock);
        menu.insert(
            &webkit6::ContextMenuItem::from_gaction(&action, "Save Image As…", None),
            position as i32,
        );
        false // show the (edited) menu
    });
}

/// Navigation decisions: a clicked link, or a request for a new window, is
/// handed to the desktop when it is a web or mail link and never followed
/// in the reader; a reload is refused (#301). Anything else gets WebKit's
/// default.
fn launch_links(webview: &webkit6::WebView) {
    webview.connect_decide_policy(|view, decision, decision_type| {
        // Links (including ones inside sandboxed message iframes, and `_blank`
        // links that request a new window) open in the external browser.
        let is_nav = decision_type == webkit6::PolicyDecisionType::NavigationAction;
        let is_new_window = decision_type == webkit6::PolicyDecisionType::NewWindowAction;
        if is_nav || is_new_window {
            if let Some(nav) = decision.downcast_ref::<webkit6::NavigationPolicyDecision>() {
                if let Some(mut action) = nav.navigation_action() {
                    // The documents are loaded from strings under a made-up
                    // `hylki.localhost` base, so a reload has nothing to fetch
                    // and shows "Connection refused" (#301).
                    if action.navigation_type() == webkit6::NavigationType::Reload {
                        decision.ignore();
                        return true;
                    }
                    let clicked = is_new_window
                        || action.navigation_type() == webkit6::NavigationType::LinkClicked;
                    if clicked {
                        if let Some(uri) = action.request().and_then(|r| r.uri()) {
                            // Only web and mail links reach the desktop's handlers.
                            // An HTML body keeps its own `href` values verbatim, so
                            // without this a message could hand `file://`, `smb://`
                            // or any scheme a third-party app has registered to that
                            // app on a single click.
                            if is_launchable_uri(&uri) {
                                let window = view.root().and_downcast::<gtk::Window>();
                                crate::ui::launch::open_link(&uri, window.as_ref());
                            } else {
                                tracing::warn!(
                                    "refused to open a link with an unsupported scheme: {}",
                                    uri.split(':').next().unwrap_or("?")
                                );
                            }
                        }
                        decision.ignore();
                        return true;
                    }
                }
            }
        }
        false
    });
}

/// Show the target URL as a tooltip while hovering a link. WebKit doesn't do
/// this itself; we track the hovered link and answer GTK's query-tooltip,
/// re-querying whenever the hovered link changes so it updates immediately.
fn show_link_tooltips(webview: &webkit6::WebView) {
    let hovered: std::rc::Rc<std::cell::RefCell<Option<String>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    webview.set_has_tooltip(true);

    let hq = hovered.clone();
    webview.connect_query_tooltip(move |_view, _x, _y, _keyboard, tooltip| {
        match hq.borrow().as_deref() {
            Some(uri) => {
                tooltip.set_text(Some(uri));
                true
            }
            None => false,
        }
    });

    let hm = hovered.clone();
    webview.connect_mouse_target_changed(move |view, hit, _modifiers| {
        let uri = if hit.context_is_link() {
            hit.link_uri().map(|s| s.to_string())
        } else {
            None
        };
        if *hm.borrow() != uri {
            *hm.borrow_mut() = uri;
            view.trigger_tooltip_query();
        }
    });
}

impl MessageView {
    /// Run a script on the page, ignoring its result: a live patch whose
    /// failure leaves the page as it was.
    pub(super) fn run_js(&self, js: &str) {
        self.webview
            .evaluate_javascript(js, None, None, None::<&gtk::gio::Cancellable>, |_| {});
    }

    /// Run a script on the page and log a failure as `"{what} failed: …"`.
    pub(super) fn run_js_or_warn(&self, js: &str, what: &'static str) {
        self.webview.evaluate_javascript(
            js,
            None,
            None,
            None::<&gtk::gio::Cancellable>,
            move |r| {
                if let Err(e) = r {
                    tracing::warn!("{what} failed: {e}");
                }
            },
        );
    }
}
