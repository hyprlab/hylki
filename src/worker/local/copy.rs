//! Copying an account's mail to On This Computer as it comes down (#552).
//!
//! An account with the setting on downloads every message in the background
//! the way it keeps mail offline (#536), and each one is stored in the
//! local tree too, under a folder named after the account. It is an
//! archive: what is deleted or moved on the server stays as it was copied.
//!
//! The copy is written from the account's own worker, as a Maildir
//! delivery: the local account's worker finds it the next time it reads
//! the folder. Which messages are copied is kept in the cache, and in a
//! list beside the tree that fills the cache again when it is dropped, so
//! nothing is copied twice.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use crate::cache::{Cache, CopyKey};
use crate::config::AccountConfig;
use crate::models::FolderKind;

use super::maildir::{deliver_into, INBOX};

/// Where one account's copies go.
#[derive(Clone)]
struct Target {
    root: PathBuf,
    /// The local folder the account's folders are copied under.
    top: String,
    /// The list of what is copied.
    ledger: PathBuf,
}

static TARGETS: LazyLock<Mutex<HashMap<u32, Target>>> = LazyLock::new(Default::default);

/// Rung when a copy lands, or what is left to copy changes, so the local
/// account's worker shows it.
pub(super) static DELIVERED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// How many messages each copying account has left, as its queue last
/// counted them less what it copied since.
static PENDING: LazyLock<Mutex<HashMap<u32, usize>>> = LazyLock::new(Default::default);

/// The account's queue counted what it has left to copy.
pub(in crate::worker) fn set_pending(account_id: u32, left: usize) {
    let mut pending = PENDING.lock().unwrap_or_else(|p| p.into_inner());
    let before = pending.get(&account_id).copied().unwrap_or(0);
    if left == 0 {
        pending.remove(&account_id);
    } else {
        pending.insert(account_id, left);
    }
    if before != left {
        DELIVERED.notify_one();
    }
}

/// Messages left to copy, every account together.
pub(super) fn pending_total() -> usize {
    PENDING
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .values()
        .sum()
}

/// Take note of an account's setting as its worker starts, and read the
/// list of what it has copied back into the cache.
pub(in crate::worker) fn register(account_id: u32, account: &AccountConfig) {
    match crate::config::local_mail_dir() {
        Some(root) => register_at(account_id, account, root),
        None => forget(account_id),
    }
}

/// Stop copying an account's mail.
pub(in crate::worker) fn forget(account_id: u32) {
    TARGETS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&account_id);
    set_pending(account_id, 0);
}

/// [`register`], copying into the tree at `root`.
pub(in crate::worker) fn register_at(account_id: u32, account: &AccountConfig, root: PathBuf) {
    let mut targets = TARGETS.lock().unwrap_or_else(|p| p.into_inner());
    if !account.copy_to_local || account.is_local() {
        targets.remove(&account_id);
        drop(targets);
        set_pending(account_id, 0);
        return;
    }
    let ledger = root.join("hylki-copied").join(ledger_name(&account.email));
    let keys = read_ledger(&ledger);
    if let Ok(c) = Cache::open() {
        if c.local_copy_count(account_id) < keys.len() {
            c.note_local_copies(account_id, &keys);
        }
    }
    targets.insert(
        account_id,
        Target {
            root,
            top: top_folder(account),
            ledger,
        },
    );
}

/// Copy a message just downloaded, if its account copies and it is not
/// copied yet.
pub(in crate::worker) fn copy(cache: &Cache, account_id: u32, path: &str, uid: u32, raw: &[u8]) {
    let target = {
        let targets = TARGETS.lock().unwrap_or_else(|p| p.into_inner());
        match targets.get(&account_id) {
            Some(t) => t.clone(),
            None => return,
        }
    };
    let (unread, starred, message_id) =
        cache
            .message_state(account_id, path, uid)
            .unwrap_or((false, false, String::new()));
    let key: CopyKey = (message_id, path.to_string(), uid);
    if cache.is_local_copy(account_id, &key) {
        return;
    }
    let kind = cache
        .load_folders(account_id)
        .into_iter()
        .find(|f| f.path == path)
        .map(|f| f.kind);
    let dest = format!("{}/{}", target.top, local_name(path, kind));
    let flags = format!(
        "{}{}",
        if starred { "F" } else { "" },
        if unread { "" } else { "S" }
    );
    match deliver_into(&target.root, &dest, raw, &flags) {
        Ok(()) => {
            append_ledger(&target.ledger, &key);
            cache.note_local_copy(account_id, &key);
            if let Some(left) = PENDING
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get_mut(&account_id)
            {
                *left = left.saturating_sub(1);
            }
            DELIVERED.notify_one();
        }
        Err(e) => tracing::warn!("local mail: could not copy {path}/{uid} to {dest}: {e}"),
    }
}

/// The local folder an account's mail is copied under: its name in the
/// sidebar.
fn top_folder(account: &AccountConfig) -> String {
    let label = account.display_label();
    let label = label.trim().replace('/', "∕");
    if label.is_empty() {
        account.email.replace('/', "∕")
    } else {
        label
    }
}

/// A server folder's name under the account's local folder: the Inbox by
/// that name, levels split on `/`, or on the dots of a Dovecot or Courier
/// `INBOX.Name`, and IMAP's modified UTF-7 decoded.
fn local_name(path: &str, kind: Option<FolderKind>) -> String {
    if kind == Some(FolderKind::Inbox) || path.eq_ignore_ascii_case(INBOX) {
        return "Inbox".to_string();
    }
    let parts: Vec<&str> = if path.contains('/') {
        path.split('/').collect()
    } else if path.len() > 6 && path[..6].eq_ignore_ascii_case("INBOX.") {
        path[6..].split('.').collect()
    } else {
        vec![path]
    };
    let names: Vec<String> = parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .map(crate::mail::mutf7::decode)
        .map(|p| p.replace('/', "∕"))
        .collect();
    if names.is_empty() {
        "Inbox".to_string()
    } else {
        names.join("/")
    }
}

/// The list's file name for an account: its address, made safe.
fn ledger_name(email: &str) -> String {
    let safe: String = email
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "@.-_+".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}.txt")
}

/// One line per copied message: `id <Message-ID>`, or `at <path> <uid>`
/// for one without.
fn ledger_line(key: &CopyKey) -> String {
    if key.0.is_empty() {
        format!("at\t{}\t{}\n", key.1, key.2)
    } else {
        format!("id\t{}\n", key.0)
    }
}

fn append_ledger(ledger: &std::path::Path, key: &CopyKey) {
    let written = ledger
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(ledger)
        })
        .and_then(|mut f| f.write_all(ledger_line(key).as_bytes()));
    if let Err(e) = written {
        tracing::warn!(
            "local mail: could not note a copy in {}: {e}",
            ledger.display()
        );
    }
}

fn read_ledger(ledger: &std::path::Path) -> Vec<CopyKey> {
    let Ok(text) = std::fs::read_to_string(ledger) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            match f.next()? {
                "id" => Some((f.next()?.to_string(), String::new(), 0)),
                "at" => {
                    let path = f.next()?.to_string();
                    let uid = f.next()?.parse().ok()?;
                    Some((String::new(), path, uid))
                }
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_folders_are_named_under_the_account() {
        assert_eq!(local_name("INBOX", Some(FolderKind::Inbox)), "Inbox");
        assert_eq!(local_name("INBOX.Sent", Some(FolderKind::Sent)), "Sent");
        assert_eq!(local_name("INBOX.Work.2025", None), "Work/2025");
        assert_eq!(local_name("[Gmail]/Sent Mail", None), "[Gmail]/Sent Mail");
        assert_eq!(local_name("Entw&APw-rfe", None), "Entwürfe");
        assert_eq!(local_name("Archive", None), "Archive");
    }

    #[test]
    fn the_ledger_reads_back_what_it_wrote() {
        let dir = super::super::maildir::tests::Scratch::new();
        let ledger = dir.path().join("l").join("a.txt");
        append_ledger(&ledger, &("m1@x".into(), "INBOX".into(), 3));
        append_ledger(&ledger, &(String::new(), "Sent".into(), 9));
        assert_eq!(
            read_ledger(&ledger),
            [
                ("m1@x".to_string(), String::new(), 0),
                (String::new(), "Sent".to_string(), 9)
            ]
        );
        assert_eq!(ledger_name("a b/c@x.org"), "a_b_c@x.org.txt");
    }

    #[test]
    fn a_copy_lands_once_under_the_account_folder() {
        let dir = super::super::maildir::tests::Scratch::new();
        let root = dir.path().join("local");
        let cache = Cache::in_memory().unwrap();
        let mut m = crate::worker::summary_from_raw(5, 1, 7, RAW);
        m.unread = false;
        cache.save_messages(5, "INBOX", &[m]);
        TARGETS.lock().unwrap().insert(
            5,
            Target {
                root: root.clone(),
                top: "Work".into(),
                ledger: dir.path().join("ledger.txt"),
            },
        );
        copy(&cache, 5, "INBOX", 7, RAW);
        copy(&cache, 5, "INBOX", 7, RAW);
        TARGETS.lock().unwrap().remove(&5);
        let cur: Vec<String> = std::fs::read_dir(root.join(".Work.Inbox/cur"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        assert_eq!(cur.len(), 1);
        assert!(cur[0].ends_with(":2,S"));
        assert!(root.join(".Work/cur").is_dir());
        assert!(cache.is_local_copy(5, &("c1@x".into(), "INBOX".into(), 7)));
    }

    const RAW: &[u8] = b"From: a@x\r\nSubject: s\r\nMessage-ID: <c1@x>\r\n\r\nb\r\n";
}
