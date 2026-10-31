//! Listing an IMAP account's folders: roles, numbering, unread counts, and
//! the quota.

use async_imap::types::NameAttribute;
use futures::TryStreamExt;

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::models::{Folder, FolderKind, MailboxQuota};
use crate::worker::folders::{classify_with_source, display_name, number_folders};
use crate::worker::imap::sweep::selected_unseen;
use crate::worker::imap::wire::{box_status, exam, imap_unquote};
use crate::worker::imap::ImapSession;
use crate::worker::{chip_counts_all, WorkerEvent};

/// The mailbox size out of a GETQUOTAROOT answer: the first quota with a
/// STORAGE resource and a limit, whose figures are in units of 1024 octets.
pub(super) fn imap_storage_quota(quotas: &[async_imap::types::Quota]) -> Option<MailboxQuota> {
    use async_imap::types::QuotaResourceName;
    quotas
        .iter()
        .flat_map(|q| &q.resources)
        .find(|r| r.name == QuotaResourceName::Storage && r.limit > 0)
        .map(|r| MailboxQuota {
            used: r.usage.saturating_mul(1024),
            limit: r.limit.saturating_mul(1024),
        })
}

pub(in crate::worker) async fn list_folders(
    account_id: u32,
    session: &mut ImapSession,
    roles: &std::collections::BTreeMap<String, String>,
    hidden: &[String],
) -> Result<Vec<Folder>, async_imap::error::Error> {
    list_folder_metadata(account_id, session, roles, hidden, None).await
}

/// Discover folders without a serial STATUS round trip for every folder when
/// cached counts are supplied. The interruptible idle sweep refreshes counts.
pub(super) async fn list_folder_metadata(
    account_id: u32,
    session: &mut ImapSession,
    roles: &std::collections::BTreeMap<String, String>,
    hidden: &[String],
    cached_counts: Option<&[Folder]>,
) -> Result<Vec<Folder>, async_imap::error::Error> {
    let names: Vec<async_imap::types::Name> = session
        .list(Some(""), Some("*"))
        .await?
        .try_collect()
        .await?;

    let mut folders = Vec::new();
    let mut special_use = Vec::new(); // parallel: kind came from a SPECIAL-USE attr
    for name in names.iter() {
        // Skip containers that cannot hold messages.
        if name.attributes().contains(&NameAttribute::NoSelect) {
            continue;
        }
        let path = imap_unquote(name.name()).into_owned();
        let delimiter = name.delimiter().map(imap_unquote);
        let delimiter = delimiter.as_deref();
        // A hidden folder (#239) is left out here, ahead of everything
        // that walks the listing: the sweep, the watchers, the counts.
        if crate::models::folder_is_hidden(&path, delimiter, hidden) {
            continue;
        }
        let (kind, by_special_use) = classify_with_source(&path, delimiter, name.attributes());
        special_use.push(by_special_use);
        // A mailbox that can't hold others (RFC 3501 §7.2.2), as an mbox
        // file on Dovecot or UW-IMAP (#554).
        let childless = name.attributes().contains(&NameAttribute::NoInferiors);
        folders.push(Folder {
            id: 0, // assigned by order below
            account_id,
            name: display_name(&path, delimiter),
            path,
            kind,
            unread: 0,
            childless,
        });
    }

    // When a role has a real SPECIAL-USE folder, demote any name-matched impostors
    // of that role to Custom — otherwise a stray folder like a plain "Trash" label
    // can shadow the server's actual [Gmail]/Trash and mail "moved" there never
    // really leaves (it just gains a label).
    for role in [
        FolderKind::Sent,
        FolderKind::Drafts,
        FolderKind::Trash,
        FolderKind::Junk,
        FolderKind::Archive,
        FolderKind::Starred,
    ] {
        let has_special = folders
            .iter()
            .zip(&special_use)
            .any(|(f, su)| f.kind == role && *su);
        if has_special {
            for (f, su) in folders.iter_mut().zip(&special_use) {
                if f.kind == role && !*su {
                    f.kind = FolderKind::Custom;
                }
            }
        }
    }

    // Essential folders first in their fixed order; custom folders sorted by
    // their full hierarchical path (case-insensitively), so sub-folders sit
    // grouped under their parents in a stable, server-independent order rather
    // than in whatever order the server's LIST happened to return them.
    number_folders(&mut folders, |f| f, |f| f);
    // Manual Special Folders assignments (#82) ride over the detection
    // before anything is counted: a folder assigned as Drafts must count
    // every draft, and the cached kind drives every later re-count (the
    // sweep, the on-open refresh) too. Ids and order stay as numbered —
    // the app re-sorts its own copy.
    crate::models::assign_folder_roles(roles, &mut folders);

    if let Some(cached) = cached_counts {
        for folder in &mut folders {
            folder.unread = cached
                .iter()
                .find(|f| f.path == folder.path)
                .map_or(0, |f| f.unread);
        }
        return Ok(folders);
    }

    // Ask the server for each folder's true unread count. STATUS is cheap and
    // downloads no message content, so this stays fast even for huge mailboxes.
    for f in folders.iter_mut() {
        if let Ok(mb) = box_status(session, &f.path, "(UNSEEN MESSAGES)").await {
            f.unread = if chip_counts_all(f.kind) {
                mb.exists
            } else {
                mb.unseen.unwrap_or(0)
            };
        }
    }

    // STATUS is unreliable on some servers (notably iCloud), which leaves stale
    // inbox chips in the sidebar until the folder is opened. The inbox is the only
    // folder whose unread count is shown, so refine just that one up front with the
    // accurate EXAMINE + SEARCH UNSEEN (read-only; subsequent mutations must
    // re-select the mailbox in write mode).
    if let Some(inbox) = folders.iter_mut().find(|f| f.kind == FolderKind::Inbox) {
        if exam(session, &inbox.path).await.is_ok() {
            if let Some(n) = selected_unseen(session).await {
                inbox.unread = n;
            }
        }
    }
    Ok(folders)
}

pub(super) async fn refresh_folder_names(
    account_id: u32,
    account: &AccountConfig,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    let cached = cache
        .map(|c| c.load_folders(account_id))
        .unwrap_or_default();
    if let Ok(folders) = list_folder_metadata(
        account_id,
        session,
        &account.folder_roles,
        &account.hidden_folders,
        Some(&cached),
    )
    .await
    {
        if folders.is_empty() || crate::cache::folders_equal(&cached, &folders) {
            return;
        }
        if let Some(c) = cache {
            c.save_folders(account_id, &folders);
        }
        emit(WorkerEvent::Folders(folders));
    }
}

/// Re-list folders (e.g. after auto-creating one) and push them to the UI.
pub(super) async fn refresh_folders(
    account_id: u32,
    account: &AccountConfig,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    emit: &impl Fn(WorkerEvent),
) {
    if let Ok(folders) = list_folders(
        account_id,
        session,
        &account.folder_roles,
        &account.hidden_folders,
    )
    .await
    {
        // A mailbox always has at least INBOX (RFC 3501): an empty LIST is a
        // wedged or throttled session answering nonsense, not the truth.
        // Trusting one once wiped an account's whole folder list — cached,
        // so it stayed wiped across restarts (seen on iCloud after a burst
        // of RENAMEs).
        if folders.is_empty() {
            return;
        }
        if let Some(c) = cache {
            c.save_folders(account_id, &folders);
        }
        emit(WorkerEvent::Folders(folders));
    }
}

/// Whether `path` is the account's inbox: by the cached folder list, or by
/// name when there is no cache to ask.
pub(super) fn is_inbox_path(cache: Option<&Cache>, account_id: u32, path: &str) -> bool {
    match cached_folder_kind(cache, account_id, path) {
        Some(kind) => kind == FolderKind::Inbox,
        None => path.eq_ignore_ascii_case("INBOX"),
    }
}

/// A folder's kind as the cache last saw it, by path.
pub(super) fn cached_folder_kind(
    cache: Option<&Cache>,
    account_id: u32,
    path: &str,
) -> Option<FolderKind> {
    cache?
        .load_folders(account_id)
        .into_iter()
        .find(|f| f.path == path)
        .map(|f| f.kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imap_storage_quota_reads_kibibytes() {
        use async_imap::types::{Quota, QuotaResource, QuotaResourceName};
        let quotas = vec![Quota {
            root_name: String::new(),
            resources: vec![
                QuotaResource {
                    name: QuotaResourceName::Message,
                    usage: 12,
                    limit: 1000,
                },
                QuotaResource {
                    name: QuotaResourceName::Storage,
                    usage: 1024,
                    limit: 4096,
                },
            ],
        }];
        assert_eq!(
            imap_storage_quota(&quotas),
            Some(MailboxQuota {
                used: 1024 * 1024,
                limit: 4096 * 1024
            })
        );
        let unlimited = vec![Quota {
            root_name: String::new(),
            resources: vec![QuotaResource {
                name: QuotaResourceName::Storage,
                usage: 5,
                limit: 0,
            }],
        }];
        assert_eq!(imap_storage_quota(&unlimited), None);
    }
}
