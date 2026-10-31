//! A small reusable WYSIWYG rich-text editor: a `contentEditable` WebView with a
//! formatting toolbar. JavaScript runs only in this (our own) document, to drive
//! editing commands. Used for the compose body and the account signature.

use super::tool_flow::ToolFlow;
use adw::prelude::*;
use webkit6::prelude::WebViewExt;

mod colors;
mod context_menu;
mod documents;
mod files;
mod fonts;
mod grammar;
mod scripts;
mod setup;
mod signature;
mod spelling;
mod toolbar;

pub(crate) use documents::remote_image_urls;
use documents::{document, preview_document, source_document};
pub(crate) use files::is_inline_image;
use files::{deliver_files, paste_into};
pub(crate) use fonts::{choose_family, compose_font_choices, preview_font, recent_font_css, SIZES};
pub(crate) use signature::{signature_from_source, signature_to_html};
pub(crate) use spelling::apply_spellcheck;

/// A rich-text editor widget. Add `widget` to a container; read the content back
/// with [`RichEditor::extract_html`] (asynchronous, since it queries the WebView).
/// Clones share the same underlying widgets (GObject references).
#[derive(Clone)]
pub(super) struct RichEditor {
    /// The toolbar + editor, ready to be placed in a container.
    pub(super) widget: gtk::Box,
    webview: webkit6::WebView,
    /// The formatting commands, hidden while a message is composed as plain
    /// text (#180) or as source — the row they sit in stays either way.
    toolbar_commands: ToolFlow,
    /// The far end of that row, where a host puts controls of its own (the
    /// composer's format chooser and preview toggle).
    toolbar_end: gtk::Box,
    /// Swaps the editor for the rendered preview of a source message.
    stack: gtk::Stack,
    /// The preview view, built the first time a preview is asked for: a
    /// second WebView per composer is not something to open speculatively
    /// (issue #106).
    preview: std::rc::Rc<std::cell::RefCell<Option<webkit6::WebView>>>,
    /// Which source language the editor currently holds, if it is in source
    /// mode rather than rich mode.
    source: std::rc::Rc<std::cell::Cell<Option<SourceKind>>>,
    /// Whether the rich document is showing the message as plain text
    /// (#424): flattened to what is sent, in the monospace font, taking
    /// only text. Read by every load of the document and by whatever puts
    /// files or the clipboard into it.
    plain: std::rc::Rc<std::cell::Cell<bool>>,
    /// Where "Send as Attachment Instead" delivers the lifted image, as a
    /// temp-file path the host adds to its attachment list. Set by the host
    /// via [`RichEditor::connect_send_as_attachment`].
    attach_cb: AttachCallback,
    /// Whether the document is there to take files, and the ones waiting
    /// for it ([`RichEditor::insert_files`]).
    loaded: std::rc::Rc<std::cell::Cell<bool>>,
    pending_files: std::rc::Rc<std::cell::RefCell<Vec<std::path::PathBuf>>>,
    /// What the document's own text history can do right now, mirrored from
    /// WebKit's editor state so a host can read it without a round trip
    /// (#200 in the composer). `(undo, redo)`.
    text_history: std::rc::Rc<std::cell::Cell<(bool, bool)>>,
    /// Told whenever that pair changes, so a host can fold the body's text
    /// history into an undo stack of its own.
    history_cb: HistoryCallback,
    /// Where the right-click menu's Spelling Language choice goes, as the
    /// whole new setting (#365). The submenu is only offered once a host
    /// has connected it ([`RichEditor::connect_spell_languages`]).
    spell_langs_cb: SpellLangsCallback,
    /// Grammar checking (#452), off until a host turns it on with
    /// [`RichEditor::check_grammar`].
    grammar: grammar::GrammarState,
    /// The style manager's dark-notify handler that re-grounds the document
    /// on a live theme flip; disconnected when the last clone of the editor
    /// goes (the editor is a cloneable handle, so the guard is shared).
    _theme_handler: std::rc::Rc<ThemeHandlerGuard>,
    /// The Content-Security-Policy `<meta>` every document this editor loads
    /// carries, or empty when any picture may load. See [`RichEditor::new`].
    image_policy: std::rc::Rc<str>,
    /// The page zoom set by [`RichEditor::set_zoom`], kept for a preview
    /// built after it.
    zoom: std::rc::Rc<std::cell::Cell<f64>>,
}

impl Drop for RichEditor {
    /// The last handle takes the web process with it. A WebKit view keeps
    /// internal references to itself after it is unparented (measured: two
    /// on a bare view, one here), so it is never finalized by its owners
    /// letting go, and its WebKitWebProcess — a few hundred MB, invisible
    /// to the Flatpak sandbox's process tree — lived on for every composer
    /// ever closed (#221: six of them in one user's session monitor).
    /// Deferred past the composer's slide-out so the document does not
    /// blank mid-animation.
    fn drop(&mut self) {
        if std::rc::Rc::strong_count(&self._theme_handler) != 1 {
            return; // another handle still uses the view
        }
        let views: Vec<webkit6::WebView> = std::iter::once(self.webview.clone())
            .chain(self.preview.borrow().clone())
            .collect();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
            for v in &views {
                crate::desktop::memory_report::release_web_view(v);
            }
        });
    }
}

/// What a source-mode editor is holding. Rich text is the absence of
/// both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SourceKind {
    Markdown,
    Html,
}

/// Where a lifted image or dropped file goes when it becomes an attachment.
type AttachCallback = std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(std::path::PathBuf)>>>>;
/// Told `(can_undo, can_redo)` when the body's text history changes.
type HistoryCallback = std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(bool, bool)>>>>;
type SpellLangsCallback = std::rc::Rc<std::cell::RefCell<Option<Box<dyn Fn(String)>>>>;

/// Disconnects a style-manager handler on drop.
struct ThemeHandlerGuard(Option<gtk::glib::SignalHandlerId>);

impl Drop for ThemeHandlerGuard {
    fn drop(&mut self) {
        if let Some(id) = self.0.take() {
            adw::StyleManager::default().disconnect(id);
        }
    }
}

impl RichEditor {
    /// Step the document's own text history back (`redo` false) or forward.
    /// Asynchronous like every other page command: [`RichEditor::can_undo`]
    /// only catches up when WebKit reports the new state.
    pub(crate) fn undo(&self) {
        self.webview
            .execute_editing_command(webkit6::EDITING_COMMAND_UNDO);
    }

    pub(crate) fn redo(&self) {
        self.webview
            .execute_editing_command(webkit6::EDITING_COMMAND_REDO);
    }

    /// Run any WebCore editing command against the document ("DeleteBackward",
    /// "InsertParagraph", …). The widget API is the only route to them that
    /// reaches the page's own history — the same reason [`RichEditor::undo`]
    /// cannot go through `execCommand`.
    pub(super) fn editing_command(&self, command: &str) {
        self.webview.execute_editing_command(command);
    }

    /// Whether the body has anything left to take back. Read from the
    /// mirrored editor state, so it is a main-loop pass or two behind a
    /// command just issued — a host must treat "still true" right after
    /// [`RichEditor::undo`] as "ask again", not as a miscount.
    pub(super) fn can_undo(&self) -> bool {
        self.text_history.get().0
    }

    pub(super) fn can_redo(&self) -> bool {
        self.text_history.get().1
    }

    /// Called with `(can_undo, can_redo)` whenever the body's text history
    /// changes: an edit, a command, or a reload that drops it.
    pub(super) fn connect_history_changed(&self, f: impl Fn(bool, bool) + 'static) {
        *self.history_cb.borrow_mut() = Some(Box::new(f));
    }

    /// Offer the Spelling Language submenu, and receive the setting it
    /// makes: the comma-separated language list, empty for the system
    /// language (#365).
    pub(super) fn connect_spell_languages(&self, f: impl Fn(String) + 'static) {
        *self.spell_langs_cb.borrow_mut() = Some(Box::new(f));
    }

    /// Check the grammar and style of what is written, when Settings has a
    /// LanguageTool server for it (#452): the composer's body, not a
    /// signature.
    pub(super) fn check_grammar(&self) {
        grammar::switch_on(&self.webview, &self.grammar);
    }

    /// Send nothing for checking while `on`: a message that is to go out
    /// encrypted keeps its text on this computer, as it does from
    /// translation.
    pub(super) fn pause_grammar(&self, on: bool) {
        grammar::pause(&self.webview, &self.grammar, on);
    }

    /// What "Send as Attachment Instead" does with the lifted image: the
    /// host receives the temp file's path and adds it to its attachments.
    pub(super) fn connect_send_as_attachment(&self, f: impl Fn(std::path::PathBuf) + 'static) {
        *self.attach_cb.borrow_mut() = Some(Box::new(f));
    }

    /// Replace the editor contents with `content` (HTML), in rich mode, or
    /// flattened to plain text when [`RichEditor::set_plain`] says so.
    pub(super) fn set_html(&self, content: &str) {
        self.source.set(None);
        self.show_preview(None);
        crate::desktop::web_fonts::load_html(
            &self.webview,
            &document(content, &self.webview, &self.image_policy, self.plain.get()),
            Some("https://hylki.localhost/editor"),
        );
    }

    /// Put the editor into source mode holding `text`: a monospace
    /// plain-text surface with no formatting toolbar, which the composer
    /// converts on the way out.
    pub(super) fn set_source(&self, kind: SourceKind, text: &str) {
        self.source.set(Some(kind));
        self.show_preview(None);
        crate::desktop::web_fonts::load_html(
            &self.webview,
            &source_document(text, &self.webview),
            Some("https://hylki.localhost/editor"),
        );
    }

    /// Show the message as plain text from the next [`RichEditor::set_html`]
    /// on: the document flattens itself to the text that is sent, in the
    /// monospace font, and pastes and drops bring in text alone (#424).
    pub(super) fn set_plain(&self, on: bool) {
        self.plain.set(on);
    }

    /// The source language the editor is in, or `None` in rich mode.
    pub(super) fn source_kind(&self) -> Option<SourceKind> {
        self.source.get()
    }

    /// Read the source text back, exactly as typed.
    pub(super) fn extract_source(&self, cb: impl FnOnce(String) + 'static) {
        self.eval(
            "window.__hylkiSourceText ? window.__hylkiSourceText() : ''",
            cb,
        );
    }

    /// Show `html` rendered in place of the editor, or go back to editing.
    /// The preview is read-only and runs no scripts — it is a look at the
    /// message, not a second editor.
    pub(super) fn show_preview(&self, html: Option<&str>) {
        let Some(html) = html else {
            self.stack.set_visible_child_name("edit");
            return;
        };
        // Taken out first: a `match` on `borrow()` holds that borrow for the
        // whole match, and the arm below needs `borrow_mut()`.
        let existing = self.preview.borrow().clone();
        let view = match existing {
            Some(v) => v,
            None => {
                let settings = webkit6::Settings::new();
                settings.set_enable_javascript(false);
                settings.set_enable_developer_extras(false);
                let v = webkit6::WebView::builder()
                    .web_context(&super::message_view::shared_web_context())
                    .settings(&settings)
                    .build();
                crate::desktop::memory_report::register_web_view(&v, "editor preview");
                super::message_view::keep_tab_out_of_key_translator(&v);
                v.set_zoom_level(self.zoom.get());
                v.set_background_color(&gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
                self.stack.add_named(&v, Some("preview"));
                *self.preview.borrow_mut() = Some(v.clone());
                v
            }
        };
        crate::desktop::web_fonts::load_html(
            &view,
            &preview_document(html, &view, &self.image_policy),
            Some("https://hylki.localhost/preview"),
        );
        self.stack.set_visible_child_name("preview");
    }

    pub(crate) fn grab_focus(&self) {
        self.webview.grab_focus();
        if self.source.get().is_some() {
            // The document's own field is what takes the caret.
            exec(
                &self.webview,
                "var t=document.getElementById('src');if(t)t.focus();",
            );
        } else {
            exec(&self.webview, PLACE_CARET);
        }
    }

    /// Run a JavaScript snippet against the editor document (e.g. to swap the
    /// signature block when the From account changes).
    pub(super) fn run_js(&self, js: &str) {
        exec(&self.webview, js);
    }

    /// Run a snippet and hand back what it returns, as text ("" when it
    /// failed).
    pub(super) fn eval(&self, js: &str, cb: impl FnOnce(String) + 'static) {
        self.webview
            .evaluate_javascript(js, None, None, gtk::gio::Cancellable::NONE, move |res| {
                cb(res.map(|v| v.to_str().to_string()).unwrap_or_default())
            });
    }

    /// Whether keyboard focus is currently inside the editor's WebView — the
    /// guard a host's paste shortcut uses so it never hijacks Ctrl+V aimed at
    /// an address entry.
    pub(super) fn has_focus(&self) -> bool {
        self.webview
            .root()
            .and_then(|r| r.focus())
            .is_some_and(|f| {
                f == *self.webview.upcast_ref::<gtk::Widget>() || f.is_ancestor(&self.webview)
            })
    }

    /// Put the pictures among `paths` in the text at the caret, and hand
    /// the rest to the attachment list, as a drop on the text does. Before
    /// the document has loaded they wait for it.
    pub(super) fn insert_files(&self, paths: &[std::path::PathBuf]) {
        if !self.loaded.get() {
            self.pending_files
                .borrow_mut()
                .extend(paths.iter().cloned());
            return;
        }
        let files: Vec<_> = paths.iter().map(gtk::gio::File::for_path).collect();
        deliver_files(
            &self.webview,
            &files,
            None,
            &self.attach_cb,
            !self.plain.get(),
        );
    }

    /// Paste the clipboard into the editor, keeping (`rich`) or stripping the
    /// clipboard's formatting for this one paste, whatever the standing
    /// preference says.
    pub(super) fn paste(&self, rich: bool) {
        paste_into(&self.webview, rich, &self.attach_cb, self.plain.get());
    }

    /// Whether the body has been edited since it was loaded (async read of a JS
    /// flag set on the first `input` event). Used to avoid saving a pristine,
    /// quote-only reply to Drafts when the reader navigates away.
    pub(super) fn is_dirty(&self, cb: impl FnOnce(bool) + 'static) {
        self.eval("String(!!window.__hylkiDirty)", move |v| cb(v == "true"));
    }

    /// Show or hide the formatting commands: hidden while the message is
    /// composed as plain text (#180) or as source, where they would go
    /// nowhere. The row itself stays, carrying whatever the host put at its
    /// end — a chooser that vanished with the buttons would be the one
    /// control needed to get them back.
    pub(super) fn set_formatting_visible(&self, on: bool) {
        self.toolbar_commands.set_tools_shown(on);
    }

    /// Show the text, and the preview, at `percent` of its size. Page zoom
    /// rather than a font size, so nothing about it reaches the message.
    pub(crate) fn set_zoom(&self, percent: u32) {
        let z = f64::from(percent) / 100.0;
        self.zoom.set(z);
        self.webview.set_zoom_level(z);
        if let Some(v) = self.preview.borrow().as_ref() {
            v.set_zoom_level(z);
        }
    }

    /// Keep the text area at least `height` pixels tall, whatever the
    /// toolbar above it takes: a host with a fixed-size slot would
    /// otherwise lose text to a wrapped or expanded toolbar.
    pub(super) fn set_text_height(&self, height: i32) {
        self.stack.set_height_request(height);
    }

    /// The far end of the formatting row, for a host's own controls.
    pub(super) fn toolbar_end(&self) -> &gtk::Box {
        &self.toolbar_end
    }

    /// Read the current body HTML asynchronously.
    pub(super) fn extract_html(&self, cb: impl FnOnce(String) + 'static) {
        self.eval("window.__hylkiBodyHtml()", cb);
    }

    /// Read the body for *sending*: any picture armed with "Recompress to
    /// This Size on Send" is recut to the size it is drawn at first. Drafts
    /// go through [`RichEditor::extract`], which never touches the pixels —
    /// a save must not cost quality.
    pub(super) fn extract_for_send(&self, cb: impl FnOnce(String, String) + 'static) {
        self.read_body("window.__hylkiBodyHtmlForSend()", cb);
    }

    /// Read the current body HTML and a plain-text rendering asynchronously.
    pub(crate) fn extract(&self, cb: impl FnOnce(String, String) + 'static) {
        self.read_body("window.__hylkiBodyHtml()", cb);
    }

    fn read_body(&self, reader: &str, cb: impl FnOnce(String, String) + 'static) {
        // JSON, not a separator: the value comes back as a C string, so a NUL
        // between the halves cut the text off and plain-text mail went out
        // empty (#297).
        self.webview.evaluate_javascript(
            &format!("JSON.stringify([{reader}, window.__hylkiBodyText()])"),
            None,
            None,
            gtk::gio::Cancellable::NONE,
            move |res| {
                let json = res.map(|v| v.to_str().to_string()).unwrap_or_default();
                let (html, text) = split_body(&json);
                cb(html, text);
            },
        );
    }
}

/// The HTML and the text of the body from `read_body`'s JSON pair.
fn split_body(json: &str) -> (String, String) {
    serde_json::from_str::<(String, String)>(json).unwrap_or_default()
}

/// Whether a keystroke is an undo (`Some(false)`) or a redo (`Some(true)`).
/// Ctrl+Y is bound alongside Ctrl+Shift+Z because both spellings are in wide
/// use and the app's own history (#200) already answers to each.
pub(super) fn history_key(keyval: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> Option<bool> {
    if !state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
        return None;
    }
    let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
    if !shift && keyval == gtk::gdk::Key::z {
        return Some(false);
    }
    if (shift && matches!(keyval, gtk::gdk::Key::z | gtk::gdk::Key::Z))
        || keyval == gtk::gdk::Key::y
    {
        return Some(true);
    }
    None
}

/// Give the rich document's body the caret. Keyboard focus on the view
/// alone left no caret in it, so a reply opened with nowhere to type
/// until the body was clicked (#266). A caret already in the body stays
/// where it is; otherwise it goes to the top, above any quote. Only a
/// contenteditable body is touched: the source document's field takes
/// focus itself.
const PLACE_CARET: &str = "(function(){var b=document.body;\
    if(!b||!b.isContentEditable)return;\
    var s=getSelection();\
    var kept=s.rangeCount&&b.contains(s.anchorNode)?s.getRangeAt(0).cloneRange():null;\
    b.focus();\
    var r=kept;\
    if(!r){r=document.createRange();r.setStart(b,0);r.collapse(true);}\
    s.removeAllRanges();s.addRange(r);})()";

/// GTK's emoji chooser over the editor, pointing at the caret (#348). It is
/// parented only while it is open, so a closed composer leaves nothing
/// behind.
fn show_emoji_chooser(webview: &webkit6::WebView, anchor: &gtk::Stack) {
    let weak = webview.downgrade();
    let anchor = anchor.downgrade();
    webview.evaluate_javascript(
        "window.__hylkiEmojiMark ? window.__hylkiEmojiMark() : ''",
        None,
        None,
        gtk::gio::Cancellable::NONE,
        move |res| {
            let (Some(v), Some(anchor)) = (weak.upgrade(), anchor.upgrade()) else {
                return;
            };
            let chooser = gtk::EmojiChooser::new();
            chooser.set_parent(&anchor);
            let at: Vec<f64> = res
                .map(|r| r.to_str().to_string())
                .unwrap_or_default()
                .split(',')
                .filter_map(|n| n.trim().parse().ok())
                .collect();
            if let [x, y, w, h] = at[..] {
                // CSS pixels are widget pixels times the zoom.
                let z = v.zoom_level();
                let (x, y) = v
                    .compute_point(
                        &anchor,
                        &gtk::graphene::Point::new((x * z) as f32, (y * z) as f32),
                    )
                    .map_or((x * z, y * z), |p| (p.x() as f64, p.y() as f64));
                chooser.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                    x as i32,
                    y as i32,
                    (w * z).max(1.0) as i32,
                    (h * z).max(1.0) as i32,
                )));
            }
            let weak = v.downgrade();
            chooser.connect_emoji_picked(move |_, emoji| {
                let Some(v) = weak.upgrade() else { return };
                v.grab_focus();
                let text = serde_json::to_string(emoji).unwrap_or_default();
                exec(
                    &v,
                    &format!("window.__hylkiEmojiPut && window.__hylkiEmojiPut({text})"),
                );
            });
            chooser.connect_closed(|c| {
                // Unparented after the pick has been handled, not inside the
                // signal that is still running on it.
                let c = c.clone();
                gtk::glib::idle_add_local_once(move || c.unparent());
            });
            chooser.popup();
        },
    );
}

fn exec(webview: &webkit6::WebView, js: &str) {
    webview.evaluate_javascript(js, None, None, gtk::gio::Cancellable::NONE, |_| {});
}

/// Fade the editor view to full opacity. The animation object is parked on
/// the view itself so it stays alive for its 200ms (a dropped animation
/// stops); a repeat call replaces the parked one harmlessly.
fn fade_in(webview: &webkit6::WebView) {
    let anim = adw::TimedAnimation::new(
        webview,
        webview.opacity(),
        1.0,
        200,
        adw::CallbackAnimationTarget::new({
            let v = webview.clone();
            move |o| v.set_opacity(o)
        }),
    );
    anim.set_easing(adw::Easing::EaseOutCubic);
    anim.play();
    unsafe {
        webview.set_data("hylki-fade-anim", anim);
    }
}

/// Escape a string for inclusion inside a single-quoted JavaScript string.
pub(super) fn js_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\x3c"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod body_tests {
    use super::documents::css_monospace_family;
    use super::*;

    /// The text half survives the trip out of WebKit: a plain-text message
    /// went out empty when the halves were joined with a NUL (#297).
    #[test]
    fn both_halves_of_the_body_arrive() {
        let json = r#"["<p>Hi</p>","Hi\nthere \u0000 \"quoted\""]"#;
        assert_eq!(
            split_body(json),
            (
                "<p>Hi</p>".to_string(),
                "Hi\nthere \u{0} \"quoted\"".to_string()
            )
        );
        assert_eq!(split_body("null"), (String::new(), String::new()));
    }

    /// The plain-text view's font is the desktop's monospace family, quoted
    /// so a name with spaces or digits holds, with the generic one behind
    /// it (#424).
    #[test]
    fn plain_text_uses_the_desktop_monospace_family() {
        assert_eq!(
            css_monospace_family("Adwaita Mono 11"),
            "\"Adwaita Mono\",monospace"
        );
        assert_eq!(
            css_monospace_family("Source Code Pro Bold 10"),
            "\"Source Code Pro\",monospace"
        );
        assert_eq!(css_monospace_family("Monospace 10"), "monospace");
        assert_eq!(css_monospace_family(""), "monospace");
        assert_eq!(
            css_monospace_family("Evil\"}body{x 10"),
            "\"Evilbodyx\",monospace"
        );
    }
}

#[cfg(test)]
mod pinned_tests {
    //! Characterization tests (restructure, area D).
    use super::scripts::{EMOJI_SCRIPT, HISTORY_SCRIPT, LINES_SCRIPT, PASTE_SCRIPT};
    use super::signature::{find_src_attr, quoted_value};
    use super::*;

    fn fnv(s: &str) -> (usize, u64) {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in s.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
        (s.len(), h)
    }

    /// The scripts go into every editor page as they are: pin their bytes so
    /// moving them into files cannot change them.
    #[test]
    fn embedded_scripts_are_unchanged() {
        assert_eq!(fnv(HISTORY_SCRIPT), (1287, 0x8e997368982bb599));
        assert_eq!(fnv(EMOJI_SCRIPT), (1621, 0x748b009b19f3f0ec));
        assert_eq!(fnv(LINES_SCRIPT), (5153, 0xb927290405c83fda));
        assert_eq!(fnv(PASTE_SCRIPT), (40419, 0xc68b9a6a876b9791));
    }

    #[test]
    fn history_keys() {
        use gtk::gdk::{Key, ModifierType as M};
        let c = M::CONTROL_MASK;
        let cs = c | M::SHIFT_MASK;
        assert_eq!(history_key(Key::z, c), Some(false));
        assert_eq!(history_key(Key::z, cs), Some(true));
        assert_eq!(history_key(Key::Z, cs), Some(true));
        assert_eq!(history_key(Key::Z, c), None);
        assert_eq!(history_key(Key::y, c), Some(true));
        assert_eq!(history_key(Key::y, cs), Some(true));
        assert_eq!(history_key(Key::z, M::empty()), None);
        assert_eq!(history_key(Key::x, c), None);
    }

    #[test]
    fn js_escape_quotes_breaks_and_tags() {
        assert_eq!(
            js_escape("a'b\"c\\d\ne\rf\tg<h>"),
            "a\\'b\\\"c\\\\d\\ne\\rf\\tg\\x3ch>"
        );
        assert_eq!(js_escape("plain"), "plain");
    }

    #[test]
    fn src_attributes_are_found_after_a_space_in_any_case() {
        assert_eq!(find_src_attr("<img SRC=\"a\">"), Some(5));
        assert_eq!(find_src_attr("<img data-src=\"a\" src='b'>"), Some(18));
        assert_eq!(find_src_attr("src=\"a\""), None);
        assert_eq!(quoted_value("src=\"a b\" x"), Some(("src=\"", "a b")));
        assert_eq!(quoted_value("src='c'"), Some(("src='", "c")));
        assert_eq!(quoted_value("src=c"), None);
        assert_eq!(quoted_value("src=\"open"), None);
    }

    #[test]
    fn a_signature_with_a_less_than_sign_is_taken_as_html() {
        assert_eq!(signature_to_html("a < b"), "a < b");
        assert_eq!(signature_to_html("a & b\nc"), "a &amp; b<br>c");
    }
}
