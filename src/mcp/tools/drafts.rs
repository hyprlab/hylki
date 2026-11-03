//! The drafting tools: a new message, a reply and a forward, each opened
//! in a composer of Hylki's own for the user to check and send. Nothing
//! here sends, and nothing can attach a file from disk.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{accounts_in_scope, failure, json, Mail};
use crate::app::AppMsg;
use crate::mcp::draft::{self, Draft, DraftKind, Original};
use crate::mcp::AppCall;

/// How long the app is given to open the composer: a forward waits for
/// the original's attachments to download first.
const OPEN_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

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

#[derive(Serialize)]
struct OpenedOut {
    opened: bool,
    note: String,
}

#[tool_router(router = draft_tools, vis = "pub(super)")]
impl Mail {
    #[tool(
        description = "Prepare a new message and open it in Hylki for the user to check \
        and send. It is not sent. Recipients must be email addresses from find_contacts or \
        the user; the body is plain text."
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
        not sent. Extra recipients must be email addresses; the body is plain text."
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
        sent. Recipients must be email addresses; the body is plain text."
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
            Ok(Ok(Ok(note))) => json(&OpenedOut { opened: true, note }),
            Ok(Ok(Err(e))) => failure(e),
            Ok(Err(_)) => failure("Hylki could not open the message".into()),
            Err(_) => failure(
                "Hylki took too long to open the message (a forward waits for its \
                 attachments to download); it may still open"
                    .into(),
            ),
        }
    }
}
