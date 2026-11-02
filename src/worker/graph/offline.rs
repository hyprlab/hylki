//! Mail kept offline (#536) on a Microsoft 365 account: once the background
//! index is in, the period's messages are downloaded one a step, so a
//! request waits behind one message at most.

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::worker::blocking;
use crate::worker::graph::http::{graph_get_bytes, graph_token};
use crate::worker::graph::index::GraphState;
use crate::worker::offline::{keep, Offline, RECHECK};

/// The pause when no token could be had, before the next try.
const NO_TOKEN: std::time::Duration = std::time::Duration::from_secs(60);

/// One step: queue what the period lacks if nothing is, then download the
/// next message's MIME.
pub(super) async fn graph_offline_step(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    state: &GraphState,
    offline: &mut Offline,
) {
    let Some(c) = cache else {
        offline.rest(RECHECK);
        return;
    };
    if !offline.fill(c, account_id, account) {
        return;
    }
    // A sign-in that needs renewing is said by the next request the user
    // makes, not by a background download.
    let Some(token) = graph_token(account, &|_| {}).await else {
        offline.rest(NO_TOKEN);
        return;
    };
    keep_next(&token, account, account_id, c, state, offline).await;
}

/// Download the queue's next message and write it to the cache.
pub(super) async fn keep_next(
    token: &str,
    account: &AccountConfig,
    account_id: u32,
    cache: &Cache,
    state: &GraphState,
    offline: &mut Offline,
) {
    let Some((path, uid)) = offline.next() else {
        return;
    };
    // The index listed every id; mail it did not name is gone from the
    // server since the cache had it.
    let Some(gid) = state.uids.get(&uid) else {
        offline.not_kept(&path, uid);
        return;
    };
    let (token, url) = (
        token.to_string(),
        format!("{}/me/messages/{gid}/$value", state.base),
    );
    match blocking(move || graph_get_bytes(&token, &url)).await {
        Ok(raw) => {
            offline.succeeded();
            if !keep(
                cache,
                account_id,
                &path,
                uid,
                &raw,
                account.offline_attachments,
            ) {
                offline.not_kept(&path, uid);
            }
        }
        Err(e) => offline.failed(&path, &[uid], &e),
    }
}
