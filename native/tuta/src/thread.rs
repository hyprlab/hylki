//! Where a send or a draft sits in a conversation, and the mark a reply or a
//! forward leaves on the mail it answers. Mirrors the official client: the
//! draft names its parent by Message-ID in `DraftCreateData.previousMessageId`
//! with conversation type REPLY or FORWARD (`SendMailModel.initAsResponse`,
//! `MailFacade.createDraft`), and once it is sent the parent's `replyType`
//! moves on (`SendMailModel.updatePreviousMail`), written as a PATCH of that
//! one attribute as TS `EntityRestClient.update` writes it.
//!
//! Tuta takes no References of its own; a send's `references` only names the
//! parent when it has no `in_reply_to`. The server links the conversation
//! from the parent's id and writes the threading headers itself.
//!
//! The other way round, every mail's own Message-ID is the one its
//! conversation entry keeps (`ConversationEntry.messageId`, which the
//! official client names as `previousMessageId`): mail between Tuta
//! accounts carries no header block, so that is the only place it is.

use crate::{aggregate_id, diag, id, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use crypto_primitives::{aes::InitializationVector, randomizer_facade::RandomizerFacade};
use serde::Deserialize;
use std::collections::HashMap;
use tutasdk::{
    entities::{
        generated::{
            sys::{Patch, PatchList},
            tutanota::{ConversationEntry, Mail},
        },
        Entity,
    },
    GeneratedId, IdTupleGenerated, LoggedInSdk,
};

/// TS `ConversationType`.
pub(crate) const CONVERSATION_NEW: i64 = 0;
const CONVERSATION_REPLY: i64 = 1;
const CONVERSATION_FORWARD: i64 = 2;
/// TS `ReplyType`.
const REPLY_TYPE_NONE: i64 = 0;
const REPLY_TYPE_REPLY: i64 = 1;
const REPLY_TYPE_FORWARD: i64 = 2;
const REPLY_TYPE_REPLY_FORWARD: i64 = 3;
/// TS `PatchOperationType.REPLACE`.
const PATCH_REPLACE: i64 = 2;
/// RFC 5322's line limit bounds a single Message-ID.
const MAX_MESSAGE_ID_BYTES: usize = 998;
/// A References header past this is not a thread, it is a payload.
const MAX_REFERENCES: usize = 100;
/// TS `MailState.DRAFT`.
const MAIL_STATE_DRAFT: i64 = 0;
/// Conversation-entry list loads a listing keeps in flight at once.
const ENTRY_LOADS_IN_FLIGHT: usize = 8;

/// What a send or draft does to the mail it answers. A reply-all answers
/// like a reply: Tuta keeps no separate mark for it.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    Reply,
    ReplyAll,
    Forward,
}

/// A checked request's place in a conversation: the parent's Message-ID
/// (bare, without angle brackets) and the conversation type to create the
/// draft with. New mail has no parent and type NEW.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Thread {
    pub(crate) parent: Option<String>,
    pub(crate) conversation_type: i64,
}

impl Thread {
    /// Check the optional `in_reply_to`, `references` and `action`. Every
    /// Message-ID must be well formed and within the caps: a malformed one
    /// refuses the request rather than being dropped, so the caller learns
    /// its input was wrong. So does an action with no Message-ID to apply it
    /// to. A parent without an action is answered as a reply.
    pub(crate) fn of(
        in_reply_to: Option<&str>,
        references: Option<&str>,
        action: Option<Action>,
    ) -> Result<Self> {
        let replied = in_reply_to.map(bare_message_id).transpose()?;
        let referenced = match references {
            Some(value) => reference_ids(value)?,
            None => Vec::new(),
        };
        // The newest reference is the parent's own Message-ID.
        let parent = replied.or_else(|| referenced.last().cloned());
        let conversation_type = match (&parent, action) {
            (None, None) => CONVERSATION_NEW,
            (None, Some(_)) => return Err("invalid_thread"),
            (Some(_), Some(Action::Forward)) => CONVERSATION_FORWARD,
            (Some(_), _) => CONVERSATION_REPLY,
        };
        Ok(Self {
            parent,
            conversation_type,
        })
    }
}

/// One Message-ID without its one pair of angle brackets: printable ASCII
/// without spaces or brackets, within RFC 5322's line limit. No "@" is
/// required: real mail carries ids without one.
fn bare_message_id(value: &str) -> Result<String> {
    let trimmed = value.trim();
    let id = trimmed
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .unwrap_or(trimmed);
    let well_formed = !id.is_empty()
        && id.len() <= MAX_MESSAGE_ID_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'<' && b != b'>');
    if well_formed {
        Ok(id.to_string())
    } else {
        Err("invalid_thread")
    }
}

/// A References value's Message-IDs, oldest first: whitespace-separated, in
/// angle brackets or bare. Bracketed ids are read bracket to bracket, so a
/// space inside one is caught rather than splitting it in two, and glued
/// ids ("<a@b><c@d>") still separate.
fn reference_ids(value: &str) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    let mut rest = value.trim_start();
    while !rest.is_empty() {
        let end = if rest.starts_with('<') {
            rest.find('>').map(|end| end + 1).ok_or("invalid_thread")?
        } else {
            rest.find(|c: char| c.is_ascii_whitespace() || c == '<')
                .unwrap_or(rest.len())
        };
        ids.push(bare_message_id(&rest[..end])?);
        if ids.len() > MAX_REFERENCES {
            return Err("invalid_thread");
        }
        rest = rest[end..].trim_start();
    }
    Ok(ids)
}

/// The parent's reply type once a mail of `conversation_type` answering it
/// went out, or `None` when it stays as it is (TS `updatePreviousMail`).
fn next_reply_type(current: i64, conversation_type: i64) -> Option<i64> {
    match (current, conversation_type) {
        (REPLY_TYPE_NONE, CONVERSATION_REPLY) => Some(REPLY_TYPE_REPLY),
        (REPLY_TYPE_NONE, CONVERSATION_FORWARD) => Some(REPLY_TYPE_FORWARD),
        (REPLY_TYPE_FORWARD, CONVERSATION_REPLY) | (REPLY_TYPE_REPLY, CONVERSATION_FORWARD) => {
            Some(REPLY_TYPE_REPLY_FORWARD)
        }
        _ => None,
    }
}

/// After `draft` answering `thread` went out, mark the mail it answers
/// replied or forwarded. The parent is the mail the server linked the
/// draft's conversation entry to, found by the Message-ID the draft named;
/// one the account does not hold (an outside mail never received here, or
/// deleted) has no mail to mark. Whether the parent ends up marked: never
/// an error, since the send already went out.
pub(crate) async fn mark_parent(sdk: &LoggedInSdk, draft: &Mail, thread: &Thread) -> bool {
    let Some(parent_id) = thread.parent.as_deref() else {
        return false;
    };
    match parent_mail(sdk, draft, parent_id).await {
        Ok(Some(parent)) => match set_reply_type(sdk, &parent, thread.conversation_type).await {
            Ok(()) => true,
            Err(stage) => {
                diag::note(&format!("parent reply mark failed: {stage}"));
                false
            }
        },
        Ok(None) => {
            diag::note("parent reply mark: no parent mail in this account");
            false
        }
        Err(stage) => {
            diag::note(&format!("parent reply mark failed: {stage}"));
            false
        }
    }
}

/// The mail `draft`'s conversation entry follows, when it carries the
/// Message-ID the draft was created with.
async fn parent_mail(
    sdk: &LoggedInSdk,
    draft: &Mail,
    parent_id: &str,
) -> std::result::Result<Option<Mail>, &'static str> {
    let client = sdk.mail_facade().get_crypto_entity_client();
    let entry: ConversationEntry =
        client
            .load(&draft.conversationEntry)
            .await
            .map_err(|error| {
                diag::failed("conversation entry load", &error);
                "conversation entry load"
            })?;
    let Some(previous) = entry.previous else {
        return Ok(None);
    };
    let previous: ConversationEntry = client.load(&previous).await.map_err(|error| {
        diag::failed("parent entry load", &error);
        "parent entry load"
    })?;
    if bare_message_id(&previous.messageId).ok().as_deref() != Some(parent_id) {
        return Ok(None);
    }
    let Some(mail) = previous.mail else {
        return Ok(None);
    };
    client.load(&mail).await.map(Some).map_err(|error| {
        diag::failed("parent mail load", &error);
        "parent mail load"
    })
}

/// Move `parent`'s reply type on for a sent mail of `conversation_type`,
/// patching that attribute alone, encrypted with the parent's session key.
async fn set_reply_type(
    sdk: &LoggedInSdk,
    parent: &Mail,
    conversation_type: i64,
) -> std::result::Result<(), &'static str> {
    let Some(reply_type) = next_reply_type(parent.replyType, conversation_type) else {
        return Ok(());
    };
    let mail_id: &IdTupleGenerated = parent._id.as_ref().ok_or("parent mail id")?;
    let entities = sdk.get_entity_client();
    let type_ref = Mail::type_ref();
    let attribute = entities
        .resolve_client_type_ref(&type_ref)
        .ok()
        .and_then(|model| model.get_attribute_id_by_attribute_name("replyType").ok())
        .ok_or("reply type attribute")?;
    let server_model = entities
        .resolve_server_type_ref(&type_ref)
        .map_err(|_| "mail type model")?;
    let parsed = entities.load(&type_ref, mail_id).await.map_err(|error| {
        diag::failed("parent mail load (raw)", &error);
        "parent mail load (raw)"
    })?;
    let session_key = sdk
        .mail_facade()
        .get_crypto_entity_client()
        .get_crypto_facade()
        .resolve_session_key(&parsed, &server_model)
        .await
        .map_err(|error| {
            diag::failed("parent session key", &error);
            "parent session key"
        })?
        .ok_or("parent session key")?
        .session_key;
    let randomizer = RandomizerFacade::from_core(rand_core::OsRng);
    let encrypted = session_key
        .encrypt_data(
            reply_type.to_string().as_bytes(),
            InitializationVector::generate(&randomizer),
        )
        .map_err(|_| "reply type encryption")?;
    let patches = PatchList {
        _id: aggregate_id(&randomizer),
        patches: vec![Patch {
            _id: aggregate_id(&randomizer),
            patchOperation: PATCH_REPLACE,
            attributePath: attribute,
            value: Some(STANDARD.encode(encrypted)),
        }],
    };
    sdk.mail_facade()
        .get_crypto_entity_client()
        .patch_instance::<Mail>(mail_id, patches)
        .await
        .map_err(|error| {
            diag::failed("parent reply mark (Mail patch)", &error);
            "parent reply mark (Mail patch)"
        })
}

/// The conversation entries to read for `mails`, grouped by list for one
/// request per list: every mail but a draft, whose Message-ID is the
/// composing client's business until it is sent.
fn entry_lists<'a>(
    mails: impl IntoIterator<Item = (i64, &'a IdTupleGenerated)>,
) -> Vec<(GeneratedId, Vec<GeneratedId>)> {
    let mut lists: Vec<(GeneratedId, Vec<GeneratedId>)> = Vec::new();
    for (_, entry) in mails
        .into_iter()
        .filter(|(state, _)| *state != MAIL_STATE_DRAFT)
    {
        match lists.iter_mut().find(|(list, _)| *list == entry.list_id) {
            Some((_, elements)) if elements.contains(&entry.element_id) => {}
            Some((_, elements)) => elements.push(entry.element_id.clone()),
            None => lists.push((entry.list_id.clone(), vec![entry.element_id.clone()])),
        }
    }
    lists
}

/// Each mail's Message-ID, bare, by its conversation entry's compound id.
/// Drafts have none here, and a list that fails to load leaves its mails
/// out: a listing never fails over threading. A page of mail is mostly one
/// list per mail, so up to `ENTRY_LOADS_IN_FLIGHT` lists load at once
/// rather than doubling a sync's round trips end to end.
pub(crate) async fn message_ids(sdk: &LoggedInSdk, mails: &[Mail]) -> HashMap<String, String> {
    let client = sdk.mail_facade().get_crypto_entity_client();
    let mut lists = entry_lists(mails.iter().map(|m| (m.state, &m.conversationEntry)));
    let mut loads = tokio::task::JoinSet::new();
    let mut ids = HashMap::new();
    loop {
        while loads.len() < ENTRY_LOADS_IN_FLIGHT {
            let Some((list, elements)) = lists.pop() else {
                break;
            };
            let client = client.clone();
            loads.spawn(async move {
                client
                    .load_multiple::<ConversationEntry>(&list, &elements)
                    .await
            });
        }
        let Some(loaded) = loads.join_next().await else {
            break;
        };
        match loaded {
            Ok(Ok(entries)) => ids.extend(entries.into_iter().filter_map(|entry| {
                let message_id = bare_message_id(&entry.messageId).ok()?;
                Some((id(entry._id.as_ref()?), message_id))
            })),
            Ok(Err(error)) => diag::failed("conversation entries load", &error),
            Err(error) => diag::failed("conversation entries load (task)", &error),
        }
    }
    ids
}

/// `mail`'s own Message-ID (see [`message_ids`]).
pub(crate) fn message_id_of<'a>(ids: &'a HashMap<String, String>, mail: &Mail) -> Option<&'a str> {
    ids.get(&id(&mail.conversationEntry)).map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(
        in_reply_to: Option<&str>,
        references: Option<&str>,
        action: Option<Action>,
    ) -> Result<Thread> {
        Thread::of(in_reply_to, references, action)
    }

    #[test]
    fn new_mail_has_no_parent() {
        assert_eq!(
            thread(None, None, None),
            Ok(Thread {
                parent: None,
                conversation_type: CONVERSATION_NEW
            })
        );
    }

    #[test]
    fn a_reply_names_its_parent_without_brackets() {
        for action in [None, Some(Action::Reply), Some(Action::ReplyAll)] {
            assert_eq!(
                thread(Some("<Parent.1@Example.com>"), Some("<r@x> <p@x>"), action),
                Ok(Thread {
                    parent: Some("Parent.1@Example.com".into()),
                    conversation_type: CONVERSATION_REPLY
                }),
                "{action:?}"
            );
        }
        let forward = thread(Some("fwd.1@example.com"), None, Some(Action::Forward));
        assert_eq!(forward.unwrap().conversation_type, CONVERSATION_FORWARD);
    }

    #[test]
    fn without_in_reply_to_the_newest_reference_is_the_parent() {
        for references in [
            "<root@x> <parent@x>",
            "<root@x><parent@x>",
            "root@x\r\n parent@x",
        ] {
            assert_eq!(
                thread(None, Some(references), None)
                    .unwrap()
                    .parent
                    .as_deref(),
                Some("parent@x"),
                "{references:?}"
            );
        }
    }

    #[test]
    fn malformed_ids_and_orphan_actions_are_refused() {
        let long = "a".repeat(MAX_MESSAGE_ID_BYTES + 1);
        for bad in [
            "", "<>", "a b@x", "<a b@x>", "a@x\nb", "é@x", "<a@x", "a>@x", &long,
        ] {
            assert_eq!(
                thread(Some(bad), None, None),
                Err("invalid_thread"),
                "{bad:?}"
            );
        }
        for bad in ["<a@x> <b c@x>", "<a@x", "<a@x> <>"] {
            assert_eq!(
                thread(None, Some(bad), None),
                Err("invalid_thread"),
                "{bad:?}"
            );
        }
        assert_eq!(
            thread(None, None, Some(Action::Reply)),
            Err("invalid_thread")
        );
        assert_eq!(
            thread(None, Some("  "), Some(Action::Forward)),
            Err("invalid_thread")
        );
        let longest = "a".repeat(MAX_MESSAGE_ID_BYTES);
        assert!(thread(Some(&longest), None, None).is_ok());
    }

    #[test]
    fn references_are_capped_at_a_hundred_ids() {
        let ids = |n: usize| (0..n).map(|i| format!("<{i}@x>")).collect::<Vec<_>>();
        let hundred = ids(MAX_REFERENCES).join(" ");
        assert_eq!(
            thread(None, Some(&hundred), None)
                .unwrap()
                .parent
                .as_deref(),
            Some("99@x")
        );
        let more = ids(MAX_REFERENCES + 1).join(" ");
        assert_eq!(thread(None, Some(&more), None), Err("invalid_thread"));
    }

    #[test]
    fn the_parent_mark_moves_on_as_the_official_client_moves_it() {
        let reply = CONVERSATION_REPLY;
        let forward = CONVERSATION_FORWARD;
        assert_eq!(
            next_reply_type(REPLY_TYPE_NONE, reply),
            Some(REPLY_TYPE_REPLY)
        );
        assert_eq!(
            next_reply_type(REPLY_TYPE_NONE, forward),
            Some(REPLY_TYPE_FORWARD)
        );
        assert_eq!(
            next_reply_type(REPLY_TYPE_FORWARD, reply),
            Some(REPLY_TYPE_REPLY_FORWARD)
        );
        assert_eq!(
            next_reply_type(REPLY_TYPE_REPLY, forward),
            Some(REPLY_TYPE_REPLY_FORWARD)
        );
        assert_eq!(next_reply_type(REPLY_TYPE_REPLY, reply), None);
        assert_eq!(next_reply_type(REPLY_TYPE_FORWARD, forward), None);
        assert_eq!(next_reply_type(REPLY_TYPE_REPLY_FORWARD, reply), None);
        assert_eq!(next_reply_type(REPLY_TYPE_NONE, CONVERSATION_NEW), None);
    }

    #[test]
    fn every_mail_but_a_draft_has_its_entry_read_once_per_list() {
        let entry = |list: &str, element: &str| {
            IdTupleGenerated::new(GeneratedId(list.into()), GeneratedId(element.into()))
        };
        let (a1, a2, b1, draft) = (
            entry("a", "1"),
            entry("a", "2"),
            entry("b", "1"),
            entry("c", "1"),
        );
        let received = 2;
        let lists = entry_lists([
            (received, &a1),
            (received, &b1),
            (received, &a2),
            (received, &a1),
            (MAIL_STATE_DRAFT, &draft),
        ]);
        let ids = |list: &(GeneratedId, Vec<GeneratedId>)| {
            let elements: Vec<String> = list.1.iter().map(|e| e.to_string()).collect();
            (list.0.to_string(), elements)
        };
        assert_eq!(
            lists.iter().map(ids).collect::<Vec<_>>(),
            [
                ("a".to_string(), vec!["1".to_string(), "2".to_string()]),
                ("b".to_string(), vec!["1".to_string()]),
            ]
        );
    }
}
