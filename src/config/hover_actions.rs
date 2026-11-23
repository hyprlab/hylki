//! The buttons a message row shows under its text while the pointer is on
//! it (Settings → Message List → Hover Actions).

use super::load_privacy;
use crate::i18n::i18n_noop;

/// One button of a row's hover actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HoverAction {
    Star,
    ReadUnread,
    Archive,
    Delete,
    Spam,
    Reply,
    ReplyAll,
    Forward,
}

/// How many buttons a row shows at most: every action there is.
pub(crate) const HOVER_ACTIONS_MAX: usize = 8;

impl HoverAction {
    pub(crate) const ALL: [HoverAction; 8] = [
        HoverAction::Star,
        HoverAction::ReadUnread,
        HoverAction::Archive,
        HoverAction::Delete,
        HoverAction::Spam,
        HoverAction::Reply,
        HoverAction::ReplyAll,
        HoverAction::Forward,
    ];

    pub(crate) const DEFAULT: [HoverAction; 3] =
        [HoverAction::Star, HoverAction::Archive, HoverAction::Delete];

    /// The stable name written to `privacy.toml`.
    pub(crate) fn key(self) -> &'static str {
        match self {
            HoverAction::Star => "star",
            HoverAction::ReadUnread => "read_unread",
            HoverAction::Archive => "archive",
            HoverAction::Delete => "delete",
            HoverAction::Spam => "spam",
            HoverAction::Reply => "reply",
            HoverAction::ReplyAll => "reply_all",
            HoverAction::Forward => "forward",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<HoverAction> {
        HoverAction::ALL.iter().copied().find(|a| a.key() == key)
    }

    /// The icon in its usual state: a star not yet set, a message read.
    pub(crate) fn icon(self) -> &'static str {
        match self {
            HoverAction::Star => "hylki-non-starred-symbolic",
            HoverAction::ReadUnread => "mail-unread-symbolic",
            HoverAction::Archive => "mail-archive-symbolic",
            HoverAction::Delete => "user-trash-symbolic",
            HoverAction::Spam => "mail-mark-junk-symbolic",
            HoverAction::Reply => "mail-reply-sender-symbolic",
            HoverAction::ReplyAll => "mail-reply-all-symbolic",
            HoverAction::Forward => "mail-forward-symbolic",
        }
    }

    /// The untranslated label (callers pass it through `i18n`).
    pub(crate) fn label(self) -> &'static str {
        match self {
            HoverAction::Star => i18n_noop("Star"),
            HoverAction::ReadUnread => i18n_noop("Read/Unread"),
            HoverAction::Archive => i18n_noop("Archive"),
            HoverAction::Delete => i18n_noop("Delete"),
            HoverAction::Spam => i18n_noop("Spam"),
            HoverAction::Reply => i18n_noop("Reply"),
            HoverAction::ReplyAll => i18n_noop("Reply All"),
            HoverAction::Forward => i18n_noop("Forward"),
        }
    }

    /// The buttons, in order, from their saved names: names this version
    /// does not know and repeats are skipped, and a hand-edited list stops
    /// at the most a row shows.
    pub(crate) fn from_keys(keys: &[String]) -> Vec<HoverAction> {
        let mut out: Vec<HoverAction> = Vec::new();
        for a in keys.iter().filter_map(|k| HoverAction::from_key(k)) {
            if !out.contains(&a) && out.len() < HOVER_ACTIONS_MAX {
                out.push(a);
            }
        }
        out
    }

    pub(crate) fn to_keys(actions: &[HoverAction]) -> Vec<String> {
        actions.iter().map(|a| a.key().to_string()).collect()
    }
}

pub(super) fn default_hover_actions() -> Vec<String> {
    HoverAction::to_keys(&HoverAction::DEFAULT)
}

/// The buttons rows show on hover, none when the setting is off.
pub(crate) fn load_hover_actions() -> (bool, Vec<HoverAction>) {
    let p = load_privacy();
    (
        p.hover_actions,
        HoverAction::from_keys(&p.hover_action_buttons),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_names_keep_their_order_without_repeats_or_strangers() {
        let keys: Vec<String> = ["delete", "bogus", "star", "delete", "reply"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            HoverAction::from_keys(&keys),
            vec![HoverAction::Delete, HoverAction::Star, HoverAction::Reply]
        );
        assert_eq!(
            HoverAction::from_keys(&default_hover_actions()),
            HoverAction::DEFAULT.to_vec()
        );
    }

    #[test]
    fn a_row_can_show_every_action() {
        let all = HoverAction::to_keys(&HoverAction::ALL);
        assert_eq!(HoverAction::from_keys(&all), HoverAction::ALL.to_vec());
        assert!(HOVER_ACTIONS_MAX >= HoverAction::ALL.len());
    }

    #[test]
    fn on_for_a_new_install_and_off_for_an_older_file() {
        assert!(crate::config::PrivacyFile::default().hover_actions);
        let p: crate::config::PrivacyFile = toml::from_str("").unwrap();
        assert!(!p.hover_actions);
        assert_eq!(
            HoverAction::from_keys(&p.hover_action_buttons),
            HoverAction::DEFAULT.to_vec()
        );
    }
}
