//! Sending from a Microsoft account: sendMail, the answered mark Outlook
//! shows, and the Outbox.

use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::i18n::{i18n, i18n_f};
use crate::services::graph_api::mailbox_root;
use crate::worker::compose::build::build_email;
use crate::worker::compose::editable::{queued_with_bcc, with_bcc};
use crate::worker::graph::http::{graph_post_mime, graph_token};
use crate::worker::graph::index::GraphState;
use crate::worker::graph::messages::{ICON_INDEX, LAST_VERB};
use crate::worker::graph::ops::graph_patch_message;
use crate::worker::outbox::{due_outbox_items, emit_outbox, note_answered, raw_in_reply_to};
use crate::worker::{answered_copies, blocking, offline_text, OutgoingMessage, WorkerEvent};

/// Send over Graph: `sendMail` takes the same raw MIME `build_email` produces
/// and files the Sent copy itself.
pub(super) async fn graph_send_message(
    account: &AccountConfig,
    message: &OutgoingMessage,
    emit: &impl Fn(WorkerEvent),
) -> Result<(), String> {
    let email = build_email(account, message).map_err(|e| e.to_string())?;
    // `sendMail` takes its recipients from the headers, so without the Bcc
    // header the Bcc recipients get nothing (#476). Exchange removes it on
    // delivery and keeps it on the Sent Items copy.
    let raw = with_bcc(&email.formatted(), &message.bcc);
    if !crate::desktop::power::reaches_internet() {
        return Err(offline_text());
    }
    let token = graph_token(account, emit)
        .await
        .ok_or_else(|| "no sign-in token from GNOME Online Accounts".to_string())?;
    let url = format!("{}/sendMail", mailbox_root(account));
    blocking(move || graph_post_mime(&token, &url, &raw))
        .await
        .map_err(|e| refused_send(account, e))?;
    Ok(())
}

/// A refused `sendMail` from a shared mailbox (#701) is a missing Send As
/// right: being able to read the mailbox does not carry it.
fn refused_send(account: &AccountConfig, e: String) -> String {
    if account.shared_mailbox && e.starts_with("Microsoft Graph returned 403") {
        i18n_f(
            "This account may not send from {address}. Your organization gives that right in Exchange.",
            &[("address", &account.email)],
        )
    } else {
        e
    }
}

/// [`super::mark_answered`] for Microsoft 365 (#454): the last verb, and
/// the icon Outlook draws from it.
pub(super) async fn graph_mark_answered(
    account: &AccountConfig,
    state: &mut GraphState,
    cache: Option<&Cache>,
    account_id: u32,
    in_reply_to: &str,
    forwards: &str,
    emit: &impl Fn(WorkerEvent),
) {
    for (path, uid, forwarded) in answered_copies(cache, account_id, in_reply_to, forwards) {
        let (verb, icon) = if forwarded {
            ("104", "262")
        } else {
            ("102", "261")
        };
        let body = serde_json::json!({ "singleValueExtendedProperties": [
            { "id": LAST_VERB, "value": verb },
            { "id": ICON_INDEX, "value": icon },
        ] });
        graph_patch_message(account, state, &path, uid, body, emit).await;
        note_answered(cache, account_id, path, uid, forwarded, emit);
    }
}

/// The Outbox retry loop for Graph accounts: same queue and bookkeeping as
/// [`flush_outbox`], with `sendMail` as the transport (Sent copy automatic).
pub(super) async fn graph_flush_outbox(
    cache: Option<&Cache>,
    account_id: u32,
    account: &AccountConfig,
    id: Option<u32>,
    state: &mut GraphState,
    emit: &impl Fn(WorkerEvent),
) {
    let Some(cache) = cache else { return };
    let items = due_outbox_items(cache, account_id, id);
    if items.is_empty() {
        return;
    }
    emit(WorkerEvent::Status(i18n("Sending…")));
    let Some(token) = graph_token(account, emit).await else {
        emit(WorkerEvent::Status(String::new()));
        return;
    };
    let mut sent_any = false;
    for item in items {
        let t = token.clone();
        let raw = queued_with_bcc(&item.raw, &item.rcpts);
        let url = format!("{}/sendMail", state.mailbox);
        let r = blocking(move || graph_post_mime(&t, &url, &raw))
            .await
            .map_err(|e| refused_send(account, e));
        match r {
            Ok(_) => {
                sent_any = true;
                cache.delete_outbox(item.id);
                graph_mark_answered(
                    account,
                    state,
                    Some(cache),
                    account_id,
                    &raw_in_reply_to(&item.raw),
                    &item.forwards,
                    emit,
                )
                .await;
            }
            Err(e) => {
                cache.record_outbox_failure(item.id, &e);
                emit(WorkerEvent::error(i18n_f(
                    "Still could not send “{subject}”: {e}",
                    &[
                        ("subject", &item.subject.to_string()),
                        ("e", &e.to_string()),
                    ],
                )));
                break;
            }
        }
    }
    emit(WorkerEvent::Status(String::new()));
    if sent_any {
        emit(WorkerEvent::Sent);
    }
    emit_outbox(Some(cache), account_id, emit);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #701: a shared mailbox refused at sending says which right is
    /// missing; anything else, and any other account, keeps Graph's words.
    #[test]
    fn a_shared_mailbox_refused_at_sending_names_the_right() {
        let refused = "Microsoft Graph returned 403: Access is denied.".to_string();
        let mut account = AccountConfig {
            email: "team@example.com".into(),
            ..crate::config::blank_account()
        };
        assert_eq!(refused_send(&account, refused.clone()), refused);
        account.shared_mailbox = true;
        assert!(refused_send(&account, refused).contains("team@example.com"));
        let other = "Microsoft Graph returned 500: busy".to_string();
        assert_eq!(refused_send(&account, other.clone()), other);
    }
}
