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

/// A conversation on screen, as the cache has it.
struct OnScreen {
    /// "main_window" or "message_window".
    shown_in: &'static str,
    address: String,
    selected_id: String,
    thread_id: Option<String>,
    subject: String,
    /// Its messages, oldest first, each copy once.
    rows: Vec<(String, Message)>,
}

/// Another conversation open beside the one read, to name.
#[derive(Serialize)]
struct AlsoOpenOut {
    shown_in: &'static str,
    selected_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
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

    /// A conversation the app has on screen, looked up in the cache; `None`
    /// when none of it is there (its account gone, its messages moved).
    fn on_screen(&self, shown_in: &'static str, t: crate::mcp::OpenThread) -> Option<OnScreen> {
        let (account_id, path, uid) = t.selected;
        let account = accounts().into_iter().find(|a| a.id == account_id)?;
        let mut rows: Vec<(String, Message)> = {
            let cache = self.lock();
            t.members
                .into_iter()
                .filter(|(a, _, _)| *a == account_id)
                .filter_map(|(_, p, u)| cache.message_at(account_id, &p, u).map(|m| (p, m)))
                .collect()
        };
        // Named before copies are folded away: the one the user is on may
        // be the copy that goes.
        let selected = rows.iter().find(|(p, m)| *p == path && m.uid == uid);
        let thread_id = selected.map(|(p, m)| thread_id(&account.address, p, m));
        let subject = selected
            .or(rows.first())
            .map(|(_, m)| m.subject.clone())
            .unwrap_or_default();
        dedupe(&mut rows);
        if rows.is_empty() {
            return None;
        }
        Some(OnScreen {
            shown_in,
            selected_id: message_id(&account.address, &path, uid),
            address: account.address,
            thread_id,
            subject,
            rows,
        })
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

    #[tool(
        description = "The conversation the user has open in Hylki now: the one in the \
        window they were in last, the main window (the message open in the reader, or \
        selected in the list with the reading pane off) or a message window of its own. \
        Its messages in full, oldest first, replies filed in Sent or other folders included; \
        `selected_id` is the message the user is on, `shown_in` says where. `also_open` names \
        the other conversations open, in the main window or other message windows; read \
        one with read_thread or read_message. Says so when nothing is open. Does not mark \
        mail as read."
    )]
    async fn read_open_thread(
        &self,
        Parameters(args): Parameters<ReadOpenThreadArgs>,
    ) -> CallToolResult {
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
        // The main window's first, then each message window's; the one the
        // user was in last is read, else the main window's, else any.
        let mut all: Vec<OnScreen> = Vec::new();
        let mut read = None;
        all.extend(open.main.and_then(|t| self.on_screen("main_window", t)));
        for (i, t) in open.windows.into_iter().enumerate() {
            if let Some(s) = self.on_screen("message_window", t) {
                if open.focused == Some(i) {
                    read = Some(all.len());
                }
                all.push(s);
            }
        }
        if all.is_empty() {
            return json(&serde_json::json!({
                "open": false,
                "note": "No message is open in Hylki now. Ask the user which one they mean, \
                         or find it with list_messages or search."
            }));
        }
        let shown = all.remove(read.unwrap_or(0));
        let also_open: Vec<AlsoOpenOut> = all
            .into_iter()
            .map(|s| AlsoOpenOut {
                shown_in: s.shown_in,
                selected_id: s.selected_id,
                thread_id: s.thread_id,
                subject: s.subject,
                message_count: s.rows.len(),
            })
            .collect();
        let total = shown.rows.len();
        let offset = args.offset.unwrap_or(0);
        let limit = args.limit.unwrap_or(20).clamp(1, 50);
        let mut messages = Vec::new();
        for (p, m) in shown.rows.into_iter().skip(offset).take(limit) {
            if let Ok(out) = self
                .message_out(&message_id(&shown.address, &p, m.uid))
                .await
            {
                messages.push(out);
            }
        }
        let end = offset.saturating_add(limit).min(total);
        let mut out = serde_json::json!({
            "open": true,
            "shown_in": shown.shown_in,
            "selected_id": shown.selected_id,
            "thread_id": shown.thread_id,
            "message_count": total,
            "messages": messages,
            "next_offset": if end < total { Some(end) } else { None },
        });
        if !also_open.is_empty() {
            out["also_open"] = serde_json::json!(also_open);
        }
        json(&out)
    }
}
