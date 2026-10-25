//! Signatures: which one, where it goes, and swapping one for another in rich
//! and source bodies.

use super::messages::ComposeInput;
use super::types::{ComposeAccount, SigPlace};
use super::Compose;
use super::ComposeWidgets;
use crate::config::SignaturePosition;
use crate::i18n::i18n;
use crate::ui::context_menu::{show_context_menu, MenuEntry};
use crate::ui::rich_editor::{self, js_escape, SourceKind};
use adw::prelude::*;
use relm4::prelude::*;

/// A signature block as HTML, under a `-- ` line when `dashes` is set. The
/// stored signature is HTML (legacy plain-text signatures are converted).
pub(super) fn sig_html(sig: &str, dashes: bool) -> String {
    let body = rich_editor::signature_to_html(sig);
    let dashes = if dashes { "-- <br>" } else { "" };
    format!("<div class=\"hylki-sig\"><br>{dashes}{body}</div>")
}

/// The signature as it reads in a source-mode body: Markdown (under its
/// `-- ` line when `dashes` is set), or the same HTML block the rich
/// editor holds.
pub(super) fn sig_source(kind: SourceKind, sig: &str, dashes: bool) -> String {
    if sig.is_empty() {
        return String::new();
    }
    match kind {
        SourceKind::Html => format!("\n{}\n", sig_html(sig, dashes)),
        SourceKind::Markdown => format!(
            "\n\n{}{}\n",
            if dashes { "-- \n" } else { "" },
            crate::mail::markdown::from_html(&rich_editor::signature_to_html(sig))
        ),
    }
}

impl Compose {
    /// The signature Insert Signature puts in: the one the message last
    /// had, if it is one of `account`'s, else its default, else its first.
    fn chosen_signature(&self, account: &ComposeAccount) -> String {
        if account
            .signatures
            .iter()
            .any(|s| s.html == self.current_sig)
        {
            return self.current_sig.clone();
        }
        if !account.signature.is_empty() {
            return account.signature.clone();
        }
        account
            .signatures
            .first()
            .map(|s| s.html.clone())
            .unwrap_or_default()
    }

    /// Put `new_sig` in the editor in place of the current signature block,
    /// or where the setting puts a signature when there is none.
    pub(super) fn put_signature(&mut self, new_sig: String) {
        // Source mode holds the signature as text, so the swap is a
        // text replacement in the field rather than a DOM one.
        if let Some(kind) = self.editor.source_kind() {
            // The old block is looked for with and without its
            // `-- ` line: a draft keeps the form it was written in,
            // whatever the setting says now.
            let old = sig_source(kind, &self.current_sig, self.sig_dashes);
            let old_other = sig_source(kind, &self.current_sig, !self.sig_dashes);
            let new = sig_source(kind, &new_sig, self.sig_dashes);
            // With no old block to replace (the previous account had
            // none), the new one goes where the setting puts it: at
            // the end, or above the quoted original (#237), which
            // in source is found by its text. HTML keeps the quote's
            // tags; Markdown has the `> ` lines, with the attribution
            // line or the forward header above.
            let place = self.source_sig_place(kind);
            self.editor.run_js(&format!(
                "(function(){{var t=document.getElementById('src');if(!t)return;\
                 var o='{}',p='{}',n='{}';var v=t.value;\
                 var i=o?v.lastIndexOf(o):-1;\
                 if(i<0&&p){{i=v.lastIndexOf(p);if(i>=0)o=p;}}\
                 if(i>=0){{v=v.slice(0,i)+n+v.slice(i+o.length);}}else{{{place}}}\
                 t.value=v;window.__hylkiDirty=true;}})()",
                js_escape(&old),
                js_escape(&old_other),
                js_escape(&new)
            ));
            self.current_sig = new_sig;
            return;
        }
        {
            let replacement = if new_sig.is_empty() {
                String::new()
            } else {
                sig_html(&new_sig, self.sig_dashes)
            };
            // No signature block to replace (the previous account
            // had none): a new one goes where the setting puts it,
            // above the quoted original or at the end (#237).
            let anchor = self.rich_sig_anchor();
            let js = format!(
                "(function(){{var s=[].filter.call(document.querySelectorAll('.hylki-sig'),\
                 function(x){{return !x.closest('blockquote');}})[0];\
                 var h='{}';\
                 if(s){{if(h){{s.outerHTML=h;}}else{{s.remove();}}}}\
                 else if(h){{{anchor}}}\
                 window.__hylkiPlainFix&&window.__hylkiPlainFix();}})()",
                rich_editor::js_escape(&replacement)
            );
            self.editor.run_js(&js);
            self.current_sig = new_sig;
        }
    }

    /// Source-mode JS that puts `n` into `v` where the setting puts a
    /// signature: at the end, or above the quoted original (#237), which in
    /// source is found by its text. HTML keeps the quote's tags; Markdown
    /// has the `> ` lines, with the attribution line ("On …, X wrote:" or
    /// the forward header) above. Both are in the app's language (#528), so
    /// they are found by shape: a line ending in a colon, or a paragraph
    /// opening with the forward header's dashes.
    fn source_sig_place(&self, kind: SourceKind) -> &'static str {
        match (self.signature_position, kind) {
            (SignaturePosition::BelowQuote, _) => "v=v+n;",
            (SignaturePosition::AboveQuote, SourceKind::Html) => {
                "var q=v.search(/<p class=\"hylki-quote-attr\"|<blockquote/);\
                 if(q<0){v=v+n;}else{v=v.slice(0,q).replace(/\\s*$/,'')+n+v.slice(q);}"
            }
            (SignaturePosition::AboveQuote, SourceKind::Markdown) => {
                "var L=v.split('\\n');var qi=-1;\
                 for(var k=0;k<L.length;k++){if(L[k]==='>'||L[k].indexOf('> ')===0){qi=k;break;}}\
                 if(qi<0){v=v+n;}else{var a=qi-1;while(a>=0&&L[a].trim()==='')a--;\
                 if(a>=0){var s=a;while(s>0&&L[s-1].trim()!=='')s--;\
                 if(/^-{5,} /.test(L[s]))qi=s;else if(/[:\\uFF1A]\\s*$/.test(L[a]))qi=a;}\
                 v=L.slice(0,qi).join('\\n').replace(/\\s*$/,'')+n+'\\n'+L.slice(qi).join('\\n');}"
            }
        }
    }

    /// Rich-mode JS that puts the HTML `h` where the setting puts a
    /// signature: above the quoted original, or at the end (#237).
    fn rich_sig_anchor(&self) -> &'static str {
        match self.signature_position {
            SignaturePosition::AboveQuote => {
                "var b=document.body;var q=null;\
                 var cands=b.querySelectorAll('.hylki-quote-attr,blockquote');\
                 for(var i=0;i<cands.length;i++){var t=cands[i];\
                 while(t.parentNode&&t.parentNode!==b)t=t.parentNode;\
                 if(t.parentNode===b&&(!q||(t.compareDocumentPosition(q)&Node.DOCUMENT_POSITION_FOLLOWING)))q=t;}\
                 if(q){q.insertAdjacentHTML('beforebegin',h);}else{b.insertAdjacentHTML('beforeend',h);}"
            }
            SignaturePosition::BelowQuote => "document.body.insertAdjacentHTML('beforeend',h);",
        }
    }

    /// Insert Signature (#385): `sig` at the caret, or at the top or the
    /// end of the message. The message's own signature block moves rather
    /// than doubling; one inside a quote is the other person's and stays.
    fn insert_signature(&mut self, sig: String, place: SigPlace) {
        if sig.is_empty() {
            return;
        }
        let place_js = match place {
            SigPlace::Cursor => "cursor",
            SigPlace::Start => "start",
            SigPlace::End => "end",
        };
        if let Some(kind) = self.editor.source_kind() {
            let old = sig_source(kind, &self.current_sig, self.sig_dashes);
            let old_other = sig_source(kind, &self.current_sig, !self.sig_dashes);
            let new = sig_source(kind, &sig, self.sig_dashes);
            // A caret only counts while the field has focus: a fresh
            // composer's field reports 0, which is the top, not a choice.
            let js = r#"(function(){var t=document.getElementById('src');if(!t)return;
var o=@OLD@,p=@OTHER@,n=@NEW@,where=@WHERE@,v=t.value;
var a=document.activeElement===t?t.selectionEnd:-1;
var i=o?v.lastIndexOf(o):-1;if(i<0&&p){i=v.lastIndexOf(p);if(i>=0)o=p;}
if(i>=0){v=v.slice(0,i)+v.slice(i+o.length);if(a>i+o.length)a-=o.length;else if(a>i)a=i;}
if(where==='start'){v=n.replace(/^\s+/,'')+'\n'+v;a=-1;}
else if(where==='end'){v=v.replace(/\s*$/,'')+n;a=-1;}
else if(a>=0){v=v.slice(0,a)+n+v.slice(a);}
else{@PLACE@}
t.value=v;if(a>=0){t.selectionStart=t.selectionEnd=a;}window.__hylkiDirty=true;})()"#
                // The signature text goes in last, so nothing in it is
                // taken for a placeholder.
                .replace("@WHERE@", &format!("'{place_js}'"))
                .replace("@PLACE@", self.source_sig_place(kind))
                .replace("@OTHER@", &format!("'{}'", js_escape(&old_other)))
                .replace("@OLD@", &format!("'{}'", js_escape(&old)))
                .replace("@NEW@", &format!("'{}'", js_escape(&new)));
            self.editor.run_js(&js);
            self.current_sig = sig;
            return;
        }
        // At the caret the line it sits on is split, so the signature
        // stands as a block of its own between the two halves; an empty
        // line gives up its place to it.
        let js = r#"(function(){var b=document.body,h=@SIG@,where=@WHERE@;
var olds=[].filter.call(b.querySelectorAll('.hylki-sig'),function(s){return !s.closest('blockquote');});
var sel=window.getSelection(),r=null;
if(where==='cursor'&&sel.rangeCount){r=sel.getRangeAt(0).cloneRange();r.collapse(false);if(!b.contains(r.startContainer)||r.startContainer===b&&b.childNodes.length===0)r=null;}
if(r){for(var k=0;k<olds.length;k++){if(olds[k].contains(r.startContainer)){r.setStartBefore(olds[k]);r.collapse(true);}}}
var spot=null;
if(r){var top=r.startContainer;
 if(top===b){spot=b.childNodes[r.startOffset]||null;}
 else{while(top.parentNode!==b)top=top.parentNode;
  if(olds.indexOf(top)>=0){spot=top;}
  else{var tail=document.createRange();tail.setStart(r.startContainer,r.startOffset);tail.setEndAfter(top);
   var rest=tail.extractContents();var keep=function(n){return n&&(n.textContent.trim()!==''||n.querySelector&&n.querySelector('img,table,hr'));};
   spot=top.nextSibling;
   if(keep(rest)){b.insertBefore(rest,spot);spot=top.nextSibling;}
   if(!keep(top)){spot=top.nextSibling;top.remove();}}}}
for(var k=0;k<olds.length;k++){if(spot===olds[k])spot=olds[k].nextSibling;olds[k].remove();}
var tpl=document.createElement('template');tpl.innerHTML=h;var sig=tpl.content.firstElementChild;
if(where==='start'){b.insertBefore(sig,b.firstChild);}
else if(where==='end'){b.appendChild(sig);}
else if(r){b.insertBefore(sig,spot);var c=document.createRange();c.setStartBefore(sig);c.collapse(true);sel.removeAllRanges();sel.addRange(c);}
else{@ANCHOR@}
window.__hylkiPlainFix&&window.__hylkiPlainFix();
window.__hylkiDirty=true;})()"#
            .replace("@WHERE@", &format!("'{place_js}'"))
            .replace("@ANCHOR@", self.rich_sig_anchor())
            .replace("@SIG@", &format!("'{}'", rich_editor::js_escape(&sig_html(&sig, self.sig_dashes))));
        self.editor.run_js(&js);
        self.current_sig = sig;
    }

    /// Put Sign where the From account's default has it (#267), until the
    /// user has set it by hand. While encrypting it stays on: encrypted
    /// mail is always signed.
    pub(super) fn follow_account_signing(&mut self, widgets: &ComposeWidgets) {
        if self.sign_touched || self.encrypt || !crate::mail::pgp::available() {
            return;
        }
        let idx = widgets.from_row.selected() as usize;
        let on = self.accounts.get(idx).is_some_and(|a| a.sign_default);
        if on != widgets.sign_btn.is_active() {
            self.sign_expected = Some(on);
            widgets.sign_btn.set_active(on);
        }
    }

    /// Handles [`ComposeInput::InsertSignature`].
    pub(super) fn on_insert_signature(&mut self, place: SigPlace, widgets: &ComposeWidgets) {
        let idx = widgets.from_row.selected() as usize;
        let sig = self
            .accounts
            .get(idx)
            .map(|a| self.chosen_signature(a))
            .unwrap_or_default();
        self.insert_signature(sig, place);
    }

    /// Handles [`ComposeInput::SignatureClicked`].
    pub(super) fn on_signature_clicked(
        &mut self,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let idx = widgets.from_row.selected() as usize;
        let Some(account) = self.accounts.get(idx) else {
            return;
        };
        if account.signatures.len() < 2 {
            sender
                .input_sender()
                .emit(ComposeInput::InsertSignature(SigPlace::Cursor));
            return;
        }
        // The one in the message is drawn as chosen; a body that
        // started without one (Edit as New, a draft) may still
        // carry the default's text, which is no harm to show.
        let list: Vec<MenuEntry> = account
            .signatures
            .iter()
            .enumerate()
            .map(|(i, sig)| {
                let s = sender.clone();
                MenuEntry::new(&sig.name, move || {
                    s.input_sender().emit(ComposeInput::UseSignature(Some(i)))
                })
                .selected(!self.current_sig.is_empty() && sig.html == self.current_sig)
            })
            .collect();
        let s = sender.clone();
        let none = MenuEntry::new(i18n("No Signature"), move || {
            s.input_sender().emit(ComposeInput::UseSignature(None))
        })
        .selected(self.current_sig.is_empty());
        let btn = &self.signature_btn;
        show_context_menu(
            btn,
            (btn.width() / 2) as f64,
            btn.height() as f64,
            vec![list, vec![none]],
        );
    }

    /// Handles [`ComposeInput::UseSignature`].
    pub(super) fn on_use_signature(&mut self, choice: Option<usize>, widgets: &ComposeWidgets) {
        let idx = widgets.from_row.selected() as usize;
        let sig = match choice {
            Some(i) => match self.accounts.get(idx).and_then(|a| a.signatures.get(i)) {
                Some(sig) => sig.html.clone(),
                None => return,
            },
            None => String::new(),
        };
        if sig != self.current_sig {
            self.put_signature(sig);
        }
    }

    /// Handles [`ComposeInput::SignatureMenu`].
    pub(super) fn on_signature_menu(&mut self, sender: &ComponentSender<Self>) {
        let entry = |label: String, place: SigPlace| {
            let s = sender.clone();
            MenuEntry::new(&label, move || {
                s.input_sender().emit(ComposeInput::InsertSignature(place))
            })
        };
        let btn = &self.signature_btn;
        show_context_menu(
            btn,
            (btn.width() / 2) as f64,
            btn.height() as f64,
            vec![vec![
                entry(i18n("Insert Before the Text"), SigPlace::Start),
                entry(i18n("Insert After the Text"), SigPlace::End),
            ]],
        );
    }
}
