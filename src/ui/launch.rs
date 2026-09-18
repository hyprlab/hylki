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

use crate::i18n::{i18n, i18n_f};
use gtk::gio;
use gtk::glib;
use gtk::glib::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// Whether this process runs inside a Flatpak sandbox.
pub fn in_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// Open a web or mail link in the app the desktop has for it.
pub fn open_link(uri: &str, parent: Option<&gtk::Window>) {
    tracing::info!("link: opening {}", describe(uri));
    if in_flatpak() {
        portal_open_uri(uri.to_string(), false, parent.cloned());
    } else if let Err(e) = gio::AppInfo::launch_default_for_uri(uri, gio::AppLaunchContext::NONE) {
        tracing::warn!("link: gio launch failed ({e}), trying the portal");
        let owned = parent.cloned();
        gtk::UriLauncher::new(uri).launch(parent, gio::Cancellable::NONE, move |res| {
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
            link_failed_dialog(retry_parent.as_ref(), &e);
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
