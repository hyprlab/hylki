//! One running instance: a second launch hands its arguments to the first
//! over D-Bus and exits.

use crate::APP_ID;

/// What an argument or an opened file's URI asks for, by its scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UriKind {
    /// A `mailto:` link: a new message.
    Mailto,
    /// A Message-ID link (`mid:`, #130): open that message.
    Mid,
    /// A path or any other URI: a saved message, or files to attach.
    Other,
}

impl UriKind {
    pub(crate) fn of(arg: &str) -> Self {
        if arg.starts_with("mailto:") {
            UriKind::Mailto
        } else if arg.starts_with("mid:") || arg.starts_with("MID:") {
            UriKind::Mid
        } else {
            UriKind::Other
        }
    }
}

/// A view a launcher action opens (`hylki --contacts`, `hylki --attachments`):
/// the desktop file's Contacts and Attachments actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartView {
    Contacts,
    Attachments,
}

impl StartView {
    const ALL: [StartView; 2] = [StartView::Contacts, StartView::Attachments];

    /// The command-line flag that asks for this view.
    pub(crate) fn flag(self) -> &'static str {
        match self {
            StartView::Contacts => "--contacts",
            StartView::Attachments => "--attachments",
        }
    }

    /// The app action that shows it, which a second launch activates on the
    /// running instance.
    pub(crate) fn action(self) -> &'static str {
        match self {
            StartView::Contacts => "show-contacts",
            StartView::Attachments => "show-attachments",
        }
    }

    /// The view flags taken out of `args` (GTK would reject them as unknown,
    /// as it would `--hidden`), and the last one asked for.
    pub(crate) fn strip(mut args: Vec<String>) -> (Vec<String>, Option<StartView>) {
        let view = args
            .iter()
            .rev()
            .find_map(|a| Self::ALL.into_iter().find(|v| v.flag() == a));
        args.retain(|a| !Self::ALL.iter().any(|v| v.flag() == a));
        (args, view)
    }
}

/// Whether another instance already owns the app's D-Bus name.
pub(crate) fn primary_instance_running() -> bool {
    use gtk::glib::prelude::ToVariant;
    let Ok(conn) = gtk::gio::bus_get_sync(gtk::gio::BusType::Session, gtk::gio::Cancellable::NONE)
    else {
        return false;
    };
    conn.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&(APP_ID,).to_variant()),
        None,
        gtk::gio::DBusCallFlags::NONE,
        3000,
        gtk::gio::Cancellable::NONE,
    )
    .ok()
    .and_then(|v| v.get::<(bool,)>())
    .is_some_and(|(owned,)| owned)
}

/// Forward this invocation to the running primary instance and return once
/// it has been accepted: `Open` with any mailto:/mid:/file arguments, plain
/// `Activate` (present the window) otherwise.
///
/// The hand-off goes through a throwaway `GtkApplication` registered as a
/// remote instance rather than a hand-rolled D-Bus call, because of the
/// launcher's activation token (issue #187). GNOME hands it to us in
/// `XDG_ACTIVATION_TOKEN` (`DESKTOP_STARTUP_ID` on X11), and GTK 4 removes
/// both from the environment in a library constructor -- before `main` even
/// runs -- keeping the value for itself. Only GApplication's own remote
/// path puts that stashed token into the platform data of the `Activate`
/// call, and only with it does the primary's window complete the launch:
/// without it, GNOME Shell keeps the busy pointer and its "starting" state
/// for the whole 15 s timeout, during which clicking the icon again does
/// nothing at all. (The token is also what lets the window take the focus
/// from whoever launched us: Nautilus's "Send by email", a mailto: link in
/// a browser.)
///
/// A `view` (a launcher action) is asked for with that app action instead of
/// `Activate`; it goes through the same remote path, so it carries the token.
///
/// Returns false when no primary answered after all (it quit between the
/// name check and the hand-off): the caller then starts normally.
pub(crate) fn relay_to_primary(args: &[String], view: Option<StartView>) -> bool {
    use gtk::gio::prelude::ApplicationExt;
    let files: Vec<gtk::gio::File> = args
        .iter()
        .skip(1)
        .map(|a| {
            if UriKind::of(a) == UriKind::Other {
                // A plain path or a non-mailto URI (e.g. file://): the same
                // normalization GLib applies to HANDLES_OPEN arguments.
                gtk::gio::File::for_commandline_arg(a)
            } else {
                gtk::gio::File::for_uri(a)
            }
        })
        .collect();
    let remote = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    if let Err(e) = remote.register(gtk::gio::Cancellable::NONE) {
        tracing::warn!("hand-off to the running instance failed: {e}");
        return false;
    }
    if !remote.is_remote() {
        // The name was free after all: we own it now, and releasing it
        // (dropping the object) lets the real app take it below.
        tracing::info!("the running instance left before the hand-off; starting instead");
        drop(remote);
        return false;
    }
    tracing::info!(
        "handing off to the running instance ({} argument(s))",
        files.len()
    );
    use gtk::gio::prelude::ActionGroupExt;
    match view {
        // A primary still starting up has not added the action yet: it
        // only comes to the front then.
        Some(view) if files.is_empty() && remote.has_action(view.action()) => {
            remote.activate_action(view.action(), None);
            // Unlike Activate and Open, an action goes out without waiting
            // for a reply: flush it before the process ends.
            if let Some(conn) = remote.dbus_connection() {
                let _ = conn.flush_sync(gtk::gio::Cancellable::NONE);
            }
        }
        _ if files.is_empty() => remote.activate(),
        _ => remote.open(&files, ""),
    }
    // The call has been delivered (both are synchronous round trips). The
    // process ends right after this; letting the object go instead would
    // only print GLib's "did not unregister from D-Bus" warning to the
    // terminal, so it is deliberately left alive.
    std::mem::forget(remote);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_flags_are_taken_out_of_the_arguments() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            StartView::strip(args(&["hylki", "--contacts"])),
            (args(&["hylki"]), Some(StartView::Contacts))
        );
        assert_eq!(
            StartView::strip(args(&["hylki", "--contacts", "mailto:", "--attachments"])),
            (args(&["hylki", "mailto:"]), Some(StartView::Attachments))
        );
        assert_eq!(
            StartView::strip(args(&["hylki", "a.pdf"])),
            (args(&["hylki", "a.pdf"]), None)
        );
    }

    #[test]
    fn arguments_are_told_apart_by_their_scheme() {
        assert_eq!(UriKind::of("mailto:a@example.org"), UriKind::Mailto);
        assert_eq!(UriKind::of("mailto:"), UriKind::Mailto);
        assert_eq!(UriKind::of("mid:abc@example.org"), UriKind::Mid);
        assert_eq!(UriKind::of("MID:abc@example.org"), UriKind::Mid);
        // Only those spellings: the schemes are matched as written.
        assert_eq!(UriKind::of("MAILTO:a@example.org"), UriKind::Other);
        assert_eq!(UriKind::of("Mid:abc"), UriKind::Other);
        assert_eq!(UriKind::of("/home/me/letter.eml"), UriKind::Other);
        assert_eq!(UriKind::of("file:///home/me/a.pdf"), UriKind::Other);
        assert_eq!(UriKind::of("--hidden"), UriKind::Other);
        assert_eq!(UriKind::of(""), UriKind::Other);
    }
}
