//! The composing formats (rich, Markdown, HTML, plain), switching between them,
//! and the body's zoom.

use super::messages::ComposeInput;
use super::Compose;
use crate::config::ComposeFormat;
use crate::i18n::{i18n, i18n_f};
use crate::ui::context_menu::show_context_menu;
use crate::ui::context_menu::MenuEntry;
use crate::ui::rich_editor::SourceKind;
use adw::prelude::*;
use relm4::prelude::*;

/// A body as Markdown, keeping the empty line a reply opens with above its
/// quote and signature. The conversion drops leading blank lines, which
/// left the caret at the start of the "On … wrote:" line, so whatever was
/// typed joined it.
pub(super) fn markdown_body(html: &str) -> String {
    let md = crate::mail::markdown::from_html(html);
    let top = html.trim_start();
    if !md.is_empty() && (top.starts_with("<p><br></p>") || top.starts_with("<div><br></div>")) {
        format!("\n\n{md}")
    } else {
        md
    }
}

/// The icon a composing format carries, in the header and in its menu.
pub(super) fn format_icon(format: ComposeFormat) -> &'static str {
    match format {
        ComposeFormat::Rich => "format-text-rich-symbolic",
        ComposeFormat::Markdown => "markdown-symbolic",
        ComposeFormat::Html => "code-symbolic",
        ComposeFormat::Plain => "text-x-generic-symbolic",
    }
}

/// What a format is called where the user meets it.
pub(super) fn format_label(format: ComposeFormat) -> String {
    match format {
        ComposeFormat::Rich => i18n("Rich text"),
        ComposeFormat::Markdown => i18n("Markdown"),
        ComposeFormat::Html => i18n("HTML source"),
        ComposeFormat::Plain => i18n("Plain text"),
    }
}

/// The one-line explanation under each format in the menu.
fn format_hint(format: ComposeFormat) -> String {
    match format {
        ComposeFormat::Rich => i18n("Format as you type; it is sent as HTML"),
        ComposeFormat::Markdown => i18n("Write Markdown; it is sent as formatted mail"),
        ComposeFormat::Html => i18n("Write the message's HTML by hand"),
        ComposeFormat::Plain => i18n("No formatting at all"),
    }
}

/// The zoom of the body, in percent: the session's, so the next composer
/// opens at the size the last one was left at. 0 until the first change,
/// which reads as Settings → Composing → Default zoom, itself the reader's
/// unless set: a message written at one size and read at another looked
/// shrunk in the writing (#535).
static ZOOM: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The body's zoom now; see [`ZOOM`].
pub(super) fn session_zoom() -> u32 {
    match ZOOM.load(std::sync::atomic::Ordering::Relaxed) {
        0 => crate::config::load_compose_zoom(),
        z => z,
    }
}

/// Back to the default from Settings for the next composer.
pub(crate) fn forget_zoom() {
    ZOOM.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// The body's zoom as a chip at the start of the format bar, shown only
/// away from 100%: in rich text the size picker names the size that is
/// sent, and the chip is what says the text on screen is not at it. A
/// click goes back to 100%, as Ctrl+0 does.
pub(super) fn zoom_chip(sender: &ComponentSender<Compose>) -> gtk::Label {
    let chip = gtk::Label::new(None);
    chip.set_valign(gtk::Align::Center);
    chip.add_css_class("caption");
    chip.add_css_class("hylki-zoom-chip");
    chip.set_cursor_from_name(Some("pointer"));
    chip.set_tooltip_text(Some(
        i18n("Zoom of the text on screen, not of the message sent. A click here, or Ctrl+0, puts it back to 100%.")
            .as_str(),
    ));
    let click = gtk::GestureClick::new();
    let s = sender.input_sender().clone();
    click.connect_released(move |_, _, _, _| {
        let _ = s.send(ComposeInput::Zoom(0));
    });
    chip.add_controller(click);
    show_zoom(&chip, session_zoom());
    chip
}

pub(super) fn show_zoom(chip: &gtk::Label, zoom: u32) {
    chip.set_label(&format!("{zoom}%"));
    chip.set_visible(zoom != 100);
}

/// What HYLKI_SHOWCASE_PLAIN_LOG reports about the body (demo only).
pub(super) const SHOWCASE_PLAIN_LOG: &str = r#"JSON.stringify({plain:!!window.__hylkiPlain,
font:document.body?getComputedStyle(document.body).fontFamily:'',
bold:document.body&&document.body.querySelector('b,strong')?getComputedStyle(document.body.querySelector('b,strong')).fontWeight:'',
html:document.body?document.body.innerHTML:document.documentElement.outerHTML.slice(0,400),
text:window.__hylkiBodyText?window.__hylkiBodyText():''})"#;

impl Compose {
    /// The format chooser's icon and tooltip for the format it is currently
    /// set to, and whether the preview toggle can afford its label.
    pub(super) fn dress_format_buttons(&self) {
        self.format_btn.set_icon_name(format_icon(self.format));
        self.format_btn.set_tooltip_text(Some(
            i18n_f(
                "Writing in {format}",
                &[("format", &format_label(self.format))],
            )
            .as_str(),
        ));
        // The one label on the row goes when there is no width for it, the
        // same trade the header makes when it folds.
        let preview = i18n("Preview");
        self.preview_content
            .set_label(if self.narrow { "" } else { preview.as_str() });
    }

    /// The four formats as menu entries, the current one wearing a tick in
    /// place of its icon (as the OpenPGP toggles do).
    fn format_entries(&self, sender: &ComponentSender<Self>) -> Vec<MenuEntry> {
        [
            ComposeFormat::Rich,
            ComposeFormat::Markdown,
            ComposeFormat::Html,
            ComposeFormat::Plain,
        ]
        .into_iter()
        .map(|format| {
            let s = sender.clone();
            let label = format!("{} — {}", format_label(format), format_hint(format));
            MenuEntry::new(&label, move || {
                s.input_sender().emit(ComposeInput::SetFormat(format))
            })
            .icon(format_icon(format))
            .selected(format == self.format)
        })
        .collect()
    }

    /// Handles [`ComposeInput::Zoom`].
    pub(super) fn on_zoom(&mut self, step: i8) {
        use crate::config::READER_ZOOM_STEPS as STEPS;
        let now = session_zoom();
        let at = STEPS.iter().position(|&z| z == now).unwrap_or(5) as i32;
        let zoom = match step {
            0 => 100,
            s => STEPS[(at + i32::from(s.signum())).clamp(0, STEPS.len() as i32 - 1) as usize],
        };
        ZOOM.store(zoom, std::sync::atomic::Ordering::Relaxed);
        self.editor.set_zoom(zoom);
        show_zoom(&self.zoom_chip, zoom);
    }

    /// Handles [`ComposeInput::FormatMenu`].
    pub(super) fn on_format_menu(&mut self, sender: &ComponentSender<Self>) {
        let btn = &self.format_btn;
        show_context_menu(
            btn,
            (btn.width() / 2) as f64,
            btn.height() as f64,
            vec![self.format_entries(sender)],
        );
    }

    /// Handles [`ComposeInput::SetFormat`].
    pub(super) fn on_set_format(&mut self, to: ComposeFormat, sender: &ComponentSender<Self>) {
        if to == self.format {
            return;
        }
        // Plain text shows only what is sent (#424), so moving a
        // formatted body there drops what it cannot show, and the
        // way back does not bring it back.
        if self.format == ComposeFormat::Rich && to == ComposeFormat::Plain {
            let s = sender.clone();
            self.editor.eval(
                "String(!!(window.__hylkiPlainLoses&&window.__hylkiPlainLoses()))",
                move |loses| {
                    s.input_sender().emit(if loses == "true" {
                        ComposeInput::AskPlain
                    } else {
                        ComposeInput::ApplyFormat(ComposeFormat::Plain)
                    });
                },
            );
            return;
        }
        sender.input_sender().emit(ComposeInput::ApplyFormat(to));
    }

    /// Handles [`ComposeInput::AskPlain`].
    pub(super) fn on_ask_plain(
        &mut self,
        sender: &ComponentSender<Self>,
        root: &adw::BreakpointBin,
    ) {
        let dialog = adw::AlertDialog::new(
            Some(&i18n("Switch to plain text?")),
            Some(&i18n("Formatting, images and links are removed.")),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("switch", &i18n("Switch"));
        dialog.set_response_appearance("switch", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let s = sender.input_sender().clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "switch" {
                let _ = s.send(ComposeInput::ApplyFormat(ComposeFormat::Plain));
            }
        });
        dialog.present(Some(root));
        // HYLKI_SHOWCASE_PLAIN_ANSWER=switch|cancel answers it two
        // seconds on (demo only), for a check without a pointer.
        if let (Ok(answer), Some(_)) = (
            std::env::var("HYLKI_SHOWCASE_PLAIN_ANSWER"),
            std::env::var_os("HYLKI_DEMO"),
        ) {
            tracing::info!("showcase plain: asked before switching");
            let (d, s) = (dialog.clone(), sender.input_sender().clone());
            gtk::glib::timeout_add_seconds_local_once(2, move || {
                d.force_close();
                if answer == "switch" {
                    let _ = s.send(ComposeInput::ApplyFormat(ComposeFormat::Plain));
                }
            });
        }
    }

    /// Handles [`ComposeInput::ApplyFormat`].
    pub(super) fn on_apply_format(&mut self, to: ComposeFormat, sender: &ComponentSender<Self>) {
        let from = self.format;
        if to == from {
            return;
        }
        self.format = to;
        self.editor
            .set_formatting_visible(to == ComposeFormat::Rich);
        if let Some(zones) = &self.drop_zones {
            zones.set_allow_inline(to == ComposeFormat::Rich);
        }
        self.dress_format_buttons();
        self.preview_btn.set_visible(to.is_source());
        // A preview of the old format's render would be a lie about
        // the new one.
        self.preview_btn.set_active(false);
        // Every change reloads the body, plain text included: it
        // shows the message flattened to what is sent (#424), so it
        // cannot share the rich document's undo history.
        let s = sender.clone();
        self.read_body_as(from, false, move |body, _| {
            s.input_sender()
                .emit(ComposeInput::LoadAs { from, to, body });
        });
    }

    /// Handles [`ComposeInput::LoadAs`].
    pub(super) fn on_load_as(&mut self, from: ComposeFormat, to: ComposeFormat, body: String) {
        // Every conversion goes through HTML: it is the one form
        // all four formats can be written to and read back from.
        let html = match from {
            ComposeFormat::Markdown => crate::mail::markdown::to_html(&body),
            _ => body,
        };
        self.editor.set_plain(to == ComposeFormat::Plain);
        match to {
            ComposeFormat::Markdown => self
                .editor
                .set_source(SourceKind::Markdown, &markdown_body(&html)),
            ComposeFormat::Html => self
                .editor
                .set_source(SourceKind::Html, &crate::mail::markdown::pretty_html(&html)),
            ComposeFormat::Rich | ComposeFormat::Plain => self.editor.set_html(&html),
        }
        self.editor.grab_focus();
        // HYLKI_SHOWCASE_PLAIN_LOG logs the reloaded body, its font
        // and the text it sends (demo only).
        if std::env::var_os("HYLKI_SHOWCASE_PLAIN_LOG").is_some()
            && std::env::var_os("HYLKI_DEMO").is_some()
        {
            let ed = self.editor.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
                ed.eval(SHOWCASE_PLAIN_LOG, move |v| {
                    tracing::info!("showcase plain: {to:?} {v}")
                });
            });
        }
    }

    /// Handles [`ComposeInput::TogglePreview`].
    pub(super) fn on_toggle_preview(&mut self, on: bool, sender: &ComponentSender<Self>) {
        // Keep the button with the state, for the times the message
        // arrives from somewhere other than a click on it.
        if self.preview_btn.is_active() != on {
            self.preview_btn.set_active(on);
        }
        if !on {
            self.editor.show_preview(None);
            return;
        }
        let s = sender.clone();
        self.read_body_as(self.format, false, move |body, _| {
            s.input_sender().emit(ComposeInput::ShowPreview(body));
        });
    }
}
