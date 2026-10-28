//! Folded cards (#326): which cards open folded, and their bars.

use super::html::attr_escape;
use super::snippets::{fold_bar_date, inline_icon_svg, FOLD_BAR_FACE};
use super::MessageView;
use crate::encoding::escape_html;
use crate::i18n::i18n;
use crate::models::Message;

impl MessageView {
    /// The cards of the conversation on screen that show folded (#326):
    /// those folded by hand, and the ones Settings folds: the read
    /// messages, or every message, but the newest either way. Opening a conversation makes its first message the
    /// current one, so "the one opened" would have kept the oldest open; a
    /// message stepped to inside the conversation is opened by hand instead
    /// (see `Show`).
    pub(super) fn folded_cards(&self) -> std::collections::HashSet<(u32, u32)> {
        if self.thread.len() <= 1 {
            return Default::default();
        }
        let newest = self
            .thread
            .iter()
            .max_by_key(|m| (m.timestamp, m.id))
            .map(|m| (m.account_id, m.id));
        self.thread
            .iter()
            .map(|m| (m.account_id, m.id, m.unread))
            .filter(|&(aid, id, unread)| match self.folds.get(&(aid, id)) {
                Some(folded) => *folded,
                None => {
                    Some((aid, id)) != newest
                        && match self.fold_messages {
                            crate::config::FoldMessages::Never => false,
                            crate::config::FoldMessages::Read => !unread,
                            crate::config::FoldMessages::AllButNewest => true,
                        }
                }
            })
            .map(|(aid, id, _)| (aid, id))
            .collect()
    }

    /// A folded card's one line, as Proton Mail draws it: who it is from,
    /// then whether it is starred, whether it was sent or received, whether
    /// it carries files, and the day. A click on it opens the card.
    pub(super) fn fold_bar_html(&self, m: &Message) -> String {
        let key = (m.account_id, m.id);
        let from = crate::contacts::shown_name_or_addr(&m.from_name, &m.from_addr);
        let icon = |name: &str, title: &str| {
            format!(
                "<span class=\"hylki-fbar-ico\" title=\"{}\">{}</span>",
                attr_escape(title),
                inline_icon_svg(name)
            )
        };
        let star = if m.starred {
            icon("starred-symbolic", &i18n("Starred"))
        } else {
            icon("hylki-non-starred-symbolic", "")
        };
        let place = if self.sent.contains(&key) {
            icon("mail-send-symbolic", &i18n("Sent"))
        } else {
            icon("mail-inbox-symbolic", &i18n("Received"))
        };
        // An empty slot when there are no files, so the icons of every bar
        // stand in the same columns.
        let clip = if m.has_attachment {
            icon("mail-attachment-symbolic", &i18n("Has attachments"))
        } else {
            "<span class=\"hylki-fbar-ico hylki-fbar-none\"></span>".to_string()
        };
        // Below the sender, as many lines of preview as the message list
        // shows (#326). No subject: the conversation's is the heading above.
        let preview = crate::models::preview_display(&m.preview);
        let preview = if self.fold_preview_lines > 0 && !preview.trim().is_empty() {
            format!(
                "<div class=\"hylki-fbar-prev\" style=\"-webkit-line-clamp:{}\">{}</div>",
                self.fold_preview_lines,
                escape_html(preview.trim()),
            )
        } else {
            String::new()
        };
        format!(
            "<div class=\"hylki-fbar\" data-key=\"{}:{}\" title=\"{}\">\
             {FOLD_BAR_FACE}<div class=\"hylki-fbar-main\"><div class=\"hylki-fbar-top\">\
             <span class=\"hylki-fbar-from\">{}</span>{star}{place}{clip}\
             <span class=\"hylki-fbar-date\">{}</span></div>{preview}</div></div>",
            key.0,
            key.1,
            attr_escape(&i18n("Click to open this message")),
            escape_html(&from),
            escape_html(&fold_bar_date(m)),
        )
    }

    /// Fold and open the cards of the live document to match
    /// [`MessageView::folded_cards`], without building it again.
    pub(super) fn patch_folds(&self) {
        if !self.webview_ready || self.thread.len() <= 1 {
            return;
        }
        let keys: Vec<String> = self
            .folded_cards()
            .iter()
            .map(|(a, i)| format!("{a}:{i}"))
            .collect();
        let js = format!(
            "(function(k){{var ms=document.querySelectorAll('.hylki-msg');\
             for(var i=0;i<ms.length;i++){{var on=k.indexOf(ms[i].dataset.key)>=0;\
             if(ms[i].classList.contains('hylki-folded')===on)continue;\
             ms[i].classList.toggle('hylki-folded',on);\
             if(!on){{var f=ms[i].querySelector('iframe.hylki-frame');if(f){{f._h=0;s(f);}}}}}}}})({})",
            serde_json::to_string(&keys).unwrap_or_else(|_| "[]".into()),
        );
        self.run_js(&js);
    }
}
