//! The Outbox: queueing a message that could not go yet or is sent later,
//! where its sent copy and the copy of what it answers belong, and parking
//! the Undo Send holds at quit.

use lettre::Address;

use crate::cache::Cache;
use crate::config::{AccountConfig, ReplyCopies};
use crate::i18n::{i18n, i18n_f};
use crate::models::{Folder, FolderKind};
use crate::worker::compose::build::{build_email, restore_msgid_case};
use crate::worker::ids::first_msgid;
use crate::worker::{offline_text, OutgoingMessage, WorkerEvent};

/// Push the account's queue to the UI (empty list = nothing waiting).
pub(super) fn emit_outbox(cache: Option<&Cache>, account_id: u32, emit: &impl Fn(WorkerEvent)) {
    let items = cache
        .map(|c| c.outbox_items(account_id))
        .unwrap_or_default();
    emit(WorkerEvent::Outbox { items });
}

/// The user deleted a queued message from the Outbox.
pub(super) fn delete_queued(
    cache: Option<&Cache>,
    account_id: u32,
    id: u32,
    emit: &impl Fn(WorkerEvent),
) {
    if let Some(c) = cache {
        c.delete_outbox(id);
    }
    emit_outbox(cache, account_id, emit);
}

/// The message went out, or was saved as a draft instead: the queued copy it
/// was edited from is superseded.
pub(super) fn drop_superseded_outbox(
    cache: Option<&Cache>,
    account_id: u32,
    msg: &OutgoingMessage,
    emit: &impl Fn(WorkerEvent),
) {
    if let (Some(queued), Some(c)) = (msg.outbox_origin, cache) {
        c.delete_outbox(queued);
        emit_outbox(cache, account_id, emit);
    }
}

/// A send failed. Hold the message in the Outbox rather than losing it: the
/// composer is already closed by the time this runs, so anything not queued
/// here is gone (issue #15). Being offline is the usual reason, which is
/// exactly when saving to the server's Drafts folder would fail too. With no
/// cache there is nowhere to keep it, and the error must not claim otherwise.
/// `refused` is a server that read the message and said no, to a recipient
/// most often (#368). Sending it again would only fail again, so the message
/// goes back to the composer to be fixed, not to the Outbox to wait.
pub(super) fn send_failed(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    msg: &OutgoingMessage,
    sent_path: Option<&str>,
    error: &str,
    refused: bool,
    emit: &impl Fn(WorkerEvent),
) {
    let queued =
        !refused && queue_outbox_message(cache, account_id, account, msg, sent_path, error, None);
    // Queue first, drop the superseded row second: a crash in between leaves
    // the message queued twice, which is recoverable, rather than not at all.
    if let (true, Some(old), Some(c)) = (queued, msg.outbox_origin, cache) {
        c.delete_outbox(old);
    }
    if queued && error == offline_text() {
        // Nothing failed: the message waits for the network, which the
        // Refresh button already shows is down (#442).
        emit(WorkerEvent::Notice(i18n(
            "No network connection. The message is in the Outbox and will be sent when the connection is back.",
        )));
    } else {
        emit(WorkerEvent::error(if queued {
            i18n_f(
                "Send failed: {e}. The message is in the Outbox and will be sent when the connection is back.",
                &[("e", error)],
            )
        } else {
            i18n_f("Send failed: {e}", &[("e", error)])
        }));
    }
    if !queued {
        emit(WorkerEvent::Unsent(Box::new(msg.clone())));
    }
    emit_outbox(cache, account_id, emit);
}

/// Send Later (#145): park the message in the Outbox until `at`. The bytes are
/// built now, like a failed send's, so the attachments are read while they
/// exist; the app's clock sends it when the time comes, or the next launch
/// does if Hylki was not running then. Replaces the queued row it was edited
/// from, and drops the draft it was opened from (the caller handles the
/// server-side copy).
pub(super) fn schedule_send(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    msg: &OutgoingMessage,
    sent_path: Option<&str>,
    at: i64,
    emit: &impl Fn(WorkerEvent),
) {
    let queued = queue_outbox_message(cache, account_id, account, msg, sent_path, "", Some(at));
    if queued {
        if let (Some(old), Some(c)) = (msg.outbox_origin, cache) {
            c.delete_outbox(old);
        }
        emit(WorkerEvent::Notice(i18n_f(
            "“{subject}” will be sent {when}",
            &[
                ("subject", &msg.subject),
                ("when", &crate::datefmt::date_time(at)),
            ],
        )));
    } else {
        emit(WorkerEvent::error(i18n("Could not schedule the message.")));
        emit(WorkerEvent::Unsent(Box::new(msg.clone())));
    }
    emit_outbox(cache, account_id, emit);
}

fn queue_outbox_message(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    msg: &OutgoingMessage,
    sent_path: Option<&str>,
    error: &str,
    send_at: Option<i64>,
) -> bool {
    let Some(cache) = cache else {
        return false;
    };
    // Build once, here: the composed attachments are read from disk now, while
    // their files certainly exist. A retry sends these bytes verbatim.
    let email = match build_email(account, msg) {
        Ok(email) => email,
        Err(e) => {
            tracing::warn!("could not build the failed message for the outbox: {e}");
            return false;
        }
    };
    let envelope = email.envelope().clone();
    let raw = email.formatted();
    let recipients = [&msg.to, &msg.cc]
        .iter()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let preview: String = msg.body.split_whitespace().collect::<Vec<_>>().join(" ");
    let preview: String = preview.chars().take(160).collect();
    let id = cache.queue_outbox(
        account_id,
        envelope
            .from()
            .map(|f| f.to_string())
            .unwrap_or_default()
            .as_str(),
        &envelope
            .to()
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>(),
        &recipients,
        &msg.subject,
        &preview,
        &raw,
        sent_path,
        error,
        send_at,
    );
    if let Some(id) = id.filter(|_| !msg.forwards.is_empty()) {
        cache.set_outbox_forwards(id, &msg.forwards);
    }
    id.is_some()
}

/// Quitting while a message waits out the Undo Send delay (#434): queue it
/// in the Outbox, due now, for the next launch to send. The exit that
/// follows would otherwise take the only copy with it.
pub(crate) fn park_for_next_launch(
    cache: &Cache,
    account_id: u32,
    account: &AccountConfig,
    msg: &OutgoingMessage,
    sent_path: Option<&str>,
) -> bool {
    let mut msg = msg.clone();
    restore_msgid_case(Some(cache), &mut msg);
    let queued = queue_outbox_message(
        Some(cache),
        account_id,
        account,
        &msg,
        sent_path,
        "",
        Some(crate::datefmt::now()),
    );
    if let (true, Some(old)) = (queued, msg.outbox_origin) {
        cache.delete_outbox(old);
    }
    queued
}

/// Where a sent copy belongs *now*, rather than where it was headed when the
/// message was queued (#199).
///
/// A message can sit in the Outbox for days — Send Later puts it there on
/// purpose — and the account's choice can move on while it waits. Reading the
/// stored path back meant a message scheduled last week landed in the folder
/// that was configured last week. The queued path stays as the fallback for
/// when the folder list can't be read, so a send never loses its copy over
/// this.
pub(super) fn current_sent_copy_path(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    queued: Option<&str>,
) -> Option<String> {
    let folders = cache
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default();
    pick_queued_sent_copy(account, &folders, queued)
}

/// [`current_sent_copy_path`] once the folder list is in hand.
fn pick_queued_sent_copy(
    account: &AccountConfig,
    folders: &[Folder],
    queued: Option<&str>,
) -> Option<String> {
    // Set since the message was queued (or all along): the server keeps its
    // own copy and ours would be a duplicate.
    if account.server_saves_sent {
        return None;
    }
    if folders.is_empty() {
        return queued.map(str::to_string);
    }
    account
        .sent_copy_path
        .as_deref()
        // Renamed or removed since it was chosen: fall through to Sent rather
        // than appending into a name the server would have to recreate.
        .filter(|p| folders.iter().any(|f| f.path == *p))
        .map(str::to_string)
        .or_else(|| {
            folders
                .iter()
                .find(|f| f.kind == FolderKind::Sent)
                .map(|f| f.path.clone())
        })
}

/// The cached copies a sent message answered, in this account (#454): the
/// one it replies to, found by its In-Reply-To, then the one it forwards,
/// each with whether it was forwarded.
pub(crate) fn answered_copies(
    cache: Option<&Cache>,
    account_id: u32,
    in_reply_to: &str,
    forwards: &str,
) -> Vec<(String, u32, bool)> {
    let Some(cache) = cache else {
        return Vec::new();
    };
    [(in_reply_to, false), (forwards, true)]
        .into_iter()
        .filter_map(|(ids, forwarded)| Some((first_msgid(ids)?, forwarded)))
        .flat_map(|(id, forwarded)| {
            cache
                .locate_by_message_id(&id)
                .into_iter()
                .filter(|(a, _, _)| *a == account_id)
                .map(move |(_, path, uid)| (path, uid, forwarded))
        })
        .collect()
}

/// An answered mark the server has taken (or, on POP3, which keeps no
/// flags, one kept here only): into the cache, and the views told (#454).
pub(super) fn note_answered(
    cache: Option<&Cache>,
    account_id: u32,
    path: String,
    uid: u32,
    forwarded: bool,
    emit: &impl Fn(WorkerEvent),
) {
    if let Some(c) = cache {
        c.set_answered(account_id, &path, uid, forwarded);
    }
    emit(WorkerEvent::Answered {
        path,
        uid,
        forwarded,
    });
}

/// Where the copy of a reply is filed when the account keeps replies
/// beside what they answer (#468): the folder its original is in now,
/// found by Message-ID in the cache, so a message moved or deleted while
/// the reply was written is followed or let go. `None` files it with the
/// rest of sent mail: the setting is off, this is no reply, or the original
/// is in no folder of this account's that a reply belongs in.
pub(crate) fn reply_copy_path(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    in_reply_to: &str,
) -> Option<String> {
    // A server that files its own copy files it in Sent, and a second
    // copy beside the original would be one more than the user asked for.
    if account.reply_copies == ReplyCopies::WithSent || account.server_saves_sent {
        return None;
    }
    let cache = cache?;
    let id = first_msgid(in_reply_to)?;
    let holding: Vec<String> = cache
        .locate_by_message_id(&id)
        .into_iter()
        .filter(|(a, _, _)| *a == account_id)
        .map(|(_, path, _)| path)
        .collect();
    pick_reply_folder(
        account.reply_copies,
        &cache.load_folders(account_id),
        &holding,
    )
}

/// [`reply_copy_path`] once it is known which folders hold the original.
fn pick_reply_folder(
    choice: ReplyCopies,
    folders: &[Folder],
    holding: &[String],
) -> Option<String> {
    holding
        .iter()
        // A folder no longer listed was deleted, or hidden and so unsynced.
        .filter_map(|p| folders.iter().find(|f| f.path == *p))
        .filter_map(|f| match f.kind {
            FolderKind::Custom => Some((0, f)),
            FolderKind::Archive => Some((1, f)),
            FolderKind::Inbox if choice != ReplyCopies::OriginalNotInbox => Some((2, f)),
            // Sent takes it anyway; Drafts and Templates hold mail not yet
            // sent, Trash and Junk mail on its way out, and Starred is a
            // view of other folders.
            _ => None,
        })
        // Gmail lists one message under each of its labels and All Mail:
        // the label the user filed it under is where the reply belongs.
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, f)| f.path.clone())
}

/// The In-Reply-To of a message queued as raw bytes, for
/// [`reply_copy_path`].
pub(in crate::worker) fn raw_in_reply_to(raw: &[u8]) -> String {
    let parsed = mail_parser::MessageParser::default().parse_headers(raw);
    match parsed.as_ref().map(|p| p.in_reply_to()) {
        Some(mail_parser::HeaderValue::Text(t)) => t.to_string(),
        Some(mail_parser::HeaderValue::TextList(v)) => {
            v.first().map(|t| t.to_string()).unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// Rebuild the SMTP envelope stored alongside a queued message.
pub(super) fn outbox_envelope(
    item: &crate::models::OutboxItem,
) -> Option<lettre::address::Envelope> {
    let from = item.from_addr.parse::<Address>().ok();
    let to: Vec<Address> = item
        .rcpts
        .iter()
        .filter_map(|a| a.parse::<Address>().ok())
        .collect();
    if to.len() != item.rcpts.len() || to.is_empty() {
        return None;
    }
    lettre::address::Envelope::new(from, to).ok()
}

/// The queued messages a flush sends. A scheduled message waits for its
/// time; asking for it by id (Send Now, or the app's clock) sends it
/// regardless.
pub(super) fn due_outbox_items(
    cache: &Cache,
    account_id: u32,
    id: Option<u32>,
) -> Vec<crate::models::OutboxItem> {
    let now = crate::datefmt::now();
    cache
        .outbox_items(account_id)
        .into_iter()
        .filter(|item| match id {
            Some(wanted) => wanted == item.id,
            None => item.send_at.is_none_or(|t| t <= now),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::models::Message;
    use crate::worker::test_support::{flagged_message, queued, sample_account, sample_outgoing};

    /// A sent message marks what it answers in its own account only, the
    /// reply's original as replied and the forward's as forwarded (#454).
    #[test]
    fn a_sent_message_finds_what_it_answered() {
        let cache = Cache::in_memory().unwrap();
        let copy = |account_id: u32, uid: u32, id: &str| Message {
            account_id,
            message_id: id.into(),
            ..flagged_message(uid)
        };
        cache.upsert_messages(1, "INBOX", &[copy(1, 1, "a@x"), copy(1, 2, "b@x")]);
        cache.upsert_messages(1, "Archive", &[copy(1, 7, "a@x")]);
        cache.upsert_messages(2, "INBOX", &[copy(2, 1, "a@x")]);
        let mut found = answered_copies(Some(&cache), 1, "<A@x>", "");
        found.sort();
        assert_eq!(
            found,
            [
                ("Archive".to_string(), 7, false),
                ("INBOX".to_string(), 1, false)
            ]
        );
        assert_eq!(
            answered_copies(Some(&cache), 1, "", "b@x"),
            [("INBOX".to_string(), 2, true)]
        );
        assert!(answered_copies(Some(&cache), 1, "", "").is_empty());
        assert!(answered_copies(Some(&cache), 1, "gone@x", "").is_empty());
        assert!(answered_copies(None, 1, "a@x", "").is_empty());
    }

    /// #340: a send that fails on a file gone since it was attached cannot
    /// be queued either, so the message comes back for a composer instead
    /// of being dropped.
    #[test]
    fn an_unqueueable_failed_send_comes_back() {
        let cache = Cache::in_memory().expect("cache");
        let account = sample_account();
        let mut msg = sample_outgoing();
        msg.to = "ann@example.com".into();
        msg.attachments = vec!["/nonexistent/hylki-test/moved.pdf".into()];
        let events = std::cell::RefCell::new(Vec::new());
        send_failed(
            Some(&cache),
            1,
            &account,
            &msg,
            None,
            "could not read the attachment",
            false,
            &|e| events.borrow_mut().push(e),
        );
        let events = events.into_inner();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, WorkerEvent::Unsent(m) if m.subject == "Subject")),
            "{events:?}"
        );
        // A message that does build is queued and not handed back.
        msg.attachments.clear();
        let events = std::cell::RefCell::new(Vec::new());
        send_failed(
            Some(&cache),
            1,
            &account,
            &msg,
            None,
            "offline",
            false,
            &|e| events.borrow_mut().push(e),
        );
        assert!(!events
            .into_inner()
            .iter()
            .any(|e| matches!(e, WorkerEvent::Unsent(_))));
    }

    /// #442: a message sent with no network waits in the Outbox with a
    /// notice, not an error: nothing failed.
    #[test]
    fn an_offline_send_waits_quietly() {
        let cache = Cache::in_memory().expect("cache");
        let account = sample_account();
        let mut msg = sample_outgoing();
        msg.to = "ann@example.com".into();
        let events = std::cell::RefCell::new(Vec::new());
        send_failed(
            Some(&cache),
            1,
            &account,
            &msg,
            None,
            &offline_text(),
            false,
            &|e| events.borrow_mut().push(e),
        );
        let events = events.into_inner();
        assert!(
            events.iter().any(|e| matches!(e, WorkerEvent::Notice(_))),
            "{events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, WorkerEvent::Error { .. })),
            "{events:?}"
        );
        assert_eq!(cache.outbox_items(1).len(), 1);
    }

    /// #368: a recipient the server refuses comes back to the composer and
    /// is not queued, where it would fail again on every retry.
    #[test]
    fn a_refused_send_comes_back_unqueued() {
        let cache = Cache::in_memory().expect("cache");
        let account = sample_account();
        let mut msg = sample_outgoing();
        msg.to = "ann@example.com".into();
        let events = std::cell::RefCell::new(Vec::new());
        send_failed(
            Some(&cache),
            1,
            &account,
            &msg,
            None,
            "550 no such user",
            true,
            &|e| events.borrow_mut().push(e),
        );
        assert!(events
            .into_inner()
            .iter()
            .any(|e| matches!(e, WorkerEvent::Unsent(_))));
        assert!(cache.outbox_items(1).is_empty());
    }

    #[test]
    fn a_queued_message_takes_the_sent_copy_folder_it_finds_on_the_way_out() {
        use crate::models::{Folder, FolderKind};
        let folder = |id: u32, path: &str, kind: FolderKind| Folder {
            id,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
            childless: false,
        };
        let folders = vec![
            folder(1, "INBOX", FolderKind::Inbox),
            folder(2, "Sent", FolderKind::Sent),
        ];
        let account = |chosen: Option<&str>| AccountConfig {
            sent_copy_path: chosen.map(str::to_string),
            ..sample_account()
        };
        // The account says Inbox now; the message was queued for Sent. It goes
        // where the account says today, not where it said last week.
        assert_eq!(
            pick_queued_sent_copy(&account(Some("INBOX")), &folders, Some("Sent")).as_deref(),
            Some("INBOX"),
        );
        // Turned back off since queuing: back to the Sent folder.
        assert_eq!(
            pick_queued_sent_copy(&account(None), &folders, Some("INBOX")).as_deref(),
            Some("Sent"),
        );
        // Chosen folder gone from the server: Sent rather than a dead name.
        assert_eq!(
            pick_queued_sent_copy(&account(Some("Gone")), &folders, Some("Gone")).as_deref(),
            Some("Sent"),
        );
        // No folder list to read (offline, no cache): the queued path stands,
        // so a send never loses its copy over this.
        assert_eq!(
            pick_queued_sent_copy(&account(Some("INBOX")), &[], Some("Sent")).as_deref(),
            Some("Sent"),
        );
        // The server files its own: nothing of ours goes anywhere, whatever
        // was queued and whatever folder was picked.
        let server_saves = AccountConfig {
            sent_copy_path: Some("INBOX".into()),
            server_saves_sent: true,
            ..sample_account()
        };
        assert_eq!(
            pick_queued_sent_copy(&server_saves, &folders, Some("Sent")),
            None
        );
        assert_eq!(
            pick_queued_sent_copy(&server_saves, &[], Some("Sent")),
            None
        );
    }

    #[test]
    fn a_reply_is_filed_beside_the_message_it_answers() {
        use crate::config::ReplyCopies;
        let folder = |id: u32, path: &str, kind: FolderKind| Folder {
            id,
            account_id: 1,
            name: path.to_string(),
            path: path.to_string(),
            kind,
            unread: 0,
            childless: false,
        };
        let folders = vec![
            folder(1, "INBOX", FolderKind::Inbox),
            folder(2, "Sent", FolderKind::Sent),
            folder(3, "Projects/Hylki", FolderKind::Custom),
            folder(4, "[Gmail]/All Mail", FolderKind::Archive),
            folder(5, "Trash", FolderKind::Trash),
        ];
        let held = |paths: &[&str]| paths.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        let pick = |choice, paths: &[&str]| pick_reply_folder(choice, &folders, &held(paths));
        assert_eq!(
            pick(ReplyCopies::Original, &["Projects/Hylki"]).as_deref(),
            Some("Projects/Hylki")
        );
        assert_eq!(
            pick(ReplyCopies::Original, &["INBOX"]).as_deref(),
            Some("INBOX")
        );
        assert_eq!(pick(ReplyCopies::OriginalNotInbox, &["INBOX"]), None);
        // Gmail's copies under a label, All Mail and the Inbox: the label.
        assert_eq!(
            pick(
                ReplyCopies::OriginalNotInbox,
                &["INBOX", "[Gmail]/All Mail", "Projects/Hylki"]
            )
            .as_deref(),
            Some("Projects/Hylki"),
        );
        assert_eq!(
            pick(ReplyCopies::Original, &["INBOX", "[Gmail]/All Mail"]).as_deref(),
            Some("[Gmail]/All Mail")
        );
        // Binned, already in Sent, or in a folder deleted since: with sent mail.
        assert_eq!(pick(ReplyCopies::Original, &["Trash"]), None);
        assert_eq!(pick(ReplyCopies::Original, &["Sent"]), None);
        assert_eq!(pick(ReplyCopies::Original, &["Gone"]), None);
        assert_eq!(pick(ReplyCopies::Original, &[]), None);

        // Off, or the server files its own copy: nothing is looked up.
        let account = |reply_copies, server_saves_sent| AccountConfig {
            reply_copies,
            server_saves_sent,
            ..sample_account()
        };
        let cache = Cache::in_memory().unwrap();
        cache.save_folders(1, &folders);
        assert_eq!(
            reply_copy_path(
                &account(ReplyCopies::WithSent, false),
                1,
                Some(&cache),
                "a@b"
            ),
            None
        );
        assert_eq!(
            reply_copy_path(
                &account(ReplyCopies::Original, true),
                1,
                Some(&cache),
                "a@b"
            ),
            None
        );
        assert_eq!(
            reply_copy_path(&account(ReplyCopies::Original, false), 1, Some(&cache), ""),
            None
        );
        // On: the original is found by its Message-ID, spelled as the
        // reply spells it, and only among this account's folders.
        let original = Message {
            id: 1,
            account_id: 1,
            folder_id: 3,
            uid: 7,
            from_name: "Ada".into(),
            from_addr: "ada@example.com".into(),
            reply_to: String::new(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Plans".into(),
            preview: String::new(),
            body: String::new(),
            date: String::new(),
            timestamp: 1,
            unread: false,
            starred: false,
            answered: false,
            forwarded: false,
            keywords: Vec::new(),
            has_attachment: false,
            message_id: "parent@example.com".into(),
            references: String::new(),
            importance: Default::default(),
            due: 0,
            labels: Vec::new(),
        };
        cache.upsert_messages(1, "Projects/Hylki", std::slice::from_ref(&original));
        let on = account(ReplyCopies::Original, false);
        assert_eq!(
            reply_copy_path(&on, 1, Some(&cache), "<Parent@Example.com>").as_deref(),
            Some("Projects/Hylki"),
        );
        assert_eq!(
            reply_copy_path(&on, 2, Some(&cache), "parent@example.com"),
            None
        );
        assert_eq!(
            reply_copy_path(&on, 1, Some(&cache), "other@example.com"),
            None
        );
    }

    #[test]
    fn a_queued_reply_names_what_it_answers() {
        let raw =
            b"From: a@example.com\r\nTo: b@example.com\r\nIn-Reply-To: <Parent@Example.com>\r\n\
                    Subject: Re: x\r\n\r\nbody\r\n";
        assert_eq!(raw_in_reply_to(raw), "Parent@Example.com");
        assert_eq!(raw_in_reply_to(b"Subject: x\r\n\r\nbody\r\n"), "");
    }

    #[test]
    fn a_queued_message_keeps_every_recipient_on_retry() {
        // Bcc exists only in the envelope, so a retry that rebuilds it from the
        // headers would silently drop those recipients.
        let item = queued("me@example.com", &["ada@example.com", "bcc@example.com"]);
        let envelope = outbox_envelope(&item).expect("envelope rebuilds");
        assert_eq!(
            envelope.from().map(|f| f.to_string()).as_deref(),
            Some("me@example.com")
        );
        assert_eq!(
            envelope
                .to()
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>(),
            ["ada@example.com", "bcc@example.com"]
        );
    }

    #[test]
    fn a_queued_message_with_an_unusable_address_is_not_retried() {
        // Better to leave it in the Outbox saying so than to retry forever.
        assert!(outbox_envelope(&queued("me@example.com", &["not-an-address"])).is_none());
        assert!(outbox_envelope(&queued("me@example.com", &[])).is_none());
        // A missing sender is legal (SMTP's null reverse-path), so it is not a
        // reason to refuse.
        assert!(outbox_envelope(&queued("", &["ada@example.com"])).is_some());
    }
}
