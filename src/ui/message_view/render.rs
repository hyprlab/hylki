//! Rendering: deciding whether the document must be built again, building
//! it with the live state handed to the static builder, and stamping the
//! body with the settings the page script reads.

use super::frame::{
    LIVE_ATTS, LIVE_FOLDBAR, LIVE_FOLDED, LIVE_GROUNDS, LIVE_INVITE, LIVE_MARKS, LIVE_READ,
    LIVE_RECEIPT, LIVE_TAGS, LIVE_TEMPLATES, LIVE_TRANSLATE, LIVE_TRANSLATE_ON, LIVE_UNSUB,
};
use super::types::TrState;
use super::MessageView;
use crate::i18n::i18n;
use adw::prelude::*;
use relm4::prelude::*;

impl MessageView {
    pub(super) fn render(&mut self) {
        let dark = self.effective_dark();
        self.apply_webview_bg(dark);
        // Already showing exactly this — returning to a conversation, or a
        // re-render nothing changed. Loading it again would only blank the view
        // and paint the same pixels back.
        let fingerprint = self.render_fingerprint(dark);
        if self.webview_ready && self.shown_fingerprint == Some(fingerprint) {
            return;
        }
        self.shown_fingerprint = Some(fingerprint);
        // A page handed over while the window is hidden (running in the
        // background, a notification about to bring it back) may never be
        // painted: it is loaded again once the view is on screen (#332).
        if !self.webview.is_mapped() {
            self.painted_hidden = true;
        }
        // A conversation is covered until its new document has painted: the
        // spinner is already up from the moment the thread was opened, so it
        // simply stays until there is something to replace it — one transition,
        // not three.
        //
        // A single message is not. It is one small frame that paints in a few
        // milliseconds, and covering that is how a plain message ended up
        // showing a spinner every time it was opened; the view keeps what it has
        // until the new document replaces it.
        if self.thread.len() > 1 && !self.instant {
            self.webview_ready = false;
        }
        // Only a page that stays in view dissolves into the next; behind the
        // spinner there is nothing to see.
        let fade = self.webview_ready && !self.loading && !std::mem::take(&mut self.skip_fade);
        let html = self.document_html(dark);
        // A message painted alone that has since grown into a conversation
        // gets the conversation's open scroll, unless the reader has been
        // scrolled meanwhile: otherwise it stays at the top, on the oldest
        // part, often the user's own sent original (#351).
        if self.did_autoscroll
            && self.autoscroll_lone
            && self.thread.len() > 1
            && self.saved_anchor.is_none()
        {
            self.did_autoscroll = false;
        }
        // A conversation opened behind the loading cover spends its open
        // scroll on a document nobody saw; the repaint once it is on screen
        // would otherwise keep that unseen place, the top.
        if self.webview.is_mapped() {
            if std::mem::take(&mut self.scroll_owed) && self.saved_anchor.is_none() {
                self.did_autoscroll = false;
            }
        } else if !self.did_autoscroll {
            self.scroll_owed = true;
        }
        let stamps = body_stamps(&BodyStamps {
            noscroll: self.did_autoscroll,
            anchor: self.saved_anchor,
            anchor_gutter: self.anchor_gutter,
            card_actions_hover: self.card_actions_hover,
            card_actions_auto: self.card_actions_auto,
            palette_collapse_secs: self.palette_collapse_secs,
            card_palette_menu: self.card_palette_menu,
            pgp_labels: self.pgp_labels,
            // The newest member, for the open-scroll fallback (#101).
            newest: self
                .thread
                .iter()
                .max_by_key(|m| m.timestamp)
                .filter(|_| self.thread.len() > 1)
                .map(|m| (m.account_id, m.id)),
            read_mark: self.read_mark,
        });
        let html = html.replacen("<body", &format!("<body{stamps}"), 1);
        self.did_autoscroll = true;
        self.autoscroll_lone = self.thread.len() <= 1;
        let n = self.seq.get().wrapping_add(1);
        self.seq.set(n);
        self.load_after_paint(html, format!("https://hylki.localhost/message/{n}"), fade);
    }

    /// Everything the rendered document depends on, in one number. Ordered by
    /// the thread so it is stable — the sets it consults are read through it
    /// rather than iterated, whose order is not.
    fn render_fingerprint(&self, dark: bool) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        dark.hash(&mut h);
        // A theme change can move the grounds without flipping `dark` (a
        // different GTK theme variant, say) — the document must follow.
        self.theme_grounds(dark).hash(&mut h);
        self.remote_allowed.hash(&mut h);
        self.reader_style.hash(&mut h);
        self.reader_mode.hash(&mut h);
        // Translations (#327): which cards show one, and whether cards
        // offer Translate at all.
        crate::services::translate::generation().hash(&mut h);
        self.fold_messages.hash(&mut h);
        self.fold_preview_lines.hash(&mut h);
        let mut shown: Vec<(u32, u32, usize)> = self
            .translations
            .iter()
            .filter_map(|(k, t)| match t {
                TrState::Shown(t) => Some((k.0, k.1, t.html.len())),
                _ => None,
            })
            .collect();
        shown.sort_unstable();
        shown.hash(&mut h);
        self.card_actions_hover.hash(&mut h);
        self.card_actions_auto.hash(&mut h);
        self.palette_collapse_secs.hash(&mut h);
        self.card_palette_menu.hash(&mut h);
        // Whether cards list their attachments (#213); the lists themselves
        // are patched in live rather than rendered for.
        self.chips_shown().hash(&mut h);
        // The picture a mailbox of your own shows can change while a
        // conversation is open (#189) — the cards must be built again.
        crate::services::avatar::own_faces_generation().hash(&mut h);
        // Names from Contacts came in, or the switch for them moved (#541).
        crate::contacts::contact_names_generation().hash(&mut h);
        self.thread.len().hash(&mut h);
        for m in &self.thread {
            let key = (m.account_id, m.id);
            key.hash(&mut h);
            m.unread.hash(&mut h);
            m.from_name.hash(&mut h);
            m.from_addr.hash(&mut h);
            m.to.hash(&mut h);
            m.cc.hash(&mut h);
            m.bcc.hash(&mut h);
            m.body.hash(&mut h);
            m.datetime_full().hash(&mut h);
            self.no_autoread.contains(&key).hash(&mut h);
            self.sender_style.contains(&key).hash(&mut h);
            self.folder_labels.get(&key).hash(&mut h);
            self.sent.contains(&key).hash(&mut h);
            self.template_folders
                .contains(&(m.account_id, m.folder_id))
                .hash(&mut h);
            // Tags are patched into a card live; Gmail labels (#374) only
            // change with a sync, and are drawn with the card.
            m.labels.hash(&mut h);
        }
        h.finish()
    }

    /// The wrapper document for what is currently on screen.
    fn document_html(&self, dark: bool) -> String {
        // Hand the live theme grounds to the (display-free, testable) document
        // builder without widening its signature — see LIVE_GROUNDS.
        LIVE_GROUNDS.with(|g| *g.borrow_mut() = Some(self.theme_grounds(dark)));
        LIVE_TAGS.with(|t| *t.borrow_mut() = self.tags.clone());
        LIVE_ATTS.with(|a| {
            *a.borrow_mut() = if self.chips_shown() {
                self.card_atts.clone()
            } else {
                Default::default()
            }
        });
        LIVE_UNSUB.with(|u| {
            *u.borrow_mut() = self
                .thread
                .iter()
                .map(|m| ((m.account_id, m.id), self.unsub_inner_html(m)))
                .collect()
        });
        LIVE_INVITE.with(|i| {
            *i.borrow_mut() = self
                .thread
                .iter()
                .map(|m| ((m.account_id, m.id), self.invite_inner_html(m)))
                .collect()
        });
        LIVE_RECEIPT.with(|r| {
            *r.borrow_mut() = self
                .thread
                .iter()
                .map(|m| ((m.account_id, m.id), self.receipt_inner_html(m)))
                .collect()
        });
        LIVE_READ.with(|r| {
            *r.borrow_mut() = self
                .thread
                .iter()
                .filter_map(|m| Some(((m.account_id, m.id), self.receipt_marks.of(m)?)))
                .collect()
        });
        LIVE_FOLDED.with(|f| *f.borrow_mut() = self.folded_cards());
        LIVE_TEMPLATES.with(|t| {
            *t.borrow_mut() = self
                .thread
                .iter()
                .filter(|m| self.template_folders.contains(&(m.account_id, m.folder_id)))
                .map(|m| (m.account_id, m.id))
                .collect()
        });
        LIVE_FOLDBAR.with(|b| {
            *b.borrow_mut() = if self.carded() {
                self.thread
                    .iter()
                    .map(|m| ((m.account_id, m.id), self.fold_bar_html(m)))
                    .collect()
            } else {
                Default::default()
            }
        });
        LIVE_MARKS.with(|b| {
            *b.borrow_mut() = if self.carded() {
                self.thread
                    .iter()
                    .map(|m| ((m.account_id, m.id), self.card_marks_html(m)))
                    .collect()
            } else {
                Default::default()
            }
        });
        let translation = crate::services::translate::load();
        LIVE_TRANSLATE_ON.with(|on| on.set(translation.enabled()));
        LIVE_TRANSLATE.with(|t| {
            *t.borrow_mut() = self
                .thread
                .iter()
                .map(|m| {
                    let key = (m.account_id, m.id);
                    let shown = match self.translations.get(&key) {
                        Some(TrState::Shown(t)) => Some((t.html.clone(), t.reader)),
                        _ => None,
                    };
                    (key, (self.translate_inner_html(m, &translation), shown))
                })
                .collect()
        });
        Self::conversation_document(
            &self.thread,
            &self.card_labels(),
            &self.no_autoread,
            &self.frame_heights,
            &self.selected_cards,
            &self.accent_hex(),
            !self.remote_allowed,
            dark,
            self.always_show_recipients,
            self.single_message_card,
            &self.reader_style,
            &self.sender_style,
            self.reader_mode,
            self.zoom,
        )
    }
}

/// The settings the page script reads off `<body>` attributes. They ride on
/// the body rather than the builder's parameters so the static document
/// builder (and its tests) need not know them.
#[derive(Debug, Clone, Copy)]
struct BodyStamps {
    /// The conversation has had its open scroll: keep the reader's place.
    noscroll: bool,
    /// The card and offset the reader was scrolled to, if saved.
    anchor: Option<(u32, u32, u32)>,
    /// The saved place was in the gutter between cards.
    anchor_gutter: bool,
    card_actions_hover: bool,
    card_actions_auto: bool,
    palette_collapse_secs: u64,
    card_palette_menu: bool,
    pgp_labels: bool,
    /// The newest member of a conversation (none for a single message).
    newest: Option<(u32, u32)>,
    pub(super) read_mark: crate::config::ReadMark,
}

/// The attributes spliced into the wrapper's `<body` tag, in a fixed order.
fn body_stamps(s: &BodyStamps) -> String {
    // Only a conversation's first document may auto-scroll to the unread
    // mark; every later render of the same thread (bodies streaming in, a
    // theme change) is stamped no-scroll so the reader's place is kept.
    let noscroll = if s.noscroll {
        let anchor = s
            .anchor
            .map(|(a, i, o)| {
                let g = if s.anchor_gutter { ":g" } else { "" };
                format!(" data-hylki-anchor=\"{a}:{i}:{o}{g}\"")
            })
            .unwrap_or_default();
        format!(" data-hylki-noscroll=\"1\"{anchor}")
    } else {
        String::new()
    };
    // How card actions show is a body stamp too, so the static document
    // builder (and its tests) need no extra parameter: "toggle" hides them
    // behind the ⋯, "hover" shows them while the card is hovered, and no
    // stamp means always shown.
    let hover = if s.card_actions_hover {
        " data-hylki-acts=\"toggle\""
    } else if s.card_actions_auto {
        " data-hylki-acts=\"hover\""
    } else {
        ""
    };
    // The palette timeout rides along in ms; the script reads it for the
    // toggle mode's collapse timer and the hover mode's fade-out delay.
    let delay = format!(
        " data-hylki-actsdelay=\"{}\"",
        s.palette_collapse_secs.max(1) * 1000
    );
    // With the ⋯ set to open the card's menu there is no palette to
    // slide, so the script hands the click back to the app instead.
    let acts_menu = if s.card_palette_menu {
        " data-hylki-actsmenu=\"1\""
    } else {
        ""
    };
    // The OpenPGP chip as its icons alone, without the words (#300).
    let pgp_icons = if s.pgp_labels {
        ""
    } else {
        " data-hylki-pgpicons"
    };
    // The newest member, for the open-scroll fallback (#101): with no
    // unread mail the reader lands on the newest message rather than
    // wherever the document happens to start.
    let newest = s
        .newest
        .map(|(aid, id)| format!(" data-hylki-newest=\"{aid}:{id}\""))
        .unwrap_or_default();
    // Read-marking policy (#100): the observer marks conversation members
    // as they come into view; "manual" installs no observer at all.
    let readmark = match s.read_mark {
        crate::config::ReadMark::Shown => " data-hylki-readmark=\"250\"",
        crate::config::ReadMark::Delay => " data-hylki-readmark=\"2000\"",
        crate::config::ReadMark::Manual => "",
    };
    let copied = format!(
        " data-hylki-copied=\"{}\"",
        i18n("Copied").replace('"', "&quot;")
    );
    format!("{noscroll}{hover}{delay}{acts_menu}{pgp_icons}{newest}{readmark}{copied}")
}

#[cfg(test)]
mod tests {
    use super::{body_stamps, BodyStamps};
    use crate::config::ReadMark;

    fn stamps() -> BodyStamps {
        BodyStamps {
            noscroll: false,
            anchor: None,
            anchor_gutter: false,
            card_actions_hover: false,
            card_actions_auto: false,
            palette_collapse_secs: 0,
            card_palette_menu: false,
            pgp_labels: true,
            newest: None,
            read_mark: ReadMark::Manual,
        }
    }

    #[test]
    fn a_first_render_stamps_little() {
        assert_eq!(
            body_stamps(&stamps()),
            " data-hylki-actsdelay=\"1000\" data-hylki-copied=\"Copied\""
        );
    }

    #[test]
    fn every_stamp_in_its_order() {
        let s = BodyStamps {
            noscroll: true,
            anchor: Some((1, 2, 30)),
            anchor_gutter: true,
            card_actions_hover: true,
            card_actions_auto: true,
            palette_collapse_secs: 5,
            card_palette_menu: true,
            pgp_labels: false,
            newest: Some((1, 9)),
            read_mark: ReadMark::Delay,
        };
        assert_eq!(
            body_stamps(&s),
            " data-hylki-noscroll=\"1\" data-hylki-anchor=\"1:2:30:g\" \
             data-hylki-acts=\"toggle\" data-hylki-actsdelay=\"5000\" \
             data-hylki-actsmenu=\"1\" data-hylki-pgpicons data-hylki-newest=\"1:9\" \
             data-hylki-readmark=\"2000\" data-hylki-copied=\"Copied\""
        );
    }

    #[test]
    fn hover_actions_and_a_plain_anchor() {
        let s = BodyStamps {
            noscroll: true,
            anchor: Some((3, 4, 0)),
            card_actions_auto: true,
            read_mark: ReadMark::Shown,
            ..stamps()
        };
        assert_eq!(
            body_stamps(&s),
            " data-hylki-noscroll=\"1\" data-hylki-anchor=\"3:4:0\" \
             data-hylki-acts=\"hover\" data-hylki-actsdelay=\"1000\" \
             data-hylki-readmark=\"250\" data-hylki-copied=\"Copied\""
        );
        // Without the open scroll done, a saved anchor is not stamped.
        let s = BodyStamps {
            anchor: Some((3, 4, 0)),
            ..stamps()
        };
        assert!(!body_stamps(&s).contains("anchor"));
    }
}
