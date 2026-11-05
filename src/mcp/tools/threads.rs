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

#[derive(Deserialize, JsonSchema)]
pub(super) struct ReadOpenThreadArgs {
    /// Number of messages (default 20, at most 50).
    limit: Option<usize>,
    /// Number of messages to skip, oldest first (default 0).
    offset: Option<usize>,
}

/// How long the app is given to say what is open: it answers from the
/// main loop, at once unless it is busy.
const OPEN_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// What an assistant is told when the message the user is on is not one it
/// may know of, or there is none: the same, so that says nothing either.
const NOTHING_OPEN: &str = "No message shared with agents is what the user has open now. Ask \
    the user which one they mean, or find it with list_messages or search.";

/// A conversation on screen an assistant may know of, as the cache has it.
struct OnScreen {
    /// "main_window" or "message_window".
    shown_in: &'static str,
    account: Account,
    selected_id: String,
    thread_id: String,
    subject: String,
    /// Its messages in shared folders, oldest first, each copy once.
    rows: Vec<(String, Message)>,
}

/// Another conversation open beside the one read, to name.
#[derive(Serialize)]
struct AlsoOpenOut {
    shown_in: &'static str,
    selected_id: String,
    thread_id: String,
    subject: String,
    message_count: usize,
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

    /// A conversation the app has on screen, as far as it is shared; `None`
    /// when the message the user is on is not, in an account or a folder
    /// that is out of reach, or when the cache has lost it.
    fn on_screen(
        &self,
        shown_in: &'static str,
        t: crate::mcp::OpenThread,
        accounts: &[Account],
    ) -> Option<OnScreen> {
        let (account_id, path, uid) = t.selected;
        let account = accounts.iter().find(|a| a.id == account_id)?.clone();
        let mut rows: Vec<(String, Message)> = {
            let cache = self.lock();
            let shared = shared_paths(&cache, &account);
            if !shared.contains(&path) {
                return None;
            }
            t.members
                .into_iter()
                .filter(|(a, p, _)| *a == account_id && shared.contains(p))
                .filter_map(|(_, p, u)| cache.message_at(account_id, &p, u).map(|m| (p, m)))
                .collect()
        };
        // Named before copies are folded away: the one the user is on may
        // be the copy that goes.
        let (thread_id, subject) = rows
            .iter()
            .find(|(p, m)| *p == path && m.uid == uid)
            .map(|(p, m)| (thread_id(&account.address, p, m), m.subject.clone()))?;
        dedupe(&mut rows);
        Some(OnScreen {
            shown_in,
            selected_id: message_id(&account.address, &path, uid),
            account,
            thread_id,
            subject,
            rows,
        })
    }

    /// The messages from `offset`, as many as `limit` and as fit in one answer.
    async fn gather(
        &self,
        account: &Account,
        rows: Vec<(String, Message)>,
        offset: usize,
        limit: usize,
    ) -> Vec<MessageOut> {
        let started = std::time::Instant::now();
        let mut messages = Vec::new();
        let mut size = 0;
        for (path, m) in rows.into_iter().skip(offset).take(limit) {
            let out = self
                .message_out(
                    account,
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
        messages
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
        let messages = self.gather(&account, rows, offset, limit).await;
        let end = offset.saturating_add(messages.len());
        json(&serde_json::json!({
            "thread_id": args.thread_id,
            "cached_message_count": total,
            "messages": messages,
            "next_offset": if end < total { Some(end) } else { None },
            "scope_note": "Cached members in shared folders only; lookup capped at 100 folder copies."
        }))
    }

    #[tool(
        description = "The conversation the user has open in Hylki now: the one in the \
        window they were in last, the main window (the message open in the reader, or \
        selected in the list with the reading pane off) or a message window of its own. \
        Its messages in full, oldest first, as far as they are shared; `selected_id` is the \
        message the user is on, `shown_in` says where. `also_open` names other shared \
        conversations open; read one with read_thread or read_message. Says so when what \
        the user is on is not shared, or nothing is open. Does not mark mail as read.",
        annotations(read_only_hint = true)
    )]
    async fn read_open_thread(
        &self,
        Parameters(args): Parameters<ReadOpenThreadArgs>,
    ) -> CallToolResult {
        let accounts = match self.scope(None, AssistantCap::OpenConversation) {
            Ok(accounts) => accounts,
            Err(e) => return failure(e),
        };
        let (done, answer) = tokio::sync::oneshot::channel();
        if self
            .app
            .send(AppMsg::Assistant(AppCall::ReadOpen { done }))
            .is_err()
        {
            return failure("Hylki is closing".into());
        }
        let open = match tokio::time::timeout(OPEN_WAIT, answer).await {
            Ok(Ok(open)) => open,
            _ => return failure("Hylki did not answer".into()),
        };
        // What the user is on, by where they were last: if that is out of
        // reach nothing else stands in for it, or "this message" would
        // answer about another one.
        let main_open = open.main.is_some();
        let front = open
            .focused
            .or_else(|| (!main_open && !open.windows.is_empty()).then_some(0));
        let mut main = open
            .main
            .and_then(|t| self.on_screen("main_window", t, &accounts));
        let mut windows: Vec<Option<OnScreen>> = open
            .windows
            .into_iter()
            .map(|t| self.on_screen("message_window", t, &accounts))
            .collect();
        let shown = match front {
            Some(i) => windows.get_mut(i).and_then(Option::take),
            None => main.take(),
        };
        let also_open: Vec<AlsoOpenOut> = main
            .into_iter()
            .chain(windows.into_iter().flatten())
            .map(|s| AlsoOpenOut {
                shown_in: s.shown_in,
                selected_id: s.selected_id,
                thread_id: s.thread_id,
                subject: s.subject,
                message_count: s.rows.len(),
            })
            .collect();
        let mut out = match shown {
            None => serde_json::json!({ "open": false, "note": NOTHING_OPEN }),
            Some(shown) => {
                let total = shown.rows.len();
                let offset = args.offset.unwrap_or(0);
                let limit = args.limit.unwrap_or(20).clamp(1, 50);
                let messages = self.gather(&shown.account, shown.rows, offset, limit).await;
                let end = offset.saturating_add(messages.len());
                serde_json::json!({
                    "open": true,
                    "shown_in": shown.shown_in,
                    "selected_id": shown.selected_id,
                    "thread_id": shown.thread_id,
                    "message_count": total,
                    "messages": messages,
                    "next_offset": if end < total { Some(end) } else { None },
                })
            }
        };
        if !also_open.is_empty() {
            out["also_open"] = serde_json::json!(also_open);
        }
        json(&out)
    }
}
