//! The HTML documents the editor and its preview load: themed, with the image
//! policy and the scripts in their heads.

use super::exec;
use super::fonts::EDITOR_FONT;
use super::fonts::{body_font, font_style};
use super::scripts::{EMOJI_SCRIPT, HISTORY_SCRIPT, LINES_SCRIPT, PASTE_SCRIPT};
use super::signature::{find_src_attr, quoted_value};
use super::toolbar::js_str;

/// The contentEditable HTML document, themed for light/dark. Its ground is
/// the theme's own view background (#148), read through `webview`, so a
/// custom GNOME theme reaches the editor as it does the reader — `Canvas`
/// would be WebKit's stock shade whatever the theme says.
///
/// With `plain` set the document is the plain-text view (#424): it
/// flattens what it is given to the text that is sent once it has parsed
/// it, and shows it in the desktop's monospace font. Every block of the
/// body is a paragraph there, a line's height above the next, because
/// that is the empty line the sent text has between them.
pub(super) fn document(
    content: &str,
    webview: &webkit6::WebView,
    image_policy: &str,
    plain: bool,
) -> String {
    let dark = adw::StyleManager::default().is_dark();
    let scheme = if dark { "dark" } else { "light" };
    let (ground, _, _) = crate::ui::message_view::theme_grounds_for(webview, dark);
    let paste_rich = !crate::config::load_privacy().paste_plain;
    let return_paragraph = crate::config::load_privacy().return_paragraph;
    let body_font = body_font();
    let prefs = crate::config::load_privacy();
    let compose_font = js_str(&font_style(&prefs.compose_font, prefs.compose_font_size));
    let script = format!(
        "<script>window.__hylkiPasteRich={paste_rich};\
         window.__hylkiComposeFont={compose_font};\
         window.__hylkiReturnParagraph={return_paragraph};\
         window.__hylkiPlain={plain};</script>{LINES_SCRIPT}{PASTE_SCRIPT}{HISTORY_SCRIPT}{EMOJI_SCRIPT}"
    );
    let (plain_attr, plain_css) = if plain {
        let mono = monospace_family();
        (
            " data-plain",
            format!(
                "html[data-plain] body{{font:13px/1.6 {mono};}}\
                 html[data-plain] body *{{font:inherit!important;color:inherit;\
                   background:transparent;text-decoration:none;opacity:1;\
                   margin:0;padding:0;border:0;}}\
                 html[data-plain] body>*{{margin:0 0 1.6em;}}\
                 html[data-plain] body>*:last-child{{margin-bottom:0;}}"
            ),
        )
    } else {
        ("", String::new())
    };
    format!(
        "<!doctype html><html{plain_attr}><head><meta charset=\"utf-8\">\
         <meta name=\"color-scheme\" content=\"{scheme}\">{image_policy}\
         <style>\
           :root{{color-scheme:{scheme};}}\
           html,body{{height:100%;box-sizing:border-box;}}\
           body{{margin:0;padding:20px;font:{body_font};outline:none;\
             background:{ground};color:CanvasText;}}\
           /* Every image fits the writing width — pasted, dropped, or\
              quoted. The inline style on inserted images serves the\
              recipient; this rule is what the composer itself obeys,\
              whatever survives insertHTML. */\
           img{{max-width:100%;height:auto;}}\
           /* A picture armed to be recut when the message is sent. */\
           img[data-hylki-fit]{{outline:2px dashed #e01b24;outline-offset:2px;}}\
           /* The in-progress word's misspelling mark (Custom Highlight API);\
              the tint backs up engines that skip decorations in highlights. */\
           ::highlight(hylki-misspell){{\
             text-decoration:underline wavy #e01b24;\
             background-color:rgba(224,27,36,0.10);}}\
           blockquote{{margin:0;margin-inline-start:8px;padding-inline-start:10px;\
             border-inline-start:3px solid rgba(128,128,128,0.4);}}\
           /* Each paragraph written here takes the direction of its own\
              text (#366): Persian under English, or the other way round. */\
           body,body>div,body>p,li{{unicode-bidi:plaintext;}}\
           /* A paragraph's space goes below it, so the first line of the\
              message sits where it would in a <div>. */\
           body>p:first-child{{margin-top:0;}}\
           /* The lines of one paragraph, each a <p> while it is written\
              (#524), sit together as the lines of one. */\
           body>p[data-hylki-line]{{margin-top:0;}}\
           body>p:has(+p[data-hylki-line]){{margin-bottom:0;}}\
           .hylki-quote-attr{{opacity:0.7;margin:10px 0 4px;}}\
           /* A quoted message that sets its own colors, on the light\
              ground it was designed for. */\
           blockquote.hylki-quote-mail{{background:#ffffff;color:#1e1e1e;\
             color-scheme:light;}}\
           .hylki-sig{{opacity:0.85;}}\
           /* The font set for new messages is for what is written here:\
              the signature and the quoted original keep the editor's. */\
           body>.hylki-sig,body>.hylki-quote-attr,\
           body>.hylki-quote-attr+blockquote{{font:{EDITOR_FONT};}}\
           a{{color:#3584e4;}}\
           {plain_css}\
         </style>{script}\
         <script>window.__hylkiDirty=false;\
           document.addEventListener('input',function(){{window.__hylkiDirty=true;}},true);\
         </script></head>\
         <body contenteditable=\"true\" dir=\"auto\">{content}</body></html>"
    )
}

/// The source-mode document: one textarea filling the view, in the
/// same ground as the rich editor so switching format does not change the
/// look of the pane. A textarea, rather than a plain-text `contenteditable`,
/// because what comes back out has to be exactly what was typed — no
/// normalised whitespace, no inserted line divs — and because undo, paste
/// and the engine's own spell checking all behave there without help.
///
/// The text goes in after a newline: an HTML parser drops the first newline
/// of a textarea's content, which took the blank line a Markdown reply opens
/// with and left the caret on the quote's "wrote:" line.
pub(super) fn source_document(text: &str, webview: &webkit6::WebView) -> String {
    let dark = adw::StyleManager::default().is_dark();
    let scheme = if dark { "dark" } else { "light" };
    let (ground, _, _) = crate::ui::message_view::theme_grounds_for(webview, dark);
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"color-scheme\" content=\"{scheme}\">\
         <style>\
           :root{{color-scheme:{scheme};}}\
           html,body{{height:100%;margin:0;background:{ground};}}\
           textarea{{display:block;width:100%;height:100%;box-sizing:border-box;\
             border:0;outline:none;resize:none;padding:18px 20px;\
             background:{ground};color:CanvasText;\
             font:13px/1.6 {mono};\
             tab-size:2;}}\
         </style></head><body>\
         <textarea id=\"src\" dir=\"auto\" spellcheck=\"true\" autocapitalize=\"off\" \
           autocorrect=\"off\">\n{text}</textarea>\
         <script>\
         (function(){{\
           var t=document.getElementById('src');\
           window.__hylkiDirty=false;\
           window.__hylkiSourceText=function(){{return t.value;}};\
           t.addEventListener('input',function(){{window.__hylkiDirty=true;}});\
           /* Tab indents instead of leaving the field: in a source view it\
              is a character, and there is nowhere else in the document to\
              tab to anyway. */\
           t.addEventListener('keydown',function(e){{\
             if(e.key!=='Tab'||e.ctrlKey||e.altKey)return;\
             e.preventDefault();\
             var a=t.selectionStart,b=t.selectionEnd;\
             t.setRangeText('  ',a,b,'end');\
             window.__hylkiDirty=true;\
           }});\
           t.focus();t.setSelectionRange(0,0);\
         }})();\
         </script>{HISTORY_SCRIPT}{EMOJI_SCRIPT}</body></html>",
        text = gtk::glib::markup_escape_text(text),
        mono = monospace_family(),
    )
}

/// The desktop's monospace font as a CSS family list, for the plain-text
/// and source views: the family picked for terminals and code, with the
/// generic family behind it. Read once, since asking costs a round trip to
/// the settings portal.
fn monospace_family() -> String {
    static FAMILY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FAMILY
        .get_or_init(|| css_monospace_family(&crate::desktop::session::monospace_font()))
        .clone()
}

/// A Pango font description's families, quoted, then `monospace`.
pub(super) fn css_monospace_family(desc: &str) -> String {
    let fd = gtk::pango::FontDescription::from_string(desc);
    let family = fd.family().map(|f| f.to_string()).unwrap_or_default();
    let mut families: Vec<String> = family
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty() && !f.eq_ignore_ascii_case("monospace"))
        .map(|f| {
            format!(
                "\"{}\"",
                f.replace(['\\', '"', '<', '>', '{', '}', ';'], "")
            )
        })
        .collect();
    families.push("monospace".to_string());
    families.join(",")
}

/// The preview document: the message as the recipient would see it, on the
/// editor's own ground.
pub(super) fn preview_document(
    html: &str,
    webview: &webkit6::WebView,
    image_policy: &str,
) -> String {
    let dark = adw::StyleManager::default().is_dark();
    let scheme = if dark { "dark" } else { "light" };
    let (ground, _, _) = crate::ui::message_view::theme_grounds_for(webview, dark);
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"color-scheme\" content=\"{scheme}\">{image_policy}\
         <style>\
           :root{{color-scheme:{scheme};}}\
           html,body{{height:100%;box-sizing:border-box;}}\
           body{{margin:0;padding:20px;font:14px/1.55 system-ui,sans-serif;\
             background:{ground};color:CanvasText;}}\
           img{{max-width:100%;height:auto;}}\
           a{{color:#3584e4;}}\
         </style></head><body>{html}</body></html>"
    )
}

/// The `<meta>` that limits the pictures a document loads to embedded ones
/// and `allowed`, or nothing when `allowed` is `None`. Only `img-src` is
/// set: the editor's own scripts are inline and have to keep running.
pub(super) fn image_policy_meta(allowed: Option<&[String]>) -> String {
    let Some(allowed) = allowed else {
        return String::new();
    };
    let mut sources = String::from("data: blob:");
    for url in allowed {
        // A source expression names a path, never a query, and must not
        // break the policy's own syntax.
        let url = url.split(['?', '#']).next().unwrap_or_default();
        let lower = url.to_ascii_lowercase();
        let usable = (lower.starts_with("https://") || lower.starts_with("http://"))
            && !url.contains(|c: char| {
                c.is_whitespace() || matches!(c, ';' | ',' | '"' | '\'' | '<' | '>' | '&')
            });
        if usable {
            sources.push(' ');
            sources.push_str(url);
        }
    }
    format!("<meta http-equiv=\"Content-Security-Policy\" content=\"img-src {sources}\">")
}

/// Every remote picture `html` shows, by its `src` address.
pub(crate) fn remote_image_urls(html: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut rest = html;
    while let Some(at) = find_src_attr(rest) {
        let tail = &rest[at..];
        match quoted_value(tail) {
            Some((prefix, value)) => {
                let lower = value.trim().to_ascii_lowercase();
                if lower.starts_with("https://") || lower.starts_with("http://") {
                    urls.push(value.trim().replace("&amp;", "&"));
                }
                rest = &tail[prefix.len() + value.len()..];
            }
            None => rest = &tail[4..],
        }
    }
    urls
}

/// Re-ground an open editor document (#148): the color scheme and the page
/// ground are baked into the document when it loads, so a theme change — a
/// light/dark flip, or a new palette — has to be pushed into it. Deferred to
/// the next main-loop pass, because the theme's named colors are only
/// re-resolved after the signal that announced the change. `false` means the
/// view is gone and this editor needs telling no more.
pub(super) fn reground(weak: &gtk::glib::WeakRef<webkit6::WebView>, dark: bool) -> bool {
    if weak.upgrade().is_none() {
        return false;
    }
    let weak = weak.clone();
    gtk::glib::idle_add_local_once(move || {
        let Some(v) = weak.upgrade() else { return };
        let (ground, _, _) = crate::ui::message_view::theme_grounds_for(&v, dark);
        let scheme = if dark { "dark" } else { "light" };
        exec(
            &v,
            &format!(
                "(function(){{\
                   var m=document.querySelector('meta[name=color-scheme]');\
                   if(m)m.content='{scheme}';\
                   document.documentElement.style.colorScheme='{scheme}';\
                   document.body.style.background='{ground}';\
                 }})()"
            ),
        );
    });
    true
}
