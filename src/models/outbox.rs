//! Messages waiting in the outbox.

use super::message::Message;
use crate::i18n::{i18n, i18n_f};

/// A message that could not be sent and is waiting in the Outbox.
///
/// The built MIME bytes are stored rather than the composed fields: a retry has
/// to send exactly what was composed, and the attachments' files may be gone by
/// then — under Flatpak the portal paths handed to the file chooser expire.
/// `from_addr` and `rcpts` are the SMTP envelope, which is not always what the
/// headers say (Bcc is in the envelope only).
#[derive(Debug, Clone)]
pub(crate) struct OutboxItem {
    pub(crate) id: u32,
    pub(crate) account_id: u32,
    /// Envelope sender.
    pub(crate) from_addr: String,
    /// Envelope recipients (To + Cc + Bcc), one per line as stored.
    pub(crate) rcpts: Vec<String>,
    /// Header recipients, for display ("Ada Lovelace, bob@example.com").
    pub(crate) recipients: String,
    pub(crate) subject: String,
    pub(crate) preview: String,
    pub(crate) raw: Vec<u8>,
    /// The account's Sent folder at queue time, appended to once it goes out.
    pub(crate) sent_path: Option<String>,
    /// Unix seconds when it was first queued.
    pub(crate) queued_at: i64,
    pub(crate) attempts: u32,
    /// Why the last attempt failed, shown in the list.
    pub(crate) last_error: String,
    /// Send Later (#145): unix seconds it is due; `None` goes as soon as it can.
    pub(crate) send_at: Option<i64>,
    /// The Message-ID of the message a forward forwards, to mark once it
    /// goes (#454); a reply's original is in its own In-Reply-To.
    pub(crate) forwards: String,
    /// Set only for a message still on its way out (#434), never stored:
    /// what it is doing ("Sending…"), shown instead of how long it waited.
    pub(crate) status: String,
}

/// What starts `last_error` when a send of this message may have gone out
/// (Tuta's `send_uncertain`): such a message is never sent again on its
/// own, since a second try could deliver it twice. Kept in `last_error`,
/// where the reason already is, so the table needs no new column; it is
/// not shown.
pub(crate) const SEND_UNCERTAIN_MARK: &str = "send-uncertain:";

impl OutboxItem {
    /// Whether a send of this message may already have gone out.
    pub(crate) fn is_send_uncertain(&self) -> bool {
        self.last_error.starts_with(SEND_UNCERTAIN_MARK)
    }

    /// "Waiting since 10 minutes ago" — what the reader shows where a received
    /// message shows its date, since an unsent message has no send time yet.
    pub(crate) fn waiting_label(queued_at: i64) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(queued_at);
        let secs = (now - queued_at).max(0);
        let ago = match secs {
            s if s < 90 => i18n("just now"),
            s if s < 3600 => i18n_f("{n} minutes ago", &[("n", &(s / 60).to_string())]),
            s if s < 7200 => i18n("an hour ago"),
            s if s < 86400 => i18n_f("{n} hours ago", &[("n", &(s / 3600).to_string())]),
            s if s < 172_800 => i18n("yesterday"),
            s => i18n_f("{n} days ago", &[("n", &(s / 86400).to_string())]),
        };
        i18n_f("Waiting since {ago}", &[("ago", &ago)])
    }

    /// The queued message as a list row. Everything the list needs is stored with
    /// the message, so the Outbox reads as an ordinary folder: same list, same
    /// reader, same sorting. `folder_id` is `OUTBOX_FOLDER_ID`, which no real
    /// folder uses, so a row can always be traced back here.
    pub(crate) fn as_message(&self) -> Message {
        Message {
            id: self.id,
            account_id: self.account_id,
            folder_id: OUTBOX_FOLDER_ID,
            uid: self.id,
            // The list's headline column is the sender everywhere else; for
            // unsent mail the useful name is who it is going to.
            from_name: if self.recipients.trim().is_empty() {
                "(no recipients)".to_string()
            } else {
                self.recipients.clone()
            },
            from_addr: self.from_addr.clone(),
            reply_to: String::new(),
            to: self.recipients.clone(),
            cc: String::new(),
            bcc: String::new(),
            subject: if self.subject.trim().is_empty() {
                "(no subject)".to_string()
            } else {
                self.subject.clone()
            },
            // The row's one line of context: how long it has been stuck and what
            // is stopping it, falling back to the body when nothing has failed
            // yet (a message queued while offline never got an error).
            preview: if !self.status.is_empty() {
                self.status.clone()
            } else {
                // A scheduled message says when it goes, until it is due.
                let waiting = match self.send_at {
                    Some(at) if at > crate::datefmt::now() => i18n_f(
                        "Scheduled for {when}",
                        &[("when", &crate::datefmt::date_time(at))],
                    ),
                    _ => Self::waiting_label(self.queued_at),
                };
                let attempts = match self.attempts {
                    0 | 1 => String::new(),
                    n => format!(" · {n} attempts"),
                };
                let reason = if self.last_error.trim().is_empty() {
                    self.preview.trim().to_string()
                } else {
                    self.last_error
                        .trim_start_matches(SEND_UNCERTAIN_MARK)
                        .trim()
                        .to_string()
                };
                if reason.is_empty() {
                    format!("{waiting}{attempts}")
                } else {
                    format!("{waiting}{attempts} · {reason}")
                }
            },
            body: String::new(),
            date: String::new(),
            timestamp: self.send_at.unwrap_or(self.queued_at),
            // Never dimmed as read: it is still waiting to go out.
            unread: true,
            starred: false,
            answered: false,
            forwarded: false,
            keywords: Vec::new(),
            has_attachment: false,
            message_id: String::new(),
            references: String::new(),
            importance: Default::default(),
            due: 0,
            labels: Vec::new(),
        }
    }
}

/// Folder id for the Outbox's synthetic rows. Real folders are numbered from 1
/// by the worker, so this can't collide.
pub(super) const OUTBOX_FOLDER_ID: u32 = u32::MAX;
