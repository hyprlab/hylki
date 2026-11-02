//! What is injected into each message document: the reader style (#56),
//! the Content-Security-Policy and the text direction.

use relm4::prelude::*;

/// The stylesheet that lays the reader's own fonts and colors over a message
/// (#56), or nothing when `style` overrides nothing.
///
/// `scope` is the element the rules hang under: `:root` in a sandboxed frame,
/// the message's own block in the printed document. Each selector carries
/// three `:not(#…)` clauses it trivially satisfies, so it outranks anything
/// a sender wrote short of an inline `!important`, ids included — the sheet
/// goes in last, and at equal weight the later rule wins.
///
/// Fonts: one family and size for everything, headings scaled from it so a
/// message keeps its hierarchy, code kept monospaced. Colors: the reader's
/// text on a transparent ground (the frame paints the card's), links in the
/// accent; `img` carries no color, so pictures stand. Background images
/// go with the backgrounds: they are decoration in the same sense.
pub(super) fn reader_style_css(
    style: &crate::config::ReaderStyle,
    dark: bool,
    accent: &str,
    scope: &str,
) -> String {
    if !style.active() {
        return String::new();
    }
    // "everything under the scope", at id weight.
    let all = format!("{scope} :not(#hylki-a):not(#hylki-b):not(#hylki-c)");
    let of = |sel: &str| format!("{scope} {sel}:not(#hylki-a):not(#hylki-b):not(#hylki-c)");
    let mut css = String::new();
    if let Some(font) = &style.font {
        let (family, size, face) = css_font(font);
        css.push_str(&format!(
            "{scope},{all}{{font-family:{family} !important;font-size:{size} !important;\
             line-height:1.45 !important;{face}}}\
             {h}{{line-height:1.25 !important;}}\
             {h1}{{font-size:calc({size} * 1.6) !important;}}\
             {h2}{{font-size:calc({size} * 1.35) !important;}}\
             {h3}{{font-size:calc({size} * 1.15) !important;}}\
             {small}{{font-size:calc({size} * 0.85) !important;}}\
             {code}{{font-family:monospace !important;font-size:calc({size} * 0.9) !important;}}",
            h = of(":is(h1,h2,h3,h4,h5,h6)"),
            h1 = of("h1"),
            h2 = of("h2"),
            h3 = of("h3"),
            small = of(":is(small,sub,sup)"),
            code = of(":is(pre,code,kbd,samp,tt)"),
        ));
    }
    // Plain-text messages (#181): their parts are wrapped in `.hylki-plain`
    // when the body is rendered, so the fixed-width font lands on exactly
    // them. After the message font above, and one class more specific, so
    // it wins where both apply.
    if let Some(font) = &style.plain_font {
        let (family, size, face) = css_font(font);
        css.push_str(&format!(
            "{p},{p} :not(#hylki-a):not(#hylki-b):not(#hylki-c)\
             {{font-family:{family} !important;font-size:{size} !important;{face}}}",
            p = of(".hylki-plain"),
        ));
    }
    if style.colors {
        let fg = if dark { "#e6e6e6" } else { "#1a1a1a" };
        css.push_str(&format!(
            "{scope},{all}{{color:{fg} !important;-webkit-text-fill-color:{fg} !important;\
             background-color:transparent !important;background-image:none !important;\
             text-shadow:none !important;}}\
             {a},{a} *{{color:{accent} !important;-webkit-text-fill-color:{accent} !important;}}",
            a = of("a"),
        ));
    }
    css
}

/// A Pango font description as a CSS family list, a size, and the face's
/// declarations. Pango sizes are points (or device pixels when absolute);
/// `0` means none was given, and the interface font's customary 11pt stands
/// in. The face — weight, italic, stretch — is written only where it departs
/// from regular: "Adwaita Sans Black" must come out black, but a regular
/// pick must not flatten the sender's own bold and italic.
pub(super) fn css_font(desc: &str) -> (String, String, String) {
    let fd = gtk::pango::FontDescription::from_string(desc);
    let family = fd
        .family()
        .map(|f| f.to_string())
        .filter(|f| !f.trim().is_empty())
        .unwrap_or_else(|| "sans-serif".to_string());
    // Every family quoted (a name with spaces or digits needs it; the rest
    // tolerate it); a generic fallback closes the list.
    let families: Vec<String> = family
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(|f| format!("\"{}\"", f.replace(['\\', '"'], "")))
        .collect();
    let generic = if family.to_ascii_lowercase().contains("mono") {
        "monospace"
    } else {
        "sans-serif"
    };
    let family = format!("{},{generic}", families.join(","));
    let units = fd.size();
    let size = if units <= 0 {
        "11pt".to_string()
    } else if fd.is_size_absolute() {
        format!("{}px", units as f64 / gtk::pango::SCALE as f64)
    } else {
        format!("{}pt", units as f64 / gtk::pango::SCALE as f64)
    };
    let mut face = String::new();
    use gtk::glib::translate::IntoGlib;
    let weight = fd.weight().into_glib();
    if weight != gtk::pango::Weight::Normal.into_glib() {
        // CSS takes 1–1000; Pango's scale is the same, `Ultraheavy` = 1000.
        face.push_str(&format!(
            "font-weight:{} !important;",
            weight.clamp(1, 1000)
        ));
    }
    match fd.style() {
        gtk::pango::Style::Italic => face.push_str("font-style:italic !important;"),
        gtk::pango::Style::Oblique => face.push_str("font-style:oblique !important;"),
        _ => {}
    }
    let stretch = match fd.stretch() {
        gtk::pango::Stretch::UltraCondensed => "ultra-condensed",
        gtk::pango::Stretch::ExtraCondensed => "extra-condensed",
        gtk::pango::Stretch::Condensed => "condensed",
        gtk::pango::Stretch::SemiCondensed => "semi-condensed",
        gtk::pango::Stretch::SemiExpanded => "semi-expanded",
        gtk::pango::Stretch::Expanded => "expanded",
        gtk::pango::Stretch::ExtraExpanded => "extra-expanded",
        gtk::pango::Stretch::UltraExpanded => "ultra-expanded",
        _ => "",
    };
    if !stretch.is_empty() {
        face.push_str(&format!("font-stretch:{stretch} !important;"));
    }
    (family, size, face)
}

/// Put a stylesheet at the end of a document's body, after everything the
/// sender wrote, so its rules are the last word.
pub(super) fn inject_reader_style(doc: &str, css: &str) -> String {
    if css.is_empty() {
        return doc.to_string();
    }
    let block = format!("<style>{css}</style>");
    let lower = doc.to_ascii_lowercase();
    for close in ["</body", "</html"] {
        if let Some(at) = lower.rfind(close) {
            return format!("{}{block}{}", &doc[..at], &doc[at..]);
        }
    }
    format!("{doc}{block}")
}

/// Where a document's real `tag` opens ("<body", say), skipping longer
/// names that merely start the same ("<bodyguard"). `lower` is the
/// document lowercased, so offsets match.
fn find_tag(lower: &str, tag: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(at) = lower[from..].find(tag).map(|i| from + i) {
        let next = lower
            .as_bytes()
            .get(at + tag.len())
            .copied()
            .unwrap_or(b'>');
        if next == b'>' || next == b'/' || next.is_ascii_whitespace() {
            return Some(at);
        }
        from = at + tag.len();
    }
    None
}

/// Whether the tag opening at `at` carries a `dir` attribute.
fn tag_has_dir(lower: &str, at: usize) -> bool {
    let end = lower[at..].find('>').map_or(lower.len(), |i| at + i);
    let tag = &lower[at..end];
    tag.match_indices("dir").any(|(i, _)| {
        tag[..i].ends_with(|c: char| c.is_ascii_whitespace())
            && tag[i + 3..].trim_start().starts_with('=')
    })
}

/// Let a message take the direction of its own text (#366): `dir="auto"`
/// on its body, so Persian or Hebrew mail reads right to left whatever the
/// interface language, unless the sender set a direction on `<html>` or
/// `<body>`, which stands. Plain text is set line by line (`plaintext`
/// bidi), so an English reply over a Persian quote reads right in both.
pub(super) fn auto_direction(doc: &str) -> String {
    let lower = doc.to_ascii_lowercase();
    let html = find_tag(&lower, "<html");
    let body = find_tag(&lower, "<body");
    let doc = if html.is_some_and(|at| tag_has_dir(&lower, at))
        || body.is_some_and(|at| tag_has_dir(&lower, at))
    {
        doc.to_string()
    } else if let Some(at) = body.or(html) {
        // Both "<body" and "<html" are five bytes long.
        let at = at + 5;
        format!("{} dir=\"auto\"{}", &doc[..at], &doc[at..])
    } else {
        doc.to_string()
    };
    let plain = if crate::mail::quote::body_is_pre_wrap(&doc) {
        "body,.hylki-plain"
    } else if lower.contains("hylki-plain") {
        ".hylki-plain"
    } else {
        return doc;
    };
    inject_reader_style(&doc, &format!("{plain}{{unicode-bidi:plaintext;}}"))
}

/// Inject a Content-Security-Policy `<meta>` into the document head. When remote
/// content is disallowed only inline styles and `data:` URIs are permitted.
///
/// `allow_remote` is the user's own choice, never the output of
/// [`has_remote_resources`]. That is what makes this an independent second line
/// of defence: if the detector fails to spot a reference, the engine still
/// refuses the fetch.
pub(super) fn inject_csp(html: &str, allow_remote: bool, dark: bool) -> String {
    let policy = if allow_remote {
        "default-src 'none'; img-src http: https: hylki-http: hylki-https: data: cid:; \
         style-src 'unsafe-inline' http: https: hylki-http: hylki-https: data:; \
         font-src http: https: hylki-http: hylki-https: data:; \
         media-src http: https: hylki-http: hylki-https: data:"
    } else {
        "default-src 'none'; img-src data: cid:; style-src 'unsafe-inline' data:; \
         font-src data:; media-src data:"
    };
    let lower = html.to_ascii_lowercase();
    // Every message gets a comfortable default inset: the UA's 8px body margin
    // is reset so content sits at exactly 20px of breathing room. Injected
    // ahead of the email's own CSS, so a message that styles its body (a
    // full-bleed design, say) still wins.
    let body_pad = "body{margin:0;padding:20px;box-sizing:border-box;}";
    // An image with no size of its own (a photo sent from a phone is a bare
    // `<img>`) would otherwise lay out at its pixel width, and the frame
    // widens to whatever its content needs, so the card ran off the pane.
    // Capped at the card instead; a message that sizes its images keeps its
    // own rules, which come later. No `height:auto`: that would stretch a
    // spacer image whose `height` attribute is all that keeps it thin.
    let image_fit = "img{max-width:100%;}";
    // The frame's own document must never scroll: WebKit latches a wheel
    // gesture to the innermost scrollable area under the pointer, and a
    // frame document that can scroll sideways (a message wider than the
    // pane whose width grows as the frame is widened, so the widening
    // never quite catches it) swallows the vertical deltas the page
    // needed. The wrapper widens the frame to the content and scrolls it
    // in `.hylki-pan`; whatever is left over is clipped rather than
    // scrollable. `scrollWidth`/`scrollHeight` still measure the content.
    // `color-scheme` makes the browser's default colors (for content that sets
    // none of its own) follow the app's light/dark setting; styled emails keep
    // their own colors untouched.
    let scheme = if dark { "dark" } else { "light" };
    let supported = if dark { "dark light" } else { "light dark" };
    let theme = format!(
        "<meta name=\"color-scheme\" content=\"{supported}\">\
         <style>:root{{color-scheme:{scheme};}}{body_pad}{image_fit}\
         html{{overflow:hidden !important;}}\
         @media print{{:root{{color-scheme:light;}}html{{overflow:visible !important;}}html,body{{background:#fff !important;}}}}\
         </style>"
    );
    // `no-referrer` keeps the synthetic `hylki.localhost` base URI from leaking as
    // a Referer/Origin header — both for privacy and because hotlink-protected
    // servers (e.g. some DreamHost sites) return 403 to foreign referrers, which
    // otherwise blocks legitimate images even once the sender is trusted.
    let meta = format!(
        "{theme}<meta name=\"referrer\" content=\"no-referrer\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{policy}\">"
    );

    if let Some(head) = lower.find("<head") {
        if let Some(close) = html[head..].find('>') {
            let at = head + close + 1;
            return format!("{}{meta}{}", &html[..at], &html[at..]);
        }
    }
    if let Some(htmltag) = lower.find("<html") {
        if let Some(close) = html[htmltag..].find('>') {
            let at = htmltag + close + 1;
            return format!("{}<head>{meta}</head>{}", &html[..at], &html[at..]);
        }
    }
    format!("<!doctype html><html><head>{meta}</head><body>{html}</body></html>")
}
