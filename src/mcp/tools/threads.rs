//! Conversation reads share the reader's References-based cache lookup.
use super::*;

/// The most text one answer carries: a long conversation goes on in the next
/// call rather than filling the assistant's context.
const ANSWER_MAX: usize = 256 * 1024;
/// How long one answer may take to gather: bodies may have to be downloaded,
/// and the client gives up on a tool that stays silent too long.
const ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Deserialize, JsonSchema)]
pub(super) struct ReadThreadArgs {
    /// The thread_id from a message summary or read_message.
    thread_id: String,
    /// Number of messages (default 20, at most 50).
    limit: Option<usize>,
    /// Number of messages to skip, oldest first (default 0).
    offset: Option<usize>,
}

pub(super) fn thread_id(account: &str, path: &str, m: &Message) -> String {
    let root = m
        .references
        .split_whitespace()
        .next()
        .unwrap_or(&m.message_id);
    // A header-less message remains a singleton, identified by its location.
    serde_json::to_string(&(
        account,
        root,
        if root.is_empty() {
            message_id(account, path, m.uid)
        } else {
            String::new()
        },
    ))
    .expect("thread id serializes")
}

impl Mail {
    pub(super) fn thread_members(
        &self,
        account: u32,
        path: &str,
        m: &Message,
    ) -> Vec<(String, Message)> {
        let mut ids: Vec<String> = m.references.split_whitespace().map(str::to_owned).collect();
        ids.push(m.message_id.clone());
        let mut rows = self.lock().messages_by_thread_ids(account, &ids);
        rows.push((path.to_owned(), m.clone()));
        dedupe(&mut rows);
        rows
    }
}

fn dedupe(rows: &mut Vec<(String, Message)>) {
    rows.sort_by(|a, b| (a.1.timestamp, &a.0, a.1.uid).cmp(&(b.1.timestamp, &b.0, b.1.uid)));
    let mut seen = std::collections::HashSet::new();
    rows.retain(|(path, m)| {
        let key = if m.message_id.is_empty() {
            format!("location:{path}|{}", m.uid)
        } else {
            format!("header:{}|{}|{}", m.message_id, m.from_addr, m.timestamp)
        };
        seen.insert(key)
    });
}

#[tool_router(router = thread_tools, vis = "pub(super)")]
impl Mail {
    #[tool(
        description = "Read a conversation by thread_id, across the account's shared folders. Returns deduplicated messages oldest first, as read_message gives them, and next_offset when there are more to fetch. Does not mark mail as read. Only cached thread members are discoverable; bodies may be downloaded. Lookup is capped at 100 folder copies, so this may not be the entire server conversation.",
        annotations(read_only_hint = true)
    )]
    async fn read_thread(&self, Parameters(args): Parameters<ReadThreadArgs>) -> CallToolResult {
        let Ok((address, root, singleton)) =
            serde_json::from_str::<(String, String, String)>(&args.thread_id)
        else {
            return failure("Invalid thread_id; use one returned by a message tool.".into());
        };
        let account = match self.scope(Some(&address), AssistantCap::Read) {
            Ok(mut accounts) if accounts.len() == 1 => accounts.remove(0),
            Ok(_) => return failure("The thread's account is unavailable.".into()),
            Err(e) => return failure(e),
        };
        let mut rows = if root.is_empty() {
            match self.message(&singleton, AssistantCap::Read) {
                Ok((a, m)) if a.id == account.id => vec![(m.path, m.message)],
                Ok(_) => return failure("The thread's message is no longer cached.".into()),
                Err(e) => return failure(e),
            }
        } else {
            self.lock().messages_by_thread_ids(account.id, &[root])
        };
        dedupe(&mut rows);
        let shared = shared_paths(&self.lock(), &account);
        rows.retain(|(path, _)| shared.contains(path));
        if rows.is_empty() {
            return failure("No cached messages found for this thread.".into());
        }
        let total = rows.len();
        let offset = args.offset.unwrap_or(0);
        let limit = args.limit.unwrap_or(20).clamp(1, 50);
        let started = std::time::Instant::now();
        let mut messages = Vec::new();
        let mut size = 0;
        for (path, m) in rows.into_iter().skip(offset).take(limit) {
            let out = self
                .message_out(
                    &account,
                    &Located {
                        path,
                        uid: m.uid,
                        message: m,
                    },
                )
                .await;
            size += out.weight();
            messages.push(out);
            if size >= ANSWER_MAX || started.elapsed() >= ANSWER_WAIT {
                break;
            }
        }
        let end = offset.saturating_add(messages.len());
        json(&serde_json::json!({
            "thread_id": args.thread_id,
            "cached_message_count": total,
            "messages": messages,
            "next_offset": if end < total { Some(end) } else { None },
            "scope_note": "Cached members in shared folders only; lookup capped at 100 folder copies."
        }))
    }
}
