//! Mail kept offline (#536) on a POP3 account. A sync downloads new mail in
//! full, but only the newest [`POP3_LIMIT`](super::POP3_LIMIT) messages; with
//! a period chosen, older mail is downloaded too, a few messages a step, back
//! to the period's start, and a body the cache has lost is downloaded again.

use std::cmp::Reverse;
use std::collections::HashSet;

use super::{Pop3Worker, INBOX, INBOX_ID};
use crate::worker::auth::sign_in_password;
use crate::worker::ids::hash_uid;
use crate::worker::offline::{keep, RECHECK};
use crate::worker::pop3::client::Pop3;
use crate::worker::{summary_from_raw, WorkerEvent};

/// Messages one step downloads over one connection.
const BATCH: usize = 10;

/// One message a step downloads: its number this session, its uid, and
/// whether it is a cached message whose body went rather than older mail.
#[derive(Debug, PartialEq)]
struct Wanted {
    num: u32,
    uid: u32,
    lost: bool,
}

/// What one step downloads from the mailbox listing (newest first): the
/// lost bodies still on the server, then the newest mail the cache lacks,
/// `max` at most. `left` says which messages are not tried again.
fn plan(
    entries: &[(u32, String)],
    lost: &[u32],
    cached: &HashSet<u32>,
    left: impl Fn(u32) -> bool,
    max: usize,
) -> Vec<Wanted> {
    let numbered: Vec<(u32, u32)> = entries.iter().map(|(n, u)| (*n, hash_uid(u))).collect();
    let lost = numbered
        .iter()
        .filter(|(_, uid)| lost.contains(uid))
        .map(|&(num, uid)| Wanted {
            num,
            uid,
            lost: true,
        });
    let older = numbered
        .iter()
        .filter(|(_, uid)| !cached.contains(uid) && !left(*uid))
        .map(|&(num, uid)| Wanted {
            num,
            uid,
            lost: false,
        });
    lost.chain(older).take(max).collect()
}

impl<E: Fn(WorkerEvent)> Pop3Worker<E> {
    /// One step: download a few messages the period lacks over one
    /// connection. Each is written to the cache as it comes, so a request
    /// that cuts the step short loses only the message on its way.
    pub(super) async fn keep_offline_batch(&mut self) {
        let Some(since) = self.account.offline_mail.since(crate::datefmt::now()) else {
            return;
        };
        let Some(c) = self.cache.as_ref() else {
            self.offline.rest(RECHECK);
            return;
        };
        let account_id = self.account_id;
        let offline = &mut self.offline;
        let lost: Vec<u32> = c
            .offline_missing(account_id, since, false, &[], BATCH + offline.tried_count())
            .into_iter()
            .filter(|(path, uid)| !offline.was_tried(path, *uid))
            .map(|(_, uid)| uid)
            .collect();
        let cached: HashSet<u32> = c.cached_uid_times(account_id, INBOX).into_keys().collect();

        let mut current = None;
        let result: Result<(), String> = async {
            let mut pop = Pop3::connect(&self.account).await?;
            pop.login(
                &self.account.username,
                &sign_in_password(&self.account, false).await?,
            )
            .await?;
            let mut entries = pop.uidl().await?;
            entries.sort_by_key(|e| Reverse(e.0));
            let wanted = plan(
                &entries,
                &lost,
                &cached,
                |uid| offline.was_tried(INBOX, uid),
                BATCH,
            );
            // Mail deleted from the server: the next sync drops it.
            let on_server: HashSet<u32> = entries.iter().map(|(_, u)| hash_uid(u)).collect();
            for uid in lost.iter().filter(|u| !on_server.contains(u)) {
                offline.not_kept(INBOX, *uid);
            }
            if wanted.is_empty() {
                tracing::debug!(account_id, "offline mail: the period is all on disk");
                offline.rest(RECHECK);
            }
            for w in wanted {
                current = Some(w.uid);
                let raw = pop.retr(w.num).await?;
                if w.lost {
                    if !keep(c, account_id, INBOX, w.uid, &raw, true) {
                        offline.not_kept(INBOX, w.uid);
                    }
                    continue;
                }
                let msg = summary_from_raw(account_id, INBOX_ID, w.uid, &raw);
                if msg.timestamp < since {
                    // Mail is numbered as it arrived: past the period's
                    // start, the rest is older still.
                    for (_, u) in entries.iter().filter(|(n, _)| *n <= w.num) {
                        offline.not_kept(INBOX, hash_uid(u));
                    }
                    offline.rest(RECHECK);
                    break;
                }
                // OpenPGP mail joins the list, its body left for the reader.
                keep(c, account_id, INBOX, w.uid, &raw, true);
                c.upsert_messages(account_id, INBOX, std::slice::from_ref(&msg));
                (self.emit)(WorkerEvent::MessagesAppend {
                    folder_id: INBOX_ID,
                    messages: vec![msg],
                });
            }
            current = None;
            offline.succeeded();
            pop.quit().await;
            Ok(())
        }
        .await;
        if let Err(e) = result {
            let failing: Vec<u32> = current.into_iter().collect();
            self.offline.paused(INBOX, &failing, &e);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use futures::TryStreamExt;

    use super::*;
    use crate::cache::Cache;
    use crate::config::AccountConfig;
    use crate::worker::offline::Offline;
    use crate::worker::test_support::sample_account;

    /// With `POP3_LIVE=host,user,password` (POP3 on 995 and IMAP on 993 of
    /// one server, Stalwart say): three messages 2, 10 and 60 days old land
    /// in the inbox past what the cache holds, and a week kept offline
    /// brings in the first only. Its body, removed, comes back on the next
    /// step. The messages are deleted again over IMAP.
    #[test]
    #[ignore]
    fn live_offline_period_is_kept() {
        let Ok(spec) = std::env::var("POP3_LIVE") else {
            return;
        };
        let p: Vec<&str> = spec.splitn(3, ',').collect();
        let base = AccountConfig {
            imap_host: p[0].into(),
            username: p[1].into(),
            password: p[2].into(),
            security: Some(crate::config::ServerSecurity {
                imap_starttls: false,
                imap_accept_invalid_certs: true,
                ..Default::default()
            }),
            ..sample_account()
        };
        let imap = AccountConfig {
            imap_port: 993,
            ..base.clone()
        };
        let pop = AccountConfig {
            imap_port: 995,
            protocol: crate::config::Protocol::Pop3,
            offline_mail: crate::config::OfflineMail::Week,
            ..base
        };
        let pid = std::process::id();
        let tag = format!("hylki-536-pop3-{pid}");
        let now = crate::datefmt::now();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let cache = Cache::in_memory().expect("cache");
        let events = RefCell::new(Vec::new());
        rt.block_on(async {
            // What the inbox holds already is cached, as a sync leaves it.
            let plain = AccountConfig {
                offline_mail: Default::default(),
                ..pop.clone()
            };
            super::super::pop3_sync(1, &plain, INBOX_ID, Some(&cache))
                .await
                .expect("sync");
            let mut sess = crate::worker::imap::connect::connect(&imap)
                .await
                .expect("imap");
            // Oldest first, so the newest is numbered last, as mail arrives.
            for (n, age) in [(3, 60), (2, 10), (1, 2)] {
                let date = chrono::DateTime::from_timestamp(now - age * 86_400, 0).unwrap();
                let raw = format!(
                    "From: Ann <ann@example.test>\r\nTo: me@example.test\r\n\
                     Subject: offline {n} {tag}\r\nMessage-ID: <{tag}-{n}@example.test>\r\n\
                     Date: {}\r\n\r\nbody {n}\r\n",
                    date.to_rfc2822()
                );
                sess.append("INBOX", None, None, raw.as_bytes())
                    .await
                    .expect("append");
            }

            let mut worker = Pop3Worker {
                account_id: 1,
                account: pop.clone(),
                cache: Some(cache),
                emit: |e| events.borrow_mut().push(e),
                offline: Offline::default(),
            };
            for _ in 0..20 {
                if worker.offline.wait().is_some() {
                    break;
                }
                worker.keep_offline_batch().await;
            }
            let c = worker.cache.as_ref().unwrap();
            let listed = c.load_messages(1, INBOX, INBOX_ID);
            let ours: Vec<String> = listed
                .iter()
                .filter(|m| m.message_id.contains(&tag))
                .map(|m| m.subject.clone())
                .collect();
            let first = listed.iter().find(|m| m.message_id.contains(&tag));
            let kept = first.is_some_and(|m| c.load_body(1, INBOX, m.uid).is_some());

            // A body removed (Keep downloaded mail, say) comes back.
            c.remove_downloaded(None).expect("remove");
            worker.offline = Offline::default();
            for _ in 0..20 {
                if worker.offline.wait().is_some() {
                    break;
                }
                worker.keep_offline_batch().await;
            }
            let c = worker.cache.as_ref().unwrap();
            let back = first.is_some_and(|m| c.load_body(1, INBOX, m.uid).is_some());

            // By the subject: Stalwart's search does not index Message-ID.
            sess.select("INBOX").await.expect("select");
            let uids = sess
                .uid_search(format!("SUBJECT {tag}"))
                .await
                .expect("search");
            let set = uids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let _: Vec<_> = sess
                .uid_store(&set, "+FLAGS (\\Deleted)")
                .await
                .expect("store")
                .try_collect()
                .await
                .expect("stored");
            let _: Vec<_> = sess
                .expunge()
                .await
                .expect("expunge")
                .try_collect()
                .await
                .expect("expunged");
            assert_eq!(uids.len(), 3, "the three test messages were found");
            assert_eq!(
                ours,
                [format!("offline 1 {tag}")],
                "only the week's mail joins the list"
            );
            assert!(kept, "its body is on disk");
            assert!(back, "a lost body is downloaded again");
        });
        let appended = events
            .borrow()
            .iter()
            .filter(|e| matches!(e, WorkerEvent::MessagesAppend { .. }))
            .count();
        assert_eq!(appended, 1);
    }

    fn listing() -> Vec<(u32, String)> {
        (1..=5).rev().map(|n| (n, format!("u{n}"))).collect()
    }

    #[test]
    fn lost_bodies_come_before_older_mail() {
        let cached: HashSet<u32> = ["u5", "u4", "u3"].iter().map(|u| hash_uid(u)).collect();
        let wanted = plan(&listing(), &[hash_uid("u4")], &cached, |_| false, 10);
        let nums: Vec<(u32, bool)> = wanted.iter().map(|w| (w.num, w.lost)).collect();
        assert_eq!(nums, [(4, true), (2, false), (1, false)]);
    }

    #[test]
    fn mail_left_for_the_session_is_not_asked_again() {
        let cached: HashSet<u32> = ["u5"].iter().map(|u| hash_uid(u)).collect();
        let left = |uid| uid == hash_uid("u4");
        let wanted = plan(&listing(), &[], &cached, left, 2);
        let nums: Vec<u32> = wanted.iter().map(|w| w.num).collect();
        assert_eq!(nums, [3, 2]);
    }
}
