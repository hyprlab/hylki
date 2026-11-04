//! An AI assistant at an open composer (`crate::mcp`): what it reads of
//! the message (read_draft), and an edit it asks for (update_draft), shown
//! in a bar over the composer with everything it would change until the
//! user applies or rejects it. Nothing changes before. Applied, the fields
//! take the new values, and the new text goes in place of what is written
//! above the signature and any quote, as one edit Undo takes back; the
//! signature and the quote stay as they are.

use adw::prelude::*;
use relm4::prelude::*;

use super::assistant::{fill_banner, outside};
use super::messages::ComposeInput;
use super::signature::sig_source;
use super::{Compose, ComposeWidgets};
use crate::config::ComposeFormat;
use crate::i18n::i18n;
use crate::mcp::{DraftEdit, EditOutcome, OpenDraft};
use crate::ui::dialog::alert;
use crate::ui::rich_editor::SourceKind;

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

/// A value as a JavaScript literal.
fn js(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "''".into())
}

impl Compose {
    /// Handles [`ComposeInput::AssistantRead`]: the fields at once, and
    /// with `text` what is written, once the editor hands it over. A
    /// message that is to go out encrypted keeps its text on this
    /// computer, as it does from translation and the grammar check.
    pub(super) fn on_assistant_read(
        &self,
        text: bool,
        done: tokio::sync::oneshot::Sender<OpenDraft>,
        widgets: &ComposeWidgets,
    ) {
        let mut draft = self.open_draft(widgets);
        if !text || self.encrypt {
            let _ = done.send(draft);
            return;
        }
        match self.editor.source_kind() {
            Some(kind) => {
                let js_text = READ_SOURCE
                    .replace("@E@", SOURCE_WRITTEN)
                    .replace("@ARGS@", &self.source_args(kind));
                self.editor.eval(&js_text, move |written| {
                    draft.text = Some(match kind {
                        SourceKind::Html => crate::cache::body_text(&written).trim().to_string(),
                        SourceKind::Markdown => written,
                    });
                    let _ = done.send(draft);
                });
            }
            None => {
                self.editor
                    .eval(&READ_RICH.replace("@W@", RICH_WRITTEN), move |html| {
                        draft.text = Some(crate::cache::body_text(&html).trim().to_string());
                        let _ = done.send(draft);
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
            encrypted: self.encrypt,
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
        self.fill_edit_bar(&edit, widgets, sender);
        self.edit_bar.set_visible(true);
        self.assistant_edit = Some(edit);
    }

    /// Handles [`ComposeInput::RejectAssistantEdit`].
    pub(super) fn on_reject_assistant_edit(&mut self) {
        if self.assistant_edit.take().is_some() {
            self.last_edit = Some(EditOutcome::Rejected);
        }
        self.edit_bar.set_visible(false);
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
                &i18n("Wait for the translation to come back, then apply the changes."),
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
        if let Some(text) = &edit.text {
            self.put_assistant_text(text);
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

    /// `text` in place of what is written above the signature and quote.
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
        self.editor.run_js(&script);
    }

    /// The bar's contents for `edit`: every field it changes, recipients
    /// set against those there now, then Reject and Apply.
    fn fill_edit_bar(
        &self,
        edit: &DraftEdit,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        let bar = &self.edit_bar;
        while let Some(child) = bar.first_child() {
            bar.remove(&child);
        }
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        head.append(&gtk::Image::from_icon_name("document-edit-symbolic"));
        let title = gtk::Label::new(Some(&i18n(
            "An AI assistant suggests changes to this message. Nothing changes until you apply them.",
        )));
        title.set_wrap(true);
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.add_css_class("heading");
        head.append(&title);
        bar.append(&head);

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
            bar.append(&self.recipient_change(&label, bcc, list, &row.text()));
        }
        if let Some(subject) = &edit.subject {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let field = gtk::Label::new(Some(&i18n("Subject:")));
            field.add_css_class("dim-label");
            field.set_valign(gtk::Align::Start);
            line.append(&field);
            let value = gtk::Label::new(Some(subject));
            value.set_wrap(true);
            value.set_xalign(0.0);
            value.set_selectable(true);
            line.append(&value);
            bar.append(&line);
        }
        if let Some(text) = &edit.text {
            let note = gtk::Label::new(Some(&if text.trim().is_empty() {
                i18n("Takes out what is written above the signature and the quoted message.")
            } else {
                i18n("New text, in place of what is written above the signature and the quoted message:")
            }));
            note.set_wrap(true);
            note.set_xalign(0.0);
            note.add_css_class("dim-label");
            bar.append(&note);
            if !text.trim().is_empty() {
                let body = gtk::Label::new(Some(text));
                body.set_wrap(true);
                body.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                body.set_xalign(0.0);
                body.set_yalign(0.0);
                body.set_selectable(true);
                let scroll = gtk::ScrolledWindow::new();
                scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
                scroll.set_propagate_natural_height(true);
                scroll.set_max_content_height(180);
                scroll.set_child(Some(&body));
                bar.append(&scroll);
            }
        }

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        let reject = gtk::Button::with_label(&i18n("Reject"));
        let s = sender.input_sender().clone();
        reject.connect_clicked(move |_| s.emit(ComposeInput::RejectAssistantEdit));
        let apply = gtk::Button::with_label(&i18n("Apply"));
        apply.add_css_class("suggested-action");
        let s = sender.input_sender().clone();
        apply.connect_clicked(move |_| s.emit(ComposeInput::ApplyAssistantEdit));
        buttons.append(&reject);
        buttons.append(&apply);
        bar.append(&buttons);
    }

    /// One recipient field as the edit would leave it: those it adds, those
    /// it keeps, and those it takes out, struck through. Additions outside
    /// the user's domains, and all of Bcc, are set apart as on the banner.
    fn recipient_change(&self, label: &str, bcc: bool, new: &[String], now: &str) -> gtk::Box {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let field = gtk::Label::new(Some(label));
        field.set_valign(gtk::Align::Start);
        field.add_css_class(if bcc { "warning" } else { "dim-label" });
        line.append(&field);
        let flow = gtk::FlowBox::new();
        flow.set_selection_mode(gtk::SelectionMode::None);
        flow.set_hexpand(true);
        flow.set_max_children_per_line(8);
        flow.set_column_spacing(6);
        flow.set_row_spacing(2);
        let changes = recipient_changes(now, new);
        if changes.is_empty() {
            let none = gtk::Label::new(Some(&i18n("No one")));
            none.add_css_class("dim-label");
            flow.insert(&none, -1);
        }
        for (text, addr, change) in changes {
            let chip = gtk::Label::new(None);
            chip.set_xalign(0.0);
            let escaped = gtk::glib::markup_escape_text(&text);
            match change {
                Change::Removed => {
                    chip.set_markup(&format!("<s>{escaped}</s>"));
                    chip.add_css_class("dim-label");
                    chip.set_tooltip_text(Some(&i18n("Taken out")));
                }
                Change::Added => {
                    chip.set_markup(&format!("<b>+ {escaped}</b>"));
                    let flagged = bcc || outside(&addr, &self.own_domains);
                    chip.set_tooltip_text(Some(&if bcc {
                        i18n("Added in Bcc: the other recipients will not see this address")
                    } else if flagged {
                        i18n("Added, outside your own domains")
                    } else {
                        i18n("Added")
                    }));
                    if flagged {
                        chip.add_css_class("warning");
                    }
                }
                Change::Kept => chip.set_text(&text),
            }
            flow.insert(&chip, -1);
        }
        line.append(&flow);
        line
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
