//! Mail kept offline (#536) on a Tuta account: once the background index is
//! in, the period's messages are decrypted and stored one a step, so a
//! request waits behind one message at most.

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::worker::offline::{keep, Offline, RECHECK};
use crate::worker::tuta::index::{run, TutaState};
use crate::worker::tuta::mime::mime_message;
use crate::worker::tuta::wire::message;

/// The pause when the account has no usable sign-in, before the next try.
const NO_SESSION: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// One step: queue what the period lacks if nothing is, then download the
/// next message.
pub(super) async fn tuta_offline_step(
    account: &AccountConfig,
    account_id: u32,
    cache: Option<&Cache>,
    state: &TutaState,
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
    // The index listed every id; mail it did not name is gone from the
    // server since the cache had it.
    let Some(id) = state.uids.get(&uid).cloned() else {
        offline.not_kept(&path, uid);
        return;
    };
    // A sign-in that needs renewing is said by the next request the user
    // makes, not by a background download.
    let Ok(client) = state.client(account) else {
        offline.restore(&path, &[uid]);
        offline.rest(NO_SESSION);
        return;
    };
    match run(move || message(&client, &id).and_then(|reply| mime_message(&reply))).await {
        Ok(raw) => {
            offline.succeeded();
            if !keep(c, account_id, &path, uid, &raw, account.offline_attachments) {
                offline.not_kept(&path, uid);
            }
        }
        Err(e) => offline.failed(&path, &[uid], &e.code),
    }
}
