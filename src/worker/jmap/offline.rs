//! Mail kept offline (#536) on a JMAP account: once the background index is
//! in, the period's messages are downloaded a blob a step, so a request
//! waits behind one message at most.

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::worker::blocking;
use crate::worker::jmap::index::JmapState;
use crate::worker::jmap::messages::jmap_get_messages;
use crate::worker::jmap::wire::{jmap_download, JmapSession};
use crate::worker::offline::{keep, Offline, RECHECK};

/// How many blob ids one `Email/get` asks for ahead of their downloads.
const RESOLVE_BATCH: usize = 50;

/// One step: queue what the period lacks if nothing is, then download the
/// next message. Its blob id comes from the uid map, or from one `Email/get`
/// for it and the folder's next queued messages.
pub(super) async fn jmap_offline_step(
    s: &JmapSession,
    account_id: u32,
    account: &AccountConfig,
    cache: Option<&Cache>,
    state: &mut JmapState,
    offline: &mut Offline,
) {
    let Some(c) = cache else {
        offline.rest(RECHECK);
        return;
    };
    if !offline.fill(c, account_id, account) {
        return;
    }
    let Some((path, uid)) = offline.next() else {
        return;
    };
    if !state.uids.contains_key(&uid) {
        // The index listed every id; mail no listing named is gone from the
        // server since the cache had it.
        let ids: Vec<String> = std::iter::once(uid)
            .chain(offline.queued_in(&path))
            .filter(|u| !state.uids.contains_key(u))
            .filter_map(|u| state.listed_ids.get(&u).cloned())
            .take(RESOLVE_BATCH)
            .collect();
        if !ids.is_empty() {
            let sess = s.clone();
            match blocking(move || jmap_get_messages(&sess, &ids, 0, 0)).await {
                Ok(found) => {
                    for (m, id, blob) in found {
                        state.uids.insert(m.uid, (id, blob));
                    }
                }
                Err(e) => {
                    offline.failed(&path, &[uid], &e);
                    return;
                }
            }
        }
    }
    let Some(blob) = state
        .uids
        .get(&uid)
        .map(|(_, blob)| blob.clone())
        .filter(|blob| !blob.is_empty())
    else {
        offline.not_kept(&path, uid);
        return;
    };
    let sess = s.clone();
    match blocking(move || jmap_download(&sess, &blob)).await {
        Ok(raw) => {
            offline.succeeded();
            if !keep(c, account_id, &path, uid, &raw, account.offline_attachments) {
                offline.not_kept(&path, uid);
            }
        }
        Err(e) => offline.failed(&path, &[uid], &e),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::worker::jmap::folders::{jmap_create_mailbox, jmap_list_folders};
    use crate::worker::jmap::index::jmap_backfill_with;
    use crate::worker::jmap::ops::{jmap_destroy_all, jmap_import_email};
    use crate::worker::jmap::wire::{args, call, jmap_call, jmap_connect, CAP_CORE, CAP_MAIL};
    use crate::worker::test_support::sample_account;

    /// With `JMAP_LIVE=url,user,pw` (as for the JMAP `live` test): a mailbox
    /// of mail 2, 10 and 60 days old, kept offline for a week, has the first
    /// message's body and attachment on disk and nothing of the others.
    /// The uid map is emptied first, as a fresh session's is, so the blob
    /// ids come from the index's listing. Leaves the account as it was.
    #[test]
    #[ignore]
    fn live_offline_period_is_kept() {
        let Ok(spec) = std::env::var("JMAP_LIVE") else {
            return;
        };
        let mut parts = spec.splitn(3, ',');
        let acc = AccountConfig {
            imap_host: parts.next().unwrap_or_default().into(),
            imap_port: 443,
            username: parts.next().unwrap_or_default().into(),
            password: parts.next().unwrap_or_default().into(),
            jmap_token: std::env::var_os("JMAP_LIVE_TOKEN").is_some(),
            offline_mail: crate::config::OfflineMail::Week,
            offline_attachments: true,
            ..sample_account()
        };
        let s = jmap_connect(&acc).expect("session");
        let pid = std::process::id();
        let name = format!("Offline-{pid}");
        let made = jmap_create_mailbox(&s, &name, None).expect("create");
        let now = crate::datefmt::now();
        let mut ids = Vec::new();
        for (n, age) in [(1, 2), (2, 10), (3, 60)] {
            let date = chrono::DateTime::from_timestamp(now - age * 86_400, 0).unwrap();
            let raw = format!(
                "From: Ann <ann@example.test>\r\nTo: me@example.test\r\nSubject: offline {n}\r\n\
                 Message-ID: <hylki-536-jmap-{pid}-{n}@example.test>\r\nDate: {}\r\n\
                 MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n\
                 --b\r\nContent-Type: text/plain\r\n\r\nbody {n}\r\n\
                 --b\r\nContent-Type: application/octet-stream\r\n\
                 Content-Disposition: attachment; filename=n{n}.bin\r\n\
                 Content-Transfer-Encoding: base64\r\n\r\naGVsbG8=\r\n--b--\r\n",
                date.to_rfc2822()
            );
            let email = serde_json::json!({
                "mailboxIds": { made.clone(): true },
                "keywords": { "$seen": true },
                "receivedAt": date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            });
            ids.push(jmap_import_email(&s, raw.as_bytes(), email).expect("import"));
        }

        let cache = Cache::in_memory().expect("cache");
        let mut state = JmapState::new(&acc);
        let folders = jmap_list_folders(&s, 1, &BTreeMap::new()).expect("folders");
        cache.save_folders(
            1,
            &folders.iter().map(|f| f.folder.clone()).collect::<Vec<_>>(),
        );
        let folder = folders
            .iter()
            .find(|f| f.folder.path == name)
            .expect("listed")
            .folder
            .clone();
        state.adopt_folders(&folders);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut offline = Offline::default();
        rt.block_on(async {
            while !state.backfill.is_empty() {
                assert!(jmap_backfill_with(&s, 1, Some(&cache), &mut state, &|_| {}).await);
            }
            state.uids.clear();
            for _ in 0..500 {
                if offline.wait().is_some() {
                    break;
                }
                jmap_offline_step(&s, 1, &acc, Some(&cache), &mut state, &mut offline).await;
            }
        });

        let listed = cache.load_messages(1, &name, folder.id);
        let uid = |n: u32| {
            listed
                .iter()
                .find(|m| m.subject == format!("offline {n}"))
                .expect("indexed")
                .uid
        };
        let body = |n| cache.load_body(1, &name, uid(n));
        let files = |n| cache.load_attachments(1, &name, uid(n));
        let kept = body(1).is_some() && !files(1).is_empty();
        let others = body(2).is_none() && body(3).is_none() && files(2).is_empty();

        jmap_destroy_all(&s, &ids).expect("drop mail");
        jmap_call(
            &s,
            &[CAP_CORE, CAP_MAIL],
            vec![call(
                0,
                "Mailbox/set",
                serde_json::json!({ "accountId": s.account, "destroy": [made] }),
            )],
        )
        .map(|r| args(&r, 0).clone())
        .expect("drop mailbox");
        assert!(kept, "the week's message and its attachment are on disk");
        assert!(others, "older mail is left on the server");
        assert!(offline.wait().is_some(), "the period is complete");
    }
}
