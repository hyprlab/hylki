//! The reader's wrapper document: one sandboxed frame per message, under
//! the card chrome a conversation shows. A static builder (no `&self`) so the
//! tests can exercise it without a display; the live theme reaches it through
//! the `LIVE_*` thread-locals in `frame.rs`.

use super::wrapper_css::wrapper_css;

use super::frame::{
    message_frame, CHROME, GROUND, LIVE_ATTS, LIVE_FOLDBAR, LIVE_FOLDED, LIVE_GROUNDS, LIVE_INVITE,
    LIVE_MARKS, LIVE_READ, LIVE_RECEIPT, LIVE_TAGS, LIVE_TEMPLATES, LIVE_TRANSLATE,
    LIVE_TRANSLATE_ON, LIVE_UNSUB, PAGE,
};
use super::html::attr_escape;
use super::invite::invite_row_html;
use super::receipt_banner::{read_mark_html, receipt_row_html};
use super::remote::has_remote_resources;
use super::size_script::SIZE_SCRIPT;
use super::snippets::{
    att_row_html, card_action_button, inline_icon_svg, paints_own_background, recipient_count,
    recipients_html, tag_chips_html, template_row_html, unsub_row_html,
};
use super::MessageView;
use crate::encoding::escape_html;
use crate::i18n::{i18n, ni18n_f};
use crate::models::Message;
use relm4::prelude::*;

/// The wrapper's Content-Security-Policy and its one script (the frame
/// sizer), under a nonce made for this render.
fn wrapper_scripts() -> (String, String) {
    // Defence in depth for the wrapper: the only script allowed to run is the
    // one carrying this render's nonce, which is ours. Anything a message
    // manages to smuggle into this document — an injected `<script>`, an
    // `onerror=` handler — is refused by the engine even if the escaping
    // above is ever wrong again.
    //
    // Deliberately *only* `script-src`/`object-src`/`base-uri`: no
    // `default-src`. A wrapper policy is inherited by the `srcdoc` frames, so
    // restricting images or styles here would silently re-block the remote
    // content the user has chosen to load. Each frame carries its own
    // `default-src 'none'` policy for that.
    let nonce = crate::rng::nonce(24).ok();
    let csp = match &nonce {
        Some(n) => format!(
            "<meta http-equiv=\"Content-Security-Policy\" content=\"\
             script-src 'nonce-{n}'; object-src 'none'; base-uri 'none'\">"
        ),
        // No entropy, no nonce we can trust — allow no script at all. The
        // iframes render at their default height instead of resizing, which
        // is a visual degradation, not a security one.
        None => "<meta http-equiv=\"Content-Security-Policy\" content=\"\
                 script-src 'none'; object-src 'none'; base-uri 'none'\">"
            .to_string(),
    };
    let sizer = match &nonce {
        Some(n) => {
            // The fold's tooltips and the address menu, in the user's
            // language: the script has no gettext of its own (#529).
            let js = |labels: &[String]| {
                serde_json::to_string(labels)
                    .unwrap_or_default()
                    .replace("</", "<\\/")
            };
            let qt = js(&[i18n("Show quoted text"), i18n("Hide quoted text")]);
            let am = js(&[
                i18n("New Message"),
                i18n("Copy Address"),
                i18n("Add to Contacts"),
            ]);
            format!(
                "<script nonce=\"{n}\">var vireoQT={qt};var hylkiAM={am};{SIZE_SCRIPT}</script>"
            )
        }
        None => String::new(),
    };
    (csp, sizer)
}

/// The grounds (wrapper, conversation page, chrome) the document paints
/// before the messages render: the live theme's when the reader set them
/// (issue #62), the stock GNOME values otherwise (tests).
fn grounds(dark: bool) -> (String, String, String) {
    LIVE_GROUNDS
        .with(|g| g.borrow().clone())
        .unwrap_or_else(|| {
            let (g, p, c) = if dark {
                (GROUND.1, PAGE.1, CHROME.1)
            } else {
                (GROUND.0, PAGE.0, CHROME.0)
            };
            (g.to_string(), p.to_string(), c.to_string())
        })
}

/// What every card of one document shares.
#[derive(Clone, Copy)]
struct Cards<'a> {
    thread: &'a [Message],
    folder_labels: &'a std::collections::HashMap<(u32, u32), String>,
    heights: &'a std::collections::HashMap<(u32, u32), u32>,
    selected: &'a [(u32, u32)],
    accent: &'a str,
    restrict: bool,
    dark: bool,
    always_show_recipients: bool,
    style: &'a crate::config::ReaderStyle,
    sender_style: &'a std::collections::HashSet<(u32, u32)>,
    reader: bool,
    zoom: u32,
    /// Card selection only means something between cards.
    mark_selection: bool,
}

impl MessageView {
    /// The wrapper document: one sandboxed iframe per message (so each email's
    /// CSS is fully isolated and its scripts can't run), with per-message
    /// headers in conversation mode. A small script sizes each iframe to its
    /// content.
    ///
    /// Takes the thread rather than `&self` so it can be exercised without a GTK
    /// display: what it emits is a security boundary, and one that needs
    /// regression cover.
    // Positional, as the reader and the characterization tests call it; the
    // settings become one `Cards` inside.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn conversation_document(
        thread: &[Message],
        folder_labels: &std::collections::HashMap<(u32, u32), String>,
        _no_autoread: &std::collections::HashSet<(u32, u32)>,
        heights: &std::collections::HashMap<(u32, u32), u32>,
        selected: &[(u32, u32)],
        accent: &str,
        restrict: bool,
        dark: bool,
        always_show_recipients: bool,
        single_message_card: bool,
        style: &crate::config::ReaderStyle,
        sender_style: &std::collections::HashSet<(u32, u32)>,
        reader: bool,
        zoom: u32,
    ) -> String {
        // Every message renders with the conversation chrome — a thread of one
        // gets the same in-document header. But only a real conversation is
        // *carded*: inset rounded cards on the deeper page. A lone message
        // goes full-bleed — no gutter, no radius, its ground filling the whole
        // view — so it reads as a message rather than a card in a margin.
        let conversation = !thread.is_empty();
        // A lone message cards up too when the preference asks (#57);
        // otherwise it keeps the full-bleed treatment below.
        let carded = thread.len() > 1 || (conversation && single_message_card);
        // A full-bleed message is no card to outline.
        let mark_selection = carded;
        let cards = Cards {
            thread,
            folder_labels,
            heights,
            selected,
            accent,
            restrict,
            dark,
            always_show_recipients,
            style,
            sender_style,
            reader,
            zoom,
            mark_selection,
        };
        let mut sections = String::new();
        for m in thread {
            let body = card_body(&cards, m);
            if conversation {
                let ava_html = card_avatar(m, dark);
                sections.push_str(&card_html(&cards, m, &ava_html, body));
            } else {
                sections.push_str(&body);
            }
        }
        let scheme = if dark { "dark" } else { "light" };
        // The ⋯ actions toggle: plain white in dark mode — the inherited text
        // color reads too pale there.
        let toggle_color = if dark { "#ffffff" } else { "inherit" };
        // Paint the wrapper and the (still-loading) iframes in the theme color so
        // there's no white flash before each message's content renders.
        let (bg, deep, chrome) = grounds(dark);
        // Each message card only reads as a card against a slightly deeper
        // ground than its own; a full-bleed single message sits on its own
        // ground — the chrome ground when it paints no background (see
        // `plain_css` below) — so the whole view is one color.
        let single_ground = if !carded
            && thread.len() == 1
            && (reader || !paints_own_background(&thread[0].body))
        {
            chrome.clone()
        } else {
            bg.clone()
        };
        let page = if carded { deep } else { single_ground };
        let body_class = if carded { " class=\"hylki-conv\"" } else { "" };
        let (csp, sizer) = wrapper_scripts();
        // A single message that paints no background of its own (plain mail,
        // and much styled mail too — Apple Mail styles fonts and wrapping but
        // sets no background) takes the chrome ground for its *whole* frame,
        // matching the GTK subject block and the header above it: one
        // unbroken pane from subject to the end of the message. A message
        // that declares a background keeps the plain ground so its design
        // renders as intended.
        let single_plain =
            !carded && thread.len() == 1 && (reader || !paints_own_background(&thread[0].body));
        let plain_css = if single_plain {
            format!(
                "body:not(.hylki-conv) .hylki-msg,\
                 body:not(.hylki-conv) iframe.hylki-frame{{background:{chrome};}}"
            )
        } else {
            String::new()
        };
        // The headers around the messages are the window's own chrome, so
        // they follow its direction; the messages inside their frames take
        // their own (#366).
        let dir = if crate::i18n::is_rtl() {
            " dir=\"rtl\""
        } else {
            ""
        };
        format!(
            "<!doctype html><html{dir}><head><meta charset=\"utf-8\">{csp}\
             <meta name=\"color-scheme\" content=\"{scheme}\">\
             <style>{css}</style>{sizer}\
             </head><body{body_class}>{sections}</body></html>",
            css = wrapper_css(
                scheme,
                &page,
                &bg,
                &chrome,
                accent,
                toggle_color,
                &plain_css
            ),
        )
    }
}

/// One card's message: its frame, or a placeholder while the body loads.
fn card_body(cards: &Cards<'_>, m: &Message) -> String {
    let Cards {
        restrict,
        dark,
        heights,
        style,
        sender_style,
        accent,
        reader,
        zoom,
        ..
    } = *cards;
    // A card showing its translation (#327): the message with its
    // text replaced, or for plain text a Reader View fragment.
    let (translated, translated_reader) = LIVE_TRANSLATE
        .with(|t| {
            t.borrow()
                .get(&(m.account_id, m.id))
                .and_then(|(_, shown)| shown.clone())
        })
        .map_or((None, false), |(html, reader)| (Some(html), reader));
    let body = if m.body.trim().is_empty() && translated.is_none() {
        "<div class=\"hylki-loading\">Loading…</div>".to_string()
    } else {
        message_frame(
            translated.as_deref().unwrap_or(&m.body),
            restrict,
            dark,
            (m.account_id, m.id),
            heights.get(&(m.account_id, m.id)).copied(),
            // The card's toggle wins over the preference.
            if sender_style.contains(&(m.account_id, m.id)) {
                &crate::config::ReaderStyle::NONE
            } else {
                style
            },
            accent,
            reader || translated_reader,
            zoom,
        )
    };
    body
}

/// The face on a card's header: a mailbox of your own wears the one it was
/// given (#189), anyone else tinted initials.
fn card_avatar(m: &Message, dark: bool) -> String {
    // One of your own mailboxes wrote this card (#189):
    // show what its account chose — the sidebar's picture,
    // or its emoji on the account color — so your own
    // replies in a conversation wear the face you gave
    // that mailbox rather than plain initials.
    let own = crate::services::avatar::own_face(&m.from_addr).and_then(|face| {
        // The account's own Gravatar leads when it asked
        // for one and the address has one; the picture and
        // the emoji are what it falls back to.
        let gravatar = face
            .gravatar
            .then(|| crate::services::avatar::own_gravatar_data_uri(&m.from_addr, 26))
            .flatten();
        if let Some(uri) = gravatar {
            return Some(format!("<img class=\"hylki-ava\" src=\"{uri}\" alt=\"\">"));
        }
        match (&face.picture, &face.emoji) {
            (Some(path), _) => crate::ui::initials::picture_data_uri(path, 26)
                .map(|uri| format!("<img class=\"hylki-ava\" src=\"{uri}\" alt=\"\">")),
            (None, Some(emoji)) => {
                let png = gtk::gdk::RGBA::parse(&face.color)
                    .ok()
                    .and_then(|bg| crate::ui::initials::png_data_uri(emoji, bg, 26));
                Some(match png {
                    Some(uri) => {
                        format!("<img class=\"hylki-ava\" src=\"{uri}\" alt=\"\">")
                    }
                    None => format!(
                        "<span class=\"hylki-ava\" style=\"background:{bg}\">{glyph}</span>",
                        bg = attr_escape(&face.color),
                        glyph = escape_html(emoji),
                    ),
                })
            }
            (None, None) => None,
        }
    });
    own.unwrap_or_else(|| {
        let initial = m
            .shown_from_name()
            .trim()
            .chars()
            .next()
            .or_else(|| m.from_addr.trim().chars().next())
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string());
        let hue = m
            .from_addr
            .to_ascii_lowercase()
            .bytes()
            .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))
            % 360;
        let l = if dark { 38 } else { 45 };
        // Drawn ink-centred by ui::initials and embedded as
        // a PNG (the same tint); the markup span stands in
        // only before a window exists to render with.
        let bg = crate::ui::initials::hsl(f64::from(hue), 0.52, f64::from(l) / 100.0);
        match crate::ui::initials::png_data_uri(&initial, bg, 26) {
            Some(uri) => format!("<img class=\"hylki-ava\" src=\"{uri}\" alt=\"\">"),
            None => format!(
                "<span class=\"hylki-ava\" style=\"background:hsl({hue},52%,{l}%)\">{}</span>",
                escape_html(&initial),
            ),
        }
    })
}

/// One conversation card: header, banners, the message and its attachments.
fn card_html(cards: &Cards<'_>, m: &Message, ava_html: &str, body: String) -> String {
    let Cards {
        thread,
        mark_selection,
        selected,
        always_show_recipients,
        folder_labels,
        ..
    } = *cards;
    // The sender-authentication seal (#88): hidden until a
    // verdict arrives (SenderCheckFor patches it live), then
    // tinted by trust — Bazaar's fixed blue for a pass.
    let verify = format!(
        "<button type=\"button\" class=\"hylki-verify\" data-key=\"{aid}:{id}\" \
             title=\"\">{svg}</button>\
             <button type=\"button\" class=\"hylki-pgp\" data-key=\"{aid}:{id}\" \
             title=\"\">{lock}{sig}<span class=\"hylki-pgp-text\"></span></button>\
             <span class=\"hylki-sens\" data-key=\"{aid}:{id}\" title=\"\">{sens}\
             <span class=\"hylki-sens-text\"></span></span>",
        aid = m.account_id,
        id = m.id,
        svg = inline_icon_svg("verified-checkmark-symbolic"),
        // The OpenPGP chip (#133): a lock for an encrypted
        // message, a shield for a signed one, then the
        // verdict in words (#300); hidden until the verdict
        // is patched in.
        lock = inline_icon_svg("channel-secure-symbolic"),
        sig = inline_icon_svg("security-high-symbolic"),
        // The sensitivity label (#411), hidden until a
        // label is patched in.
        sens = inline_icon_svg("tag-outline-symbolic"),
    );
    format!(
        "<section class=\"hylki-msg{sel}{unread_cls}{starred_cls}{folded_cls}\" data-key=\"{aid}:{id}\">\
               <header class=\"hylki-msg-hdr\" data-key=\"{aid}:{id}\" \
                 title=\"{hdr_title}\">\
                 <div class=\"hylki-hdr-line\">\
                   {ava}{dot}<span class=\"hylki-from\">{from}</span>{verify}{addr}\
                   <span class=\"hylki-tags\" data-key=\"{aid}:{id}\">{tags}</span>\
                   <span class=\"hylki-hdr-meta\">{folder}{rcpt_toggle}{marks}{read}\
                     <span class=\"hylki-date\">{date}</span></span>\
                   {acts_toggle}{acts}\
                 </div>{rcpt}\
               </header>{fbar}{tpl}{invite}{receipt}{unsub}{tr}{body}{atts}</section>",
        aid = m.account_id,
        id = m.id,
        // The message's own attachments beneath its body (#213).
        // The row is always emitted, so files arriving after
        // the paint have somewhere to be patched into.
        atts = LIVE_ATTS.with(|a| {
            let key = (m.account_id, m.id);
            att_row_html(key, a.borrow().get(&key).map(|v| v.as_slice()))
        }),
        // A template's banner (#360), first: what the message
        // is for matters more than anything it says.
        tpl = if LIVE_TEMPLATES.with(|t| t.borrow().contains(&(m.account_id, m.id))) {
            template_row_html((m.account_id, m.id))
        } else {
            String::new()
        },
        // The invitation banner (#223) at the top of the card,
        // above the Unsubscribe one and on the same terms: the
        // container is always there, empty (hidden) for a
        // message about no meeting, so an invitation read after
        // the paint is patched in without a re-render.
        invite = LIVE_INVITE.with(|i| {
            invite_row_html(
                (m.account_id, m.id),
                i.borrow()
                    .get(&(m.account_id, m.id))
                    .map(String::as_str)
                    .unwrap_or(""),
            )
        }),
        // Sent mail's read receipt checkmarks (#520), beside the date:
        // always there, empty (hidden) unless the message asked for one.
        read = LIVE_READ.with(|r| {
            read_mark_html((m.account_id, m.id), r.borrow().get(&(m.account_id, m.id)))
        }),
        // The read receipt banner (#520), on the same terms.
        receipt = LIVE_RECEIPT.with(|r| {
            receipt_row_html(
                (m.account_id, m.id),
                r.borrow()
                    .get(&(m.account_id, m.id))
                    .map(String::as_str)
                    .unwrap_or(""),
            )
        }),
        // The Unsubscribe banner between header and body: the
        // container is always there, empty (hidden) for a
        // message from no list, so a verdict arriving after the
        // paint is patched in without a re-render.
        // The translation banner (#327), on the same terms.
        tr = LIVE_TRANSLATE.with(|t| {
            format!(
                "<div class=\"hylki-tr\" data-key=\"{}:{}\">{}</div>",
                m.account_id,
                m.id,
                t.borrow()
                    .get(&(m.account_id, m.id))
                    .map(|(b, _)| b.as_str())
                    .unwrap_or("")
            )
        }),
        unsub = LIVE_UNSUB.with(|u| {
            unsub_row_html(
                (m.account_id, m.id),
                u.borrow()
                    .get(&(m.account_id, m.id))
                    .map(String::as_str)
                    .unwrap_or(""),
            )
        }),
        hdr_title = gtk::glib::markup_escape_text(&i18n("Double-click to open in a new window")),
        tags = LIVE_TAGS.with(|t| tag_chips_html(&t.borrow(), &m.keywords, &m.labels)),
        // The ⋯ toggle that expands/collapses the action row when
        // the hidden-until-hover preference is on; CSS keeps it
        // display:none otherwise, so it costs nothing when off.
        acts_toggle = if !thread.is_empty() {
            format!(
                "<button type=\"button\" class=\"hylki-acts-toggle\" \
                     title=\"{acts_title}\" data-key=\"{aid}:{id}\">{svg}</button>",
                aid = m.account_id,
                id = m.id,
                acts_title = gtk::glib::markup_escape_text(&i18n("Actions")),
                // The same ⋯ the list's actions palette toggle uses.
                svg = inline_icon_svg("view-more-horizontal-symbolic"),
            )
        } else {
            String::new()
        },
        // Per-card actions, on every message — single messages
        // included: the reader toolbar carries no per-message
        // icons any more, so the card's own row (and the overflow
        // menu) is where a message is acted on.
        acts = if !thread.is_empty() {
            card_actions(cards, m)
        } else {
            String::new()
        },
        sel = if mark_selection && selected.contains(&(m.account_id, m.id)) {
            " selected"
        } else {
            ""
        },
        unread_cls = if m.unread { " unread" } else { "" },
        starred_cls = if m.starred { " starred" } else { "" },
        marks = LIVE_MARKS.with(|b| {
            b.borrow()
                .get(&(m.account_id, m.id))
                .cloned()
                .unwrap_or_default()
        }),
        // Folded (#326): the header and a line of preview.
        folded_cls = if LIVE_FOLDED.with(|f| f.borrow().contains(&(m.account_id, m.id))) {
            " hylki-folded"
        } else {
            ""
        },
        // What a folded card shows under its header instead of the
        // rest (#326).
        fbar = LIVE_FOLDBAR.with(|b| {
            b.borrow()
                .get(&(m.account_id, m.id))
                .cloned()
                .unwrap_or_default()
        }),
        // `escape_html`, not `attr_escape`: these land in element
        // text content, where `<` and `>` are structural. A `From:`
        // display name is attacker-controlled (and RFC 2047-decoded,
        // so any byte sequence can be delivered), and this document
        // is the trusted wrapper — not a sandboxed message frame.
        from = escape_html(&m.shown_from_name()),
        // An initials circle, tinted per sender address, so who
        // wrote each card — and which cards are your own replies —
        // reads at a glance (#22). Nothing is linked: a picture
        // arrives as an embedded PNG, never a path the document
        // could reach for, and the initial is escaped like every
        // other header field.
        ava = ava_html,
        addr = if m.from_addr.is_empty() {
            String::new()
        } else {
            // `data-addr` feeds the hover reveal's ::after overlay;
            // `data-mail` (the bare address) drives click-to-compose
            // and the copy menu. Fully entity-escaped (&<>"), so an
            // attacker-controlled address can't smuggle markup
            // through the attributes.
            format!(
                "<span class=\"hylki-addr hylki-mail\" data-mail=\"{0}\" \
                     data-addr=\"&lt;{0}&gt;\">&lt;{0}&gt;</span>",
                escape_html(&m.from_addr).replace('"', "&quot;")
            )
        },
        date = escape_html(&m.datetime_full()),
        // Everyone the message went to, tucked behind a small chip so
        // a card's header stays one line tall until asked. Escaped
        // like the sender: recipient headers are attacker-controlled.
        rcpt_toggle = match recipient_count(m) {
            0 => String::new(),
            // With the recipients line always visible, a
            // single-recipient chip would only repeat it (#40).
            1 if always_show_recipients => String::new(),
            n => format!(
                "<button type=\"button\" class=\"hylki-rcpt-toggle{open}\" \
                     title=\"{title}\">{label}</button>",
                label = gtk::glib::markup_escape_text(&ni18n_f(
                    "{n} recipient",
                    "{n} recipients",
                    n as u32,
                    &[("n", &n.to_string())],
                )),
                open = if always_show_recipients { " open" } else { "" },
                title = if always_show_recipients {
                    i18n("Hide recipients")
                } else {
                    i18n("Show recipients")
                },
            ),
        },
        rcpt = recipients_html(m, always_show_recipients),
        // Where this message was read from, when that isn't the
        // folder on screen — the reply you sent, pulled in from Sent.
        folder = match folder_labels.get(&(m.account_id, m.id)) {
            Some(label) => format!("<span class=\"hylki-folder\">{}</span>", escape_html(label)),
            None => String::new(),
        },
        body = body,
        // Unread messages in a conversation are marked; the mark
        // clears when the user clicks the card to read it —
        // scrolling past no longer counts as reading.
        dot = if m.unread {
            format!(
                "<span class=\"hylki-dot\" data-key=\"{}:{}\"></span>",
                m.account_id, m.id
            )
        } else {
            String::new()
        },
    )
}

/// A card's action buttons, in the reader toolbar's order.
fn card_actions(cards: &Cards<'_>, m: &Message) -> String {
    let Cards {
        style,
        reader,
        sender_style,
        restrict,
        ..
    } = *cards;
    let key = (m.account_id, m.id);
    // Action-showing icon (read envelope = "mark as
    // read"), like the menus and toolbar. Both icons
    // are baked in; the section's `unread` class picks
    // one, so marking read/unread flips in place.
    let toggle_read = format!(
        "<button type=\"button\" class=\"hylki-act\" data-act=\"toggleread\" \
             data-key=\"{aid}:{id}\" title=\"{title}\">\
             <span class=\"tr-when-unread\">{read_svg}</span>\
             <span class=\"tr-when-read\">{unread_svg}</span></button>",
        aid = key.0,
        id = key.1,
        title = gtk::glib::markup_escape_text(&i18n("Mark as read or unread")),
        read_svg = inline_icon_svg("hylki-mail-read-symbolic"),
        unread_svg = inline_icon_svg("mail-unread-symbolic"),
    );
    // The star keeps one glyph; the starred state is
    // color alone (`.on`, toggled optimistically on
    // click too).
    let star = format!(
        "<button type=\"button\" class=\"hylki-act{on}\" data-act=\"star\" \
             data-key=\"{aid}:{id}\" title=\"{title}\">{svg}</button>",
        on = if m.starred { " on" } else { "" },
        title = gtk::glib::markup_escape_text(&i18n("Star this message")),
        aid = key.0,
        id = key.1,
        svg = inline_icon_svg("hylki-non-starred-symbolic"),
    );
    let source_and_translate = format!(
        "{}{}",
        card_action_button(key, "viewsource", "code-symbolic", &i18n("View source")),
        if LIVE_TRANSLATE_ON.with(|on| on.get()) {
            card_action_button(
                key,
                "translate",
                "translate-symbolic",
                &i18n("Translate this message"),
            )
        } else {
            String::new()
        },
    );
    format!(
        "<span class=\"hylki-acts\">{}{}{}{}{}{}{}{}{}{}{}{}{}</span>",
        // Same order as the reader toolbar and the list's
        // actions palette, View Source closing the line.
        card_action_button(
            key,
            "reply",
            "mail-reply-sender-symbolic",
            &i18n("Reply to this message")
        ),
        card_action_button(
            key,
            "replyall",
            "mail-reply-all-symbolic",
            &i18n("Reply to everyone on this message")
        ),
        card_action_button(
            key,
            "forward",
            "mail-forward-symbolic",
            &i18n("Forward this message")
        ),
        toggle_read,
        star,
        card_action_button(
            key,
            "moveto",
            "folder-symbolic",
            &i18n("Move this message to a folder")
        ),
        card_action_button(
            key,
            "archive",
            "mail-archive-symbolic",
            &i18n("Archive this message")
        ),
        card_action_button(
            key,
            "delete",
            "user-trash-symbolic",
            &i18n("Delete this message")
        ),
        card_action_button(
            key,
            "spam",
            "mail-mark-junk-symbolic",
            &i18n("Mark as Spam")
        ),
        card_action_button(
            key,
            "contact",
            "contact-new-symbolic",
            &i18n("Add sender to Contacts")
        ),
        source_and_translate,
        // The escape from the reader's own fonts and
        // colors (#56): only offered while an override
        // is on, lit while this card shows the sender's.
        if style.active() && !reader {
            let on = sender_style.contains(&key);
            format!(
                "<button type=\"button\" class=\"hylki-act{on_cls}\" data-act=\"senderfmt\" \
                     data-key=\"{aid}:{id}\" title=\"{title}\">{svg}</button>",
                on_cls = if on { " on" } else { "" },
                aid = key.0,
                id = key.1,
                title = gtk::glib::markup_escape_text(&if on {
                    i18n("Back to my fonts and colors")
                } else {
                    i18n("Show the sender's fonts and colors")
                }),
                svg = inline_icon_svg("format-text-rich-symbolic"),
            )
        } else {
            String::new()
        },
        // Remote content, per message. The banner offers
        // the same thing, but it can be switched off
        // (Settings → Privacy) and then there is nothing to
        // click — and it only ever lets content in, where
        // this puts the block back too. Offered only on a
        // card that actually has something to load.
        if has_remote_resources(&m.body) {
            format!(
                "<button type=\"button\" class=\"hylki-act{on_cls}\" data-act=\"remote\" \
                     data-key=\"{aid}:{id}\" title=\"{title}\">{svg}</button>",
                on_cls = if restrict { "" } else { " on" },
                aid = key.0,
                id = key.1,
                title = gtk::glib::markup_escape_text(&if restrict {
                    i18n("Show remote content")
                } else {
                    i18n("Block remote content")
                }),
                svg = inline_icon_svg("image-x-generic-symbolic"),
            )
        } else {
            String::new()
        },
    )
}
