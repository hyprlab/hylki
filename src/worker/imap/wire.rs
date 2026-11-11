//! The IMAP commands as Hylki sends them, each logged for the console
//! (#132): selection, UID sets, mailbox names, and thin wrappers that log
//! the command and its verdict.

use async_imap::types::Fetch;
use futures::TryStreamExt;

// ---------------------------------------------------------------------------
// The IMAP conversation, for the console (#132 and every "it failed on my
// server" report): each command Hylki sends is logged as `> …` and the
// server's verdict as `< OK` or `< <error>`, under the `hylki::imap`
// target — commands and results only, never a message body or a password.
// The console keeps it whether or not console mode is on, so "Export log"
// carries it after the fact; stderr sees only the failures.
use crate::worker::imap::fetch_mode::compact_parse_error;

use crate::worker::imap::ImapSession;

pub(in crate::worker) fn wire(cmd: &str) {
    tracing::debug!(target: "hylki::imap", "> {cmd}");
}

pub(super) fn wired<T>(cmd: &str, r: &Result<T, async_imap::error::Error>) {
    match r {
        Ok(_) => tracing::debug!(target: "hylki::imap", "< OK ({})", cmd_head(cmd)),
        Err(e) => wired_err(cmd, e),
    }
}

/// [`wired`] for a failure the caller still holds.
pub(super) fn wired_err(cmd: &str, e: &async_imap::error::Error) {
    tracing::warn!(target: "hylki::imap", "< {} ({cmd})", compact_parse_error(&e.to_string()))
}

/// The command's verb and first argument, for the terse OK line.
pub(super) fn cmd_head(cmd: &str) -> String {
    cmd.split_whitespace().take(3).collect::<Vec<_>>().join(" ")
}

pub(super) async fn sel(
    session: &mut ImapSession,
    path: &str,
) -> Result<async_imap::types::Mailbox, async_imap::error::Error> {
    let cmd = format!("SELECT {}", quote_mailbox(path));
    wire(&cmd);
    session.as_mut().set_selected_folder(None);
    let r = session.select(path).await;
    if r.is_ok() {
        session.as_mut().set_selected_folder(Some(path));
        session.as_mut().set_selected_writable();
    }
    wired(&cmd, &r);
    r
}

pub(super) async fn exam(
    session: &mut ImapSession,
    path: &str,
) -> Result<async_imap::types::Mailbox, async_imap::error::Error> {
    let cmd = format!("EXAMINE {}", quote_mailbox(path));
    wire(&cmd);
    session.as_mut().set_selected_folder(None);
    let r = session.examine(path).await;
    if r.is_ok() {
        session.as_mut().set_selected_folder(Some(path));
    }
    wired(&cmd, &r);
    r
}

/// PEEK reads work in either SELECT or EXAMINE. Reusing the mailbox already
/// selected on this connection avoids a round trip for every prefetched body.
/// List syncs still select explicitly to get current metadata, and writes
/// go through [`select_for_write`]. A new connection starts unselected.
pub(super) async fn select_for_read(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    if !session.as_mut().is_selected(path) {
        sel(session, path).await?;
    }
    Ok(())
}

/// A STORE or MOVE by UID needs the mailbox open read-write and nothing
/// fresher: UIDs do not change while it stays selected. Re-selecting before
/// each one cost a full SELECT per read toggle or archive, 2 to 3 seconds
/// apiece on a large Gmail inbox, and the reader queued behind them (#296).
/// Callers that read the SELECT reply (EXISTS, PERMANENTFLAGS) use [`sel`].
pub(super) async fn select_for_write(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    if !session.as_mut().is_selected_writable(path) {
        sel(session, path).await?;
    }
    Ok(())
}

pub(super) async fn fetch_uids<'a, S1: AsRef<str>, S2: AsRef<str>>(
    session: &'a mut ImapSession,
    set: S1,
    items: S2,
) -> Result<
    impl futures::Stream<Item = Result<Fetch, async_imap::error::Error>> + 'a + Send + Unpin,
    async_imap::error::Error,
> {
    let (set, items) = (set.as_ref().to_string(), items.as_ref().to_string());
    let cmd = format!("UID FETCH {set} {items}");
    wire(&cmd);
    let r = session.uid_fetch(set, items).await;
    wired(&cmd, &r);
    r
}

pub(super) async fn store_uids<'a, S1: AsRef<str>, S2: AsRef<str>>(
    session: &'a mut ImapSession,
    set: S1,
    query: S2,
) -> Result<
    impl futures::Stream<Item = Result<Fetch, async_imap::error::Error>> + 'a + Send + Unpin,
    async_imap::error::Error,
> {
    let (set, query) = (set.as_ref().to_string(), query.as_ref().to_string());
    let cmd = format!("UID STORE {set} {query}");
    wire(&cmd);
    let r = session.uid_store(set, query).await;
    wired(&cmd, &r);
    r
}

pub(super) async fn copy_uids(
    session: &mut ImapSession,
    set: &str,
    dest: &str,
) -> Result<(), async_imap::error::Error> {
    let cmd = format!("UID COPY {set} {}", quote_mailbox(dest));
    wire(&cmd);
    let r = session.uid_copy(set, quote_mailbox(dest)).await;
    wired(&cmd, &r);
    r
}

pub(super) async fn mv_uids(
    session: &mut ImapSession,
    set: &str,
    dest: &str,
) -> Result<(), async_imap::error::Error> {
    let cmd = format!("UID MOVE {set} {}", quote_mailbox(dest));
    wire(&cmd);
    let r = session.uid_mv(set, dest).await;
    wired(&cmd, &r);
    r
}

pub(super) async fn expunge_uids<'a, S: AsRef<str>>(
    session: &'a mut ImapSession,
    set: S,
) -> Result<
    impl futures::Stream<Item = Result<u32, async_imap::error::Error>> + 'a + Send,
    async_imap::error::Error,
> {
    let set = set.as_ref().to_string();
    let cmd = format!("UID EXPUNGE {set}");
    wire(&cmd);
    let r = session.uid_expunge(set).await;
    wired(&cmd, &r);
    r
}

pub(super) async fn expunge_all<'a>(
    session: &'a mut ImapSession,
) -> Result<
    impl futures::Stream<Item = Result<u32, async_imap::error::Error>> + 'a + Send,
    async_imap::error::Error,
> {
    let cmd = "EXPUNGE";
    wire(cmd);
    let r = session.expunge().await;
    wired(cmd, &r);
    r
}

pub(super) async fn search_uids<S: AsRef<str>>(
    session: &mut ImapSession,
    query: S,
) -> Result<std::collections::HashSet<u32>, async_imap::error::Error> {
    let cmd = format!("UID SEARCH {}", query.as_ref());
    wire(&cmd);
    let r = session.uid_search(query.as_ref()).await;
    wired(&cmd, &r);
    r
}

pub(super) async fn create_box(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    let cmd = format!("CREATE {}", quote_mailbox(path));
    wire(&cmd);
    let r = session.create(path).await;
    wired(&cmd, &r);
    r
}

pub(super) async fn subscribe_box(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    let cmd = format!("SUBSCRIBE {}", quote_mailbox(path));
    wire(&cmd);
    let r = session.subscribe(path).await;
    wired(&cmd, &r);
    r
}

pub(super) async fn unsubscribe_box(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    let cmd = format!("UNSUBSCRIBE {}", quote_mailbox(path));
    wire(&cmd);
    let r = session.unsubscribe(path).await;
    wired(&cmd, &r);
    r
}

pub(super) async fn rename_box(
    session: &mut ImapSession,
    from: &str,
    to: &str,
) -> Result<(), async_imap::error::Error> {
    let cmd = format!("RENAME {} {}", quote_mailbox(from), quote_mailbox(to));
    wire(&cmd);
    let r = session.rename(from, to).await;
    wired(&cmd, &r);
    r
}

pub(super) async fn delete_box(
    session: &mut ImapSession,
    path: &str,
) -> Result<(), async_imap::error::Error> {
    let cmd = format!("DELETE {}", quote_mailbox(path));
    wire(&cmd);
    let r = session.delete(path).await;
    wired(&cmd, &r);
    r
}

pub(super) async fn box_status(
    session: &mut ImapSession,
    path: &str,
    items: &str,
) -> Result<async_imap::types::Mailbox, async_imap::error::Error> {
    let cmd = format!("STATUS {} {items}", quote_mailbox(path));
    wire(&cmd);
    let r = session.status(path, items).await;
    wired(&cmd, &r);
    r
}

pub(super) async fn append_msg(
    session: &mut ImapSession,
    path: &str,
    flags: Option<&str>,
    raw: &[u8],
) -> Result<(), async_imap::error::Error> {
    let cmd = format!(
        "APPEND {} {} ({} bytes)",
        quote_mailbox(path),
        flags.unwrap_or(""),
        raw.len()
    );
    wire(&cmd);
    let r = session.append(path, flags, None, raw).await;
    wired(&cmd, &r);
    r
}

pub(super) async fn append_dated(
    session: &mut ImapSession,
    path: &str,
    flags: Option<&str>,
    date: Option<&str>,
    raw: &[u8],
) -> Result<(), async_imap::error::Error> {
    let cmd = format!(
        "APPEND {} {} {} ({} bytes)",
        quote_mailbox(path),
        flags.unwrap_or(""),
        date.unwrap_or(""),
        raw.len()
    );
    wire(&cmd);
    let r = session.append(path, flags, date, raw).await;
    wired(&cmd, &r);
    r
}

/// A mailbox name as an IMAP quoted string: backslashes and quotes escaped,
/// the whole wrapped in quotes (RFC 3501 §4.3).
pub(super) fn quote_mailbox(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    out.push('"');
    for c in name.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// A LIST or LSUB name as the server meant it. imap-proto hands a quoted
/// string back with its escapes still in, so `"\\All"` arrives as `\\All`
/// and quote_mailbox would escape it a second time (#514). Atoms cannot hold
/// a backslash, so only a literal could be wrongly unescaped, and only one
/// that holds `\\` or `\"`.
pub(super) fn imap_unquote(raw: &str) -> std::borrow::Cow<'_, str> {
    if !raw.contains('\\') {
        return std::borrow::Cow::Borrowed(raw);
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next @ ('\\' | '"')) => out.push(next),
                Some(next) => {
                    out.push(c);
                    out.push(next);
                }
                None => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// A sequence-number FETCH through the conversation log, like the UID
/// wrappers: the summary fetch is the one a server rejects (#226), and its
/// command and verdict were the two lines missing from the export.
pub(super) async fn logged_fetch(
    session: &mut ImapSession,
    range: &str,
    query: &str,
) -> Result<Vec<Fetch>, async_imap::error::Error> {
    let cmd = format!("FETCH {range} {query}");
    wire(&cmd);
    let r: Result<Vec<Fetch>, async_imap::error::Error> =
        async { session.fetch(range, query).await?.try_collect().await }.await;
    wired(&cmd, &r);
    r
}

/// A UID list as an IMAP sequence set: sorted, without repeats, and with
/// runs collapsed to ranges (`3,4,5,9` → `3:5,9`). Every command that takes
/// a set goes through here: iCloud (and Exchange) answer an unsorted set —
/// which a selection taken in list order, newest first, always was — with
/// "Parse Error", so a bulk delete or move there failed outright.
pub(super) fn uid_set(uids: &[u32]) -> String {
    let mut sorted: Vec<u32> = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = String::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        if !out.is_empty() {
            out.push(',');
        }
        if end > start {
            out.push_str(&format!("{start}:{end}"));
        } else {
            out.push_str(&start.to_string());
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod uid_set_tests {
    use super::{imap_unquote, quote_mailbox, uid_set};
    use crate::worker::imap::wire::cmd_head;

    #[test]
    fn mailbox_names_are_quoted_for_copy() {
        assert_eq!(quote_mailbox("Deleted Messages"), "\"Deleted Messages\"");
        assert_eq!(quote_mailbox("INBOX"), "\"INBOX\"");
        assert_eq!(quote_mailbox("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn listed_names_go_back_to_the_server_as_listed() {
        use async_imap::imap_proto::{MailboxDatum, Response};
        let line = b"* LIST (\\HasNoChildren) \"\\\\\" \"\\\\All\"\r\n";
        let Ok((
            _,
            Response::MailboxData(MailboxDatum::List {
                delimiter, name, ..
            }),
        )) = async_imap::imap_proto::parser::parse_response(line)
        else {
            panic!("LIST did not parse");
        };
        assert_eq!(imap_unquote(&name), "\\All");
        assert_eq!(
            delimiter.as_deref().map(imap_unquote).as_deref(),
            Some("\\")
        );
        assert_eq!(quote_mailbox(&imap_unquote(&name)), "\"\\\\All\"");
        assert_eq!(imap_unquote("[Gmail]/All Mail"), "[Gmail]/All Mail");
        assert_eq!(imap_unquote("a\\\"b\\x"), "a\"b\\x");
    }

    #[test]
    fn sets_are_sorted_deduplicated_and_ranged() {
        assert_eq!(uid_set(&[1005, 1003, 999, 1004]), "999,1003:1005");
        assert_eq!(uid_set(&[7, 7, 7]), "7");
        assert_eq!(uid_set(&[3, 4, 5, 9]), "3:5,9");
        assert_eq!(uid_set(&[]), "");
    }

    #[test]
    fn the_wire_log_shows_three_words_of_a_command() {
        assert_eq!(cmd_head("UID FETCH 1:5 (FLAGS)"), "UID FETCH 1:5");
        assert_eq!(cmd_head("  NOOP "), "NOOP");
        assert_eq!(cmd_head(""), "");
    }
}
