//! Gmail over IMAP: labels (X-GM-LABELS) read for the list and written for
//! tags (#374), and deleting for good through [Gmail]/Trash (X-GM-MSGID).

use async_imap::types::Fetch;
use async_imap::Session;
use futures::TryStreamExt;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::models::{Folder, FolderKind, Message};
use crate::worker::imap::fetch_mode::is_parse_error;
use crate::worker::imap::mutate::{flag_deleted_and_expunge, uid_move};
use crate::worker::imap::wire::{
    cmd_head, search_uids, sel, store_uids, uid_set, wire, wired, wired_err,
};
use crate::worker::imap::ImapSession;
use crate::worker::WorkerEvent;

/// Gmail accounts whose labels are read (#374): `true` while they are,
/// `false` once the server has refused `X-GM-LABELS` or answered it with
/// something the parser cannot read, for the rest of the session. A server
/// that only looks like Gmail by its folder names (a mailbox migrated with
/// its `[Gmail]/` folders) refuses the item, and an unreadable reply would
/// otherwise fail every sync after it.
static GMAIL_LABELS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<u32, bool>>> =
    std::sync::LazyLock::new(Default::default);

/// Start reading the account's Gmail labels if it is Gmail, unless the
/// server has already refused them this session.
pub(super) fn note_gmail_account(account_id: u32, account: &AccountConfig, folders: &[Folder]) {
    if has_gmail_labels(account, folders) {
        if let Ok(mut g) = GMAIL_LABELS.lock() {
            g.entry(account_id).or_insert(true);
        }
    }
}

pub(super) fn gmail_labels_wanted(account_id: u32) -> bool {
    GMAIL_LABELS
        .lock()
        .map(|g| g.get(&account_id) == Some(&true))
        .unwrap_or(false)
}

fn refuse_gmail_labels(account_id: u32) {
    if let Ok(mut g) = GMAIL_LABELS.lock() {
        g.insert(account_id, false);
    }
}

/// Whether the account is Gmail, where a folder is a label and one message
/// sits in several of them at once.
pub(crate) fn has_gmail_labels(account: &AccountConfig, folders: &[Folder]) -> bool {
    let host = account.imap_host.to_ascii_lowercase();
    host.ends_with("gmail.com")
        || host.ends_with("googlemail.com")
        || folders
            .iter()
            .any(|f| f.path.starts_with("[Gmail]/") || f.path.starts_with("[Google Mail]/"))
}

/// After a move into Trash or Spam on Gmail, drop the moved mail's copies
/// under its other labels from the cache: Gmail removed them with the move
/// (see [`Cache::drop_label_copies`]). Any other server keeps a copy in
/// another folder as a message of its own, so nothing is dropped there.
pub(super) fn drop_gmail_label_copies(
    cache: &Cache,
    account_id: u32,
    account: &AccountConfig,
    from: &str,
    uids: &[u32],
    dest: &str,
) {
    let folders = cache.load_folders(account_id);
    let labels = has_gmail_labels(account, &folders);
    let binned = folders
        .iter()
        .any(|f| f.path == dest && matches!(f.kind, FolderKind::Trash | FolderKind::Junk));
    if labels && binned {
        let n = cache.drop_label_copies(account_id, from, uids, dest);
        if n > 0 {
            tracing::debug!("dropped {n} cached label copies of mail moved to {dest}");
        }
    }
}

/// A Gmail account's Trash, where an erase has to happen: expunged from
/// any other folder, Gmail mail only loses that label. `None` elsewhere.
pub(super) fn gmail_trash(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
) -> Option<String> {
    let folders = cache?.load_folders(account_id);
    if !has_gmail_labels(account, &folders) {
        return None;
    }
    folders
        .into_iter()
        .find(|f| f.kind == FolderKind::Trash)
        .map(|f| f.path)
}

/// Erase Gmail mail from outside Trash, the way Gmail's own "Delete
/// forever" goes: into Trash, then expunged there. The move gives the mail
/// new UIDs, so it is found again in Trash by `X-GM-MSGID`, which stays the
/// same under every label. A server that does not know the item (a mailbox
/// that only looks like Gmail by its folder names) erases where it is.
pub(super) async fn gmail_erase(
    session: &mut ImapSession,
    path: &str,
    uids: &[u32],
    trash: &str,
) -> Result<(), async_imap::error::Error> {
    if uids.is_empty() {
        return Ok(());
    }
    sel(session, path).await?;
    let Some(ids) = fetch_gmail_msgids(session, uids).await? else {
        return flag_deleted_and_expunge(session, &uid_set(uids)).await;
    };
    for chunk in uids.chunks(300) {
        uid_move(session, &uid_set(chunk), trash).await?;
    }
    sel(session, trash).await?;
    let mut binned = std::collections::HashSet::new();
    for chunk in ids.chunks(100) {
        binned.extend(search_uids(session, gmail_msgid_query(chunk)).await?);
    }
    if binned.is_empty() {
        return Ok(());
    }
    let binned: Vec<u32> = binned.into_iter().collect();
    flag_deleted_and_expunge(session, &uid_set(&binned)).await
}

/// `X-GM-MSGID a`, or `OR X-GM-MSGID a OR X-GM-MSGID b X-GM-MSGID c` for
/// several: IMAP's OR takes two keys.
fn gmail_msgid_query(ids: &[u64]) -> String {
    let mut q = "OR ".repeat(ids.len().saturating_sub(1));
    q.push_str(
        &ids.iter()
            .map(|id| format!("X-GM-MSGID {id}"))
            .collect::<Vec<_>>()
            .join(" "),
    );
    q
}

/// The `X-GM-MSGID` of each message at `uids` in the selected folder, or
/// `None` when the server answers `BAD` (it is not Gmail). Run raw for the
/// same reason as [`fetch_gmail_labels`].
async fn fetch_gmail_msgids<T>(
    session: &mut Session<T>,
    uids: &[u32],
) -> Result<Option<Vec<u64>>, async_imap::error::Error>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + std::fmt::Debug + Send,
{
    use async_imap::imap_proto::{AttributeValue, Response, Status};
    let cmd = format!("UID FETCH {} (UID X-GM-MSGID)", uid_set(uids));
    wire(&cmd);
    let id = session.run_command(&cmd).await?;
    let mut out = Vec::new();
    loop {
        let reply = match session.read_response().await {
            Some(Ok(reply)) => reply,
            Some(Err(e)) => {
                let e = async_imap::error::Error::Io(e);
                wired_err(&cmd, &e);
                return Err(e);
            }
            None => {
                wired::<()>(&cmd, &Err(async_imap::error::Error::ConnectionLost));
                return Err(async_imap::error::Error::ConnectionLost);
            }
        };
        match reply.parsed() {
            Response::Fetch(_, attrs) => {
                out.extend(attrs.iter().find_map(|a| match a {
                    AttributeValue::GmailMsgId(id) => Some(*id),
                    _ => None,
                }));
            }
            Response::Done {
                tag,
                status,
                information,
                ..
            } if *tag == id => {
                let why = information.as_deref().unwrap_or("");
                return match status {
                    Status::Ok => {
                        tracing::debug!(target: "hylki::imap", "< OK ({})", cmd_head(&cmd));
                        Ok(Some(out))
                    }
                    Status::Bad => {
                        tracing::warn!(target: "hylki::imap", "< BAD {why} ({cmd})");
                        Ok(None)
                    }
                    _ => {
                        tracing::warn!(target: "hylki::imap", "< {status:?} {why} ({cmd})");
                        Err(async_imap::error::Error::No(why.to_string()))
                    }
                };
            }
            _ => {}
        }
    }
}

/// The Gmail labels a tag means (#374), as the server writes label names:
/// the tag's name when it is put on; when it comes off, every label that
/// stands for it, its name's and its keyword's.
pub(super) fn tag_labels(keyword: &str, add: bool) -> Vec<String> {
    let name = crate::config::load_tags()
        .into_iter()
        .find(|t| t.keyword.eq_ignore_ascii_case(keyword))
        .map(|t| t.name)
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| keyword.to_string());
    let mut out = vec![crate::mail::mutf7::encode(&name)];
    if !add && !name.eq_ignore_ascii_case(keyword) {
        out.push(crate::mail::mutf7::encode(keyword));
    }
    out
}

/// Put Gmail labels on a message in the selected folder, or take them off.
pub(super) async fn store_gmail_labels(
    session: &mut ImapSession,
    uid: u32,
    labels: &[String],
    add: bool,
) -> Result<(), async_imap::error::Error> {
    let quoted: Vec<String> = labels
        .iter()
        .map(|l| format!("\"{}\"", l.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    let op = if add { "+X-GM-LABELS" } else { "-X-GM-LABELS" };
    let _: Vec<Fetch> = store_uids(
        session,
        uid.to_string(),
        format!("{op} ({})", quoted.join(" ")),
    )
    .await?
    .try_collect()
    .await?;
    Ok(())
}

/// Put each message's Gmail labels on it, for the `uids` asked about, on an
/// account whose labels are read (#374). A message the server said nothing
/// about keeps the labels it has.
pub(super) async fn read_gmail_labels<T>(
    account_id: u32,
    session: &mut Session<T>,
    path: &str,
    uids: &[u32],
    messages: &mut [Message],
) -> Result<(), async_imap::error::Error>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + std::fmt::Debug + Send,
{
    if uids.is_empty() || !gmail_labels_wanted(account_id) {
        return Ok(());
    }
    let Some(mut labels) = fetch_gmail_labels(account_id, session, path, uids).await? else {
        return Ok(());
    };
    for m in messages.iter_mut() {
        if let Some(l) = labels.remove(&m.uid) {
            m.labels = l;
        }
    }
    Ok(())
}

/// Each message's shown Gmail labels by UID, or `None` when the server
/// refused to say and the labels already known should stand.
///
/// async-imap's `Fetch` keeps its parsed reply private and has no getter for
/// `X-GM-LABELS`, so the command is run raw and its replies read here. An
/// unreadable reply leaves the rest of it in the stream, so it is an error
/// the caller reconnects after, and labels are left off for the session so
/// the same reply cannot fail the retry too. It is not passed on as a parse
/// error: that would move the summary fetch down its ladder (#226) for a
/// fault that is not the summary's.
async fn fetch_gmail_labels<T>(
    account_id: u32,
    session: &mut Session<T>,
    path: &str,
    uids: &[u32],
) -> Result<Option<std::collections::HashMap<u32, Vec<String>>>, async_imap::error::Error>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + std::fmt::Debug + Send,
{
    use async_imap::imap_proto::{Response, Status};
    let cmd = format!("UID FETCH {} (UID X-GM-LABELS)", uid_set(uids));
    wire(&cmd);
    let id = session.run_command(&cmd).await?;
    let mut out = std::collections::HashMap::new();
    loop {
        let reply = match session.read_response().await {
            Some(Ok(reply)) => reply,
            Some(Err(e)) => {
                let e = async_imap::error::Error::Io(e);
                wired_err(&cmd, &e);
                if !is_parse_error(&e) {
                    return Err(e);
                }
                refuse_gmail_labels(account_id);
                return Err(async_imap::error::Error::Io(std::io::Error::other(
                    "unreadable X-GM-LABELS reply",
                )));
            }
            None => {
                wired::<()>(&cmd, &Err(async_imap::error::Error::ConnectionLost));
                return Err(async_imap::error::Error::ConnectionLost);
            }
        };
        match reply.parsed() {
            Response::Fetch(_, attrs) => {
                if let Some((uid, labels)) = gmail_labels_of(attrs, path) {
                    out.insert(uid, labels);
                }
            }
            Response::Done {
                tag,
                status,
                information,
                ..
            } if *tag == id => {
                let why = information.as_deref().unwrap_or("");
                return Ok(match status {
                    Status::Ok => {
                        tracing::debug!(target: "hylki::imap", "< OK ({})", cmd_head(&cmd));
                        Some(out)
                    }
                    // A server that does not know the item. Gmail itself
                    // answers NO when a message went while it was being
                    // read, and what it did send still holds.
                    Status::Bad => {
                        tracing::warn!(
                            target: "hylki::imap",
                            "< BAD {why} ({cmd}): Gmail labels left off for the session"
                        );
                        refuse_gmail_labels(account_id);
                        None
                    }
                    _ => {
                        tracing::warn!(target: "hylki::imap", "< {status:?} {why} ({cmd})");
                        Some(out)
                    }
                });
            }
            // EXISTS, EXPUNGE and the like: the next sync reads them.
            _ => {}
        }
    }
}

/// The UID and shown labels in one `FETCH` reply's attributes, if it carries
/// `X-GM-LABELS`.
fn gmail_labels_of(
    attrs: &[async_imap::imap_proto::AttributeValue<'_>],
    folder: &str,
) -> Option<(u32, Vec<String>)> {
    use async_imap::imap_proto::AttributeValue;
    let uid = attrs.iter().find_map(|a| match a {
        AttributeValue::Uid(uid) => Some(*uid),
        _ => None,
    })?;
    let labels = attrs.iter().find_map(|a| match a {
        AttributeValue::GmailLabels(labels) => Some(labels),
        _ => None,
    })?;
    Some((
        uid,
        shown_gmail_labels(labels.iter().map(|l| l.as_ref()), folder),
    ))
}

/// The labels worth a pill: not Gmail's system labels (`\Inbox`,
/// `\Important`, `\Starred`…), which the folder list and the star already
/// show, and not the one the folder being read is, which every message in
/// it carries.
fn shown_gmail_labels<'a>(labels: impl Iterator<Item = &'a str>, folder: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in labels {
        if l.is_empty() || l.starts_with('\\') || l.eq_ignore_ascii_case(folder) {
            continue;
        }
        if !out.iter().any(|o| o == l) {
            out.push(l.to_string());
        }
    }
    out
}

/// How many messages one label-fill step asks Gmail about. A label reply is a
/// line per message, smaller than a References one.
const LABEL_FILL_CHUNK: usize = 1_000;

/// One step of the one-time Gmail label fill for the folder at the head of
/// the queue (#374).
///
/// Labels come down with the summaries, and a sync asks again only about its
/// recent window, so mail indexed before Hylki read labels would never get
/// any. This asks about the rest of each folder once, newest first, resuming
/// from a watermark like [`run_one_refs_repair`].
pub(super) async fn run_one_label_fill(
    queue: &mut std::collections::VecDeque<(u32, String)>,
    session: &mut Option<ImapSession>,
    account_id: u32,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(c) = cache else {
        queue.clear();
        return;
    };
    if !gmail_labels_wanted(account_id) {
        // Refused this session: the fill waits for the next one.
        queue.clear();
        return;
    }
    let Some((folder_id, path)) = queue.pop_front() else {
        return;
    };
    let Some(mut s) = session.take() else {
        queue.push_front((folder_id, path));
        return;
    };
    let (below, _) = c.label_fill_state(account_id, &path);
    let chunk = c.uids_below(account_id, &path, below, LABEL_FILL_CHUNK);
    if chunk.is_empty() {
        c.set_label_fill_state(account_id, &path, 0, true);
        *session = Some(s);
        return;
    }
    let read = async {
        sel(&mut s, &path).await?;
        fetch_gmail_labels(account_id, &mut s, &path, &chunk).await
    }
    .await;
    match read {
        Ok(Some(found)) => {
            let changed = c.set_gmail_labels(account_id, &path, &found);
            let lowest = chunk.iter().copied().min().unwrap_or(0);
            c.set_label_fill_state(account_id, &path, lowest, false);
            queue.push_back((folder_id, path));
            *session = Some(s);
            if changed > 0 {
                emit(WorkerEvent::RefsRepaired { folder_id });
            }
        }
        // Refused; the queue is dropped at the next step.
        Ok(None) => *session = Some(s),
        // The session may be left mid-reply: drop it, and the main loop
        // reconnects when it next needs one.
        Err(_) => queue.push_back((folder_id, path)),
    }
}

#[cfg(test)]
mod gmail_label_tests {
    use super::*;

    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

    async fn session() -> (Session<DuplexStream>, BufReader<DuplexStream>) {
        let (client, server) = tokio::io::duplex(4096);
        let login = tokio::spawn(async move {
            async_imap::Client::new(client)
                .login("test", "test")
                .await
                .unwrap()
        });
        let mut server = BufReader::new(server);
        let tag = command(&mut server, "LOGIN").await;
        server
            .get_mut()
            .write_all(format!("{tag} OK logged in\r\n").as_bytes())
            .await
            .unwrap();
        (login.await.unwrap(), server)
    }

    async fn command(server: &mut BufReader<DuplexStream>, expected: &str) -> String {
        let mut line = String::new();
        server.read_line(&mut line).await.unwrap();
        assert!(line.contains(expected), "{line:?}");
        line.split_whitespace().next().unwrap().to_string()
    }

    fn labelled(uid: u32, labels: &[&str]) -> Message {
        Message {
            id: uid,
            account_id: 7,
            folder_id: 1,
            uid,
            from_name: String::new(),
            from_addr: String::new(),
            reply_to: String::new(),
            reply_to_name: String::new(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: String::new(),
            preview: String::new(),
            body: String::new(),
            date: String::new(),
            timestamp: 0,
            unread: false,
            starred: false,
            answered: false,
            forwarded: false,
            keywords: Vec::new(),
            has_attachment: false,
            message_id: String::new(),
            references: String::new(),
            importance: Default::default(),
            due: 0,
            labels: labels.iter().map(|l| l.to_string()).collect(),
        }
    }

    /// Each message's X-GM-MSGID is read past the server's other news, and
    /// a server that is not Gmail says so with BAD (#471).
    #[tokio::test]
    async fn gmail_msgids_are_read_or_refused() {
        for (reply, want) in [
            (
                "* 3 FETCH (UID 7 X-GM-MSGID 1278455344230334865)\r\n\
                 * 12 EXISTS\r\n\
                 * 4 FETCH (UID 8 X-GM-MSGID 42)\r\n\
                 {tag} OK Success\r\n",
                Some(vec![1278455344230334865, 42]),
            ),
            ("{tag} BAD Unknown fetch item\r\n", None),
        ] {
            let (mut client, mut server) = session().await;
            let task = tokio::spawn(async move { fetch_gmail_msgids(&mut client, &[7, 8]).await });
            let tag = command(&mut server, "UID FETCH 7:8 (UID X-GM-MSGID)").await;
            server
                .get_mut()
                .write_all(reply.replace("{tag}", &tag).as_bytes())
                .await
                .unwrap();
            let got = tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("the read finished")
                .unwrap();
            assert_eq!(got.expect("read"), want);
        }
    }

    #[test]
    fn gmail_msgid_search_pairs_its_keys() {
        assert_eq!(gmail_msgid_query(&[1]), "X-GM-MSGID 1");
        assert_eq!(
            gmail_msgid_query(&[1, 2, 3]),
            "OR OR X-GM-MSGID 1 X-GM-MSGID 2 X-GM-MSGID 3"
        );
    }

    /// Read the labels for UIDs 7, 8 and 9 against a server that answers
    /// with `reply` (`{tag}` standing for the command's tag).
    async fn read(
        account_id: u32,
        reply: &str,
    ) -> (Result<(), async_imap::error::Error>, Vec<Message>) {
        GMAIL_LABELS.lock().unwrap().insert(account_id, true);
        let (mut client, mut server) = session().await;
        let mut msgs = vec![
            labelled(7, &["Old"]),
            labelled(8, &["Gone"]),
            labelled(9, &["Kept"]),
        ];
        let task = tokio::spawn(async move {
            let r =
                read_gmail_labels(account_id, &mut client, "INBOX", &[7, 8, 9], &mut msgs).await;
            (r, msgs)
        });
        let tag = command(&mut server, "UID FETCH 7:9 (UID X-GM-LABELS)").await;
        server
            .get_mut()
            .write_all(reply.replace("{tag}", &tag).as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the read finished")
            .unwrap()
    }

    fn labels_of(msgs: &[Message]) -> Vec<Vec<String>> {
        msgs.iter().map(|m| m.labels.clone()).collect()
    }

    /// The labels are read past what else the server says in between, a
    /// message left with none loses its old ones, and one the server said
    /// nothing about keeps what it had.
    #[tokio::test]
    async fn labels_are_read_past_unsolicited_replies() {
        let account_id = 900_374;
        let (r, msgs) = read(
            account_id,
            "* 3 FETCH (UID 7 X-GM-LABELS (\\Inbox \"My Label\"))\r\n\
             * 12 EXISTS\r\n\
             * 4 FETCH (UID 8 X-GM-LABELS ())\r\n\
             {tag} OK Success\r\n",
        )
        .await;
        r.expect("read");
        assert_eq!(
            labels_of(&msgs),
            [
                vec!["My Label".to_string()],
                vec![],
                vec!["Kept".to_string()]
            ]
        );
        assert!(gmail_labels_wanted(account_id));
    }

    /// A server that only looks like Gmail refuses the item: nothing
    /// changes, the session carries on, and it is not asked again.
    #[tokio::test]
    async fn a_refused_item_leaves_labels_alone_and_is_not_asked_again() {
        let account_id = 900_375;
        let (r, msgs) = read(account_id, "{tag} BAD Unknown FETCH item\r\n").await;
        r.expect("a refusal is no error");
        assert_eq!(
            labels_of(&msgs),
            [
                vec!["Old".to_string()],
                vec!["Gone".to_string()],
                vec!["Kept".to_string()]
            ]
        );
        assert!(!gmail_labels_wanted(account_id));
    }

    /// An unreadable reply wedges the stream: the caller has to reconnect,
    /// but not move its summary fetch down the ladder (#226), and the retry
    /// must not ask for labels again. A label sent as a literal is one such
    /// reply: imap-proto reads labels as atoms and quoted strings only.
    #[tokio::test]
    async fn an_unreadable_reply_asks_for_a_reconnect_without_labels() {
        let account_id = 900_376;
        let (r, mut msgs) = read(
            account_id,
            "* 3 FETCH (UID 7 X-GM-LABELS ({5}\r\nHello))\r\n{tag} OK Success\r\n",
        )
        .await;
        let e = r.expect_err("the stream cannot be trusted");
        assert!(!is_parse_error(&e), "{e}");
        assert_eq!(labels_of(&msgs)[0], ["Old"]);
        assert!(!gmail_labels_wanted(account_id));
        // The retry on a fresh connection sends the server nothing.
        let (mut client, _server) = session().await;
        read_gmail_labels(account_id, &mut client, "INBOX", &[7], &mut msgs)
            .await
            .expect("skipped");
    }

    fn fetch_attrs(raw: &[u8]) -> Vec<async_imap::imap_proto::AttributeValue<'_>> {
        match async_imap::imap_proto::parser::parse_response(raw) {
            Ok((_, async_imap::imap_proto::Response::Fetch(_, attrs))) => attrs,
            other => panic!("not a FETCH reply: {other:?}"),
        }
    }

    /// What Gmail answers `X-GM-LABELS` with (#374): system labels and the
    /// folder's own label stay off the pills, the rest come through as the
    /// server spells them.
    #[test]
    fn gmail_labels_are_read_from_a_fetch_reply() {
        let attrs = fetch_attrs(
            b"* 3 FETCH (UID 7 X-GM-LABELS (\\Inbox \\Important \"My Label\" Work &AOk-t&AOk- \"Clients/Acme\" \\Starred))\r\n",
        );
        let (uid, labels) = gmail_labels_of(&attrs, "INBOX").expect("labels");
        assert_eq!(uid, 7);
        assert_eq!(labels, ["My Label", "Work", "&AOk-t&AOk-", "Clients/Acme"]);
        // Read in the Work folder, Work is the folder, not a label on top.
        let (_, labels) = gmail_labels_of(&attrs, "Work").expect("labels");
        assert_eq!(labels, ["My Label", "&AOk-t&AOk-", "Clients/Acme"]);
        // Nested labels keep their whole path; the folder match is exact.
        let (_, labels) = gmail_labels_of(&attrs, "Clients").expect("labels");
        assert!(labels.contains(&"Clients/Acme".to_string()));

        let none = fetch_attrs(b"* 4 FETCH (UID 8 X-GM-LABELS ())\r\n");
        assert_eq!(gmail_labels_of(&none, "INBOX"), Some((8, Vec::new())));
        let system_only = fetch_attrs(b"* 4 FETCH (UID 8 X-GM-LABELS (\\Sent \\Draft))\r\n");
        assert_eq!(
            gmail_labels_of(&system_only, "INBOX"),
            Some((8, Vec::new()))
        );
        // A flag change Gmail volunteers says nothing about labels.
        let flags = fetch_attrs(b"* 5 FETCH (UID 9 FLAGS (\\Seen))\r\n");
        assert_eq!(gmail_labels_of(&flags, "INBOX"), None);
    }

    #[test]
    fn gmail_labels_shown_skip_system_labels_the_folder_and_repeats() {
        let labels = ["\\Inbox", "Work", "", "INBOX", "work", "Work", "Travel"];
        assert_eq!(
            shown_gmail_labels(labels.into_iter(), "inbox"),
            ["Work", "work", "Travel"]
        );
    }
}
