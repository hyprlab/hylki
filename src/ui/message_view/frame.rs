//! One message's document inside its card (`message_frame`), and the theme
//! grounds handed to the document builder through thread-locals.

use super::dark::{adapt_colors_for_dark, pin_color_scheme};
use super::html::{attr_escape, body_html};
use super::remote::{route_remote, strip_remote};
use super::style::{auto_direction, inject_csp, inject_reader_style, reader_style_css};
use super::types::CardAttachment;
use adw::prelude::*;
use relm4::prelude::*;

/// The three grounds for `dark`, resolved from the live libadwaita theme
/// instead of hard-coded values (issue #62): GROUND is the theme's own view
/// background, PAGE shades it a step deeper so conversation cards (and the
/// composer, #148) keep reading as a layer under any theme variant, and
/// CHROME is the window's own ground. `widget` is only where the theme is
/// read from. When the caller asks for a scheme the app isn't currently in,
/// the theme can't answer for that mode, so the stock GNOME values stand in.
#[allow(deprecated)] // lookup_color: named theme colors have no successor yet
pub(crate) fn theme_grounds_for(
    widget: &impl IsA<gtk::Widget>,
    dark: bool,
) -> (String, String, String) {
    let style = widget.style_context();
    if dark == adw::StyleManager::default().is_dark() {
        if let Some(c) = style.lookup_color("view_bg_color") {
            let hex =
                |r: f32, g: f32, b: f32| crate::color::to_hex(&gtk::gdk::RGBA::new(r, g, b, 1.0));
            // The stock pairs' own ratios: #1e1e1e→#141414 and #fff→#f1f1f1.
            let f = if dark { 0.667 } else { 0.945 };
            let ground = hex(c.red(), c.green(), c.blue());
            let page = hex(c.red() * f, c.green() * f, c.blue() * f);
            // The window's own ground — what the GTK reader header (the
            // subject block) sits on. A full-bleed single message paints
            // its in-document header this color so subject and header
            // read as one surface.
            let chrome = style
                .lookup_color("window_bg_color")
                .map(|w| hex(w.red(), w.green(), w.blue()))
                .unwrap_or_else(|| (if dark { CHROME.1 } else { CHROME.0 }).to_string());
            return (ground, page, chrome);
        }
    }
    let (g, p, c) = if dark {
        (GROUND.1, PAGE.1, CHROME.1)
    } else {
        (GROUND.0, PAGE.0, CHROME.0)
    };
    (g.to_string(), p.to_string(), c.to_string())
}

/// The reader's two grounds. A message sits on [`GROUND`]; in a conversation the
/// cards sit on the slightly deeper [`PAGE`], which is what makes them read as
/// cards. The spinner and the cover behind the WebView use the same pair, so
/// handing over to the document changes nothing on screen.
pub(super) const GROUND: (&str, &str) = ("#ffffff", "#1e1e1e");

pub(super) const PAGE: (&str, &str) = ("#f1f1f1", "#141414");

/// The window chrome's ground (stock GNOME `window_bg_color`): what the GTK
/// reader header — the subject block — sits on. A full-bleed single message
/// paints its in-document header this color so the two read as one surface.
pub(super) const CHROME: (&str, &str) = ("#fafafa", "#242424");

/// Each card's translation banner, and the translation (and whether it is
/// a Reader View fragment) the card shows instead of its message.
type TranslationsShown = std::collections::HashMap<(u32, u32), (String, Option<(String, bool)>)>;

thread_local! {
    /// The grounds as resolved from the live libadwaita theme, refreshed by
    /// `document_html` just before each build (issue #62). The document
    /// builder is a static fn so tests can exercise it without a display —
    /// this hands it the theme without widening that signature. `None` (as in
    /// tests) falls back to the stock GNOME values above.
    pub(super) static LIVE_GROUNDS: std::cell::RefCell<Option<(String, String, String)>> =
        const { std::cell::RefCell::new(None) };
    /// The tags (#71), handed to the document builder the same way, for the
    /// card headers' chips. Empty in tests: no chips.
    pub(super) static LIVE_TAGS: std::cell::RefCell<Vec<crate::config::Tag>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// What each card has attached (#213), handed to the builder the same
    /// way. Empty in tests, and whenever the cards are not to list them.
    pub(super) static LIVE_ATTS: std::cell::RefCell<
        std::collections::HashMap<(u32, u32), Vec<CardAttachment>>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
    /// Each card's Unsubscribe banner, already rendered, handed to the
    /// builder the same way. Empty in tests: no banners.
    pub(super) static LIVE_UNSUB: std::cell::RefCell<std::collections::HashMap<(u32, u32), String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// And each card's invitation banner (#223), the same way again.
    pub(super) static LIVE_INVITE: std::cell::RefCell<std::collections::HashMap<(u32, u32), String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// And each card's read receipt banner (#520).
    pub(super) static LIVE_RECEIPT: std::cell::RefCell<std::collections::HashMap<(u32, u32), String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// The checkmarks of each card that is sent mail asking for a read
    /// receipt (#520). Empty in tests.
    pub(super) static LIVE_READ: std::cell::RefCell<
        std::collections::HashMap<(u32, u32), crate::models::ReceiptMark>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
    /// Each card's translation banner (#327), and the translation the card
    /// shows instead of its message, if it shows one. Empty in tests.
    pub(super) static LIVE_TRANSLATE: std::cell::RefCell<TranslationsShown> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// Each card's folded bar (#326), already rendered. Empty in tests.
    pub(super) static LIVE_FOLDBAR: std::cell::RefCell<std::collections::HashMap<(u32, u32), String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// The cards that open folded (#326). Empty in tests.
    pub(super) static LIVE_FOLDED: std::cell::RefCell<std::collections::HashSet<(u32, u32)>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
    /// The cards showing a template (#360), which wear its banner. Empty in
    /// tests.
    pub(super) static LIVE_TEMPLATES: std::cell::RefCell<std::collections::HashSet<(u32, u32)>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
    /// Whether a translation service is set up, so cards offer Translate.
    pub(super) static LIVE_TRANSLATE_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// That ground as a color the WebView itself can be painted with.
pub(super) fn ground_rgba(hex: &str) -> gtk::gdk::RGBA {
    let v = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f32 / 255.0;
    gtk::gdk::RGBA::new(v(1), v(3), v(5), 1.0)
}

/// One message body as a sandboxed iframe: its own document (so CSS can't leak to
/// other messages) with no `allow-scripts` (so the email can't run JavaScript).
/// `allow-same-origin` lets the wrapper script measure its height.
// Positional, as `conversation_document` and the tests call it: each
// argument is one independent setting of the render.
#[allow(clippy::too_many_arguments)]
pub(super) fn message_frame(
    body: &str,
    restrict: bool,
    dark: bool,
    key: (u32, u32),
    height: Option<u32>,
    style: &crate::config::ReaderStyle,
    accent: &str,
    reader: bool,
    zoom: u32,
) -> String {
    // Reader View: the message rebuilt from its content alone, in the
    // reader's own sheet. It carries no colors of the sender's to adapt
    // and needs none of the reader's color override over it; the reader's
    // font choice (#56) still applies, below.
    let doc = if reader {
        crate::mail::reader::render(body, dark, accent)
    } else {
        body_html(body)
    };
    let own_style;
    let style = if reader {
        own_style = crate::config::ReaderStyle {
            colors: false,
            ..style.clone()
        };
        &own_style
    } else {
        style
    };
    // Allowed remote content goes through the app's cache (#474), so a
    // message shown again, or fetched ahead, paints without the network.
    let doc = if restrict {
        strip_remote(&doc)
    } else {
        route_remote(&doc)
    };
    // Dark mode: adapt the message's own colors so dark-on-dark text can't
    // happen (issue #35). `color-scheme` only helps unstyled mail; anything
    // that sets explicit dark text without a background needs its colors
    // transformed, and the sandboxed frames run no JS to do it live. Moot
    // when the reader's own colors are laid over the message anyway.
    let doc = if dark && !style.colors && !reader {
        adapt_colors_for_dark(&doc)
    } else {
        doc
    };
    // Make the email's own light/dark rules follow the ground we chose rather
    // than the desktop's preference (see `pin_color_scheme`). Runs after the
    // dark adaptation so a message's hand-authored dark palette is used as-is,
    // not double-transformed.
    let doc = pin_color_scheme(&doc, dark);
    // The reader's own fonts and colors (#56), last so they win. The zoom
    // (Ctrl+ / Ctrl-) rides the same sheet: CSS `zoom` on the root scales
    // the whole body, px-sized text included, which a font-size change
    // would leave alone. Applied to the frame document only, so the card
    // headers around it keep their size.
    let mut css = reader_style_css(style, dark, accent, ":root");
    if zoom != 100 {
        css.push_str(&format!("html{{zoom:{};}}", zoom as f64 / 100.0));
    }
    let doc = inject_reader_style(&doc, &css);
    let doc = auto_direction(&inject_csp(&doc, !restrict, dark));
    format!(
        // `allow-same-origin` lets our wrapper script measure the frame height;
        // `allow-popups` lets `_blank` links reach the policy handler (which opens
        // them externally). No `allow-scripts`, so the email's own JS never runs.
        //
        // Opening at the height it had last time means the conversation lays out
        // correctly on its first frame. Without it every card starts at the
        // browser's default and jumps once its content is measured, which is a
        // visible lurch on a thread that has simply been reopened.
        // The `.hylki-pan` wrapper is the horizontal scroller for mail wider
        // than the pane. The frame itself is widened to its content (see
        // `s(f)`), so the sandboxed document is never scrollable — WebKit
        // latches wheel gestures to the innermost scrollable area under the
        // pointer, and a frame document that can scroll sideways swallows
        // vertical wheel deltas it cannot use, leaving the reader stuck.
        // A top-document div has no such latch: unused vertical deltas
        // bubble on to the page.
        "<div class=\"hylki-pan\"><iframe class=\"hylki-frame\" data-key=\"{aid}:{id}\"{style} \
         sandbox=\"allow-same-origin allow-popups\" srcdoc=\"{doc}\"></iframe></div>",
        aid = key.0,
        id = key.1,
        style = match height {
            Some(h) => format!(" style=\"height:{h}px\""),
            // Never measured: nothing to show until the document is in, so
            // no 150px default block for the card to shrink from.
            None => " style=\"height:0px\"".to_string(),
        },
        doc = attr_escape(&doc)
    )
}
