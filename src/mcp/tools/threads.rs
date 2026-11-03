//! Conversation reads share the reader's References-based cache lookup.
use super::*;

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
        description = "Read a conversation by thread_id, across the account's cached folders. Returns deduplicated messages oldest first. Does not mark mail as read. Only cached thread members are discoverable; bodies may be downloaded. Lookup is capped at 100 folder copies, so this may not be the entire server conversation."
    )]
    async fn read_thread(&self, Parameters(args): Parameters<ReadThreadArgs>) -> CallToolResult {
        let Ok((address, root, singleton)) =
            serde_json::from_str::<(String, String, String)>(&args.thread_id)
        else {
            return failure("Invalid thread_id; use one returned by a message tool.".into());
        };
        let account = match accounts_in_scope(Some(&address)) {
            Ok(mut accounts) if accounts.len() == 1 => accounts.remove(0),
            _ => return failure("The thread's account is unavailable.".into()),
        };
        let mut rows = if root.is_empty() {
            match self.message(&singleton) {
                Ok((a, m)) if a.id == account.id => vec![(m.path, m.message)],
                _ => return failure("The thread's message is no longer cached.".into()),
            }
        } else {
            self.lock().messages_by_thread_ids(account.id, &[root])
        };
        dedupe(&mut rows);
        if rows.is_empty() {
            return failure("No cached messages found for this thread.".into());
        }
        let total = rows.len();
        let offset = args.offset.unwrap_or(0);
        let limit = args.limit.unwrap_or(20).clamp(1, 50);
        let mut messages = Vec::new();
        for (path, m) in rows.into_iter().skip(offset).take(limit) {
            let result = self
                .read_message(Parameters(ReadMessageArgs {
                    id: message_id(&account.address, &path, m.uid),
                }))
                .await;
            messages.push(result);
        }
        let end = offset.saturating_add(messages.len());
        json(&serde_json::json!({
            "thread_id": args.thread_id,
            "cached_message_count": total,
            "messages": messages,
            "next_offset": if end < total { Some(end) } else { None },
            "scope_note": "Cached members only; lookup capped at 100 folder copies."
        }))
    }
}
