//! An AI assistant at an open composer (`crate::mcp`): what it reads of
//! the message (read_draft), and an edit it asks for (update_draft). The
//! edit waits for the user to accept or reject it, and nothing changes
//! before: a slim bar over the composer says what it does to the header
//! fields, and its text shows in the editor as a diff against what is
//! written there, over the document rather than in it. Accepted, the
//! fields take the new values, and the new text goes in place of what is
//! written above the signature and any quote, as one edit Undo takes
//! back; the signature and the quote stay as they are. A composer that is
//! encrypted, or holds what was decrypted, gives nothing away and takes no
//! edit.

use adw::prelude::*;
use relm4::prelude::*;

use super::assistant::{fill_banner, outside};
use super::messages::ComposeInput;
use super::signature::sig_source;
use super::text_diff::diff_html;
use super::{Compose, ComposeWidgets};
use crate::config::ComposeFormat;
use crate::i18n::i18n;
use crate::mcp::{DraftEdit, EditOutcome, OpenDraft};
use crate::ui::dialog::alert;
use crate::ui::rich_editor::SourceKind;
use std::sync::atomic::{AtomicU32, Ordering};

/// The rich document's written part, as `__w(body)`: the top-level nodes
/// before the first signature, quote attribution or quote (`first`, or
/// null when there is none). As Translate finds it.
const RICH_WRITTEN: &str = r#"function __w(b){var first=null,c=b.querySelectorAll('.hylki-sig,.hylki-quote-attr,blockquote');
for(var i=0;i<c.length;i++){var t=c[i];while(t.parentNode&&t.parentNode!==b)t=t.parentNode;
if(t.parentNode===b&&(!first||(t.compareDocumentPosition(first)&Node.DOCUMENT_POSITION_FOLLOWING)))first=t;}
var kids=[];for(var n=b.firstChild;n&&n!==first;n=n.nextSibling)kids.push(n);return {first:first,kids:kids};}"#;

/// The source field's version, as `__e(value, isHtml, sig, sigOther)`:
/// where the written part ends, at the signature (found by its text, with
/// or without its `-- ` line, or by its shape) or the quote (in Markdown
/// with its "wrote:" line or the forward header above), whichever is first.
const SOURCE_WRITTEN: &str = r#"function __e(v,H,O,P){var e=v.length;
var i=O?v.lastIndexOf(O):-1;if(i<0&&P)i=v.lastIndexOf(P);if(i>=0)e=i;
var sig=H?v.search(/<div class="hylki-sig"/):v.indexOf('\n-- \n');if(sig>=0)e=Math.min(e,sig);
var q=H?v.search(/<p class="hylki-quote-attr"|<blockquote/i):v.search(/(^|\n)[ \t]*>/);
if(q>=0){if(!H){var pre=v.slice(0,q).replace(/\s+$/,'');var ls=pre.lastIndexOf('\n');
var L=pre.split('\n'),p=L.length-1;while(p>0&&L[p-1].trim()!=='')p--;
if(/^-{5,} /.test(L[p]))q=L.slice(0,p).join('\n').length+(p?1:0);else if(/[:：]\s*$/.test(pre.slice(ls+1)))q=ls+1;}
e=Math.min(e,q);}return e;}"#;

/// The rich document's written part, as HTML.
const READ_RICH: &str = r#"(function(){var b=document.body;if(!b)return '';@W@
var d=document.createElement('div');__w(b).kids.forEach(function(n){d.appendChild(n.cloneNode(true));});
return d.innerHTML;})()"#;

/// The source field's written part, as typed.
const READ_SOURCE: &str = r#"(function(H,O,P){var t=document.getElementById('src');if(!t)return '';@E@
var v=t.value;return v.slice(0,__e(v,H,O,P)).trim();})(@ARGS@)"#;

/// `h` (HTML) in place of the rich document's written part, as one edit.
/// The range starts and ends inside the first and last blocks written, as
/// Translate's does, so the last paragraph does not run into the quote's
/// "wrote:" line. With nothing written, the text goes on the first empty
/// line, with an empty line under it, as a new message is laid out.
const APPLY_RICH: &str = r#"(function(h){var b=document.body;if(!b||!b.isContentEditable)return 'locked';@W@
var w=__w(b),kids=w.kids;
var full=function(n){return !!(n.textContent.trim()||(n.nodeType===1&&(n.tagName==='IMG'||n.querySelector('img'))));};
var a=0,e=kids.length;while(a<e&&!full(kids[a]))a++;while(e>a&&!full(kids[e-1]))e--;
if(a>=e){if(!kids.length){var d=document.createElement('div');d.appendChild(document.createElement('br'));b.insertBefore(d,w.first);kids=[d];}
a=0;e=1;if(h)h+='<div><br></div>';}
var r=document.createRange(),A=kids[a],E=kids[e-1];
if(A.nodeType===1)r.setStart(A,0);else r.setStartBefore(A);
if(E.nodeType===1)r.setEnd(E,E.childNodes.length);else r.setEndAfter(E);
b.focus();var s=getSelection();s.removeAllRanges();s.addRange(r);
if(h)document.execCommand('insertHTML',false,h);else document.execCommand('delete');
window.__hylkiPlainFix&&window.__hylkiPlainFix();window.__hylkiDirty=true;return 'ok';})(@ARGS@)"#;

/// [`APPLY_RICH`] for the source field: `x` in place of the written part.
const APPLY_SOURCE: &str = r#"(function(x,H,O,P){var t=document.getElementById('src');if(!t)return 'none';if(t.readOnly)return 'locked';@E@
var v=t.value,e=__e(v,H,O,P),a=0;
while(e>a&&/\s/.test(v.charAt(e-1)))e--;while(a<e&&/\s/.test(v.charAt(a)))a++;
if(a>=e){a=0;e=0;if(x)x+='\n\n';}
t.focus();t.setSelectionRange(a,e);if(!document.execCommand('insertText',false,x))t.setRangeText(x,a,e,'end');
window.__hylkiDirty=true;t.dispatchEvent(new Event('input'));return 'ok';})(@ARGS@)"#;

/// The diff of an edit's text, `h` (HTML), shown over the editor if `n` is
/// still the edit waiting (`__hylkiSuggestSeq`): a layer of its own in the
/// document's look, outside the body, so nothing the document hands back
/// carries it. The text under it is locked meanwhile.
const SHOW_DIFF: &str = r#"(function(n,h){if(window.__hylkiSuggestSeq!==n)return;
var o=document.querySelector('[data-hylki-ui=suggest]');
if(!o){var t=document.getElementById('src'),b=document.body,ref=t||b;if(!ref)return;var cs=getComputedStyle(ref);
o=document.createElement('div');o.setAttribute('data-hylki-ui','suggest');o.setAttribute('contenteditable','false');o.setAttribute('dir','auto');
var s=o.style;s.cssText='position:fixed;top:0;right:0;bottom:0;left:0;z-index:20;overflow:auto;box-sizing:border-box;margin:0;white-space:pre-wrap;overflow-wrap:anywhere;unicode-bidi:plaintext;';
s.padding=cs.padding;s.fontFamily=cs.fontFamily;s.fontSize=cs.fontSize;s.lineHeight=cs.lineHeight;s.color=cs.color;
s.backgroundColor=cs.backgroundColor;
if(document.activeElement&&document.activeElement.blur)document.activeElement.blur();
if(t)t.readOnly=true;else b.setAttribute('contenteditable','false');
document.documentElement.appendChild(o);}
o.innerHTML=h;o.scrollTop=0;})(@ARGS@)"#;

/// The diff taken away, and the text unlocked unless a translation still
/// holds it. One on its way is not shown.
const HIDE_DIFF: &str = r#"(function(){window.__hylkiSuggestSeq=0;var o=document.querySelector('[data-hylki-ui=suggest]');if(!o)return;o.remove();
if(window.__hylkiTr)return;var t=document.getElementById('src');if(t)t.readOnly=false;else if(document.body)document.body.setAttribute('contenteditable','true');})()"#;

/// How the diff marks what goes and what comes, on a light or dark ground.
const DIFF_STYLE: &str = "<style>[data-hylki-ui=suggest] del{text-decoration:line-through;\
text-decoration-color:#e01b24;background:rgba(224,27,36,0.16);opacity:0.8;border-radius:3px;}\
[data-hylki-ui=suggest] ins{text-decoration:none;background:rgba(46,194,126,0.26);border-radius:3px;}</style>";

/// The number of the latest diff asked for.
static DIFF_SEQ: AtomicU32 = AtomicU32::new(1);

/// A value as a JavaScript literal.
fn js(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "''".into())
}

/// The bar over an assistant's edit, hidden until there is one; its text
/// shows in the editor.
pub(super) fn new_edit_bar() -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Vertical, 4);
    bar.add_css_class("assistant-banner");
    bar.add_css_class("assistant-edit-bar");
    bar.set_margin_start(12);
    bar.set_margin_end(12);
    bar.set_margin_top(6);
    bar.set_margin_bottom(6);
    bar.set_visible(false);
    bar
}

/// Whether HTML holds anything an edit in plain text takes away: the diff
/// compares words, so it cannot show this.
fn plain_text_loses(html: &str) -> bool {
    const LOST: &[&str] = &[
        "img", "a", "b", "strong", "i", "em", "u", "s", "strike", "del", "ul", "ol", "table", "h1",
        "h2", "h3", "h4", "h5", "h6", "pre", "code",
    ];
    const STYLED: &[&str] = &[
        "color:",
        "font-weight",
        "font-style",
        "text-decoration",
        "background",
    ];
    html.split('<').skip(1).any(|tag| {
        let tag = tag.split('>').next().unwrap_or_default().to_lowercase();
        let name = tag.split(|c: char| !c.is_alphanumeric()).next();
        name.is_some_and(|n| LOST.contains(&n)) || STYLED.iter().any(|s| tag.contains(s))
    })
}

impl Compose {
    /// The inputs that are an assistant's business.
    pub(super) fn on_assistant_input(
        &mut self,
        input: ComposeInput,
        widgets: &ComposeWidgets,
        root: &adw::BreakpointBin,
        sender: &ComponentSender<Self>,
    ) {
        match input {
            ComposeInput::AssistantRead { text, done } => {
                self.on_assistant_read(text, done, widgets)
            }
            ComposeInput::AssistantEdit(edit) => self.on_assistant_edit(edit, widgets, sender),
            ComposeInput::ApplyAssistantEdit => self.on_apply_assistant_edit(widgets, root),
            ComposeInput::RejectAssistantEdit => self.on_reject_assistant_edit(),
            ComposeInput::ReshowAssistantDiff => {
                let text = self.assistant_edit.as_ref().and_then(|e| e.text.clone());
                if text.is_some() {
                    self.show_text_diff(text);
                }
            }
            _ => {}
        }
    }

    /// Whether an edit still waits for the user, and if so tell them: what
    /// is on screen is not what would go.
    pub(super) fn edit_waiting(&self, widgets: &ComposeWidgets) -> bool {
        if self.assistant_edit.is_none() {
            return false;
        }
        let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
        alert(
            parent.as_ref(),
            &i18n("Not now"),
            &i18n("Accept or reject the AI assistant's suggested changes first."),
        );
        true
    }

    /// A composer an assistant never sees into.
    fn sealed(&self) -> bool {
        self.encrypt || self.from_encrypted
    }

    /// The account the composer sends from now.
    fn sending_account(&self, widgets: &ComposeWidgets) -> u32 {
        self.accounts
            .get(widgets.from_row.selected() as usize)
            .map(|a| a.id)
            .unwrap_or(self.from_account)
    }

    /// Handles [`ComposeInput::AssistantRead`]: the fields at once, and
    /// with `text` what is written, once the editor hands it over.
    pub(super) fn on_assistant_read(
        &self,
        text: bool,
        done: tokio::sync::oneshot::Sender<OpenDraft>,
        widgets: &ComposeWidgets,
    ) {
        if self.sealed() {
            let _ = done.send(OpenDraft::sealed(
                self.compose_id,
                self.sending_account(widgets),
                self.by_assistant,
            ));
            return;
        }
        let mut draft = self.open_draft(widgets);
        if !text {
            let _ = done.send(draft);
            return;
        }
        self.read_written(move |written, _| {
            draft.text = Some(written);
            let _ = done.send(draft);
        });
    }

    /// What is written above the signature and any quote, as text, once
    /// the editor hands it over, and whether that text is all of it.
    fn read_written(&self, done: impl FnOnce(String, bool) + 'static) {
        match self.editor.source_kind() {
            Some(kind) => {
                let js_text = READ_SOURCE
                    .replace("@E@", SOURCE_WRITTEN)
                    .replace("@ARGS@", &self.source_args(kind));
                self.editor.eval(&js_text, move |written| match kind {
                    SourceKind::Html => done(
                        crate::cache::body_text(&written).trim().to_string(),
                        plain_text_loses(&written),
                    ),
                    SourceKind::Markdown => done(written, false),
                });
            }
            None => {
                self.editor
                    .eval(&READ_RICH.replace("@W@", RICH_WRITTEN), move |html| {
                        done(
                            crate::cache::body_text(&html).trim().to_string(),
                            plain_text_loses(&html),
                        );
                    });
            }
        }
    }

    /// The composer as it stands, but for its text.
    fn open_draft(&self, widgets: &ComposeWidgets) -> OpenDraft {
        let idx = widgets.from_row.selected() as usize;
        let from = if self.custom_from {
            widgets.custom_from_row.text().trim().to_string()
        } else {
            self.accounts
                .get(idx)
                .map(|a| a.alias_from.clone().unwrap_or_else(|| a.email.clone()))
                .unwrap_or_default()
        };
        let kind = if self.template {
            "template"
        } else if !self.forwards.is_empty() {
            "forward"
        } else if !self.in_reply_to.is_empty() {
            "reply"
        } else {
            "new"
        };
        OpenDraft {
            compose_id: self.compose_id,
            account_id: self.sending_account(widgets),
            from,
            to: widgets.to_row.text().trim().to_string(),
            cc: widgets.cc_row.text().trim().to_string(),
            bcc: widgets.bcc_row.text().trim().to_string(),
            subject: widgets.subject_row.text().to_string(),
            kind,
            by_assistant: self.by_assistant,
            attachments: self
                .attachments
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .collect(),
            encrypted: false,
            format: match self.format {
                ComposeFormat::Rich => "rich",
                ComposeFormat::Plain => "plain",
                ComposeFormat::Markdown => "markdown",
                ComposeFormat::Html => "html",
            },
            text: None,
            pending: self.assistant_edit.clone(),
            last_edit: self.last_edit,
        }
    }

    /// The source scripts' arguments after the first: whether the source
    /// is HTML, and the signature as it reads there, with and without its
    /// `-- ` line.
    fn source_args(&self, kind: SourceKind) -> String {
        format!(
            "{},{},{}",
            kind == SourceKind::Html,
            js(&sig_source(kind, &self.current_sig, self.sig_dashes)),
            js(&sig_source(kind, &self.current_sig, !self.sig_dashes)),
        )
    }

    /// Handles [`ComposeInput::AssistantEdit`]: shown for the user, in
    /// place of one still waiting.
    pub(super) fn on_assistant_edit(
        &mut self,
        edit: DraftEdit,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        if self.sealed() {
            return;
        }
        self.edit_loss = Some(self.fill_edit_bar(&edit, widgets, sender));
        self.edit_bar.set_visible(true);
        self.show_text_diff(edit.text.clone());
        self.assistant_edit = Some(edit);
    }

    /// `new` over the editor as a diff against what is written there, or,
    /// with no new text, no diff. Whatever diff is still on its way for an
    /// earlier edit is not shown.
    pub(super) fn show_text_diff(&self, new: Option<String>) {
        let Some(new) = new else {
            self.editor.run_js(HIDE_DIFF);
            return;
        };
        let n = DIFF_SEQ.fetch_add(1, Ordering::Relaxed);
        self.editor
            .run_js(&format!("window.__hylkiSuggestSeq={n};"));
        let editor = self.editor.clone();
        let loss = self.edit_loss.clone();
        self.read_written(move |old, loses| {
            if let Some(line) = &loss {
                line.set_visible(loses && old.trim() != new.trim());
            }
            let html = format!("{DIFF_STYLE}{}", diff_html(&old, new.trim()));
            editor.run_js(&SHOW_DIFF.replace("@ARGS@", &format!("{n},{}", js(&html))));
        });
    }

    /// Handles [`ComposeInput::RejectAssistantEdit`].
    pub(super) fn on_reject_assistant_edit(&mut self) {
        if self.assistant_edit.take().is_some() {
            self.last_edit = Some(EditOutcome::Rejected);
        }
        self.edit_bar.set_visible(false);
        self.editor.run_js(HIDE_DIFF);
    }

    /// Handles [`ComposeInput::ApplyAssistantEdit`].
    pub(super) fn on_apply_assistant_edit(
        &mut self,
        widgets: &ComposeWidgets,
        root: &adw::BreakpointBin,
    ) {
        if self.assistant_edit.is_none() {
            return;
        }
        // The text is locked while a translation is out.
        if self.translating {
            let parent = root.root().and_downcast::<gtk::Window>();
            alert(
                parent.as_ref(),
                &i18n("Not now"),
                &i18n("Wait for the translation to come back, then accept the changes."),
            );
            return;
        }
        let Some(edit) = self.assistant_edit.take() else {
            return;
        };
        self.edit_bar.set_visible(false);
        for (list, row) in [
            (&edit.to, &widgets.to_row),
            (&edit.cc, &widgets.cc_row),
            (&edit.bcc, &widgets.bcc_row),
        ] {
            if let Some(list) = list {
                row.set_text(&list.join(", "));
                if !list.is_empty() {
                    row.set_visible(true);
                }
            }
        }
        if let Some(subject) = &edit.subject {
            widgets.subject_row.set_text(subject);
        }
        match &edit.text {
            Some(text) => self.put_assistant_text(text),
            None => self.editor.run_js(HIDE_DIFF),
        }
        self.fields_dirty = true;
        self.last_edit = Some(EditOutcome::Applied);
        if let Some(banner) = &self.assistant_banner {
            fill_banner(
                banner,
                [
                    &widgets.to_row.text(),
                    &widgets.cc_row.text(),
                    &widgets.bcc_row.text(),
                ],
                &self.own_domains,
            );
        }
    }

    /// `text` in place of what is written above the signature and quote,
    /// once the diff over it is gone and it is unlocked.
    fn put_assistant_text(&self, text: &str) {
        let script = match self.editor.source_kind() {
            Some(kind) => {
                let x = match kind {
                    SourceKind::Markdown => text.to_string(),
                    SourceKind::Html => crate::mcp::text_html(text),
                };
                APPLY_SOURCE
                    .replace("@E@", SOURCE_WRITTEN)
                    .replace("@ARGS@", &format!("{},{}", js(&x), self.source_args(kind)))
            }
            None => APPLY_RICH
                .replace("@W@", RICH_WRITTEN)
                .replace("@ARGS@", &js(&crate::mcp::text_html(text))),
        };
        // The same words stay as they are, with whatever formatting they have.
        let editor = self.editor.clone();
        let new = text.trim().to_string();
        self.read_written(move |old, _| {
            if old.trim() == new {
                editor.run_js(HIDE_DIFF);
            } else {
                editor.run_js(&format!("{HIDE_DIFF};{script}"));
            }
        });
    }

    /// The bar for `edit`: one row with Reject and Accept, and under it
    /// only what it adds to and takes out of the header fields. Its text
    /// shows in the editor. Returns the line, hidden for now, that says
    /// accepting takes away what the diff cannot show.
    fn fill_edit_bar(
        &self,
        edit: &DraftEdit,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) -> gtk::Label {
        let bar = &self.edit_bar;
        while let Some(child) = bar.first_child() {
            bar.remove(&child);
        }
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        head.append(&gtk::Image::from_icon_name("document-edit-symbolic"));
        let title = gtk::Label::new(Some(&i18n("Changes suggested by an AI assistant")));
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.add_css_class("heading");
        title.set_tooltip_text(Some(&i18n("Nothing changes until you accept them.")));
        head.append(&title);
        let reject = gtk::Button::with_label(&i18n("Reject"));
        reject.set_valign(gtk::Align::Center);
        let s = sender.input_sender().clone();
        reject.connect_clicked(move |_| s.emit(ComposeInput::RejectAssistantEdit));
        let accept = gtk::Button::with_label(&i18n("Accept"));
        accept.set_valign(gtk::Align::Center);
        accept.add_css_class("suggested-action");
        let s = sender.input_sender().clone();
        accept.connect_clicked(move |_| s.emit(ComposeInput::ApplyAssistantEdit));
        head.append(&reject);
        head.append(&accept);
        bar.append(&head);

        let changes = gtk::Box::new(gtk::Orientation::Vertical, 2);
        // Under the title, past the icon.
        changes.set_margin_start(24);
        for (label, bcc, list, row) in [
            (i18n("To:"), false, &edit.to, &widgets.to_row),
            (i18n("Cc:"), false, &edit.cc, &widgets.cc_row),
            (
                i18n("Bcc, hidden from the others:"),
                true,
                &edit.bcc,
                &widgets.bcc_row,
            ),
        ] {
            let Some(list) = list else { continue };
            if let Some(line) = self.recipient_change(&label, bcc, list, &row.text()) {
                changes.append(&line);
            }
        }
        let now = widgets.subject_row.text();
        if let Some(subject) = edit.subject.as_ref().filter(|s| **s != now) {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let field = gtk::Label::new(Some(&i18n("Subject:")));
            field.add_css_class("dim-label");
            line.append(&field);
            if !now.trim().is_empty() {
                let old = gtk::Label::new(None);
                old.set_markup(&format!("<s>− {}</s>", gtk::glib::markup_escape_text(&now)));
                old.add_css_class("assistant-removed");
                old.set_ellipsize(gtk::pango::EllipsizeMode::End);
                old.set_tooltip_text(Some(&now));
                line.append(&old);
            }
            let new = gtk::Label::new(Some(&format!("+ {subject}")));
            new.add_css_class("assistant-added");
            new.set_ellipsize(gtk::pango::EllipsizeMode::End);
            new.set_xalign(0.0);
            new.set_hexpand(true);
            new.set_tooltip_text(Some(subject));
            line.append(&new);
            changes.append(&line);
        }
        if changes.first_child().is_some() {
            bar.append(&changes);
        }
        let loss = gtk::Label::new(Some(&i18n(
            "Accepting removes this text's formatting, links and pictures.",
        )));
        loss.set_xalign(0.0);
        loss.set_wrap(true);
        loss.set_margin_start(24);
        loss.add_css_class("warning");
        loss.set_visible(false);
        bar.append(&loss);
        loss
    }

    /// What the edit does to one recipient field: those it adds and those
    /// it takes out, or `None` when it leaves the field as it is. Additions
    /// outside the user's domains, and all of Bcc, are set apart as on the
    /// banner.
    fn recipient_change(
        &self,
        label: &str,
        bcc: bool,
        new: &[String],
        now: &str,
    ) -> Option<gtk::Box> {
        let changes: Vec<_> = recipient_changes(now, new)
            .into_iter()
            .filter(|(_, _, c)| *c != Change::Kept)
            .collect();
        if changes.is_empty() {
            return None;
        }
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let field = gtk::Label::new(Some(label));
        field.set_valign(gtk::Align::Start);
        field.add_css_class(if bcc { "warning" } else { "dim-label" });
        line.append(&field);
        let flow = gtk::FlowBox::new();
        flow.set_selection_mode(gtk::SelectionMode::None);
        flow.set_hexpand(true);
        flow.set_max_children_per_line(8);
        flow.set_column_spacing(10);
        flow.set_row_spacing(0);
        for (text, addr, change) in changes {
            let chip = gtk::Label::new(None);
            chip.set_xalign(0.0);
            let escaped = gtk::glib::markup_escape_text(&text);
            if change == Change::Removed {
                chip.set_markup(&format!("<s>− {escaped}</s>"));
                chip.add_css_class("assistant-removed");
                chip.set_tooltip_text(Some(&i18n("Taken out")));
            } else {
                chip.set_text(&format!("+ {text}"));
                let flagged = bcc || outside(&addr, &self.own_domains);
                chip.set_tooltip_text(Some(&if bcc {
                    i18n("Added in Bcc: the other recipients will not see this address")
                } else if flagged {
                    i18n("Added, outside your own domains")
                } else {
                    i18n("Added")
                }));
                chip.add_css_class(if flagged {
                    "warning"
                } else {
                    "assistant-added"
                });
            }
            flow.insert(&chip, -1);
        }
        line.append(&flow);
        Some(line)
    }
}

/// What an edit does to one recipient.
#[derive(Debug, PartialEq)]
enum Change {
    Added,
    Kept,
    Removed,
}

/// A field's recipients as an edit leaves them, set against those in it
/// `now`: the new list in its order, each kept or added, then those taken
/// out. Addresses are matched in any case. Each is (as shown, address).
fn recipient_changes(now: &str, new: &[String]) -> Vec<(String, String, Change)> {
    let parse = |s: &str| crate::worker::parse_recipients(s);
    let before = parse(now);
    let had = |addr: &str| before.iter().any(|(_, a)| a.eq_ignore_ascii_case(addr));
    let after: Vec<(String, String)> = new.iter().flat_map(|r| parse(r)).collect();
    let keeps = |addr: &str| after.iter().any(|(_, a)| a.eq_ignore_ascii_case(addr));
    let shown = |name: &str, addr: &str| crate::worker::format_recipient(name, addr);
    let mut out: Vec<(String, String, Change)> = after
        .iter()
        .map(|(name, addr)| {
            let change = if had(addr) {
                Change::Kept
            } else {
                Change::Added
            };
            (shown(name, addr), addr.clone(), change)
        })
        .collect();
    out.extend(
        before
            .iter()
            .filter(|(_, addr)| !keeps(addr))
            .map(|(name, addr)| (shown(name, addr), addr.clone(), Change::Removed)),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_shows_who_it_adds_keeps_and_takes_out() {
        let changes = recipient_changes(
            "Ada <ada@example.org>, bo@example.com",
            &[
                "ADA@example.org".to_string(),
                "Cy <cy@example.net>".to_string(),
            ],
        );
        let got: Vec<(&str, &Change)> = changes.iter().map(|(t, _, c)| (t.as_str(), c)).collect();
        assert_eq!(
            got,
            [
                ("ADA@example.org", &Change::Kept),
                ("Cy <cy@example.net>", &Change::Added),
                ("bo@example.com", &Change::Removed),
            ]
        );
        // Emptied: everyone goes.
        let gone = recipient_changes("bo@example.com", &[]);
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].2, Change::Removed);
        assert!(recipient_changes("", &[]).is_empty());
    }

    #[test]
    fn what_plain_text_takes_away_is_noticed() {
        for html in [
            "<div>See <a href=\"https://x.org\">this</a></div>",
            "<div><b>Now</b></div>",
            "<p><IMG src=\"cid:1\"></p>",
            "<ul><li>one</li></ul>",
            "<div><span style=\"color: red\">red</span></div>",
        ] {
            assert!(plain_text_loses(html), "{html}");
        }
        for html in [
            "",
            "just words, 1 < 2",
            "<div>one</div><div><br></div>",
            "<p class=\"x\">two</p>",
            "<div style=\"font-family: Inter; font-size: 11pt\">typed</div>",
            "<article>not an a</article><span>nor a b</span>",
        ] {
            assert!(!plain_text_loses(html), "{html}");
        }
    }

    #[test]
    fn the_text_goes_into_the_scripts_as_a_literal() {
        // Whatever the assistant wrote stays one string: no placeholder in
        // it is filled, and no quote in it ends the literal.
        let text = "it's \"@W@\" </script> @ARGS@";
        let script = APPLY_RICH
            .replace("@W@", RICH_WRITTEN)
            .replace("@ARGS@", &js(&crate::mcp::text_html(text)));
        assert_eq!(script.matches("function __w").count(), 1);
        assert!(script.ends_with(&format!("({})", js(&crate::mcp::text_html(text)))));
        assert!(!script.contains("</script>"));
    }
}
