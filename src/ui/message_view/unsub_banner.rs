//! A card's Unsubscribe banner: what it says, and patching it in place.

use super::html::attr_escape;
use super::types::UnsubState;
use super::MessageView;
use crate::encoding::escape_html;
use crate::i18n::{i18n, i18n_f};
use crate::models::Message;

/// How long a list has to act on an unsubscribe request before its mail
/// stops counting as already on its way: the two days Gmail and Yahoo
/// require of bulk senders.
const UNSUB_GRACE_SECS: i64 = 2 * 24 * 60 * 60;

impl MessageView {
    /// What a card's Unsubscribe banner holds right now: nothing for a
    /// message from no list; otherwise a line and a button that follow the
    /// request — resting, under way, failed — and say when the list was
    /// already left.
    pub(super) fn unsub_inner_html(&self, m: &Message) -> String {
        let key = (m.account_id, m.id);
        let Some(info) = self
            .member_checks
            .get(&key)
            .and_then(|c| c.unsubscribe.as_ref())
        else {
            return String::new();
        };
        let button = |label: &str, title: &str| {
            format!(
                "<button type=\"button\" class=\"hylki-unsub-btn\" data-key=\"{}:{}\" title=\"{}\">{}</button>",
                key.0,
                key.1,
                attr_escape(title),
                escape_html(label),
            )
        };
        let (text, btn) = match self.unsub_state.get(&key) {
            Some(UnsubState::Working) => (i18n("Unsubscribing…"), String::new()),
            Some(UnsubState::Failed(why)) => (
                i18n_f("Could not unsubscribe: {why}", &[("why", why)]),
                button(&i18n("Try Again"), ""),
            ),
            None => {
                let text = match self.unsubscribed.get(&info.key(&m.from_addr)) {
                    // Mail sent well after the request means the list did not
                    // act on it, or that the List-Id covers more than the one
                    // list that was left (#284); the banner must not say
                    // "unsubscribed" over the proof that it was not.
                    Some(at) if m.timestamp > at + UNSUB_GRACE_SECS => i18n_f(
                        "You unsubscribed on {date}, but this list is still sending mail.",
                        &[("date", &crate::datefmt::day_month_year(*at))],
                    ),
                    Some(at) => i18n_f(
                        "You unsubscribed from this list on {date}.",
                        &[("date", &crate::datefmt::day_month_year(*at))],
                    ),
                    // A message that said so in its headers is from a list;
                    // one whose footer link gave it away only has a link.
                    None if info.in_headers => i18n("This message is from a mailing list."),
                    None => i18n("This message has an unsubscribe link."),
                };
                let btn = if info.one_click.is_some() {
                    button(
                        &i18n("Unsubscribe"),
                        &i18n("Asks the list to stop sending you mail"),
                    )
                } else if info.direct() {
                    button(
                        &i18n("Unsubscribe"),
                        &i18n("Sends an unsubscribe request by email"),
                    )
                } else {
                    // Nothing but a web page on offer: the button says so.
                    button(
                        &i18n("Unsubscribe…"),
                        &i18n("Opens the list’s unsubscribe page in your browser"),
                    )
                };
                (text, btn)
            }
        };
        format!(
            "<span class=\"hylki-unsub-text\">{}</span>{btn}",
            escape_html(&text)
        )
    }

    /// Redraw one card's Unsubscribe banner in the live document.
    pub(super) fn patch_unsub(&self, account_id: u32, id: u32) {
        if !self.webview_ready || self.current.is_none() {
            return;
        }
        let Some(m) = self
            .thread
            .iter()
            .find(|m| m.account_id == account_id && m.id == id)
        else {
            return;
        };
        let html = self.unsub_inner_html(m);
        let js = format!(
            "(function(){{var d=document.querySelector('.hylki-unsub[data-key=\"{account_id}:{id}\"]');\
             if(d)d.innerHTML={};}})()",
            serde_json::to_string(&html).unwrap_or_else(|_| "''".into()),
        );
        self.run_js(&js);
    }
}
