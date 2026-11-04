//! The drafting tools: a new message, a reply and a forward, each opened
//! in a composer of Hylki's own for the user to check and send; and the
//! composers open now, read, and changed only as the user approves in the
//! composer itself. Nothing here sends, and nothing can attach a file from
//! disk. Drafts saved in a Drafts folder are messages like any other
//! (list_messages, read_message); these tools reach only open composers.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{accounts_in_scope, cap, failure, json, Mail, BODY_MAX};
use crate::app::AppMsg;
use crate::mcp::draft::{self, Draft, DraftEdit, DraftKind, EditOutcome, OpenDraft, Original};
use crate::mcp::AppCall;

/// How long the app is given to open the composer: a forward waits for
/// the original's attachments to download first.
const OPEN_WAIT: std::time::Duration = std::time::Duration::from_secs(120);
/// How long open composers are given to say what they hold: the text
/// comes out of the editor, which answers on the main loop.
const READ_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Deserialize, JsonSchema)]
pub(super) struct ComposeArgs {
    /// The account to send from (its address, from list_accounts). Left out, the
    /// account Hylki starts new messages from.
    account: Option<String>,
    /// Recipients: email addresses, as "Name <address>" or bare.
    #[serde(default)]
    to: Vec<String>,
    #[serde(default)]
    cc: Vec<String>,
    #[serde(default)]
    bcc: Vec<String>,
    #[serde(default)]
    subject: String,
    /// The message, in plain text. Hylki adds the user's signature.
    #[serde(default)]
    body: String,
}

#[derive(Deserialize, JsonSchema)]
pub(super) struct ReplyArgs {
    /// The `id` of the message to answer, as list_messages or search gave it.
    id: String,
    /// Answer everyone the message went to, not only its sender.
    #[serde(default)]
    all: bool,
    /// More recipients for To, beside those the reply goes to anyway.
    #[serde(default)]
    add_to: Vec<String>,
    #[serde(default)]
    cc: Vec<String>,
    #[serde(default)]
    bcc: Vec<String>,
    /// The reply, in plain text; Hylki quotes the original under it.
    #[serde(default)]
    body: String,
}

#[derive(Deserialize, JsonSchema)]
pub(super) struct ForwardArgs {
    /// The `id` of the message to forward, as list_messages or search gave it.
    id: String,
    #[serde(default)]
    to: Vec<String>,
    #[serde(default)]
    cc: Vec<String>,
    #[serde(default)]
    bcc: Vec<String>,
    /// A note above the forwarded message, in plain text. Its attachments go along.
    #[serde(default)]
    body: String,
}

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
struct OpenedOut {
    opened: bool,
    /// The composer's id, for read_draft and update_draft.
    id: String,
    note: String,
}

#[derive(Serialize)]
struct DraftOut {
    id: String,
    kind: &'static str,
    by_assistant: bool,
    from: String,
    to: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    cc: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    bcc: String,
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
    /// An edit the user has neither applied nor rejected yet.
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

#[tool_router(router = draft_tools, vis = "pub(super)")]
impl Mail {
    #[tool(
        description = "Prepare a new message and open it in Hylki for the user to check \
        and send. It is not sent. Recipients must be email addresses from find_contacts or \
        the user; the body is plain text. Returns the composer's id."
    )]
    async fn compose_message(&self, Parameters(args): Parameters<ComposeArgs>) -> CallToolResult {
        let account_id = match args.account.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(address) => match accounts_in_scope(Some(address)) {
                Ok(mut found) => {
                    let account = found.remove(0);
                    if !account.sends {
                        return failure(format!("{} cannot send mail", account.address));
                    }
                    Some(account.id)
                }
                Err(e) => return failure(e),
            },
        };
        let draft = match checked(
            DraftKind::New { account_id },
            (&args.to, &args.cc, &args.bcc),
            &args.subject,
            &args.body,
        ) {
            Ok(d) => d,
            Err(e) => return failure(e),
        };
        self.open(draft).await
    }

    #[tool(
        description = "Prepare a reply to a message and open it in Hylki, the original \
        quoted under it as when the user clicks Reply, for the user to check and send. It is \
        not sent. Extra recipients must be email addresses; the body is plain text. Returns \
        the composer's id."
    )]
    async fn reply_to_message(&self, Parameters(args): Parameters<ReplyArgs>) -> CallToolResult {
        let original = match self.original(&args.id).await {
            Ok(o) => o,
            Err(e) => return failure(e),
        };
        let draft = match checked(
            DraftKind::Reply {
                original: Box::new(original),
                all: args.all,
            },
            (&args.add_to, &args.cc, &args.bcc),
            "",
            &args.body,
        ) {
            Ok(d) => d,
            Err(e) => return failure(e),
        };
        self.open(draft).await
    }

    #[tool(
        description = "Prepare a forward of a message, with its attachments, and open it \
        in Hylki as when the user clicks Forward, for the user to check and send. It is not \
        sent. Recipients must be email addresses; the body is plain text. Returns the \
        composer's id."
    )]
    async fn forward_message(&self, Parameters(args): Parameters<ForwardArgs>) -> CallToolResult {
        let original = match self.original(&args.id).await {
            Ok(o) => o,
            Err(e) => return failure(e),
        };
        let draft = match checked(
            DraftKind::Forward {
                original: Box::new(original),
            },
            (&args.to, &args.cc, &args.bcc),
            "",
            &args.body,
        ) {
            Ok(d) => d,
            Err(e) => return failure(e),
        };
        self.open(draft).await
    }

    #[tool(
        description = "The messages being written in Hylki now: every open composer, the \
        user's own and those an assistant prepared, with its recipients and subject and any \
        edit waiting for the user. Pass an `id` to read_draft for the text. Drafts saved \
        and closed are in the drafts folder (list_messages)."
    )]
    async fn list_drafts(&self) -> CallToolResult {
        let drafts = match self.read_drafts(None, false).await {
            Ok(d) => d,
            Err(e) => return failure(e),
        };
        json(&drafts.into_iter().map(draft_out).collect::<Vec<_>>())
    }

    #[tool(
        description = "One open composer as it stands: its recipients, subject, the names \
        of its attachments, and the text written above the signature and any quoted \
        message. Says whether an edit asked for with update_draft still waits for the \
        user, and what the user did with the last one."
    )]
    async fn read_draft(&self, Parameters(args): Parameters<ReadDraftArgs>) -> CallToolResult {
        let id = match parse_composer_id(&args.id) {
            Ok(id) => id,
            Err(e) => return failure(e),
        };
        match self.read_drafts(Some(id), true).await {
            Ok(mut found) if !found.is_empty() => json(&draft_out(found.remove(0))),
            Ok(_) => failure(not_open(&args.id)),
            Err(e) => failure(e),
        }
    }

    #[tool(
        description = "Ask for a change to an open composer: each field given replaces \
        what is there (recipients as whole lists; the body replaces the text above the \
        signature and any quote), the rest stay. Nothing changes until the user applies it \
        in the composer, or rejects it; read_draft says which. A newer edit replaces one \
        still waiting. Recipients must be email addresses; the body is plain text."
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
        let (done, answer) = tokio::sync::oneshot::channel();
        if self
            .app
            .send(AppMsg::Assistant(AppCall::EditDraft { id, edit, done }))
            .is_err()
        {
            return failure("Hylki is closing".into());
        }
        match tokio::time::timeout(READ_WAIT, answer).await {
            Ok(Ok(true)) => json(&EditedOut {
                pending: true,
                note: "The edit waits in the composer for the user to apply or reject; \
                       nothing has changed yet. read_draft says what became of it.",
            }),
            Ok(Ok(false)) => failure(not_open(&args.id)),
            _ => failure("Hylki did not answer".into()),
        }
    }
}

/// The draft, its recipients and text checked.
fn checked(
    kind: DraftKind,
    (to, cc, bcc): (&[String], &[String], &[String]),
    subject: &str,
    body: &str,
) -> Result<Draft, String> {
    Ok(Draft {
        kind,
        to: draft::recipients(to)?,
        cc: draft::recipients(cc)?,
        bcc: draft::recipients(bcc)?,
        subject: draft::subject(subject),
        text: draft::text(body)?,
    })
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
fn composer_id(id: u32) -> String {
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
        "no composer {id} is open: it was sent, saved or closed. list_drafts shows those that \
         are; a saved draft is in the drafts folder."
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
        by_assistant: d.by_assistant,
        from: d.from,
        to: d.to,
        cc: d.cc,
        bcc: d.bcc,
        subject: d.subject,
        attachments: d.attachments,
        format: d.format,
        encrypted: d.encrypted,
        text_note: d.encrypted.then_some(
            "The text is not shown: the message is to go out encrypted, so it stays on this \
             computer.",
        ),
        text,
        text_truncated,
        pending_edit: d.pending.map(|e| EditOut {
            status: "waiting for the user to apply or reject it",
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

impl Mail {
    /// The message an id names, with the body a reply or forward quotes,
    /// downloaded first if it has to be.
    async fn original(&self, id: &str) -> Result<Original, String> {
        let (account, located) = self.message(id)?;
        let body = self
            .body(account.id, &located.path, located.uid)
            .await
            .ok_or(
                "Hylki could not download that message's text to quote (its account may be \
                    offline or off); try again later",
            )?;
        Ok(Original {
            message: located.message,
            path: located.path,
            body,
        })
    }

    /// Hand the draft to the app, and say what came of it.
    async fn open(&self, draft: Draft) -> CallToolResult {
        let (done, answer) = tokio::sync::oneshot::channel();
        if self
            .app
            .send(AppMsg::Assistant(AppCall::Draft { draft, done }))
            .is_err()
        {
            return failure("Hylki is closing".into());
        }
        match tokio::time::timeout(OPEN_WAIT, answer).await {
            Ok(Ok(Ok((id, note)))) => json(&OpenedOut {
                opened: true,
                id: composer_id(id),
                note,
            }),
            Ok(Ok(Err(e))) => failure(e),
            Ok(Err(_)) => failure("Hylki could not open the message".into()),
            Err(_) => failure(
                "Hylki took too long to open the message (a forward waits for its \
                 attachments to download); it may still open"
                    .into(),
            ),
        }
    }

    /// The open composers (or the one `id` names) as they stand, in the
    /// order they were opened. One that does not answer in time is left
    /// out: it was closing.
    async fn read_drafts(&self, id: Option<u32>, text: bool) -> Result<Vec<OpenDraft>, String> {
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
                out.push(d);
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
