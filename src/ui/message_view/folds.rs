//! Folded cards (#326): which cards open folded, and their bars.

use super::html::attr_escape;
use super::snippets::inline_icon_svg;
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
        if !self.carded() {
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

    /// The marks in a card's header, beside the date, as a folded card
    /// showed them once on a bar of its own (#326): whether it is starred and
    /// whether it carries files. The star is the toolbar's, in its color,
    /// and only a starred card shows it: the card's `starred` class does, so
    /// a star toggled on the card shows or goes in place. Mail you sent says
    /// so in a chip instead (see `card_labels`).
    pub(super) fn card_marks_html(&self, m: &Message) -> String {
        let icon = |class: &str, name: &str, title: &str| {
            format!(
                "<span class=\"hylki-mark{class}\" title=\"{}\">{}</span>",
                attr_escape(title),
                inline_icon_svg(name)
            )
        };
        // An empty slot when there are no files, so the marks of every card
        // stand in the same columns.
        let clip = if m.has_attachment {
            icon("", "mail-attachment-symbolic", &i18n("Has attachments"))
        } else {
            "<span class=\"hylki-mark hylki-mark-none\"></span>".to_string()
        };
        format!(
            "<span class=\"hylki-marks\">{}{clip}</span>",
            icon(
                " hylki-mark-star",
                "hylki-non-starred-symbolic",
                &i18n("Starred")
            ),
        )
    }

    /// The chip beside each card's sender: the folder a message was read
    /// from when it isn't the one on screen, and "Sent" on a card of mail you
    /// sent, which takes the place of its folder's name. Received mail is
    /// the rule, and carries no chip for it.
    pub(super) fn card_labels(&self) -> std::collections::HashMap<(u32, u32), String> {
        let mut labels = self.folder_labels.clone();
        if self.carded() {
            for key in &self.sent {
                labels.insert(*key, i18n("Sent"));
            }
        }
        labels
    }

    /// What a folded card shows under its header, which stays exactly as it
    /// is open: as many lines of preview as the message list shows (#326),
    /// and nothing when the list shows none. The box keeps all its lines
    /// when the text is shorter or missing, so every folded card stands the
    /// same height. A click on it opens the card.
    pub(super) fn fold_bar_html(&self, m: &Message) -> String {
        if self.fold_preview_lines == 0 {
            return String::new();
        }
        let preview = crate::models::preview_display(&m.preview);
        format!(
            "<div class=\"hylki-fbar\" data-key=\"{}:{}\" title=\"{}\" \
             style=\"-webkit-line-clamp:{n};height:calc({n} * 1.35em)\">{}</div>",
            m.account_id,
            m.id,
            attr_escape(&i18n("Click to open this message")),
            escape_html(preview.trim()),
            n = self.fold_preview_lines,
        )
    }

    /// Fold and open the cards of the live document to match
    /// [`MessageView::folded_cards`], without building it again.
    pub(super) fn patch_folds(&self) {
        if !self.webview_ready || !self.carded() {
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
