//! Desktop (system) notifications via GNotification.
//!
//! Uses the running `gtk::Application`'s `send_notification`, which routes through
//! the desktop notification portal automatically under Flatpak. The click actions
//! ([`PRESENT_ACTION`], [`OPEN_MESSAGE_ACTION`]) are registered on the application
//! in `app.rs` (they need the app's message channel to navigate).

use gtk::gio;
use gtk::prelude::*;
use crate::i18n::i18n;

/// App action (bare name) that raises the main window. Used by error alerts.
pub const PRESENT_ACTION: &str = "present-window";
/// App action (bare name) that raises the window and opens a specific message.
/// Its target is a `(account_id, folder_id, message_id)` `(uuu)` variant.
pub const OPEN_MESSAGE_ACTION: &str = "open-message";
/// Notification button actions (#38): act on the notified message without
/// raising the window. Same `(uuu)` target as [`OPEN_MESSAGE_ACTION`].
pub const MARK_READ_ACTION: &str = "notify-mark-read";
pub const ARCHIVE_ACTION: &str = "notify-archive";
/// App action (bare name) that raises the window and the newest composer:
/// the click on a "message ready" alert ([`compose_ready`]).
pub const PRESENT_COMPOSE_ACTION: &str = "present-compose";

const COMPOSE_READY_ID: &str = "vireo-compose-ready";
/// Whether a [`compose_ready`] alert is (or may be) showing, so that
/// withdrawing it costs nothing on the focus changes where there is none.
static COMPOSE_READY_POSTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
            if from.is_empty() { i18n("New message") } else { from.to_string() },
            subject.to_string(),
        )
    } else {
        (
            format!("{} new messages", others + 1),
            // Lead with the newest so the summary is still informative.
            if subject.is_empty() { from.to_string() } else { format!("{from} — {subject}") },
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
pub fn new_mail(
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
        crate::config::load_notification_content(),
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
    // still where the buttons will look for it.
    if others == 0 && in_place {
        n.add_button_with_target_value(
            &i18n("Mark as Read"),
            &format!("app.{MARK_READ_ACTION}"),
            Some(&target),
        );
        n.add_button_with_target_value(
            &i18n("Archive"),
            &format!("app.{ARCHIVE_ACTION}"),
            Some(&target),
        );
    }
    send(&mail_id(account_id), &n);
}

/// Withdraw an account's new-mail notification (once its mail has been read).
pub fn withdraw_mail(account_id: u32) {
    relm4::main_application().withdraw_notification(&mail_id(account_id));
}

/// Post a genuine error alert (e.g. send/auth failure).
pub fn error(account_id: u32, title: &str, body: &str) {
    let n = gio::Notification::new(title);
    if !body.is_empty() {
        n.set_body(Some(body));
    }
    n.set_priority(gio::NotificationPriority::High);
    n.set_default_action(&format!("app.{PRESENT_ACTION}"));
    send(&format!("hylki-error-{account_id}"), &n);
}

/// A message opened from outside the app (a file manager's "Send by email",
/// a mailto: link) whose window could not come to the front: the desktop's
/// word that it is waiting, with the click that brings it up. Clicking a
/// notification hands the app a fresh activation token, which is exactly
/// what the hand-off lacked.
pub fn compose_ready(attachments: u32) {
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
pub fn withdraw_compose_ready() {
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
