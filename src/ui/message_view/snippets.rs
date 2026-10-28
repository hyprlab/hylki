//! Small HTML pieces of a card: action buttons, inline icons, recipients,
//! attachment and tag chips, and the banner containers.

use super::html::attr_escape;
use super::types::CardAttachment;
use crate::encoding::escape_html;
use crate::i18n::i18n;
use crate::models::Message;
use relm4::prelude::*;

/// Where a folded card's bar takes the sender's circle, which the document
/// builder fills in with the one the card's header wears.
pub(super) const FOLD_BAR_FACE: &str = "\u{1}face\u{1}";

/// The day on a folded card's bar, short as Proton has it: the time for
/// today, the day and month this year, the year too before that.
pub(super) fn fold_bar_date(m: &Message) -> String {
    crate::models::date_short(m.timestamp, &m.date)
}

/// A template card's banner (#360): what the message is, and its two uses.
pub(super) fn template_row_html(key: (u32, u32)) -> String {
    let button = |verb: &str, label: &str, title: &str| {
        format!(
            "<button type=\"button\" class=\"hylki-unsub-btn hylki-tpl-btn\" data-key=\"{}:{}\" \
             data-tpl=\"{verb}\" title=\"{}\">{}</button>",
            key.0,
            key.1,
            attr_escape(title),
            escape_html(label),
        )
    };
    format!(
        "<div class=\"hylki-unsub hylki-tpl\"><span class=\"hylki-unsub-text\">{}</span>{}{}</div>",
        escape_html(&i18n("This message is a template.")),
        button(
            "edit",
            &i18n("Edit Template"),
            &i18n("Change the template itself")
        ),
        button(
            "use",
            &i18n("Use Template"),
            &i18n("Start a new message from a copy of the template")
        ),
    )
}

/// A card's Unsubscribe banner: the container, always emitted so a verdict
/// arriving later can be patched into it (empty, it is hidden by the
/// stylesheet). `inner` is what [`MessageView::unsub_inner_html`] rendered.
pub(super) fn unsub_row_html(key: (u32, u32), inner: &str) -> String {
    format!(
        "<div class=\"hylki-unsub\" data-key=\"{}:{}\">{inner}</div>",
        key.0, key.1
    )
}

/// A card's attachment row (#213): the container, always emitted so a later
/// arrival can be patched into it (an empty row is hidden by the stylesheet).
pub(super) fn att_row_html(key: (u32, u32), atts: Option<&[CardAttachment]>) -> String {
    format!(
        "<div class=\"hylki-atts\" data-key=\"{}:{}\">{}</div>",
        key.0,
        key.1,
        att_chips_html(key, atts.unwrap_or(&[]))
    )
}

/// The chips for one card's attachments (#213): the gallery's type icon in
/// its color, the name, the size, and a save button. The chip opens the
/// file; both post the message key and the attachment's index back.
pub(super) fn att_chips_html(key: (u32, u32), atts: &[CardAttachment]) -> String {
    use crate::ui::thumbnails::{icon_color_class, icon_for};
    let mut out = String::new();
    for (idx, a) in atts.iter().enumerate() {
        let icon = icon_for(&a.name);
        out.push_str(&format!(
            "<span class=\"hylki-attw\">\
             <button type=\"button\" class=\"hylki-attc {cls}\" data-key=\"{aid}:{id}\" \
             data-idx=\"{idx}\" title=\"{open}\">{svg}\
             <span class=\"hylki-attn\">{name}</span>\
             <span class=\"hylki-attz\">{size}</span></button>\
             <button type=\"button\" class=\"hylki-attsave\" data-key=\"{aid}:{id}\" \
             data-idx=\"{idx}\" title=\"{save}\">{dl}</button></span>",
            cls = icon_color_class(&a.name),
            aid = key.0,
            id = key.1,
            open = gtk::glib::markup_escape_text(&i18n("Open")),
            svg = inline_icon_svg(icon),
            // The name comes from the sender: text content, escaped like
            // every other header field.
            name = escape_html(&a.name),
            size = crate::models::human_size(a.size),
            save = gtk::glib::markup_escape_text(&i18n("Save\u{2026}")),
            dl = inline_icon_svg("folder-download-symbolic"),
        ));
    }
    out
}

/// The tag chips of a card header (#71): one pill per keyword naming a tag,
/// colored inline (the document has no access to the app's stylesheet).
pub(super) fn tag_chips_html(
    tags: &[crate::config::Tag],
    keywords: &[String],
    labels: &[String],
) -> String {
    let mut out = String::new();
    for pill in crate::models::pills(keywords, labels, tags) {
        match pill {
            crate::models::Pill::Tag(t) => out.push_str(&format!(
                "<span class=\"hylki-tag\" style=\"background:{bg};color:{fg}\">{name}</span>",
                bg = gtk::glib::markup_escape_text(&t.color),
                fg = crate::color::readable_text(&t.color),
                name = gtk::glib::markup_escape_text(&t.name),
            )),
            crate::models::Pill::Label(name) => {
                let color = crate::config::TAG_COLORS[crate::models::label_hue(&name)];
                out.push_str(&format!(
                    "<span class=\"hylki-tag hylki-label\" style=\"background:{color};color:{fg}\" title=\"{name}\">{name}</span>",
                    fg = crate::color::readable_text(color),
                    name = gtk::glib::markup_escape_text(&name),
                ))
            }
        }
    }
    out
}

/// One icon button on a conversation card's action line. The icon is the same
/// embedded symbolic SVG the toolbar draws, inlined (its paths carry no fill,
/// so the document's `fill:currentColor` recolors it); when the resource
/// bundle isn't registered (tests), the button simply has no glyph.
pub(super) fn card_action_button(key: (u32, u32), act: &str, icon: &str, title: &str) -> String {
    let svg = inline_icon_svg(icon);
    format!(
        "<button type=\"button\" class=\"hylki-act\" data-act=\"{act}\" \
         data-key=\"{aid}:{id}\" title=\"{title}\">{svg}</button>",
        aid = key.0,
        id = key.1,
    )
}

/// The card's "sender's formatting" toggle icon (#56), named in full here
/// so `tools/gen-icon-gresource.sh` (which scans for the prefixed literal)
/// bundles it; the card draws it inline through `inline_icon_svg`.
#[allow(dead_code)]
const SENDER_STYLE_ICON: &str = "format-text-rich-symbolic";

/// The OpenPGP chip's lock (#133), named in full for the same reason.
#[allow(dead_code)]
const PGP_LOCK_ICON: &str = "channel-secure-symbolic";

/// An embedded symbolic icon's SVG, inlined for the wrapper document (its
/// paths carry no fill, so the document's `fill:currentColor` recolors it);
/// empty when the resource bundle isn't registered (tests).
pub(super) fn inline_icon_svg(icon: &str) -> String {
    let path = format!("/co/hyprlab/Hylki/icons/scalable/actions/{icon}.svg");
    gtk::gio::resources_lookup_data(&path, gtk::gio::ResourceLookupFlags::NONE)
        .ok()
        .and_then(|b| String::from_utf8(b.to_vec()).ok())
        .map(|s| {
            s.replacen("<?xml version=\"1.0\" encoding=\"UTF-8\"?>", "", 1)
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

/// Whether a message body paints a background of its own. The colon/equals
/// forms keep prose that merely mentions "background" from counting.
pub(super) fn paints_own_background(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    [
        "background:",
        "background-color:",
        "background-image:",
        "bgcolor=",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// How many addresses a message's To, Cc and Bcc headers name.
pub(super) fn recipient_count(m: &Message) -> usize {
    [m.to.as_str(), m.cc.as_str(), m.bcc.as_str()]
        .iter()
        .flat_map(|s| s.split(','))
        .filter(|a| !a.trim().is_empty())
        .count()
}

/// The collapsible To/Cc/Bcc block inside a conversation card's header.
/// Starts hidden; the header's recipients chip toggles it. Empty when the
/// message names no recipients at all. Bcc is there only on the sender's own
/// copy (#476).
pub(super) fn recipients_html(m: &Message, always_shown: bool) -> String {
    let mut lines = String::new();
    for (label, list) in [
        ("To", m.to.trim()),
        ("Cc", m.cc.trim()),
        ("Bcc", m.bcc.trim()),
    ] {
        if list.is_empty() {
            continue;
        }
        // Each recipient becomes a `.hylki-mail` span (click composes to it,
        // right-click offers copy/new-message), keyed by its bare address.
        let mut parts = String::new();
        for entry in split_addr_list(list) {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let mail = entry
                .rfind('<')
                .and_then(|s| entry[s + 1..].find('>').map(|e| &entry[s + 1..s + 1 + e]))
                .unwrap_or(entry)
                .trim();
            if !parts.is_empty() {
                parts.push_str(", ");
            }
            // A saved contact by its saved name alone (#541): the address
            // is in the right-click menu.
            let shown = crate::contacts::contact_name(mail);
            parts.push_str(&format!(
                "<span class=\"hylki-mail\" data-mail=\"{}\">{}</span>",
                escape_html(mail).replace('"', "&quot;"),
                escape_html(shown.as_deref().unwrap_or(entry))
            ));
        }
        lines.push_str(&format!("<div><b>{label}:</b> {parts}</div>"));
    }
    if lines.is_empty() {
        return String::new();
    }
    let hidden = if always_shown { "" } else { " hidden" };
    format!("<div class=\"hylki-rcpt\"{hidden}>{lines}</div>")
}

/// Split an address-list header on commas, honouring double-quoted display
/// names (`"Doe, Jane" <j@x>` stays one entry).
pub(super) fn split_addr_list(list: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    for (i, c) in list.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                out.push(&list[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&list[start..]);
    out
}
