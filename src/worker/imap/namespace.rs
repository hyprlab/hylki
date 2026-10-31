//! What an IMAP server says about its folder hierarchy (#554): the
//! delimiter and the personal namespace, from NAMESPACE (RFC 2342) where
//! the server has it and from `LIST "" ""` (RFC 3501 §6.3.8) where not.
//!
//! async-imap has no NAMESPACE command, and its parser fails the whole
//! session on an untagged reply it doesn't know. So while the command is
//! out, [`ImapIo`](super::stream::ImapIo) runs what arrives through a
//! [`NamespaceFilter`], which keeps the `* NAMESPACE` line back and lets
//! everything else through.

use futures::TryStreamExt;

use crate::models::{Folder, FolderHierarchy, FolderKind};
use crate::worker::imap::stream::ImapIo;
use crate::worker::imap::wire::{cmd_head, imap_unquote, wire, wired, wired_err};

/// Any session over [`ImapIo`]: the real one, or a test's.
type Over<T> = async_imap::Session<ImapIo<T>>;

pub(super) trait Wire:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + std::fmt::Debug + Send
{
}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + std::fmt::Debug + Send> Wire for T {}

/// Splits the bytes off the wire into whole responses, literals and all,
/// and holds back a `* NAMESPACE` one.
#[derive(Debug, Default)]
pub(super) struct NamespaceFilter {
    /// The response being put together.
    line: Vec<u8>,
    /// Where in `line` the last literal ended: a CRLF only ends the
    /// response after it.
    scan_from: usize,
    /// Bytes of a literal still to come.
    literal: usize,
    found: Option<Vec<u8>>,
}

impl NamespaceFilter {
    /// Take `bytes` off the wire, adding to `out` what goes on to async-imap.
    pub(super) fn feed(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        for &b in bytes {
            self.line.push(b);
            if self.literal > 0 {
                self.literal -= 1;
                if self.literal == 0 {
                    self.scan_from = self.line.len();
                }
                continue;
            }
            if b != b'\n' || self.line.len() < self.scan_from + 2 || !self.line.ends_with(b"\r\n") {
                continue;
            }
            let text = &self.line[self.scan_from..self.line.len() - 2];
            if let Some(n) = literal_len(text) {
                self.literal = n;
                self.scan_from = self.line.len();
                continue;
            }
            if is_namespace(&self.line) && self.found.is_none() {
                self.found = Some(std::mem::take(&mut self.line));
            } else {
                out.append(&mut self.line);
            }
            self.scan_from = 0;
        }
    }

    /// The command is done: anything still held goes on, and the NAMESPACE
    /// answer, if one came, comes back.
    pub(super) fn finish(self, out: &mut Vec<u8>) -> Option<Vec<u8>> {
        out.extend(self.line);
        self.found
    }
}

/// The length of the literal a line announces at its end (`{12}` or the
/// LITERAL+ `{12+}`), if it does.
fn literal_len(text: &[u8]) -> Option<usize> {
    let body = text.strip_suffix(b"}")?;
    let open = body.iter().rposition(|&b| b == b'{')?;
    let digits = &body[open + 1..];
    let digits = digits.strip_suffix(b"+").unwrap_or(digits);
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

fn is_namespace(line: &[u8]) -> bool {
    line.get(..12)
        .is_some_and(|head| head.eq_ignore_ascii_case(b"* NAMESPACE "))
}

/// The personal namespace in a NAMESPACE answer: `Ok(None)` for NIL, the
/// server having none; else the first one's prefix and delimiter (NIL for
/// a flat one). `Err` for an answer that doesn't read as one.
pub(super) fn personal_namespace(raw: &[u8]) -> Result<Option<(String, Option<char>)>, ()> {
    let mut p = Reader {
        s: raw.get(12..).ok_or(())?,
        at: 0,
    };
    p.spaces();
    if p.nil() {
        return Ok(None);
    }
    p.expect(b'(')?;
    p.spaces();
    p.expect(b'(')?;
    let prefix = p.string()?;
    p.spaces();
    let delimiter = if p.nil() {
        None
    } else {
        let d = String::from_utf8(p.string()?).map_err(|_| ())?;
        let mut chars = d.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Some(c),
            _ => return Err(()),
        }
    };
    Ok(Some((
        String::from_utf8_lossy(&prefix).into_owned(),
        delimiter,
    )))
}

struct Reader<'a> {
    s: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn spaces(&mut self) {
        while self.s.get(self.at) == Some(&b' ') {
            self.at += 1;
        }
    }

    fn nil(&mut self) -> bool {
        let hit = self
            .s
            .get(self.at..self.at + 3)
            .is_some_and(|w| w.eq_ignore_ascii_case(b"NIL"));
        if hit {
            self.at += 3;
        }
        hit
    }

    fn expect(&mut self, b: u8) -> Result<(), ()> {
        if self.s.get(self.at) == Some(&b) {
            self.at += 1;
            Ok(())
        } else {
            Err(())
        }
    }

    /// A quoted string or a literal.
    fn string(&mut self) -> Result<Vec<u8>, ()> {
        match self.s.get(self.at) {
            Some(b'"') => {
                self.at += 1;
                let mut out = Vec::new();
                loop {
                    match *self.s.get(self.at).ok_or(())? {
                        b'"' => {
                            self.at += 1;
                            return Ok(out);
                        }
                        b'\\' => {
                            out.push(*self.s.get(self.at + 1).ok_or(())?);
                            self.at += 2;
                        }
                        b => {
                            out.push(b);
                            self.at += 1;
                        }
                    }
                }
            }
            Some(b'{') => {
                let close = self.s[self.at..]
                    .iter()
                    .position(|&b| b == b'}')
                    .ok_or(())?
                    + self.at;
                let n = literal_len(&self.s[self.at..=close]).ok_or(())?;
                let start = close + 3; // past "}\r\n"
                let body = self.s.get(start..start + n).ok_or(())?;
                self.at = start + n;
                Ok(body.to_vec())
            }
            _ => Err(()),
        }
    }
}

/// Ask for NAMESPACE, the reply kept away from async-imap's parser. `None`
/// when the server refused it or the answer didn't read.
async fn ask_namespace<T: Wire>(session: &mut Over<T>) -> Option<Option<(String, Option<char>)>> {
    use async_imap::imap_proto::{Response, Status};
    let cmd = "NAMESPACE";
    wire(cmd);
    session.as_mut().hold_namespace();
    let answered = match session.run_command(cmd).await {
        Ok(id) => loop {
            match session.read_response().await {
                Some(Ok(reply)) => match reply.parsed() {
                    Response::Done { tag, status, .. } if *tag == id => {
                        let ok = matches!(status, Status::Ok);
                        wired::<()>(
                            cmd,
                            &if ok {
                                Ok(())
                            } else {
                                Err(async_imap::error::Error::No(format!("{status:?}")))
                            },
                        );
                        break ok;
                    }
                    _ => {}
                },
                Some(Err(e)) => {
                    wired_err(cmd, &async_imap::error::Error::Io(e));
                    break false;
                }
                None => {
                    wired_err(cmd, &async_imap::error::Error::ConnectionLost);
                    break false;
                }
            }
        },
        Err(e) => {
            wired_err(cmd, &e);
            false
        }
    };
    let raw = session.as_mut().release_namespace();
    if !answered {
        return None;
    }
    let raw = raw?;
    tracing::debug!(
        target: "hylki::imap",
        "< {} ({})",
        String::from_utf8_lossy(&raw).trim_end(),
        cmd_head(cmd)
    );
    personal_namespace(&raw).ok()
}

/// The root's delimiter from `LIST "" ""`: `Some(None)` for a flat server,
/// `None` when the server didn't say.
async fn root_delimiter<T: Wire>(session: &mut Over<T>) -> Option<Option<char>> {
    let names: Vec<async_imap::types::Name> = match session.list(Some(""), None).await {
        Ok(stream) => stream.try_collect().await.ok()?,
        Err(_) => return None,
    };
    let name = names.first()?;
    Some(
        name.delimiter()
            .map(|d| imap_unquote(d).into_owned())
            .and_then(|d| d.chars().next()),
    )
}

/// Whether the top level is Proton Bridge's: `Folders` and `Labels`, two
/// containers that hold no mail but can hold mailboxes. Bridge has no
/// NAMESPACE and refuses a folder at the top; Proton keeps the user's
/// folders in `Folders` and labels in `Labels`.
async fn keeps_proton_folders<T: Wire>(session: &mut Over<T>) -> bool {
    use async_imap::types::NameAttribute;
    let names: Vec<async_imap::types::Name> = match session.list(Some(""), Some("%")).await {
        Ok(stream) => stream.try_collect().await.unwrap_or_default(),
        Err(_) => return false,
    };
    let container = |want: &str| {
        names.iter().any(|n| {
            imap_unquote(n.name()) == want
                && n.attributes().contains(&NameAttribute::NoSelect)
                && !n.attributes().contains(&NameAttribute::NoInferiors)
        })
    };
    container("Folders") && container("Labels")
}

/// Where a server without NAMESPACE puts its folders: RFC 3501 has no way
/// to ask, so the listing answers it. A server that keeps every folder of
/// the account inside the Inbox (Courier and Cyrus before NAMESPACE) makes
/// new ones there too.
fn listed_prefix(folders: &[Folder], delimiter: Option<char>) -> String {
    let Some(d) = delimiter else {
        return String::new();
    };
    let root = format!("INBOX{d}");
    let mut others = folders
        .iter()
        .filter(|f| f.kind != FolderKind::Inbox)
        .peekable();
    let inside = |f: &Folder| {
        f.path
            .get(..root.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(&root))
    };
    if others.peek().is_some() && others.all(inside) {
        // As the server spells it: "INBOX" may come back in any case.
        folders
            .iter()
            .find(|f| f.kind != FolderKind::Inbox)
            .map_or(root.clone(), |f| f.path[..root.len()].to_string())
    } else {
        String::new()
    }
}

/// The account's hierarchy, as the server tells it. `folders` is the
/// listing just made, for a server without NAMESPACE. `None` when the
/// server answered neither question: the one known before stands.
pub(super) async fn read_hierarchy<T: Wire>(
    session: &mut Over<T>,
    folders: &[Folder],
) -> Option<FolderHierarchy> {
    let has_namespace = session
        .capabilities()
        .await
        .is_ok_and(|caps| caps.has_str("NAMESPACE"));
    if has_namespace {
        match ask_namespace(session).await {
            Some(Some((prefix, delimiter))) => {
                return Some(FolderHierarchy::imap(delimiter, Some(prefix)))
            }
            // No personal namespace: the root's delimiter still says how
            // the folders there nest.
            Some(None) => {
                let delimiter = root_delimiter(session).await?;
                return Some(FolderHierarchy::imap(delimiter, None));
            }
            None => {}
        }
    }
    let delimiter = root_delimiter(session).await?;
    if let Some(d) = delimiter {
        if keeps_proton_folders(session).await {
            return Some(FolderHierarchy::imap(
                delimiter,
                Some(format!("Folders{d}")),
            ));
        }
    }
    Some(FolderHierarchy::imap(delimiter, Some(listed_prefix(folders, delimiter))).unsaid())
}

/// Where a folder the server refused at the top can go instead: inside the
/// Inbox, the one other place there is, when the server didn't say where
/// its folders go (no NAMESPACE) and `path` is at the top. With the
/// hierarchy it was refused under.
pub(super) fn inside_inbox(
    cache: Option<&crate::cache::Cache>,
    account_id: u32,
    path: &str,
) -> Option<(FolderHierarchy, String)> {
    let cache = cache?;
    let h = cache.load_imap_hierarchy(account_id)?;
    let d = h.delimiter?;
    if h.prefix_said || !h.prefix.is_empty() || path.contains(d) {
        return None;
    }
    // Not where the Inbox can hold no folder either.
    if cache
        .load_folders(account_id)
        .iter()
        .any(|f| f.kind == FolderKind::Inbox && f.childless)
    {
        return None;
    }
    let inside = format!("INBOX{d}{path}");
    Some((h, inside))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&[u8]]) -> (Vec<u8>, Option<Vec<u8>>) {
        let mut filter = NamespaceFilter::default();
        let mut out = Vec::new();
        for c in chunks {
            filter.feed(c, &mut out);
        }
        let found = filter.finish(&mut out);
        (out, found)
    }

    #[test]
    fn the_namespace_line_is_kept_back_and_the_rest_goes_on() {
        let (out, found) =
            run(&[b"* 3 EXISTS\r\n* NAMESPACE ((\"\" \"/\")) NIL NIL\r\nA7 OK done\r\n"]);
        assert_eq!(out, b"* 3 EXISTS\r\nA7 OK done\r\n");
        assert_eq!(found.unwrap(), b"* NAMESPACE ((\"\" \"/\")) NIL NIL\r\n");
    }

    #[test]
    fn split_reads_and_literals_are_followed() {
        // Every byte its own read, and a literal holding a line that looks
        // like the answer, which is mail and must go on untouched.
        let wire: &[u8] =
            b"* 1 FETCH (BODY[] {16}\r\n* NAMESPACE x\r\n)\r\n* namespace ((\"INBOX.\" \".\")) NIL NIL\r\nA1 OK\r\n";
        let chunks: Vec<&[u8]> = wire.chunks(1).collect();
        let (out, found) = run(&chunks);
        assert_eq!(
            out,
            b"* 1 FETCH (BODY[] {16}\r\n* NAMESPACE x\r\n)\r\nA1 OK\r\n"
        );
        assert_eq!(
            personal_namespace(&found.unwrap()).unwrap(),
            Some(("INBOX.".to_string(), Some('.')))
        );
    }

    #[test]
    fn a_partial_line_still_goes_on_when_the_command_ends() {
        let (out, found) = run(&[b"A1 OK\r\n* 4 EXI"]);
        assert_eq!(out, b"A1 OK\r\n* 4 EXI");
        assert!(found.is_none());
    }

    #[test]
    fn every_shape_of_namespace_answer_reads() {
        let read = |s: &str| personal_namespace(s.as_bytes());
        // Dovecot, Gmail, Exchange.
        assert_eq!(
            read("* NAMESPACE ((\"\" \"/\")) NIL NIL\r\n"),
            Ok(Some((String::new(), Some('/'))))
        );
        // Courier and Cyrus, with other namespaces after.
        assert_eq!(
            read("* NAMESPACE ((\"INBOX.\" \".\")(\"#mh/\" \"/\")) NIL ((\"#shared.\" \".\"))\r\n"),
            Ok(Some(("INBOX.".to_string(), Some('.'))))
        );
        // A flat server, and none of the user's own at all.
        assert_eq!(
            read("* NAMESPACE ((\"\" NIL)) NIL NIL\r\n"),
            Ok(Some((String::new(), None)))
        );
        assert_eq!(read("* NAMESPACE NIL NIL ((\"\" \"/\"))\r\n"), Ok(None));
        // A backslash, escaped; extension data; a literal prefix.
        assert_eq!(
            read("* NAMESPACE ((\"\" \"\\\\\" \"X-PARAM\" (\"A\"))) NIL NIL\r\n"),
            Ok(Some((String::new(), Some('\\'))))
        );
        assert_eq!(
            read("* NAMESPACE (({6}\r\nINBOX/ \"/\")) NIL NIL\r\n"),
            Ok(Some(("INBOX/".to_string(), Some('/'))))
        );
        assert_eq!(read("* NAMESPACE (oops)\r\n"), Err(()));
        assert_eq!(read("* NAMESPACE ((\"\" \"ab\")) NIL NIL\r\n"), Err(()));
    }

    #[test]
    fn without_namespace_the_listing_says_where_folders_go() {
        let folder = |path: &str, kind| Folder {
            id: 0,
            account_id: 1,
            name: path.into(),
            path: path.into(),
            kind,
            unread: 0,
            childless: false,
        };
        let courier = [
            folder("INBOX", FolderKind::Inbox),
            folder("INBOX.Sent", FolderKind::Sent),
            folder("INBOX.Work", FolderKind::Custom),
        ];
        assert_eq!(listed_prefix(&courier, Some('.')), "INBOX.");
        // Sent at the top: the top is where folders go, even with every
        // custom folder inside the Inbox.
        let nested = [
            folder("INBOX", FolderKind::Inbox),
            folder("Sent", FolderKind::Sent),
            folder("INBOX/Work", FolderKind::Custom),
        ];
        assert_eq!(listed_prefix(&nested, Some('/')), "");
        assert_eq!(listed_prefix(&courier[..1], Some('.')), "");
        assert_eq!(listed_prefix(&courier, None), "");
        let lower = [
            folder("INBOX", FolderKind::Inbox),
            folder("inbox.Work", FolderKind::Custom),
        ];
        assert_eq!(listed_prefix(&lower, Some('.')), "inbox.");
    }
}
