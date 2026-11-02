//! The reader's look: accent, light or dark, and the grounds the WebView is
//! painted with.

use super::chrome::wear_class;
use super::frame::{ground_rgba, theme_grounds_for};
use super::snippets::paints_own_background;
use super::MessageView;
use adw::prelude::*;
use relm4::prelude::*;
use webkit6::prelude::WebViewExt;

impl MessageView {
    /// The desktop accent color, as the document can use it. Read from the
    /// widget's style so it follows the user's choice; GNOME's own blue is the
    /// fallback when the theme doesn't define it.
    pub(super) fn accent_hex(&self) -> String {
        #[allow(deprecated)]
        self.webview
            .style_context()
            .lookup_color("accent_bg_color")
            .map(|c| crate::color::to_hex(&c))
            .unwrap_or_else(|| "#3584e4".to_string())
    }

    /// Paint the header chip in the account's color: a tint of it behind
    /// text in it, the text deepened (or lightened) where the color is too
    /// close to the window ground to read. The ground moves with the
    /// theme, so a theme change paints it again.
    pub(super) fn paint_account_chip(&self) {
        let Some(color) = &self.account_color else {
            return;
        };
        let dark = adw::StyleManager::default().is_dark();
        let (_, _, chrome) = self.theme_grounds(dark);
        let text = crate::color::tinted_text(color, 0.18, &chrome, 4.5);
        let class = format!(
            "chip-{}",
            color
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
        );
        self.chip_rules.set(vec![(
            format!(".hylki-account-chip.{class}"),
            format!(
                "background-color: {}; color: {text};",
                crate::color::pale(color, 0.18)
            ),
        )]);
        if let Some(chip) = &self.account_chip {
            wear_class(chip, "chip-", &class);
        }
    }

    /// Whether the theme draws messages dark: the message theme, or the
    /// system's when it follows the system.
    pub(super) fn theme_dark(&self) -> bool {
        self.content_dark
            .unwrap_or_else(|| adw::StyleManager::default().is_dark())
    }

    /// Whether the message on screen is drawn dark: the scheme the user
    /// chose for it, else the theme's.
    pub(super) fn effective_dark(&self) -> bool {
        self.current
            .as_ref()
            .and_then(|m| self.theme_picks.get(&(m.account_id, m.id)).copied())
            .unwrap_or_else(|| self.theme_dark())
    }

    /// The Light / Dark Mode switch's position: on only while the message is
    /// shown opposite the theme. A message the user turned light reads off
    /// once the theme turns light too, and still stays light if it turns
    /// back.
    pub(super) fn message_flipped(&self) -> bool {
        self.effective_dark() != self.theme_dark()
    }

    /// Whether cards list their own attachments (#213): by preference, and
    /// only where the drawer is not already that list — a lone message's
    /// drawer holds exactly its files, so its card repeats nothing.
    pub(super) fn chips_shown(&self) -> bool {
        self.card_atts_shown && (self.thread.len() > 1 || !self.drawer_on)
    }

    /// Whether the reader header's first line has anything on it. With the
    /// account chip and both switches hidden in Settings it would be an
    /// empty row adding its spacing above the subject.
    pub(super) fn account_line_shown(&self) -> bool {
        let chip = self.account_name.is_some() && self.account_chip_shown;
        let controls = self.current.is_some()
            && (self.theme_switch_shown
                || self.reader_switch_shown
                || self.zoom != self.zoom_default);
        chip || controls
    }

    /// The reader's two grounds for `dark` (see [`theme_grounds_for`]).
    pub(super) fn theme_grounds(&self, dark: bool) -> (String, String, String) {
        theme_grounds_for(&self.webview, dark)
    }

    /// Paint the WebView canvas in the theme color so unstyled bodies (and the
    /// gap before a load) match light/dark mode instead of flashing white.
    pub(super) fn apply_webview_bg(&self, dark: bool) {
        // Whatever is about to be shown: a conversation's cards sit on the
        // deeper page ground, a full-bleed single message on the plain ground
        // — or the chrome ground when it paints no background of its own.
        // The cover matches it so the spinner gives way to the document
        // without a change of color.
        let (ground, page, chrome) = self.theme_grounds(dark);
        let (bg, kind) = if self.carded() {
            (&page, "page")
        } else if self.thread.len() == 1
            && (self.reader_mode || !paints_own_background(&self.thread[0].body))
        {
            (&chrome, "chrome")
        } else {
            (&ground, "ground")
        };
        self.webview.set_background_color(&ground_rgba(bg));
        // The spinner and the cover stand in for the message, so they answer to
        // the message's theme: #1e1e1e matches the document's own dark ground,
        // white its light one. The label and spinner take a dimmed foreground
        // from the same side, so neither disappears into the ground.
        let (side, fg) = if dark {
            ("dark", "rgba(255,255,255,0.55)")
        } else {
            ("light", "rgba(0,0,0,0.45)")
        };
        // All three grounds at once, so moving between kinds of message
        // never loads the rules again.
        let cover = format!(".reader-cover.cover-side-{side}");
        let mut rules: Vec<(String, String)> =
            [("page", &page), ("chrome", &chrome), ("ground", &ground)]
                .iter()
                .map(|(k, c)| {
                    (
                        format!("{cover}.cover-ground-{k}"),
                        format!("background-color: {c};"),
                    )
                })
                .collect();
        rules.push((
            format!("{cover} .reader-loading label, {cover} .reader-loading spinner"),
            format!("color: {fg};"),
        ));
        self.cover_rules.set(rules);
        if let Some(cover) = &self.cover {
            wear_class(cover, "cover-side-", &format!("cover-side-{side}"));
            wear_class(cover, "cover-ground-", &format!("cover-ground-{kind}"));
        }
    }
}
