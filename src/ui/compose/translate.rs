//! Translating the body (#327): the scripts that take the text out of the
//! editor and put the translation back.

use relm4::prelude::*;

use super::messages::ComposeInput;
use super::{Compose, ComposeWidgets};
use crate::config::ComposeFormat;
use crate::i18n::{i18n, i18n_f};
use crate::ui::context_menu::MenuEntry;
use crate::ui::dialog::alert;
use adw::prelude::*;

/// Which editor a translation (#327) was taken from, and so how it goes
/// back: as HTML into the rich document, or as text into the source field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranslateHow {
    Rich,
    Markdown,
    Html,
}

/// The quoted message's text, for telling which language to offer first:
/// the reply's quote in the rich document, or the quoted lines of a
/// Markdown or HTML source.
pub(super) const TR_QUOTE_TEXT: &str = r#"(function(){var t=document.getElementById('src');
if(t){var v=t.value;var i=v.search(/<blockquote/i);
if(i>=0)return v.slice(i).replace(/<[^>]*>/g,' ').slice(0,4000);
return v.split('\n').filter(function(l){return /^\s*>/.test(l);}).map(function(l){return l.replace(/^\s*>+\s?/,'');}).join('\n').slice(0,4000);}
var q=document.querySelector('blockquote.hylki-quote-mail')||document.querySelector('blockquote');
return q?q.innerText.slice(0,4000):'';})()"#;

/// What the rich document asks to have translated: the selection, else
/// what was written above the quote and signature. The range is kept for
/// the answer, and the body is locked until it comes. Block elements go as
/// pieces of their own; runs of inline content go together. The range
/// starts and ends inside the first and last blocks, not around them: an
/// insert over whole blocks runs its last paragraph into the quote's
/// "wrote:" line.
pub(super) const TR_RICH_BEGIN: &str = r#"(function(){var b=document.body;if(!b)return '';
var s=getSelection(),r=null,sel=false;
if(s.rangeCount&&!s.isCollapsed&&b.contains(s.anchorNode)){r=s.getRangeAt(0).cloneRange();sel=true;}
else{var first=null,c=b.querySelectorAll('.hylki-sig,.hylki-quote-attr,blockquote');
for(var i=0;i<c.length;i++){var t=c[i];while(t.parentNode&&t.parentNode!==b)t=t.parentNode;
if(t.parentNode===b&&(!first||(t.compareDocumentPosition(first)&Node.DOCUMENT_POSITION_FOLLOWING)))first=t;}
var kids=[];for(var n=b.firstChild;n&&n!==first;n=n.nextSibling)kids.push(n);
var full=function(n){return !!(n.textContent.trim()||(n.nodeType===1&&(n.tagName==='IMG'||n.querySelector('img'))));};
var a=0,e=kids.length;while(a<e&&!full(kids[a]))a++;while(e>a&&!full(kids[e-1]))e--;
if(a>=e)return JSON.stringify({empty:true,sel:false});
r=document.createRange();
var A=kids[a],E=kids[e-1];
if(A.nodeType===1)r.setStart(A,0);else r.setStartBefore(A);
if(E.nodeType===1)r.setEnd(E,E.childNodes.length);else r.setEndAfter(E);}
var f=r.cloneContents();if(!f.textContent.trim())return JSON.stringify({empty:true,sel:sel});
var p=[];
if(sel){var w=document.createElement('div');w.appendChild(f);p.push(w.innerHTML);}
else{var BLOCK=/^(P|DIV|UL|OL|LI|H[1-6]|PRE|TABLE|BLOCKQUOTE|HR)$/,run='';
var flush=function(){if(run.trim())p.push(run);run='';};
for(var n=f.firstChild;n;n=n.nextSibling){
if(n.nodeType===1&&BLOCK.test(n.tagName)){flush();p.push(n.outerHTML);}
else if(n.nodeType===1){run+=n.outerHTML;}
else if(n.nodeType===3){var d=document.createElement('div');d.textContent=n.textContent;run+=d.innerHTML;}}
flush();}
window.__hylkiTr=r;b.setAttribute('contenteditable','false');
return JSON.stringify({parts:p,sel:sel});})()"#;

/// The translation in place of what was taken, as one edit the editor's
/// undo can take back. `__hylkiTrState` then follows it for the Translate
/// menu (#327): "translated", "original" once undone, and nothing once
/// anything else is done to the text.
pub(super) const TR_RICH_END: &str = r#"(function(h){var b=document.body;if(!b)return;b.setAttribute('contenteditable','true');
var r=window.__hylkiTr;window.__hylkiTr=null;if(!r)return;b.focus();
var s=getSelection();s.removeAllRanges();s.addRange(r);
document.execCommand('insertHTML',false,h);window.__hylkiPlainFix&&window.__hylkiPlainFix();window.__hylkiDirty=true;window.__hylkiTrState='translated';if(!window.__hylkiTrWatch){window.__hylkiTrWatch=true;document.addEventListener('input',function(e){var st=window.__hylkiTrState;if(!st)return;var k=e.inputType||'';if(k==='historyUndo')window.__hylkiTrState=st==='translated'?'original':'';else if(k==='historyRedo')window.__hylkiTrState=st==='original'?'translated':'';else window.__hylkiTrState='';},true);}})(ARG)"#;

/// The source field's version of [`TR_RICH_BEGIN`]: the selection, else
/// the text above the quote (and, in Markdown, its "wrote:" line) and the
/// signature. HTMLFLAG says whether the source is HTML.
pub(super) const TR_SOURCE_BEGIN: &str = r#"(function(H){var t=document.getElementById('src');if(!t)return '';
var v=t.value,a=t.selectionStart,e=t.selectionEnd,sel=e>a;
if(!sel){a=0;e=v.length;
var sig=H?v.search(/<div class="hylki-sig"/):v.indexOf('\n-- \n');if(sig>=0)e=Math.min(e,sig);
var q=H?v.search(/<p class="hylki-quote-attr"|<blockquote/i):v.search(/(^|\n)[ \t]*>/);
if(q>=0){if(!H){var pre=v.slice(0,q).replace(/\s+$/,'');var ls=pre.lastIndexOf('\n');
var L=pre.split('\n'),p=L.length-1;while(p>0&&L[p-1].trim()!=='')p--;
if(/^-{5,} /.test(L[p]))q=L.slice(0,p).join('\n').length+(p?1:0);else if(/[:：]\s*$/.test(pre.slice(ls+1)))q=ls+1;}
e=Math.min(e,q);}}
while(e>a&&/\s/.test(v.charAt(e-1)))e--;while(a<e&&/\s/.test(v.charAt(a)))a++;
var x=v.slice(a,e);if(!x.trim())return JSON.stringify({empty:true,sel:sel});
window.__hylkiTr={a:a,e:e};t.readOnly=true;return JSON.stringify({text:x,sel:sel});})(HTMLFLAG)"#;

/// [`TR_RICH_END`] for the source field.
pub(super) const TR_SOURCE_END: &str = r#"(function(x){var t=document.getElementById('src');if(!t)return;t.readOnly=false;
var r=window.__hylkiTr;window.__hylkiTr=null;if(!r)return;t.focus();t.setSelectionRange(r.a,r.e);
if(!document.execCommand('insertText',false,x))t.setRangeText(x,r.a,r.e,'end');
window.__hylkiDirty=true;t.dispatchEvent(new Event('input'));window.__hylkiTrState='translated';if(!window.__hylkiTrWatch){window.__hylkiTrWatch=true;document.addEventListener('input',function(e){var st=window.__hylkiTrState;if(!st)return;var k=e.inputType||'';if(k==='historyUndo')window.__hylkiTrState=st==='translated'?'original':'';else if(k==='historyRedo')window.__hylkiTrState=st==='original'?'translated':'';else window.__hylkiTrState='';},true);}})(ARG)"#;

/// A translation that will not come: unlock the body as it was.
pub(super) const TR_CANCEL: &str = r#"(function(){var b=document.body;if(b&&!document.getElementById('src'))b.setAttribute('contenteditable','true');
var t=document.getElementById('src');if(t)t.readOnly=false;window.__hylkiTr=null;})()"#;

impl Compose {
    /// Handles [`ComposeInput::TranslateMenu`].
    pub(super) fn on_translate_menu(&mut self, sender: &ComponentSender<Self>) {
        if self.translating {
            return;
        }
        let s = sender.input_sender().clone();
        let js = format!("JSON.stringify({{q:{TR_QUOTE_TEXT},t:window.__hylkiTrState||''}})");
        self.editor.eval(&js, move |text| {
            let _ = s.send(ComposeInput::ShowTranslateMenu(text));
        });
    }

    /// Handles [`ComposeInput::ShowTranslateMenu`].
    pub(super) fn on_show_translate_menu(&mut self, found: String, sender: &ComponentSender<Self>) {
        let v: serde_json::Value = serde_json::from_str(&found).unwrap_or_default();
        let quoted = v["q"].as_str().unwrap_or_default().to_string();
        let entry = |code: String, label: String| {
            let s = sender.clone();
            MenuEntry::new(label, move || {
                s.input_sender()
                    .emit(ComposeInput::TranslateTo(code.clone()))
            })
        };
        // Back to what was written, or to the translation again,
        // while the translation is still the last thing done to the
        // text (#327).
        let toggle = match v["t"].as_str() {
            Some("translated") => Some((i18n("Show Original"), true)),
            Some("original") => Some((i18n("Show Translation"), false)),
            _ => None,
        };
        let toggle = toggle.map(|(label, original)| {
            let s = sender.clone();
            vec![MenuEntry::new(label, move || {
                s.input_sender()
                    .emit(ComposeInput::TranslateToggle { original })
            })]
        });
        // First what the conversation suggests: the language of the
        // message being answered, then the one last used.
        let mut first = Vec::new();
        let quoted = crate::services::translate::detect(&quoted);
        if let Some(lang) = &quoted {
            first.push(
                entry(
                    lang.clone(),
                    i18n_f(
                        "{language} (the quoted message)",
                        &[("language", &crate::services::translate::language_name(lang))],
                    ),
                )
                .icon("mail-reply-sender-symbolic"),
            );
        }
        if let Some(last) = crate::services::translate::last_compose_target() {
            if quoted.as_deref().map(crate::services::translate::base)
                != Some(crate::services::translate::base(&last))
            {
                first.push(
                    entry(
                        last.clone(),
                        crate::services::translate::language_name(&last),
                    )
                    .icon("translate-symbolic"),
                );
            }
        }
        let all: Vec<MenuEntry> = crate::services::translate::LANGUAGES
            .iter()
            .map(|code| {
                entry(
                    code.to_string(),
                    crate::services::translate::language_name(code),
                )
            })
            .collect();
        let mut sections = if first.is_empty() {
            vec![all]
        } else {
            vec![
                first,
                vec![MenuEntry::submenu(i18n("Other Languages"), vec![all])],
            ]
        };
        if let Some(toggle) = toggle {
            sections.insert(0, toggle);
        }
        let btn = &self.translate_btn;
        crate::ui::context_menu::show_context_menu_with_header(
            btn,
            (btn.width() / 2) as f64,
            btn.height() as f64,
            Some(&i18n("Translate into")),
            sections,
        );
    }

    /// Handles [`ComposeInput::TranslateTo`].
    pub(super) fn on_translate_to(
        &mut self,
        target: String,
        sender: &ComponentSender<Self>,
        root: &adw::BreakpointBin,
    ) {
        if self.translating {
            return;
        }
        // A message going out encrypted must not have its text
        // handed to a third party on the way.
        if self.encrypt {
            let parent = root.root().and_downcast::<gtk::Window>();
            alert(
                parent.as_ref(),
                &i18n("Not translated"),
                &i18n(
                    "This message is set to be encrypted, so its text is not sent for translation.",
                ),
            );
            return;
        }
        self.translating = true;
        self.translate_btn.set_sensitive(false);
        let js = match self.format {
            ComposeFormat::Html => TR_SOURCE_BEGIN.replace("HTMLFLAG", "true"),
            ComposeFormat::Markdown => TR_SOURCE_BEGIN.replace("HTMLFLAG", "false"),
            _ => TR_RICH_BEGIN.to_string(),
        };
        let s = sender.input_sender().clone();
        self.editor.eval(&js, move |found| {
            let _ = s.send(ComposeInput::TranslateBegun { found, target });
        });
    }

    /// Handles [`ComposeInput::TranslateBegun`].
    pub(super) fn on_translate_begun(
        &mut self,
        found: String,
        target: String,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
        root: &adw::BreakpointBin,
    ) {
        let v: serde_json::Value = serde_json::from_str(&found).unwrap_or_default();
        let how = match self.format {
            ComposeFormat::Html => TranslateHow::Html,
            ComposeFormat::Markdown => TranslateHow::Markdown,
            _ => TranslateHow::Rich,
        };
        let parts: Vec<String> = match (&how, v["text"].as_str()) {
            (TranslateHow::Html, Some(text)) => vec![text.to_string()],
            (TranslateHow::Markdown, Some(text)) => crate::services::translate::text_to_parts(text),
            _ => v["parts"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|p| p.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        };
        if parts.is_empty() {
            self.translating = false;
            self.translate_btn.set_sensitive(true);
            self.editor.run_js(TR_CANCEL);
            if v["empty"].as_bool() == Some(true) {
                let parent = root.root().and_downcast::<gtk::Window>();
                alert(
                    parent.as_ref(),
                    &i18n("Nothing to translate yet"),
                    &i18n("Write something first, or select the text to translate."),
                );
            }
            return;
        }
        // The subject goes too when the whole message is translated,
        // unless it is a reply's or a forward's: that one is already
        // in the conversation's language.
        let subject = widgets.subject_row.text().trim().to_string();
        let subject = (v["sel"].as_bool() != Some(true)
            && !subject.is_empty()
            && self.in_reply_to.trim().is_empty()
            && !crate::models::is_reply_or_forward_subject(&subject))
        .then_some(subject);
        let mut parts = parts;
        if let Some(subject) = &subject {
            parts.push(gtk::glib::markup_escape_text(subject).to_string());
        }
        let s = sender.input_sender().clone();
        std::thread::spawn(move || {
            let settings = crate::services::translate::load();
            let key = crate::services::translate::load_key(settings.service).unwrap_or_default();
            let result =
                crate::services::translate::translate_parts(&settings, &key, &parts, &target);
            let _ = s.send(ComposeInput::Translated {
                result,
                how,
                subject,
            });
        });
    }

    /// Handles [`ComposeInput::TranslateToggle`].
    pub(super) fn on_translate_toggle(
        &mut self,
        original: bool,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        self.history_step(!original, widgets, sender);
        if let Some((written, translated)) = &self.tr_subject {
            let (from, to) = if original {
                (translated, written)
            } else {
                (written, translated)
            };
            if widgets.subject_row.text().as_str() == from.as_str() {
                widgets.subject_row.set_text(to);
            }
        }
    }

    /// Handles [`ComposeInput::Translated`].
    pub(super) fn on_translated(
        &mut self,
        result: Result<Vec<String>, String>,
        how: TranslateHow,
        subject: Option<String>,
        widgets: &ComposeWidgets,
        root: &adw::BreakpointBin,
    ) {
        self.translating = false;
        self.translate_btn.set_sensitive(true);
        match result {
            // One edit, so Ctrl+Z gives back what was written.
            Ok(mut parts) => {
                self.tr_subject = None;
                if let Some(original) = subject {
                    let translated = parts.pop().map(|t| {
                        crate::mail::markdown::plain_text(&t)
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                    });
                    if let Some(translated) = translated.filter(|t| !t.is_empty()) {
                        widgets.subject_row.set_text(&translated);
                        self.tr_subject = Some((original, translated));
                    }
                }
                let (end, text) = match how {
                    TranslateHow::Rich => (TR_RICH_END, parts.concat()),
                    TranslateHow::Html => (TR_SOURCE_END, parts.concat()),
                    TranslateHow::Markdown => (
                        TR_SOURCE_END,
                        crate::services::translate::parts_to_text(&parts),
                    ),
                };
                let arg = serde_json::to_string(&text).unwrap_or_else(|_| "''".into());
                self.editor.run_js(&end.replace("ARG", &arg));
            }
            Err(why) => {
                self.editor.run_js(TR_CANCEL);
                let parent = root.root().and_downcast::<gtk::Window>();
                alert(parent.as_ref(), &i18n("Could not translate"), &why);
            }
        }
    }
}
