//! The drafting tools: a new message, a reply and a forward, each opened
//! in a composer of Hylki's own for the user to check and send. Nothing
//! here sends, and nothing can attach a file from disk.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::bodies::Body;
use super::{failure, json, Mail, ENCRYPTED_NOTE};
use crate::app::AppMsg;
use crate::config::AssistantCap;
use crate::mcp::draft::{self, Draft, DraftKind, Original};
use crate::mcp::AppCall;

/// How long the app is given to open the composer.
const OPEN_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

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
        the user; the body is plain text.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn compose_message(&self, Parameters(args): Parameters<ComposeArgs>) -> CallToolResult {
        let accounts = match self.scope_where(
            args.account.as_deref(),
            AssistantCap::Draft,
            crate::config::AccountGrant::may_compose,
        ) {
            Ok(accounts) => accounts,
            Err(e) => return failure(e),
        };
        let accounts: Vec<_> = accounts.into_iter().filter(|a| a.sends).collect();
        if accounts.is_empty() {
            return failure("no account the user allows this in can send mail".into());
        }
        let named = args
            .account
            .as_deref()
            .is_some_and(|a| !a.trim().is_empty());
        let account_id = named.then(|| accounts[0].id);
        let allowed = accounts.iter().map(|a| a.id).collect();
        let draft = match checked(
            DraftKind::New {
                account_id,
                allowed,
            },
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
        not sent. Extra recipients must be email addresses; the body is plain text.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn reply_to_message(&self, Parameters(args): Parameters<ReplyArgs>) -> CallToolResult {
        let original = match self.original(&args.id, false).await {
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
        sent. Recipients must be email addresses; the body is plain text.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    async fn forward_message(&self, Parameters(args): Parameters<ForwardArgs>) -> CallToolResult {
        let original = match self.original(&args.id, true).await {
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
    /// downloaded first if it has to be, and with `files`, the ones it carries.
    /// An encrypted message is refused: quoting it would put its plaintext
    /// in a message going who knows where.
    async fn original(&self, id: &str, files: bool) -> Result<Original, String> {
        let (account, located) = self.message(id, AssistantCap::Draft)?;
        let body = match self.body(account.id, &located).await {
            Body::Html(body) => body,
            Body::Encrypted => return Err(ENCRYPTED_NOTE.to_string()),
            Body::Unavailable(why) => {
                return Err(format!(
                    "Hylki could not download that message's text to quote: {why}. Try again later."
                ))
            }
        };
        let attachments = if files && located.message.has_attachment {
            self.files_of(account.id, &located).await?
        } else {
            Vec::new()
        };
        Ok(Original {
            message: located.message,
            path: located.path,
            body,
            attachments,
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
            Err(_) => failure("Hylki took too long to open the message; it may still open".into()),
        }
    }
}
