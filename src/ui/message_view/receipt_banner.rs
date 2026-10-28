//! A card's read receipt banner (#520): the request, and the two answers.
//! Also the checkmarks of sent mail that asked for a receipt itself.

use super::html::attr_escape;
use super::snippets::inline_icon_svg;
use super::MessageView;
use crate::encoding::escape_html;
use crate::i18n::{i18n, i18n_f};
use crate::models::{Message, ReceiptMark, ReceiptMarks};

/// The class a card's checkmarks wear: grey while the receipt is awaited,
/// green once it came back, none (hidden) on mail that asked for nothing.
fn read_mark_class(mark: Option<&ReceiptMark>) -> &'static str {
    match mark {
        Some(m) if m.read => "hylki-read read",
        Some(_) => "hylki-read asked",
        None => "hylki-read",
    }
}

/// A card's read receipt checkmarks, beside its date: always emitted, so
/// a mark that arrives after the paint can be patched into it.
pub(super) fn read_mark_html(key: (u32, u32), mark: Option<&ReceiptMark>) -> String {
    format!(
        "<span class=\"{}\" data-key=\"{}:{}\" title=\"{}\">{}</span>",
        read_mark_class(mark),
        key.0,
        key.1,
        attr_escape(mark.map_or("", |m| m.tooltip.as_str())),
        inline_icon_svg("hylki-read-receipt-symbolic"),
    )
}

/// A card's read receipt banner: the container, always emitted so a
/// request read after the paint can be patched into it (empty, it is
/// hidden by the stylesheet). `inner` is what
/// [`MessageView::receipt_inner_html`] rendered.
pub(super) fn receipt_row_html(key: (u32, u32), inner: &str) -> String {
    format!(
        "<div class=\"hylki-mdn\" data-key=\"{}:{}\">{inner}</div>",
        key.0, key.1
    )
}

impl MessageView {
    /// What a card's read receipt banner holds: nothing unless the app left
    /// the request in the card's check, which it does only while the
    /// request is the user's to answer.
    pub(super) fn receipt_inner_html(&self, m: &Message) -> String {
        let key = (m.account_id, m.id);
        let Some(request) = self
            .member_checks
            .get(&key)
            .and_then(|c| c.receipt.as_ref())
        else {
            return String::new();
        };
        let name = if m.from_name.trim().is_empty() {
            m.from_addr.as_str()
        } else {
            m.from_name.as_str()
        };
        // A receipt that would go somewhere other than the sender says
        // where, before anything is sent there.
        let text = if request.to.eq_ignore_ascii_case(&m.from_addr) {
            i18n_f("{name} asked for a read receipt.", &[("name", name)])
        } else {
            i18n_f(
                "{name} asked for a read receipt, to be sent to {addr}.",
                &[("name", name), ("addr", &request.to)],
            )
        };
        let button = |answer: &str, label: &str, title: &str| {
            format!(
                "<button type=\"button\" class=\"hylki-unsub-btn\" data-key=\"{}:{}\" \
                 data-mdn=\"{answer}\" title=\"{}\">{}</button>",
                key.0,
                key.1,
                attr_escape(title),
                escape_html(label),
            )
        };
        format!(
            "<span class=\"hylki-unsub-text\">{}</span>{}{}",
            escape_html(&text),
            button(
                "send",
                &i18n("Send Receipt"),
                &i18n("Tells the sender you opened this message")
            ),
            button(
                "ignore",
                &i18n("Ignore"),
                &i18n("Sends nothing and stops asking")
            ),
        )
    }

    /// Redraw one card's read receipt banner in the live document.
    pub(super) fn patch_receipt(&self, account_id: u32, id: u32) {
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
        let html = self.receipt_inner_html(m);
        let js = format!(
            "(function(){{var d=document.querySelector('.hylki-mdn[data-key=\"{account_id}:{id}\"]');\
             if(d)d.innerHTML={};}})()",
            serde_json::to_string(&html).unwrap_or_else(|_| "''".into()),
        );
        self.run_js(&js);
    }

    /// The receipts asked for and come back changed: patch the cards'
    /// checkmarks.
    pub(super) fn on_set_receipt_marks(&mut self, marks: ReceiptMarks) {
        if self.receipt_marks == marks {
            return;
        }
        self.receipt_marks = marks;
        if !self.webview_ready || self.current.is_none() {
            return;
        }
        let mut js = String::from("(function(){var d;");
        for m in &self.thread {
            let mark = self.receipt_marks.of(m);
            js.push_str(&format!(
                "d=document.querySelector('.hylki-read[data-key=\"{}:{}\"]');\
                 if(d){{d.className={};d.title={};}}",
                m.account_id,
                m.id,
                serde_json::to_string(read_mark_class(mark.as_ref())).unwrap_or_default(),
                serde_json::to_string(mark.as_ref().map_or("", |m| m.tooltip.as_str()))
                    .unwrap_or_else(|_| "''".into()),
            ));
        }
        js.push_str("})()");
        self.run_js(&js);
    }
}
