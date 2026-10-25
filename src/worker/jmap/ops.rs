//! Changing emails: resolving a uid to its JMAP id, keywords, moves,
//! deleting for good, importing raw mail, removing an attachment.

use std::collections::HashSet;

use crate::cache::Cache;
use crate::i18n::i18n;
use crate::worker::ids::hash_uid;
use crate::worker::jmap::index::{jmap_load_folder, JmapState, JMAP_RELIST_AFTER};
use crate::worker::jmap::messages::{jmap_get_messages, jmap_list_messages, jmap_query_ids};
use crate::worker::jmap::wire::{
    args, call, jmap_call, jmap_download, jmap_upload, JmapSession, CAP_CORE, CAP_MAIL,
};
use crate::worker::{blocking, strip};

/// How [`jmap_submit`] says the copy was refused before anything was sent,
/// so a reply filed beside its original (#468) can go to Sent instead.
pub(super) const IMPORT_REFUSED: &str = "the server would not take the message";

/// `Email/set`: patch some messages and/or destroy others. Anything the
/// server refused is logged, not surfaced: the UI already moved on.
pub(super) fn jmap_set_emails(
    s: &JmapSession,
    update: serde_json::Value,
    destroy: &[String],
) -> Result<serde_json::Value, String> {
    let mut a = serde_json::json!({ "accountId": s.account });
    if update.as_object().is_some_and(|m| !m.is_empty()) {
        a["update"] = update;
    }
    if !destroy.is_empty() {
        a["destroy"] = serde_json::json!(destroy);
    }
    let responses = jmap_call(s, &[CAP_CORE, CAP_MAIL], vec![call(0, "Email/set", a)])?;
    let r = args(&responses, 0);
    for key in ["notUpdated", "notDestroyed"] {
        if let Some(m) = r[key].as_object() {
            for (id, why) in m {
                tracing::warn!(
                    "jmap: Email/set {key} {id}: {}",
                    why["description"]
                        .as_str()
                        .or(why["type"].as_str())
                        .unwrap_or("?")
                );
            }
        }
    }
    Ok(r)
}

/// Patch every id the same way, in batches the server accepts.
pub(super) fn jmap_patch_all(
    s: &JmapSession,
    ids: &[String],
    patch: &serde_json::Value,
) -> Result<(), String> {
    for chunk in ids.chunks(200) {
        let update: serde_json::Map<String, serde_json::Value> =
            chunk.iter().map(|id| (id.clone(), patch.clone())).collect();
        jmap_set_emails(s, serde_json::Value::Object(update), &[])?;
    }
    Ok(())
}

pub(super) fn jmap_destroy_all(s: &JmapSession, ids: &[String]) -> Result<(), String> {
    for chunk in ids.chunks(200) {
        jmap_set_emails(s, serde_json::json!({}), chunk)?;
    }
    Ok(())
}

/// Resolve a message uid to its JMAP (id, blob id), re-listing the folder
/// once if the uid isn't in the map (fresh start from cache).
async fn jmap_resolve(
    s: &JmapSession,
    state: &mut JmapState,
    path: &str,
    uid: u32,
) -> Option<(String, String)> {
    if let Some(ids) = state.uids.get(&uid) {
        return Some(ids.clone());
    }
    let _ = jmap_load_folder(s, 0, 0, path, None, state).await;
    if let Some(ids) = state.uids.get(&uid) {
        return Some(ids.clone());
    }
    // Older than the first page (#429), from an earlier session's index:
    // an id listing names it (the background index's, or one made now, once
    // a while), and a get gives its blob.
    if !state.listed_ids.contains_key(&uid)
        && state
            .looked_up
            .get(path)
            .is_none_or(|at| at.elapsed() >= JMAP_RELIST_AFTER)
    {
        let (_, mailbox_id) = state.folders.get(path).cloned()?;
        state
            .looked_up
            .insert(path.to_string(), std::time::Instant::now());
        let sess = s.clone();
        if let Ok(ids) =
            blocking(move || jmap_all_ids_in(&sess, &mailbox_id, serde_json::json!({}))).await
        {
            state
                .listed_ids
                .extend(ids.into_iter().map(|id| (hash_uid(&id), id)));
        }
    }
    let hit = vec![state.listed_ids.get(&uid)?.clone()];
    let sess = s.clone();
    let found = blocking(move || jmap_get_messages(&sess, &hit, 0, 0))
        .await
        .ok()?;
    for (m, id, blob) in found {
        state.uids.insert(m.uid, (id, blob));
    }
    state.uids.get(&uid).cloned()
}

/// Fetch a message's raw RFC 5322 bytes.
pub(super) async fn jmap_fetch_raw(
    s: &JmapSession,
    state: &mut JmapState,
    path: &str,
    uid: u32,
) -> Result<Vec<u8>, String> {
    let (_, blob) = jmap_resolve(s, state, path, uid)
        .await
        .ok_or_else(|| "message not found".to_string())?;
    if blob.is_empty() {
        return Err("message has no blob".into());
    }
    let sess = s.clone();
    blocking(move || jmap_download(&sess, &blob)).await
}

/// Patch one message (read state, flag, keyword). Errors are logged, not
/// surfaced: the optimistic UI state already changed.
pub(super) async fn jmap_patch_message(
    s: &JmapSession,
    state: &mut JmapState,
    path: &str,
    uid: u32,
    patch: serde_json::Value,
) {
    let Some((id, _)) = jmap_resolve(s, state, path, uid).await else {
        return;
    };
    let sess = s.clone();
    let update = serde_json::json!({ id: patch });
    if let Err(e) = blocking(move || jmap_set_emails(&sess, update, &[])).await {
        tracing::warn!("jmap: could not update message: {e}");
    }
}

/// Move messages to another folder (a message keeps its id across a move,
/// so the uid map stays right). `extra` patches ride along: the `$junk` /
/// `$notjunk` keywords a spam verdict sets, which is what the server's
/// classifier learns from.
pub(super) async fn jmap_move_uids(
    s: &JmapSession,
    account_id: u32,
    state: &mut JmapState,
    path: &str,
    uids: &[u32],
    dest: &str,
    extra: serde_json::Value,
    cache: Option<&Cache>,
) -> Result<(), String> {
    let dest_id = match state.folders.get(dest) {
        Some((_, id)) => id.clone(),
        None => return Err(format!("unknown folder {dest}")),
    };
    let mut update = serde_json::Map::new();
    let mut moved = Vec::new();
    for &uid in uids {
        let Some((id, _)) = jmap_resolve(s, state, path, uid).await else {
            continue;
        };
        let mut patch = serde_json::json!({ "mailboxIds": { dest_id.clone(): true } });
        if let Some(m) = extra.as_object() {
            for (k, v) in m {
                patch[k] = v.clone();
            }
        }
        update.insert(id, patch);
        moved.push(uid);
    }
    if update.is_empty() {
        return Ok(());
    }
    let sess = s.clone();
    blocking(move || jmap_set_emails(&sess, serde_json::Value::Object(update), &[])).await?;
    if let Some(c) = cache {
        for uid in moved {
            c.delete_message(account_id, path, uid);
        }
    }
    Ok(())
}

/// Undo a move: the messages are in `path` (where the move put them); find
/// them by Message-ID and move them back to `dest`, patched with `extra`
/// on the way (snoozed mail comes back unread, #527).
pub(super) async fn jmap_undo_move(
    s: &JmapSession,
    account_id: u32,
    state: &mut JmapState,
    path: &str,
    dest: &str,
    message_ids: &[String],
    extra: serde_json::Value,
    cache: Option<&Cache>,
) -> Result<usize, String> {
    let (folder_id, mailbox_id) = state
        .folders
        .get(path)
        .cloned()
        .ok_or_else(|| format!("unknown folder {path}"))?;
    let wanted: HashSet<&str> = message_ids.iter().map(|s| s.as_str()).collect();
    let sess = s.clone();
    let listed =
        blocking(move || jmap_list_messages(&sess, &mailbox_id, account_id, folder_id)).await?;
    let mut uids = Vec::new();
    let mut found: HashSet<String> = HashSet::new();
    for (m, id, blob) in listed {
        if wanted.contains(m.message_id.as_str()) {
            state.uids.insert(m.uid, (id, blob));
            uids.push(m.uid);
            found.insert(m.message_id);
        }
    }
    // An old message keeps its date when it moves, below the newest page
    // (#429): asked for by its Message-ID header, where the server indexes it.
    let rest: Vec<String> = wanted
        .iter()
        .filter(|mid| !found.contains(**mid))
        .map(|mid| mid.to_string())
        .collect();
    if !rest.is_empty() {
        let mailbox_id = state
            .folders
            .get(path)
            .map(|(_, id)| id.clone())
            .unwrap_or_default();
        let exact: Vec<String> = rest
            .iter()
            .map(|mid| {
                cache
                    .map(|c| c.exact_msgids(mid))
                    .unwrap_or_else(|| mid.clone())
            })
            .collect();
        let sess = s.clone();
        let hits = blocking(move || {
            let mut ids = Vec::new();
            for mid in exact {
                let filter = serde_json::json!({ "inMailbox": mailbox_id, "header": ["Message-ID", format!("<{mid}>")] });
                ids.extend(jmap_query_ids(&sess, filter, 5)?);
            }
            jmap_get_messages(&sess, &ids, account_id, folder_id)
        })
        .await;
        match hits {
            Ok(hits) => {
                for (m, id, blob) in hits {
                    if wanted.contains(m.message_id.as_str()) && !uids.contains(&m.uid) {
                        state.uids.insert(m.uid, (id, blob));
                        uids.push(m.uid);
                    }
                }
            }
            Err(e) => tracing::warn!("undo: Message-ID lookup in {path} failed: {e}"),
        }
    }
    if uids.is_empty() {
        return Ok(0);
    }
    let n = uids.len();
    jmap_move_uids(s, account_id, state, path, &uids, dest, extra, cache).await?;
    Ok(n)
}

/// Delete messages for good; the cache and the uid map forget every one.
pub(super) async fn jmap_purge_uids(
    s: &JmapSession,
    account_id: u32,
    state: &mut JmapState,
    path: &str,
    uids: Vec<u32>,
    cache: Option<&Cache>,
) {
    let mut ids = Vec::new();
    let mut gone = Vec::new();
    for uid in uids {
        if let Some((id, _)) = jmap_resolve(s, state, path, uid).await {
            ids.push(id);
            gone.push(uid);
        }
    }
    if ids.is_empty() {
        return;
    }
    let sess = s.clone();
    if blocking(move || jmap_destroy_all(&sess, &ids))
        .await
        .is_ok()
    {
        for uid in gone {
            state.uids.remove(&uid);
            if let Some(c) = cache {
                c.delete_message(account_id, path, uid);
            }
        }
    }
}

/// Everything in a mailbox, in pages, for a bulk operation.
pub(super) fn jmap_all_ids_in(
    s: &JmapSession,
    mailbox_id: &str,
    extra_filter: serde_json::Value,
) -> Result<Vec<String>, String> {
    let mut filter = serde_json::json!({ "inMailbox": mailbox_id });
    if let Some(m) = extra_filter.as_object() {
        for (k, v) in m {
            filter[k] = v.clone();
        }
    }
    let mut out = Vec::new();
    loop {
        let responses = jmap_call(
            s,
            &[CAP_CORE, CAP_MAIL],
            vec![call(
                0,
                "Email/query",
                serde_json::json!({
                    "accountId": s.account,
                    "filter": filter,
                    "position": out.len(),
                    "limit": 500,
                }),
            )],
        )?;
        let page: Vec<String> = args(&responses, 0)["ids"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let n = page.len();
        out.extend(page);
        if n < 500 || out.len() >= 50_000 {
            return Ok(out);
        }
    }
}

/// Save a draft: upload the bytes, import them into Drafts.
pub(super) fn jmap_import_draft(
    s: &JmapSession,
    raw: &[u8],
    drafts_mailbox: &str,
) -> Result<String, String> {
    jmap_import(
        s,
        raw,
        drafts_mailbox,
        serde_json::json!({ "$draft": true, "$seen": true }),
    )
}

/// Import a template (#360): read mail, not a draft, which another client
/// would offer to finish and send.
pub(super) fn jmap_import_template(
    s: &JmapSession,
    raw: &[u8],
    mailbox: &str,
) -> Result<String, String> {
    jmap_import(s, raw, mailbox, serde_json::json!({ "$seen": true }))
}

/// `raw` with every lone LF made CRLF, borrowed untouched when it has none.
pub(super) fn crlf_line_endings(raw: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let bare = |i: usize| raw[i] == b'\n' && (i == 0 || raw[i - 1] != b'\r');
    if !(0..raw.len()).any(bare) {
        return std::borrow::Cow::Borrowed(raw);
    }
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 32);
    for (i, &byte) in raw.iter().enumerate() {
        if bare(i) {
            out.push(b'\r');
        }
        out.push(byte);
    }
    std::borrow::Cow::Owned(out)
}

/// Upload a message's bytes and import them into `mailbox` with
/// `keywords`: a draft being saved, or mail moved in from another account
/// (#265).
pub(super) fn jmap_import(
    s: &JmapSession,
    raw: &[u8],
    mailbox: &str,
    keywords: serde_json::Value,
) -> Result<String, String> {
    jmap_import_email(
        s,
        raw,
        serde_json::json!({ "mailboxIds": { mailbox: true }, "keywords": keywords }),
    )
}

/// Upload a message's bytes and import them with the Email properties in
/// `email` (`mailboxIds`, `keywords`, `receivedAt`). Returns the new id.
pub(super) fn jmap_import_email(
    s: &JmapSession,
    raw: &[u8],
    mut email: serde_json::Value,
) -> Result<String, String> {
    email["blobId"] = jmap_upload(s, raw, "message/rfc822")?.into();
    let responses = jmap_call(
        s,
        &[CAP_CORE, CAP_MAIL],
        vec![call(
            0,
            "Email/import",
            serde_json::json!({ "accountId": s.account, "emails": { "d": email } }),
        )],
    )?;
    let r = args(&responses, 0);
    r["created"]["d"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            let why = &r["notCreated"]["d"];
            why["description"]
                .as_str()
                .or(why["type"].as_str())
                .unwrap_or("the server would not take the message")
                .to_string()
        })
}

/// Replace a message with a copy that lacks one attachment (#289): a JMAP
/// message cannot be edited, so the copy is imported with the original's
/// folders, keywords and arrival time, then the original is destroyed.
/// Returns the copy's id and bytes.
pub(super) async fn jmap_strip_attachment(
    s: &JmapSession,
    state: &mut JmapState,
    path: &str,
    uid: u32,
    name: &str,
    size: u64,
) -> Result<(String, Vec<u8>), String> {
    let (id, _) = jmap_resolve(s, state, path, uid)
        .await
        .ok_or_else(|| "message not found".to_string())?;
    let raw = jmap_fetch_raw(s, state, path, uid).await?;
    let stripped =
        strip::without_attachment(&raw, name, size, &strip::note_date()).map_err(|e| e.text())?;
    let sess = s.clone();
    blocking(move || {
        let responses = jmap_call(
            &sess,
            &[CAP_CORE, CAP_MAIL],
            vec![call(
                0,
                "Email/get",
                serde_json::json!({
                    "accountId": sess.account,
                    "ids": [id],
                    "properties": ["mailboxIds", "keywords", "receivedAt"],
                }),
            )],
        )?;
        let original = args(&responses, 0)["list"][0].clone();
        if original.is_null() {
            return Err(i18n(
                "The attachment is no longer in the message on the server",
            ));
        }
        let new_id = jmap_import_email(
            &sess,
            &stripped,
            serde_json::json!({
                "mailboxIds": original["mailboxIds"],
                "keywords": original["keywords"],
                "receivedAt": original["receivedAt"],
            }),
        )?;
        jmap_destroy_all(&sess, &[id])?;
        Ok((new_id, stripped))
    })
    .await
}

/// Drop the draft a message was opened from, server-side and in the cache.
pub(super) async fn jmap_drop_draft_origin(
    s: &JmapSession,
    account_id: u32,
    origin: &crate::models::DraftOrigin,
    cache: Option<&Cache>,
    state: &mut JmapState,
) {
    if origin.account_id != account_id {
        return;
    }
    if let Some((id, _)) = jmap_resolve(s, state, &origin.path, origin.uid).await {
        let sess = s.clone();
        let _ = blocking(move || jmap_destroy_all(&sess, &[id])).await;
        state.uids.remove(&origin.uid);
    }
    if let Some(c) = cache {
        c.delete_message(account_id, &origin.path, origin.uid);
    }
}
