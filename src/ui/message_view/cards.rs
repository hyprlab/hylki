//! Live patches to the cards on the page: selection and attachment rows.

use super::snippets::att_chips_html;
use super::MessageView;

impl MessageView {
    /// Whether the messages on screen are cards: a conversation, or a lone
    /// message when Settings shows single messages as cards (#57). A card
    /// is selected and folded the same way whether or not it has replies.
    pub(super) fn carded(&self) -> bool {
        self.thread.len() > 1 || (self.thread.len() == 1 && self.single_message_card)
    }

    /// Outline the selected cards in the document already on screen. Rendering
    /// it again would reload every frame and lose the reader's place.
    pub(super) fn apply_card_selection(&self) {
        // A full-bleed message is no card to outline.
        if !self.carded() {
            return;
        }
        let keys: Vec<String> = self
            .selected_cards
            .iter()
            .map(|(a, i)| format!("'{a}:{i}'"))
            .collect();
        let js = format!(
            "(function(){{var s=[{}];             var els=document.querySelectorAll('.hylki-msg');             for(var i=0;i<els.length;i++){{var k=els[i].dataset.key;             els[i].classList.toggle('selected', s.indexOf(k)>=0);}}}})()",
            keys.join(",")
        );
        self.run_js(&js);
    }

    /// Fill (or empty) the attachment rows of these cards in the live
    /// document (#213). Files arrive after the paint; reloading the document
    /// for them would resettle every card, so the rows are patched in place.
    pub(super) fn patch_card_atts(&self, keys: &[(u32, u32)]) {
        if !self.webview_ready || self.current.is_none() || keys.is_empty() {
            return;
        }
        let shown = self.chips_shown();
        let mut js = String::from("(function(){");
        for key in keys {
            let html = if shown {
                att_chips_html(
                    *key,
                    self.card_atts.get(key).map(|v| v.as_slice()).unwrap_or(&[]),
                )
            } else {
                String::new()
            };
            js.push_str(&format!(
                "var d=document.querySelector('.hylki-atts[data-key=\"{}:{}\"]');if(d)d.innerHTML={};",
                key.0,
                key.1,
                serde_json::to_string(&html).unwrap_or_else(|_| "''".into()),
            ));
        }
        js.push_str("})()");
        self.run_js_or_warn(&js, "attachment row patch");
    }
}
