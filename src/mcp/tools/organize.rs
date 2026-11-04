//! The tools that change mail: read state, stars, tags, moving, archiving and
//! Trash, each its own permission, and all of them undoable in the app. There
//! is no tool that erases mail for good.

use std::collections::HashMap;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{failure, json, resolve_folder, AppCall, AppMsg, Mail, Target as Folder};
use crate::config::AssistantCap;
use crate::mcp::organize::{Change, Target};

/// The most messages one call changes: enough for a real tidying, and one
/// assistant gone wrong has a limit on the mess it makes at a stroke.
const IDS_MAX: usize = 100;
/// How long the app is given to answer: it only has to start the change.
const ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Deserialize, JsonSchema)]
pub(super) struct SetReadArgs {
    /// The `id`s of messages, at most 100.
    ids: Vec<String>,
    /// True marks them read, false unread.
    read: bool,
}

#[derive(Deserialize, JsonSchema)]
pub(super) struct SetStarredArgs {
    /// The `id`s of messages, at most 100.
    ids: Vec<String>,
    /// True adds the star, false takes it off.
    starred: bool,
}

#[derive(Deserialize, JsonSchema)]
pub(super) struct SetTagArgs {
    /// The `id`s of messages, at most 100.
    ids: Vec<String>,
    /// A tag's name, from list_tags.
    tag: String,
    /// True adds the tag, false takes it off.
    add: bool,
}

#[derive(Deserialize, JsonSchema)]
pub(super) struct MoveArgs {
    /// The `id`s of messages, at most 100.
    ids: Vec<String>,
    /// A folder role or a path from list_folders, in each message's own account.
    folder: String,
}

#[derive(Deserialize, JsonSchema)]
pub(super) struct IdsArgs {
    /// The `id`s of messages, at most 100.
    ids: Vec<String>,
}

#[derive(Serialize)]
struct TagOut {
    name: String,
}

#[derive(Serialize)]
struct SkippedOut {
    id: String,
    why: String,
}

#[derive(Serialize)]
struct ChangedOut {
    changed: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    skipped: Vec<SkippedOut>,
    note: &'static str,
}

/// What every answer reminds the assistant of. A message that moved has a
/// new id, which only listing it again gives.
const NOTE: &str = "The user can undo this in Hylki. A message that moved has a new id: list or \
    search again to find it.";

#[tool_router(router = organize_tools, vis = "pub(super)")]
impl Mail {
    #[tool(
        description = "Mark messages read or unread. The user can undo it in Hylki.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn set_read(&self, Parameters(args): Parameters<SetReadArgs>) -> CallToolResult {
        self.apply(&args.ids, AssistantCap::MarkRead, Change::Read(args.read))
            .await
    }

    #[tool(
        description = "Add or remove the star on messages. The user can undo it in Hylki.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn set_starred(&self, Parameters(args): Parameters<SetStarredArgs>) -> CallToolResult {
        self.apply(&args.ids, AssistantCap::Star, Change::Star(args.starred))
            .await
    }

    #[tool(
        description = "The tags the user has made, for set_tag.",
        annotations(read_only_hint = true)
    )]
    async fn list_tags(&self) -> CallToolResult {
        if let Err(e) = self.scope(None, AssistantCap::Tag) {
            return failure(e);
        }
        let out: Vec<TagOut> = crate::config::load_tags()
            .into_iter()
            .map(|t| TagOut { name: t.name })
            .collect();
        json(&out)
    }

    #[tool(
        description = "Add a tag to messages or take it off. Only tags the user already has, \
        from list_tags. The user can undo it in Hylki.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn set_tag(&self, Parameters(args): Parameters<SetTagArgs>) -> CallToolResult {
        let tags = crate::config::load_tags();
        let wanted = args.tag.trim();
        let Some(tag) = tags.iter().find(|t| {
            t.name.eq_ignore_ascii_case(wanted) || t.keyword.eq_ignore_ascii_case(wanted)
        }) else {
            let names: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
            return failure(format!(
                "no tag \"{wanted}\"; the user has: {}",
                names.join(", ")
            ));
        };
        let change = Change::Tag {
            keyword: tag.keyword.clone(),
            add: args.add,
        };
        self.apply(&args.ids, AssistantCap::Tag, change).await
    }

    #[tool(
        description = "Move messages to a folder of their own account. The user can undo it in \
        Hylki. Messages already in that folder stay where they are.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn move_messages(&self, Parameters(args): Parameters<MoveArgs>) -> CallToolResult {
        // Each account's own folder of that name, among those it shares. Mail
        // does not change accounts here.
        let accounts = match self.scope(None, AssistantCap::Move) {
            Ok(accounts) => accounts,
            Err(e) => return failure(e),
        };
        let mut folders = HashMap::new();
        {
            let cache = self.lock();
            for account in &accounts {
                if let Some(Folder::Path(path)) = resolve_folder(&cache, account, &args.folder) {
                    folders.insert(account.id, path);
                }
            }
        }
        if folders.is_empty() {
            return failure(format!(
                "no folder \"{}\" is shared in an account allowing moves",
                args.folder
            ));
        }
        self.apply(&args.ids, AssistantCap::Move, Change::MoveTo(folders))
            .await
    }

    #[tool(
        description = "Archive messages, as Hylki's Archive button does. The user can undo it \
        in Hylki.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn archive_messages(&self, Parameters(args): Parameters<IdsArgs>) -> CallToolResult {
        self.apply(&args.ids, AssistantCap::Move, Change::Archive)
            .await
    }

    #[tool(
        description = "Move messages to Trash. The user can undo it in Hylki. Mail already in \
        Trash stays there: only the user can erase it for good.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn trash_messages(&self, Parameters(args): Parameters<IdsArgs>) -> CallToolResult {
        self.apply(&args.ids, AssistantCap::Trash, Change::Trash)
            .await
    }
}

impl Mail {
    /// Check each message against `cap` and hand those allowed to the app:
    /// one refused does not stop the others, and the answer says which.
    async fn apply(&self, ids: &[String], cap: AssistantCap, change: Change) -> CallToolResult {
        if ids.is_empty() {
            return failure("no messages named".into());
        }
        if ids.len() > IDS_MAX {
            return failure(format!(
                "{} messages are more than one call changes ({IDS_MAX}); send them in batches",
                ids.len()
            ));
        }
        let mut targets = Vec::new();
        let mut skipped = Vec::new();
        for id in ids {
            match self.message(id, cap) {
                Ok((_, located)) => targets.push(Target {
                    id: id.clone(),
                    path: located.path,
                    message: located.message,
                }),
                Err(why) => skipped.push(SkippedOut {
                    id: id.clone(),
                    why,
                }),
            }
        }
        if targets.is_empty() {
            return json(&ChangedOut {
                changed: Vec::new(),
                skipped,
                note: NOTE,
            });
        }
        let (done, answer) = tokio::sync::oneshot::channel();
        if self
            .app
            .send(AppMsg::Assistant(AppCall::Organize {
                change,
                targets,
                done,
            }))
            .is_err()
        {
            return failure("Hylki is closing".into());
        }
        match tokio::time::timeout(ANSWER_WAIT, answer).await {
            Ok(Ok(done)) => {
                skipped.extend(
                    done.skipped
                        .into_iter()
                        .map(|(id, why)| SkippedOut { id, why }),
                );
                json(&ChangedOut {
                    changed: done.changed,
                    skipped,
                    note: NOTE,
                })
            }
            Ok(Err(_)) => failure("Hylki could not make the change".into()),
            Err(_) => failure("Hylki took too long to answer; check before trying again".into()),
        }
    }
}
