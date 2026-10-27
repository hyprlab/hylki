//! Stars and read state set from another device on mail older than the
//! window a folder's sync fetches (#532). The sync re-reads the flags of
//! only its newest [`PAGE_SIZE`](super::PAGE_SIZE) messages, so a star
//! taken off older mail elsewhere stayed in the index, and in every Smart
//! Folder and Starred view read from it, for good.

use std::collections::{HashMap, HashSet};

use async_imap::types::{Fetch, Flag};
use futures::TryStreamExt;

use crate::cache::Cache;
use crate::worker::imap::wire::{exam, fetch_uids, search_uids, uid_set};
use crate::worker::imap::ImapSession;

/// A message's star and read state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FlagState {
    pub(super) uid: u32,
    pub(super) starred: bool,
    pub(super) unread: bool,
}

/// The indexed messages whose star or read state the selected folder's
/// searches disagree with. A uid the index does not hold is new mail, and
/// the folder's own sync lists it.
fn disputed(indexed: &[FlagState], flagged: &HashSet<u32>, unseen: &HashSet<u32>) -> Vec<u32> {
    indexed
        .iter()
        .filter(|s| s.starred != flagged.contains(&s.uid) || s.unread != unseen.contains(&s.uid))
        .map(|s| s.uid)
        .collect()
}

/// The server's state for those of `indexed`, in the selected folder, whose
/// state it holds otherwise. Two searches find them; where a search and the
/// index disagree the message's own flags decide, since iCloud answers
/// SEARCH from an index that lags behind STORE (see the keyword check in
/// `keywords.rs`). A message the server no longer returns is left out.
pub(super) async fn server_flag_changes(
    session: &mut ImapSession,
    indexed: &[FlagState],
) -> Result<Vec<FlagState>, async_imap::error::Error> {
    if indexed.is_empty() {
        return Ok(Vec::new());
    }
    let flagged = search_uids(session, "FLAGGED").await?;
    let unseen = search_uids(session, "UNSEEN").await?;
    let uids = disputed(indexed, &flagged, &unseen);
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let was: HashMap<u32, FlagState> = indexed.iter().map(|s| (s.uid, *s)).collect();
    let mut changes = Vec::new();
    for chunk in uids.chunks(500) {
        let fetches: Vec<Fetch> = fetch_uids(session, uid_set(chunk), "(UID FLAGS)")
            .await?
            .try_collect()
            .await?;
        for f in &fetches {
            let Some(uid) = f.uid else { continue };
            let flags: Vec<Flag> = f.flags().collect();
            let now = FlagState {
                uid,
                starred: flags.iter().any(|f| matches!(f, Flag::Flagged)),
                unread: !flags.iter().any(|f| matches!(f, Flag::Seen)),
            };
            if was.get(&uid).is_some_and(|old| *old != now) {
                changes.push(now);
            }
        }
    }
    Ok(changes)
}

/// Bring the cached stars and read state of each of `paths` in step with
/// the server, for a Smart Folder or Starred view whose folders are not
/// open. Returns the folders whose rows changed.
pub(super) async fn refresh_flags(
    account_id: u32,
    session: &mut ImapSession,
    cache: Option<&Cache>,
    paths: &[String],
) -> Vec<String> {
    let mut changed = Vec::new();
    let Some(cache) = cache else { return changed };
    for path in paths {
        let indexed: Vec<FlagState> = cache
            .flag_states(account_id, path)
            .into_iter()
            .map(|(uid, starred, unread)| FlagState {
                uid,
                starred,
                unread,
            })
            .collect();
        if indexed.is_empty() || exam(session, path).await.is_err() {
            continue;
        }
        let changes = match server_flag_changes(session, &indexed).await {
            Ok(changes) => changes,
            Err(e) => {
                tracing::warn!("[account {account_id}] flags: {path}: {e}");
                continue;
            }
        };
        for s in &changes {
            cache.set_starred(account_id, path, s.uid, s.starred);
            cache.set_unread(account_id, path, s.uid, s.unread);
        }
        if !changes.is_empty() {
            changed.push(path.clone());
        }
    }
    if !changed.is_empty() {
        tracing::info!("flags: changed {changed:?}");
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(uid: u32, starred: bool, unread: bool) -> FlagState {
        FlagState {
            uid,
            starred,
            unread,
        }
    }

    /// A star or read state the server holds otherwise is disputed; a uid
    /// the server holds and the index does not is not (#532).
    #[test]
    fn disputes_are_the_indexed_rows_the_searches_contradict() {
        let indexed = [
            state(1, true, false),  // unstarred elsewhere
            state(2, false, true),  // read elsewhere
            state(3, true, true),   // as the server has it
            state(4, false, false), // starred elsewhere
        ];
        let flagged: HashSet<u32> = [3, 4, 9].into();
        let unseen: HashSet<u32> = [3, 9].into();
        assert_eq!(disputed(&indexed, &flagged, &unseen), [1, 2, 4]);
    }

    /// #532 against a real server, with `IMAP_LIVE=host,port,user,password`
    /// (implicit TLS, any certificate): 60 starred, unread messages in a
    /// scratch folder, so the oldest sits outside the window a folder's
    /// sync fetches, and another device unstars and reads it. The view's
    /// check and the folder's own sync both bring the index in step.
    #[test]
    #[ignore]
    fn live_flags_changed_elsewhere_reach_older_mail() {
        use crate::config::AccountConfig;
        use crate::worker::imap::connect::connect;
        use crate::worker::imap::sync::load_messages;
        use crate::worker::imap::wire::{append_msg, create_box, delete_box, sel, store_uids};
        use crate::worker::test_support::sample_account;

        let Ok(spec) = std::env::var("IMAP_LIVE") else {
            return;
        };
        let p: Vec<&str> = spec.splitn(4, ',').collect();
        let account = AccountConfig {
            imap_host: p[0].into(),
            imap_port: p[1].parse().expect("port"),
            username: p[2].into(),
            password: p[3].into(),
            security: Some(crate::config::ServerSecurity {
                imap_starttls: false,
                imap_accept_invalid_certs: true,
                ..Default::default()
            }),
            ..sample_account()
        };
        let pid = std::process::id();
        let folder = format!("Flags-{pid}");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let cache = Cache::in_memory().unwrap();
            let mut sess = connect(&account).await.expect("connect");
            create_box(&mut sess, &folder).await.expect("create");
            for i in 0..60 {
                let raw = format!(
                    "From: Ada <ada@example.test>\r\nSubject: Starred {i}\r\n\
                     Message-ID: <flags-{pid}-{i}@example.test>\r\n\r\nText\r\n"
                );
                append_msg(&mut sess, &folder, Some("(\\Flagged)"), raw.as_bytes())
                    .await
                    .expect("append");
            }
            let listed = load_messages(1, &mut sess, 1, &folder, true, Some(&cache))
                .await
                .expect("load");
            assert_eq!(listed.len(), 60);
            cache.upsert_messages(1, &folder, &listed);
            let oldest = listed.iter().map(|m| m.uid).min().unwrap();
            let state_of = |cache: &Cache| {
                cache
                    .flag_states(1, &folder)
                    .into_iter()
                    .find(|(uid, _, _)| *uid == oldest)
            };
            assert_eq!(state_of(&cache), Some((oldest, true, true)));

            // Another device.
            sel(&mut sess, &folder).await.expect("select");
            for change in ["-FLAGS (\\Flagged)", "+FLAGS (\\Seen)"] {
                let _: Vec<Fetch> = store_uids(&mut sess, oldest.to_string(), change)
                    .await
                    .expect("store")
                    .try_collect()
                    .await
                    .expect("stored");
            }

            let changed = refresh_flags(1, &mut sess, Some(&cache), &[folder.clone()]).await;
            assert_eq!(changed, [folder.clone()]);
            assert_eq!(state_of(&cache), Some((oldest, false, false)));
            let again = refresh_flags(1, &mut sess, Some(&cache), &[folder.clone()]).await;
            assert!(again.is_empty(), "nothing left to change");

            // The folder's own sync, from the index as it was.
            cache.set_starred(1, &folder, oldest, true);
            cache.set_unread(1, &folder, oldest, true);
            let synced = load_messages(1, &mut sess, 1, &folder, true, Some(&cache))
                .await
                .expect("sync");
            let m = synced.iter().find(|m| m.uid == oldest).expect("listed");
            assert!(!m.starred && !m.unread, "{m:?}");
            assert_eq!(synced.iter().filter(|m| m.starred).count(), 59);

            let _ = sess.close().await;
            delete_box(&mut sess, &folder).await.expect("clean up");
        });
    }
}
