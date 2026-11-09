//! Putting a saved template into a message that is already being written.

use super::Compose;
use crate::ui::rich_editor::{self, SourceKind};

impl Compose {
    /// Insert a template's body at the caret. If the picker took the caret
    /// away, put it above the quoted original and the composer's signature,
    /// where a reply's words belong.
    pub(super) fn insert_template(&mut self, html: String) {
        if html.trim().is_empty() {
            return;
        }
        let html = crate::mail::quote::editable_copy_html(
            &crate::mail::attachment_reminder::without_signature(&html),
        );
        if let Some(kind) = self.editor.source_kind() {
            let text = match kind {
                SourceKind::Markdown => crate::mail::markdown::from_html(&html),
                SourceKind::Html => html,
            };
            self.editor
                .insert_at_caret(&format!("\n\n{}\n\n", text.trim()));
            return;
        }

        let js = r#"(function(){var b=document.body;
var r=null,sel=window.getSelection();
if(sel&&sel.rangeCount){var s=sel.getRangeAt(0);if(b.contains(s.startContainer)){r=s.cloneRange();r.collapse(false);}}
if(!r){var a=null,all=[].filter.call(b.children,function(n){return n.matches&&n.matches('.hylki-quote-attr,blockquote,.hylki-sig');});
if(all.length)a=all[0];var c=document.createRange();if(a)c.setStartBefore(a);else{c.selectNodeContents(b);c.collapse(false);}r=c;}
if(r){sel.removeAllRanges();sel.addRange(r);b.focus();}
document.execCommand('insertHTML',false,@HTML@);
window.__hylkiPlainFix&&window.__hylkiPlainFix();window.__hylkiDirty=true;})()"#
            .replace("@HTML@", &format!("'{}'", rich_editor::js_escape(&html)));
        self.editor.run_js(&js);
    }
}
