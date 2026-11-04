//! A record of what assistants did, for Settings → AI Assistants: the only
//! way to learn what one read, since reading leaves no mark on the mail.
//! It stays in memory and in the app: nothing here is written to a log file.

use serde_json::{Map, Value};

/// One tool call.
#[derive(Clone, Debug)]
pub(crate) struct Activity {
    /// The tool, by its name in `tools`.
    pub(crate) tool: String,
    /// What it was aimed at: a subject, a search, recipients.
    pub(crate) detail: String,
    /// The assistant's program, by the name it gives itself: a label for the
    /// user, not proof of who is asking.
    pub(crate) client: String,
    /// It did what was asked; otherwise it was refused or failed.
    pub(crate) ok: bool,
    /// Seconds since the epoch.
    pub(crate) at: i64,
}

/// The longest a detail is kept to.
const DETAIL_MAX: usize = 120;

/// What a call was aimed at, from its arguments. `subject` names the message
/// behind an id, when there is such a message.
pub(super) fn detail_of(
    arguments: Option<&Map<String, Value>>,
    subject: impl Fn(&str) -> Option<String>,
) -> String {
    let Some(args) = arguments else {
        return String::new();
    };
    let text = |key: &str| args.get(key).and_then(Value::as_str).map(str::trim);
    let named = |id: &str| subject(id).filter(|s| !s.is_empty());
    let found = if let Some(query) = text("query").filter(|q| !q.is_empty()) {
        Some(query.to_string())
    } else if let Some(ids) = args.get("ids").and_then(Value::as_array) {
        let first = ids.first().and_then(Value::as_str).and_then(named);
        match (ids.len(), first) {
            (1, Some(first)) => Some(first),
            (n, Some(first)) => Some(format!("{first} (+{})", n - 1)),
            (n, None) => Some(n.to_string()),
        }
    } else if let Some(id) = text("id") {
        named(id)
    } else {
        text("folder").filter(|f| !f.is_empty()).map(str::to_string)
    };
    let recipients: Vec<&str> = ["to", "add_to"]
        .iter()
        .filter_map(|key| args.get(*key).and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let whole = match (found, recipients.is_empty()) {
        (Some(found), false) => format!("{found} → {}", recipients.join(", ")),
        (Some(found), true) => found,
        (None, false) => recipients.join(", "),
        (None, true) => text("subject").unwrap_or_default().to_string(),
    };
    shorten(&whole)
}

fn shorten(text: &str) -> String {
    if text.chars().count() <= DETAIL_MAX {
        return text.to_string();
    }
    let mut short: String = text.chars().take(DETAIL_MAX).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn detail(args: Value) -> String {
        detail_of(args.as_object(), |id| {
            (id == "known").then(|| "Lunch plans".to_string())
        })
    }

    #[test]
    fn a_call_is_described_by_what_it_was_aimed_at() {
        assert_eq!(detail(json!({"query": " invoice "})), "invoice");
        assert_eq!(detail(json!({"id": "known"})), "Lunch plans");
        assert_eq!(detail(json!({"id": "gone"})), "");
        assert_eq!(detail(json!({"ids": ["known"]})), "Lunch plans");
        assert_eq!(
            detail(json!({"ids": ["known", "b", "c"]})),
            "Lunch plans (+2)"
        );
        assert_eq!(detail(json!({"ids": ["a", "b"]})), "2");
        assert_eq!(detail(json!({"folder": "inbox"})), "inbox");
        assert_eq!(
            detail(json!({"id": "known", "to": ["a@example.org", "b@example.org"]})),
            "Lunch plans → a@example.org, b@example.org"
        );
        assert_eq!(detail(json!({"subject": "Hello"})), "Hello");
        assert_eq!(detail(json!({})), "");
    }

    #[test]
    fn a_long_detail_is_cut_between_characters() {
        let long = "ä".repeat(DETAIL_MAX + 5);
        let cut = detail(json!({ "query": long }));
        assert_eq!(cut.chars().count(), DETAIL_MAX + 1);
        assert!(cut.ends_with('…'));
    }
}
