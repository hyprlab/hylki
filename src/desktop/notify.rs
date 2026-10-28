//! Desktop (system) notifications via GNotification.
//!
//! Uses the running `gtk::Application`'s `send_notification`, which routes through
//! the desktop notification portal automatically under Flatpak. The click actions
//! ([`PRESENT_ACTION`], [`OPEN_MESSAGE_ACTION`]) are registered on the application
//! in `app/notifications.rs` (they need the app's message channel to navigate).

use crate::i18n::i18n;
use gtk::gio;
use gtk::prelude::*;

/// App action (bare name) that raises the main window. Used by error alerts.
pub(crate) const PRESENT_ACTION: &str = "present-window";
/// App action (bare name) that raises the window and opens a specific message.
/// Its target is a `(account_id, folder_id, message_id)` `(uuu)` variant.
pub(crate) const OPEN_MESSAGE_ACTION: &str = "open-message";
/// Notification button actions (#38): act on the notified message without
/// raising the window. Same `(uuu)` target as [`OPEN_MESSAGE_ACTION`].
pub(crate) const MARK_READ_ACTION: &str = "notify-mark-read";
pub(crate) const ARCHIVE_ACTION: &str = "notify-archive";
pub(crate) const DELETE_ACTION: &str = "notify-delete";
/// These two raise the window as well: a composer needs it.
pub(crate) const REPLY_ACTION: &str = "notify-reply";
pub(crate) const FORWARD_ACTION: &str = "notify-forward";
pub(crate) const SPAM_ACTION: &str = "notify-spam";
/// App action (bare name) that raises the window and the newest composer:
/// the click on a "message ready" alert ([`compose_ready`]).
pub(crate) const PRESENT_COMPOSE_ACTION: &str = "present-compose";

const COMPOSE_READY_ID: &str = "vireo-compose-ready";
/// Whether a [`compose_ready`] alert is (or may be) showing, so that
/// withdrawing it costs nothing on the focus changes where there is none.
static COMPOSE_READY_POSTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Build the (title, body) for a new-mail notification. `others` is how many
/// *additional* new messages arrived beyond the newest one shown.
///
/// With `show_content` off, neither the sender nor the subject appears: a
/// notification is drawn on the lock screen by default on GNOME, where a shoulder
/// is all it takes to read who is writing to you about what.
fn compose_new_mail(
    from: &str,
    subject: &str,
    others: usize,
    show_content: bool,
) -> (String, String) {
    if !show_content {
        return (
            if others == 0 {
                i18n("New message")
            } else {
                format!("{} new messages", others + 1)
            },
            String::new(),
        );
    }
    if others == 0 {
        (
            if from.is_empty() {
                i18n("New message")
            } else {
                from.to_string()
            },
            subject.to_string(),
        )
    } else {
        (
            format!("{} new messages", others + 1),
            // Lead with the newest so the summary is still informative.
            if subject.is_empty() {
                from.to_string()
            } else {
                format!("{from} — {subject}")
            },
        )
    }
}

/// Notification id for an account's new-mail toast (one per account, so a later
/// batch replaces the previous rather than stacking).
fn mail_id(account_id: u32) -> String {
    format!("hylki-mail-{account_id}")
}

/// Post (or replace) the new-mail notification for an account. Clicking it opens
/// the newest message (`folder_id` + `message_id`) in the main window.
///
/// `in_place` says the anchor message still sits in `folder_id` (as opposed to
/// having been filed elsewhere by a mail filter, where `folder_id` is only the
/// folder to show); the action buttons need that to act on the right message.
pub(crate) fn new_mail(
    account_id: u32,
    folder_id: u32,
    message_id: u32,
    from: &str,
    subject: &str,
    others: usize,
    in_place: bool,
) {
    let (title, body) = compose_new_mail(
        from,
        subject,
        others,
        crate::config::load_privacy().notification_content,
    );
    let n = gio::Notification::new(&title);
    if !body.is_empty() {
        n.set_body(Some(&body));
    }
    n.set_priority(gio::NotificationPriority::Normal);
    let target = (account_id, folder_id, message_id).to_variant();
    n.set_default_action_and_target_value(&format!("app.{OPEN_MESSAGE_ACTION}"), Some(&target));
    // Action buttons (#38), only when the notification covers exactly one
    // message — on a "3 new messages" summary, "Mark as Read" acting on just
    // the newest would do less than it says — and only when that message is
    // still where the buttons will look for it. Which appear, up to three,
    // is the user's choice (#244).
    if others == 0 && in_place {
        use crate::config::NotificationButton;
        let buttons = crate::config::load_notification_buttons();
        for button in NotificationButton::ALL
            .into_iter()
            .filter(|b| buttons.get(*b))
        {
            let (label, action) = match button {
                NotificationButton::MarkRead => (i18n("Mark as Read"), MARK_READ_ACTION),
                NotificationButton::Archive => (i18n("Archive"), ARCHIVE_ACTION),
                NotificationButton::Delete => (i18n("Delete"), DELETE_ACTION),
                NotificationButton::Reply => (i18n("Reply"), REPLY_ACTION),
                NotificationButton::Forward => (i18n("Forward"), FORWARD_ACTION),
                NotificationButton::Spam => (i18n("Mark as Spam"), SPAM_ACTION),
            };
            n.add_button_with_target_value(&label, &format!("app.{action}"), Some(&target));
        }
    }
    send(&mail_id(account_id), &n);
    POSTED.with(|p| p.borrow_mut().insert(account_id, (folder_id, message_id)));
    sound_for_new_mail();
}

/// The sound for new mail, if Settings asks for one and the desktop does
/// not ask for quiet. Part of [`new_mail`]; on its own while the window is
/// in front, where no notification is posted (#337).
pub(crate) fn sound_for_new_mail() {
    match crate::config::new_mail_sound() {
        None => tracing::debug!("new-mail sound: switched off"),
        Some(_) if crate::desktop::session::quiet() => {
            tracing::info!("new-mail sound: not played, Do Not Disturb is on");
        }
        Some(sound) => play_sound(&sound, false),
    }
}

thread_local! {
    /// What each account's new-mail notification points at, (folder,
    /// message id), while it is up: once that mail is read, or its folder
    /// has nothing unread, the notification is stale (#333).
    static POSTED: std::cell::RefCell<std::collections::HashMap<u32, (u32, u32)>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The (folder, message id) an account's new-mail notification points at,
/// if one is up.
pub(crate) fn posted_for(account_id: u32) -> Option<(u32, u32)> {
    POSTED.with(|p| p.borrow().get(&account_id).copied())
}

thread_local! {
    /// The sound playing and when it started, held until it ends: a dropped
    /// `MediaFile` stops mid-play.
    static SOUND: std::cell::RefCell<Option<(gtk::MediaFile, std::time::Instant)>> =
        const { std::cell::RefCell::new(None) };
}

/// How long a playing sound holds off the next one. Notifications from
/// accounts syncing together land within a second or two of each other; an
/// alert sound that still says "playing" after this has not ended and never
/// will (#337), and must not silence every sound after it.
const HOLD_OFF: std::time::Duration = std::time::Duration::from_secs(10);

/// Play the new-mail sound (#292). The notification itself goes through the
/// portal, which has no way to carry a sound of the app's choosing, so the
/// app plays it. Several accounts syncing at once post several
/// notifications; a sound still playing is left to finish rather than
/// stacked with copies of itself. `restart` is Settings' Play button, which
/// starts over.
///
/// Every step is logged: the sound is the one thing a notification does that
/// leaves no trace on screen, so an exported log is all there is to go on
/// when someone hears nothing (#337).
pub(crate) fn play_sound(sound: &crate::config::SoundSource, restart: bool) {
    SOUND.with(|slot| {
        let mut slot = slot.borrow_mut();
        if !restart {
            if let Some((m, since)) = slot.as_ref() {
                if m.is_playing() {
                    let age = since.elapsed();
                    if age < HOLD_OFF {
                        tracing::info!(
                            "new-mail sound: skipped, the previous one has been playing for {:.1} s",
                            age.as_secs_f32()
                        );
                        return;
                    }
                    tracing::warn!(
                        "new-mail sound: the previous one has said playing for {:.0} s without ending; playing anyway",
                        age.as_secs_f32()
                    );
                }
            }
        }
        let (media, what) = match sound {
            crate::config::SoundSource::Resource(path) => {
                let name = path.rsplit('/').next().unwrap_or(path).trim_end_matches(".ogg");
                (gtk::MediaFile::for_resource(path), format!("{name} (built-in)"))
            }
            crate::config::SoundSource::File(path) => (
                gtk::MediaFile::for_filename(path),
                path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()),
            ),
        };
        tracing::info!(
            "new-mail sound: playing {what} {}",
            if restart { "from Settings" } else { "for new mail" }
        );
        let started = std::time::Instant::now();
        media.connect_error_notify(|m| {
            if let Some(e) = m.error() {
                tracing::warn!("new-mail sound: {e}");
            }
        });
        media.connect_prepared_notify(|m| {
            if m.is_prepared() && m.error().is_none() {
                tracing::debug!(
                    "new-mail sound: ready, {} ms, audio: {}",
                    m.duration() / 1000,
                    if m.has_audio() { "yes" } else { "no" }
                );
            }
        });
        // Let go of it once it has played, or its audio stream stays open
        // for as long as the app runs.
        media.connect_ended_notify(move |m| {
            if !m.is_ended() {
                return;
            }
            tracing::info!("new-mail sound: finished after {:.1} s", started.elapsed().as_secs_f32());
            let m = m.clone();
            gtk::glib::idle_add_local_once(move || {
                SOUND.with(|slot| {
                    let mut slot = slot.borrow_mut();
                    if slot.as_ref().is_some_and(|(held, _)| held == &m) {
                        *slot = None;
                    }
                });
            });
        });
        media.play();
        *slot = Some((media, started));
    });
}

/// Withdraw an account's new-mail notification (once its mail has been read).
pub(crate) fn withdraw_mail(account_id: u32) {
    relm4::main_application().withdraw_notification(&mail_id(account_id));
    POSTED.with(|p| p.borrow_mut().remove(&account_id));
}

/// Post a genuine error alert (e.g. send/auth failure).
pub(crate) fn error(account_id: u32, title: &str, body: &str) {
    let n = gio::Notification::new(title);
    if !body.is_empty() {
        n.set_body(Some(body));
    }
    n.set_priority(gio::NotificationPriority::High);
    n.set_default_action(&format!("app.{PRESENT_ACTION}"));
    send(&format!("hylki-error-{account_id}"), &n);
}

/// A read receipt came back (#520): who read which message. A click opens
/// the message read, `open` as `(account, folder, message)`, or raises the
/// window when it is not to be found. One per message, so a second reader
/// replaces the first's alert rather than stacking under it.
pub(crate) fn read_receipt(
    message_id: &str,
    title: &str,
    body: &str,
    open: Option<(u32, u32, u32)>,
) {
    let n = gio::Notification::new(title);
    if !body.is_empty() {
        n.set_body(Some(body));
    }
    n.set_priority(gio::NotificationPriority::Normal);
    match open {
        Some(target) => n.set_default_action_and_target_value(
            &format!("app.{OPEN_MESSAGE_ACTION}"),
            Some(&target.to_variant()),
        ),
        None => n.set_default_action(&format!("app.{PRESENT_ACTION}")),
    }
    send(&format!("hylki-receipt-{}", message_id.to_lowercase()), &n);
}

/// A message opened from outside the app (a file manager's "Send by email",
/// a mailto: link) whose window could not come to the front: the desktop's
/// word that it is waiting, with the click that brings it up. Clicking a
/// notification hands the app a fresh activation token, which is exactly
/// what the hand-off lacked.
pub(crate) fn compose_ready(attachments: u32) {
    let n = gio::Notification::new(&i18n("Message ready to send"));
    let body = if attachments == 0 {
        i18n("A new message is open in Hylki.")
    } else {
        crate::i18n::ni18n_f(
            "A new message with {n} file attached is open in Hylki.",
            "A new message with {n} files attached is open in Hylki.",
            attachments,
            &[("n", &attachments.to_string())],
        )
    };
    n.set_body(Some(&body));
    n.set_priority(gio::NotificationPriority::Normal);
    n.set_default_action(&format!("app.{PRESENT_COMPOSE_ACTION}"));
    COMPOSE_READY_POSTED.store(true, std::sync::atomic::Ordering::Relaxed);
    send(COMPOSE_READY_ID, &n);
}

/// Take down the "message ready" alert once a window of ours has the focus
/// (the user got there by themselves).
pub(crate) fn withdraw_compose_ready() {
    if COMPOSE_READY_POSTED.swap(false, std::sync::atomic::Ordering::Relaxed) {
        relm4::main_application().withdraw_notification(COMPOSE_READY_ID);
    }
}

fn send(id: &str, notification: &gio::Notification) {
    relm4::main_application().send_notification(Some(id), notification);
}

#[cfg(test)]
mod tests {
    use super::compose_new_mail;

    #[test]
    fn single_message_shows_sender_and_subject() {
        let (t, b) = compose_new_mail("Alice", "Lunch?", 0, true);
        assert_eq!(t, "Alice");
        assert_eq!(b, "Lunch?");
    }

    #[test]
    fn multiple_messages_summarize_with_count() {
        let (t, b) = compose_new_mail("Bob", "Re: report", 2, true);
        assert_eq!(t, "3 new messages");
        assert_eq!(b, "Bob — Re: report");
    }

    #[test]
    fn content_free_notifications_name_nobody() {
        let (t, b) = compose_new_mail("Alice", "Lunch?", 0, false);
        assert_eq!(t, "New message");
        assert!(b.is_empty());
        let (t, b) = compose_new_mail("Alice", "Lunch?", 2, false);
        assert_eq!(t, "3 new messages");
        assert!(b.is_empty());
    }

    #[test]
    fn missing_sender_falls_back() {
        let (t, _) = compose_new_mail("", "Hi", 0, true);
        assert_eq!(t, "New message");
    }
}
