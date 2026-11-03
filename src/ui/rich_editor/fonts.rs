//! Fonts: the toolbar's font and size menus, the font chooser, and the editor's
//! own font.

use super::toolbar::{js_str, run_restored};
use crate::i18n::{i18n, i18n_noop};
use adw::prelude::*;

/// The font menu's label for the family `fmtState` reports: the name the
/// menu gives it, or a family set some other way (a custom pick, a quoted
/// message) by its own name.
pub(super) fn font_label(family: &str) -> String {
    let key = family.to_lowercase();
    match FONTS.iter().find(|(k, _, _)| *k == key) {
        Some((_, _, label)) => i18n(label),
        None if family.is_empty() => i18n(FONTS[0].2),
        None if family.chars().count() > 18 => {
            format!("{}…", family.chars().take(17).collect::<String>())
        }
        None => family.to_string(),
    }
}

/// The font families (#358): the family `fmtState` reports, lower case,
/// the CSS family list, and the name. The generic families come first, so
/// the recipient's own fonts stand in; the named ones are the fonts most
/// mail programs offer, each with fallbacks for a system without it. Their
/// names are not translated.
pub(super) const FONTS: &[(&str, &str, &str)] = &[
    ("sans-serif", "sans-serif", i18n_noop("Sans Serif")),
    ("serif", "serif", i18n_noop("Serif")),
    ("monospace", "monospace", i18n_noop("Monospace")),
    ("arial", "Arial, Helvetica, sans-serif", "Arial"),
    ("verdana", "Verdana, Geneva, sans-serif", "Verdana"),
    ("tahoma", "Tahoma, Verdana, sans-serif", "Tahoma"),
    (
        "trebuchet ms",
        "'Trebuchet MS', Helvetica, sans-serif",
        "Trebuchet MS",
    ),
    ("georgia", "Georgia, serif", "Georgia"),
    (
        "times new roman",
        "'Times New Roman', Times, serif",
        "Times New Roman",
    ),
    ("garamond", "Garamond, serif", "Garamond"),
    (
        "courier new",
        "'Courier New', Courier, monospace",
        "Courier New",
    ),
];

/// The font sizes (#358), in points, the unit mail programs show.
pub(crate) const SIZES: &[u32] = &[8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 28, 36];

/// The font dialog's pick, or `None` when it is cancelled. Not the
/// binding's own `choose_family`: GTK answers a dialog accepted with no row
/// selected with no family and no error, which the binding takes for a
/// family and aborts on (#358).
pub(crate) fn choose_family(
    parent: Option<&gtk::Window>,
    done: impl FnOnce(Option<gtk::pango::FontFamily>) + 'static,
) {
    use gtk::glib::translate::{from_glib_full, ToGlibPtr};
    type Done = Box<dyn FnOnce(Option<gtk::pango::FontFamily>)>;
    unsafe extern "C" fn finish(
        source: *mut gtk::glib::gobject_ffi::GObject,
        result: *mut gtk::gio::ffi::GAsyncResult,
        data: gtk::glib::ffi::gpointer,
    ) {
        let done = unsafe { Box::from_raw(data as *mut Done) };
        let mut error = std::ptr::null_mut();
        let family = unsafe {
            gtk::ffi::gtk_font_dialog_choose_family_finish(
                source as *mut gtk::ffi::GtkFontDialog,
                result,
                &mut error,
            )
        };
        if !error.is_null() {
            unsafe { gtk::glib::ffi::g_error_free(error) };
        }
        done((!family.is_null()).then(|| unsafe { from_glib_full(family) }));
    }
    let dialog = gtk::FontDialog::new();
    let data: Box<Done> = Box::new(Box::new(done));
    // The task GTK makes holds the dialog until `finish` has run.
    unsafe {
        gtk::ffi::gtk_font_dialog_choose_family(
            dialog.to_glib_none().0,
            parent.to_glib_none().0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            Some(finish),
            Box::into_raw(data) as gtk::glib::ffi::gpointer,
        );
    }
}

/// `font` as a CSS family list, with the generic family it falls back to
/// where the recipient does not have it.
pub(crate) fn recent_font_css(font: &crate::config::RecentFont) -> String {
    let name: String = font
        .name
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect();
    let generic = if font.mono { "monospace" } else { "sans-serif" };
    format!("'{name}', {generic}")
}

/// The editor command that sets `font`.
pub(super) fn font_js(font: &crate::config::RecentFont) -> String {
    format!(
        "window.__hylkiStyled('fontName',{})",
        js_str(&recent_font_css(font))
    )
}

/// The fonts Settings → Composing offers for new messages, as (CSS family
/// list, name) pairs: the default, the font menu's own, the recent picks,
/// and `current` when it is none of those.
pub(crate) fn compose_font_choices(current: &str) -> Vec<(String, String)> {
    let mut out = vec![(String::new(), i18n("Default"))];
    out.extend(
        FONTS
            .iter()
            .map(|(_, css, name)| (css.to_string(), i18n(name))),
    );
    let recent = crate::config::load_recent_fonts();
    for css in recent
        .iter()
        .map(recent_font_css)
        .chain((!current.is_empty()).then(|| current.to_string()))
    {
        if !out.iter().any(|(c, _)| c.eq_ignore_ascii_case(&css)) {
            let name = first_family(&css).to_string();
            out.push((css, name));
        }
    }
    out
}

/// A CSS family list's first family, unquoted.
pub(super) fn first_family(css: &str) -> &str {
    css.split(',')
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches(|c| c == '\'' || c == '"')
}

/// A family list from the settings file made safe to write into a style
/// sheet or a `style` attribute.
fn clean_family(css: &str) -> String {
    css.chars()
        .filter(|c| !matches!(c, '"' | '<' | '>' | '{' | '}' | ';' | '\\' | '&') && !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// The editor's own font, which the quoted original keeps whatever font
/// new messages are set to.
pub(super) const EDITOR_FONT: &str = "14px/1.55 system-ui,sans-serif";

/// The body's `font` shorthand: the font and size set for new messages,
/// or the editor's own.
pub(super) fn body_font() -> String {
    let prefs = crate::config::load_privacy();
    let family = clean_family(&prefs.compose_font);
    if family.is_empty() && prefs.compose_font_size == 0 {
        return EDITOR_FONT.to_string();
    }
    let family = if family.is_empty() {
        "system-ui,sans-serif".to_string()
    } else {
        family
    };
    let size = match prefs.compose_font_size {
        0 => "14px".to_string(),
        pt => format!("{pt}pt"),
    };
    format!("{size}/1.55 {family}")
}

/// The `style` the sent message's own writing is wrapped in, for the font
/// and size set for new messages: the editor's style sheet does not go out
/// with the message. Empty with neither set, leaving the recipient's font.
pub(super) fn font_style(family: &str, pt: u32) -> String {
    let family = clean_family(family);
    let mut style = String::new();
    if !family.is_empty() {
        style.push_str(&format!("font-family:{family};"));
    }
    if pt > 0 {
        style.push_str(&format!("font-size:{pt}pt;"));
    }
    style
}

/// Draw a font menu item in the font it names (a CSS family list), so the
/// menu shows each font rather than naming it. A font not installed here
/// shows in the fallback the message itself would get.
pub(crate) fn preview_font(label: &gtk::Label, family: &str) {
    let family = family
        .split(',')
        .map(|f| f.trim().trim_matches(|c| c == '\'' || c == '"'))
        .collect::<Vec<_>>()
        .join(",");
    let attrs = gtk::pango::AttrList::new();
    attrs.insert(gtk::pango::AttrString::new_family(&family));
    label.set_attributes(Some(&attrs));
}

/// The recent fonts at the foot of the font menu: a divider and one item
/// per font, the one picked going back to the head of the list.
pub(super) fn fill_recent_fonts(
    list: &gtk::Box,
    webview: &gtk::glib::WeakRef<webkit6::WebView>,
    popover: &gtk::glib::WeakRef<gtk::Popover>,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let fonts = crate::config::load_recent_fonts();
    list.set_visible(!fonts.is_empty());
    if fonts.is_empty() {
        return;
    }
    list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    for font in fonts {
        let text = gtk::Label::new(Some(&font.name));
        text.set_xalign(0.0);
        preview_font(&text, &font.name);
        let item = gtk::Button::new();
        item.set_child(Some(&text));
        item.add_css_class("flat");
        item.add_css_class("context-menu-item");
        let (weak, pop) = (webview.clone(), popover.clone());
        item.connect_clicked(move |_| {
            if let Some(p) = pop.upgrade() {
                p.popdown();
            }
            run_restored(&weak, &font_js(&font));
            crate::config::note_recent_font(font.clone());
        });
        list.append(&item);
    }
}

#[cfg(test)]
mod compose_font_tests {
    use super::*;

    /// The default font and size add no style, so the recipient's own font
    /// stays in charge.
    #[test]
    fn style_carries_the_chosen_font() {
        assert_eq!(font_style("", 0), "");
        assert_eq!(
            font_style("Georgia, serif", 12),
            "font-family:Georgia, serif;font-size:12pt;"
        );
        assert_eq!(font_style("", 14), "font-size:14pt;");
    }

    /// A hand-edited settings file cannot close the attribute or the rule.
    #[test]
    fn family_cannot_escape_its_style() {
        assert_eq!(font_style("a\"><script>;}", 0), "font-family:ascript;");
        assert_eq!(first_family("'Comfortaa', sans-serif"), "Comfortaa");
    }
}
