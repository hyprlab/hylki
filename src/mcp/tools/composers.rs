//! The composers open now: read, and changed only as the user approves in the
//! composer itself. What an assistant reaches goes by the account a composer
//! sends from: the drafts assistants prepared need one switch, and those the
//! user is writing another on top of it. Past what is allowed a composer looks
//! like none. One that is encrypted, or answers encrypted mail, shows that it
//! is there and nothing else. Drafts saved in a Drafts folder are messages like
//! any other (list_messages, read_message); these tools reach only open
//! composers.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{cap, failure, json, Account, Mail, BODY_MAX, ENCRYPTED_NOTE};
use crate::app::AppMsg;
use crate::config::AssistantCap;
use crate::mcp::draft::{self, DraftEdit, EditAnswer, EditOutcome, OpenDraft};
use crate::mcp::AppCall;

/// How long open composers are given to say what they hold: the text
/// comes out of the editor, which answers on the main loop.
const READ_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Deserialize, JsonSchema)]
pub(super) struct ReadDraftArgs {
    /// The `id` of an open composer, as list_drafts or compose_message,
    /// reply_to_message or forward_message gave it.
    id: String,
}

#[derive(Deserialize, JsonSchema)]
pub(super) struct UpdateDraftArgs {
    /// The `id` of an open composer, as list_drafts or compose_message,
    /// reply_to_message or forward_message gave it.
    id: String,
    /// Every recipient in To, in place of those there now. Left out, To stays.
    to: Option<Vec<String>>,
    /// Every recipient in Cc, in place of those there now. Left out, Cc stays.
    cc: Option<Vec<String>>,
    /// Every recipient in Bcc, in place of those there now. Left out, Bcc stays.
    bcc: Option<Vec<String>>,
    /// The subject. Left out, it stays.
    subject: Option<String>,
    /// The text, in plain text, in place of what is written above the
    /// signature and any quoted message (those stay). Left out, the text stays.
    body: Option<String>,
}

#[derive(Serialize)]
struct DraftOut {
    id: String,
    kind: &'static str,
    by_agent: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    from: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    to: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    cc: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    bcc: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    subject: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<String>,
    format: &'static str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    encrypted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    text_truncated: bool,
    /// Why there is no text, said to the model in words.
    #[serde(skip_serializing_if = "Option::is_none")]
    text_note: Option<&'static str>,
    /// An edit the user has neither accepted nor rejected yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_edit: Option<EditOut>,
    /// What the user did with the last edit asked for: "applied" or "rejected".
    #[serde(skip_serializing_if = "Option::is_none")]
    last_edit: Option<&'static str>,
}

#[derive(Serialize)]
struct EditOut {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cc: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bcc: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
}

#[derive(Serialize)]
struct EditedOut {
    pending: bool,
    note: &'static str,
}

#[tool_router(router = composer_tools, vis = "pub(super)")]
impl Mail {
    #[tool(
        description = "The messages being written in Hylki now that the user allows \
        agents to see, with recipients, subject and any edit waiting for the user. Pass \
        an `id` to read_draft for the text. Drafts saved and closed are in the drafts folder \
        (list_messages).",
        annotations(read_only_hint = true)
    )]
    async fn list_drafts(&self) -> CallToolResult {
        match self.visible_drafts(None, false).await {
            Ok(drafts) => json(&drafts.into_iter().map(draft_out).collect::<Vec<_>>()),
            Err(e) => failure(e),
        }
    }

    #[tool(
        description = "One open composer as it stands: its recipients, subject, the names \
        of its attachments, and the text written above the signature and any quoted \
        message. Says whether an edit asked for with update_draft still waits for the \
        user, and what the user did with the last one.",
        annotations(read_only_hint = true)
    )]
    async fn read_draft(&self, Parameters(args): Parameters<ReadDraftArgs>) -> CallToolResult {
        let id = match parse_composer_id(&args.id) {
            Ok(id) => id,
            Err(e) => return failure(e),
        };
        // Asked for without its text first: the text is taken out of a
        // composer only once it is known to be allowed.
        match self.visible_drafts(Some(id), false).await {
            Ok(found) if found.is_empty() => return failure(not_open(&args.id)),
            Ok(_) => {}
            Err(e) => return failure(e),
        }
        match self.visible_drafts(Some(id), true).await {
            Ok(mut found) if !found.is_empty() => json(&draft_out(found.remove(0))),
            Ok(_) => failure(not_open(&args.id)),
            Err(e) => failure(e),
        }
    }

    #[tool(
        description = "Ask for a change to an open composer: each field given replaces \
        what is there (recipients as whole lists; the body replaces the text above the \
        signature and any quote), the rest stay. Nothing changes until the user accepts it \
        in the composer, or rejects it; read_draft says which. A newer edit replaces one \
        still waiting. It is refused for an encrypted message. Recipients must be email \
        addresses; the body is plain text.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn update_draft(&self, Parameters(args): Parameters<UpdateDraftArgs>) -> CallToolResult {
        let id = match parse_composer_id(&args.id) {
            Ok(id) => id,
            Err(e) => return failure(e),
        };
        let edit = match checked_edit(&args) {
            Ok(e) => e,
            Err(e) => return failure(e),
        };
        match self.visible_drafts(Some(id), false).await {
            Ok(found) if found.is_empty() => return failure(not_open(&args.id)),
            Ok(found) if found[0].encrypted => return failure(ENCRYPTED_NOTE.to_string()),
            Ok(_) => {}
            Err(e) => return failure(e),
        }
        let (done, answer) = tokio::sync::oneshot::channel();
        if self
            .app
            .send(AppMsg::Assistant(AppCall::EditDraft { id, edit, done }))
            .is_err()
        {
            return failure("Hylki is closing".into());
        }
        match tokio::time::timeout(READ_WAIT, answer).await {
            Ok(Ok(EditAnswer::Waits)) => json(&EditedOut {
                pending: true,
                note: "The edit waits in the composer for the user to accept or reject; \
                       nothing has changed yet. read_draft says what became of it.",
            }),
            Ok(Ok(EditAnswer::NotOpen)) => failure(not_open(&args.id)),
            Ok(Ok(EditAnswer::TooSoon(secs))) => failure(format!(
                "The composer had an edit a moment ago, and each one interrupts the user. Try \
                 again in {secs} s, if the user wants another."
            )),
            _ => failure("Hylki did not answer".into()),
        }
    }
}

/// The edit, its recipients and text checked as a new message's are.
fn checked_edit(args: &UpdateDraftArgs) -> Result<DraftEdit, String> {
    let list = |l: &Option<Vec<String>>| l.as_deref().map(draft::recipients).transpose();
    let edit = DraftEdit {
        to: list(&args.to)?,
        cc: list(&args.cc)?,
        bcc: list(&args.bcc)?,
        subject: args.subject.as_deref().map(draft::subject),
        text: args.body.as_deref().map(draft::text).transpose()?,
    };
    if edit.is_empty() {
        return Err("nothing to change: give at least one of to, cc, bcc, subject, body".into());
    }
    Ok(edit)
}

/// A composer's id as the tools hand it out: the app's own number for it,
/// which is never given to another composer while the app runs.
pub(super) fn composer_id(id: u32) -> String {
    format!("composer-{id}")
}

fn parse_composer_id(id: &str) -> Result<u32, String> {
    id.trim()
        .strip_prefix("composer-")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| {
            format!(
                "\"{id}\" is not a composer id; list_drafts gives those of the composers open \
                 now (a saved draft's message id goes to read_message)"
            )
        })
}

fn not_open(id: &str) -> String {
    format!(
        "no composer {id} is open, or it is not shared: it may have been sent, saved or closed. \
         list_drafts shows those that are; a saved draft is in the drafts folder."
    )
}

fn draft_out(d: OpenDraft) -> DraftOut {
    let (text, text_truncated) = match d.text {
        Some(t) => {
            let (t, cut) = cap(t, BODY_MAX);
            (Some(t), cut)
        }
        None => (None, false),
    };
    DraftOut {
        id: composer_id(d.compose_id),
        kind: d.kind,
        by_agent: d.by_assistant,
        from: d.from,
        to: d.to,
        cc: d.cc,
        bcc: d.bcc,
        subject: d.subject,
        attachments: d.attachments,
        format: d.format,
        encrypted: d.encrypted,
        text_note: d.encrypted.then_some(ENCRYPTED_NOTE),
        text,
        text_truncated,
        pending_edit: d.pending.map(|e| EditOut {
            status: "waiting for the user to accept or reject it",
            to: e.to,
            cc: e.cc,
            bcc: e.bcc,
            subject: e.subject,
            body: e.text,
        }),
        last_edit: d.last_edit.map(|o| match o {
            EditOutcome::Applied => "applied",
            EditOutcome::Rejected => "rejected",
        }),
    }
}

/// Whether an account's permissions reach a composer sending from it.
fn may_see(d: &OpenDraft, accounts: &[Account]) -> bool {
    accounts
        .iter()
        .find(|a| a.id == d.account_id)
        .is_some_and(|a| d.by_assistant || a.grant.allows(AssistantCap::OwnDrafts))
}

impl Mail {
    /// The open composers (or the one `id` names) an assistant may reach, in
    /// the order they were opened. One that does not answer in time is left
    /// out: it was closing.
    async fn visible_drafts(&self, id: Option<u32>, text: bool) -> Result<Vec<OpenDraft>, String> {
        let accounts = self.scope(None, AssistantCap::OpenDrafts)?;
        let (done, answer) = tokio::sync::oneshot::channel();
        self.app
            .send(AppMsg::Assistant(AppCall::ReadDrafts { id, text, done }))
            .map_err(|_| "Hylki is closing".to_string())?;
        let deadline = tokio::time::Instant::now() + READ_WAIT;
        let lines = tokio::time::timeout_at(deadline, answer)
            .await
            .map_err(|_| "Hylki did not answer".to_string())?
            .map_err(|_| "Hylki did not answer".to_string())?;
        let mut out = Vec::new();
        for line in lines {
            if let Ok(Ok(d)) = tokio::time::timeout_at(deadline, line).await {
                if may_see(&d, &accounts) {
                    out.push(d);
                }
            }
        }
        out.sort_by_key(|d| d.compose_id);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_composer_id_reads_back() {
        assert_eq!(parse_composer_id(&composer_id(17)), Ok(17));
        assert_eq!(parse_composer_id(" composer-3 "), Ok(3));
        for wrong in ["17", "composer-", "composer-x", "a@x|INBOX|1"] {
            assert!(parse_composer_id(wrong).is_err(), "{wrong}");
        }
    }

    #[test]
    fn an_edit_is_checked_as_a_message_is() {
        let args = |v: serde_json::Value| serde_json::from_value::<UpdateDraftArgs>(v).unwrap();
        let e = checked_edit(&args(serde_json::json!({"id":"composer-1",
            "cc":["Ada <ada@example.org>"], "subject":" Lunch\n on Friday "})))
        .unwrap();
        assert_eq!(e.cc, Some(vec!["Ada <ada@example.org>".to_string()]));
        assert_eq!(e.subject.as_deref(), Some("Lunch on Friday"));
        assert!(e.to.is_none() && e.text.is_none());
        // An empty list clears the field; it is not "unchanged".
        let e = checked_edit(&args(serde_json::json!({"id":"composer-1","bcc":[]}))).unwrap();
        assert_eq!(e.bcc, Some(vec![]));
        let names = checked_edit(&args(serde_json::json!({"id":"composer-1","to":["Ada"]})));
        assert!(names.unwrap_err().contains("find_contacts"));
        assert!(checked_edit(&args(serde_json::json!({"id":"composer-1"}))).is_err());
    }
}
