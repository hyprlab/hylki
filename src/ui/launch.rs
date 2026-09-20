//! Opening a link outside Hylki. The road out differs by host and every
//! road has been seen to fail without a word, so each attempt has a fallback
//! and the last one says what happened instead of leaving a click that does
//! nothing (#202).
//!
//! Outside Flatpak, GIO launches the desktop's default handler directly and
//! the portal (`UriLauncher`) is the fallback. Inside the sandbox the portal
//! is the only road out, and it is spoken to directly over D-Bus
//! ([`portal_request`]) rather than through GIO's launcher: GIO's blocking
//! call fires the request and returns without waiting for the answer, so a
//! portal whose direct default-handler launch is broken (seen on Fedora 44
//! with xdg-desktop-portal 1.22) makes every link click look dead. Watching
//! the request's `Response` means a failed quiet launch is retried with the
//! portal's own app chooser, whose launch machinery works where the direct
//! one does not, and whose "always open with" sticks in the permission
//! store. The same request plumbing opens attachments (`portal_open_file` in
//! the gallery), which is where the chooser retry was first worked out.
//!
//! On top of all that sits the user's own choice (#232): Settings names the
//! browser links open in, and it is launched by its desktop entry before any
//! of the above is reached. A sandbox cannot see the host's browsers, so
//! there the choice is between the desktop's default and the portal's
//! chooser, which remembers what it is told.

use crate::i18n::{i18n, i18n_f};
use gtk::gio;
use gtk::gio::prelude::AppInfoExt;
use gtk::glib;
use gtk::glib::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// Whether this process runs inside a Flatpak sandbox.
pub fn in_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// The stored choice that means "let the desktop's app chooser ask, every
/// time" rather than naming one browser (#232).
pub const ASK: &str = "ask";

thread_local! {
    /// Settings → System → Links: empty for the desktop's default handler,
    /// [`ASK`] for the chooser, otherwise the desktop entry id of the browser
    /// links open in. Kept here rather than read from disk per click, and
    /// updated by the app whenever the setting changes.
    static BROWSER: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Point every following link at `choice` (see [`BROWSER`]).
pub fn set_browser(choice: &str) {
    BROWSER.with(|b| *b.borrow_mut() = choice.to_string());
}

/// One browser the user can send links to: the desktop entry that launches
/// it, and the name to show.
#[derive(Debug, Clone)]
pub struct Browser {
    pub id: String,
    pub name: String,
}

/// The browsers this process can actually launch — the desktop entries
/// registered for `https`, minus Hylki itself.
///
/// Inside the Flatpak sandbox this is empty: the host's desktop entries are
/// not visible there, and nothing in them could be run from inside anyway.
/// Links then go out through the portal, which is what the "Ask each time"
/// choice is for.
pub fn browsers() -> Vec<Browser> {
    let mut found: Vec<Browser> = Vec::new();
    for info in gio::AppInfo::all_for_type("x-scheme-handler/https") {
        let Some(id) = info.id().map(|s| s.to_string()) else { continue };
        if id.starts_with(crate::APP_ID) || !info.should_show() {
            continue;
        }
        if found.iter().any(|b| b.id == id) {
            continue;
        }
        found.push(Browser { id, name: info.display_name().to_string() });
    }
    found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    found
}

/// Open a web or mail link in the app the desktop has for it — or, when one
/// was chosen in Settings, in that browser (#232).
pub fn open_link(uri: &str, parent: Option<&gtk::Window>) {
    tracing::info!("link: opening {}", describe(uri));
    let choice = BROWSER.with(|b| b.borrow().clone());
    // A mailto: link is not a browser's business: it goes to whatever the
    // desktop opens mail with, whichever browser was chosen for the web.
    let web = !uri.split(':').next().unwrap_or("").eq_ignore_ascii_case("mailto");
    if web && choice == ASK {
        portal_open_uri(uri.to_string(), true, parent.cloned());
        return;
    }
    if web && !choice.is_empty() {
        match gio::DesktopAppInfo::new(&choice) {
            Some(info) => {
                match info.launch_uris(&[uri], gio::AppLaunchContext::NONE) {
                    Ok(()) => return,
                    Err(e) => tracing::warn!(
                        "link: {choice} could not be launched ({e}); falling back to the default"
                    ),
                }
            }
            // The chosen browser is not here: a sandbox that cannot see the
            // host's applications, or an entry that has since been removed.
            None => tracing::warn!("link: {choice} is not available here; using the default"),
        }
    }
    default_open(uri, parent.cloned());
}

/// The road out when no browser was chosen: the desktop's own default
/// handler, GIO first outside the sandbox and the portal within it.
fn default_open(uri: &str, parent: Option<gtk::Window>) {
    if in_flatpak() {
        portal_open_uri(uri.to_string(), false, parent);
        return;
    }
    if let Err(e) = gio::AppInfo::launch_default_for_uri(uri, gio::AppLaunchContext::NONE) {
        tracing::warn!("link: gio launch failed ({e}), trying the portal");
        let owned = parent.clone();
        gtk::UriLauncher::new(uri).launch(parent.as_ref(), gio::Cancellable::NONE, move |res| {
            if let Err(e) = res {
                tracing::warn!("link: portal launch also failed: {e}");
                link_failed_dialog(owned.as_ref(), &e.to_string());
            }
        });
    }
}

/// What the log calls a link: its scheme and host, never its path or query,
/// which is where unsubscribe tokens and tracking ids live.
fn describe(uri: &str) -> String {
    let (scheme, rest) = uri.split_once(':').unwrap_or((uri, ""));
    if scheme.eq_ignore_ascii_case("mailto") {
        return "a mailto: link".to_string();
    }
    let host = rest.trim_start_matches('/').split(['/', '?', '#']).next().unwrap_or("");
    format!("{scheme}://{host}")
}

/// Open `uri` through the OpenURI portal. The quiet attempt (`ask == false`)
/// retries once with the app chooser on any failure; a cancelled chooser
/// (response 1) is an answer on either attempt, never a reason to ask again
/// (issue #65 covered the same for files); any other chooser failure gets
/// the dialog.
fn portal_open_uri(uri: String, ask: bool, parent: Option<gtk::Window>) {
    let retry_uri = uri.clone();
    let retry_parent = parent.clone();
    let params = move |token: &str| {
        let options = glib::VariantDict::new(None);
        options.insert_value("handle_token", &token.to_variant());
        if ask {
            options.insert_value("ask", &true.to_variant());
        }
        glib::Variant::tuple_from_iter(["".to_variant(), uri.to_variant(), options.end()])
    };
    portal_request("OpenURI", None, params, move |res| match res {
        Err(e) => {
            tracing::warn!("link: portal OpenURI call failed: {e}");
            // Outside the sandbox the portal is a convenience, not the only
            // road: an "Ask each time" that finds no portal still opens the
            // link, in the desktop's default handler.
            if in_flatpak() {
                link_failed_dialog(retry_parent.as_ref(), &e);
            } else {
                default_open(&retry_uri, retry_parent.clone());
            }
        }
        Ok(0) | Ok(1) => {}
        Ok(code) if !ask => {
            tracing::warn!("link: portal answered {code}; retrying with the chooser");
            portal_open_uri(retry_uri.clone(), true, retry_parent.clone());
        }
        Ok(code) => link_failed_dialog(
            retry_parent.as_ref(),
            &i18n_f("the portal answered response code {code}", &[("code", &code.to_string())]),
        ),
    });
}

/// Both roads failed: say so, and what still works.
fn link_failed_dialog(parent: Option<&gtk::Window>, error: &str) {
    let dialog = gtk::AlertDialog::builder()
        .message(&i18n("The link could not be opened"))
        // No Flatseal toggle can help here: portal access is not a
        // permission, and the failure is inside the portal's own launcher.
        .detail(i18n_f(
            "The desktop portal reported: {error}\n\n\
             \u{2022} Right-click the link, copy it, and open it in your browser\n\
             \u{2022} Updating \u{201c}xdg-desktop-portal\u{201d} and logging back in may fix opening links directly",
            &[("error", error)],
        ))
        .modal(true)
        .build();
    dialog.show(parent);
}

/// Make one request of the OpenURI portal, speaking the protocol directly
/// over GIO's D-Bus, and hand its outcome to `done`: `Ok(code)` is the
/// request's `Response` (0 success, 1 cancelled, 2 failure), `Err` a call
/// that never became a request. The subscription to the `Response` signal is
/// set up FIRST, on a path derived from our own `handle_token`, so a fast
/// reply cannot race it. `params` builds the method's arguments once the
/// token is known; `fd_list` carries the descriptors `OpenFile` passes.
pub(crate) fn portal_request(
    method: &str,
    fd_list: Option<gio::UnixFDList>,
    params: impl FnOnce(&str) -> glib::Variant,
    done: impl Fn(Result<u32, String>) + 'static,
) {
    let done: Rc<dyn Fn(Result<u32, String>)> = Rc::new(done);
    let conn = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        Ok(c) => c,
        Err(e) => {
            done(Err(e.to_string()));
            return;
        }
    };
    let token = crate::rng::nonce(16)
        .map(|t| t.replace('-', "_"))
        .unwrap_or_else(|_| format!("hylki{}", std::process::id()));
    let sender_token = conn
        .unique_name()
        .map(|n| n.trim_start_matches(':').replace('.', "_"))
        .unwrap_or_default();
    let request_path = format!("/org/freedesktop/portal/desktop/request/{sender_token}/{token}");

    let sub_id: Rc<RefCell<Option<gio::SignalSubscriptionId>>> = Rc::new(RefCell::new(None));
    let sub = sub_id.clone();
    let sig_conn = conn.clone();
    let on_response = done.clone();
    let id = conn.signal_subscribe(
        Some("org.freedesktop.portal.Desktop"),
        Some("org.freedesktop.portal.Request"),
        // The signal's name on the wire: never a translated string, or the
        // answer is waited for under a name it will never arrive as.
        Some("Response"),
        Some(&request_path),
        None,
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, params| {
            if let Some(id) = sub.borrow_mut().take() {
                sig_conn.signal_unsubscribe(id);
            }
            on_response(Ok(params.child_value(0).get::<u32>().unwrap_or(2)));
        },
    );
    *sub_id.borrow_mut() = Some(id);

    let params = params(&token);
    let call_conn = conn.clone();
    conn.call_with_unix_fd_list(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.OpenURI",
        method,
        Some(&params),
        None,
        gio::DBusCallFlags::NONE,
        10_000,
        fd_list.as_ref(),
        gio::Cancellable::NONE,
        move |res| {
            if let Err(e) = res {
                if let Some(id) = sub_id.borrow_mut().take() {
                    call_conn.signal_unsubscribe(id);
                }
                done(Err(e.to_string()));
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::describe;

    #[test]
    fn a_link_is_logged_by_its_host_only() {
        assert_eq!(describe("https://shop.example/unsubscribe?t=SECRET"), "https://shop.example");
        assert_eq!(describe("http://a.example#frag"), "http://a.example");
        assert_eq!(describe("mailto:someone@example.com?subject=x"), "a mailto: link");
    }
}
