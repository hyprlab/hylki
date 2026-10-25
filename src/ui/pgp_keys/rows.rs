//! The rows of the OpenPGP page: one per key, and which of other people's
//! keys a search and the show filter leave in view (#432).

use adw::prelude::*;
use relm4::prelude::*;

use super::{PgpKeys, PgpKeysInput};
use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::mail::pgp::{self, KeyInfo};

/// Which of other people's keys are listed, in the show filter's order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum KeyShow {
    #[default]
    All,
    Trusted,
    NotTrusted,
    Unusable,
}

impl KeyShow {
    const LABELS: &[&str] = &[
        crate::i18n::i18n_noop("All keys"),
        crate::i18n::i18n_noop("Trusted"),
        crate::i18n::i18n_noop("Not trusted"),
        crate::i18n::i18n_noop("Expired or revoked"),
    ];

    /// The show filter's choices, translated.
    pub(super) fn choices() -> gtk::StringList {
        let labels: Vec<String> = Self::LABELS.iter().map(|l| i18n(l)).collect();
        gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>())
    }

    pub(super) fn from_index(i: u32) -> KeyShow {
        match i {
            1 => KeyShow::Trusted,
            2 => KeyShow::NotTrusted,
            3 => KeyShow::Unusable,
            _ => KeyShow::All,
        }
    }

    pub(super) fn shows(self, key: &KeyInfo) -> bool {
        match self {
            KeyShow::All => true,
            KeyShow::Trusted => key.validity.trusted(),
            KeyShow::NotTrusted => key.usable() && !key.validity.trusted(),
            KeyShow::Unusable => !key.usable(),
        }
    }
}

/// One key as a row: who it is for, its id and standing, and its actions.
pub(super) fn key_row(
    key: &KeyInfo,
    own: bool,
    sender: &ComponentSender<PgpKeys>,
) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.add_css_class(crate::ui::preferences::NOT_SEARCHED);
    row.set_title(&gtk::glib::markup_escape_text(key.primary_uid()));
    let mut parts = vec![pgp::key_display(&key.key_id)];
    if let Some(exp) = key.expires {
        let date = chrono::DateTime::from_timestamp(exp, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        parts.push(if exp > crate::datefmt::now() {
            i18n_f("expires {date}", &[("date", &date)])
        } else {
            i18n_f("expired {date}", &[("date", &date)])
        });
    }
    if own {
        parts.push(if key.can_encrypt {
            i18n("signs and encrypts")
        } else {
            i18n("signs only: no encryption subkey")
        });
    } else {
        parts.push(key.validity.label());
    }
    if key.uids.len() > 1 {
        parts.push(ni18n_f(
            "{n} more address",
            "{n} more addresses",
            (key.uids.len() - 1) as u32,
            &[("n", &(key.uids.len() - 1).to_string())],
        ));
    }
    row.set_subtitle(&gtk::glib::markup_escape_text(&parts.join(" · ")));
    row.set_tooltip_text(Some(&key.fingerprint_display()));
    if !key.usable() {
        row.add_css_class("dim-label");
    }
    let action = |icon: &str, tip: &str| {
        let b = gtk::Button::from_icon_name(icon);
        b.set_valign(gtk::Align::Center);
        b.add_css_class("flat");
        b.set_tooltip_text(Some(tip));
        b
    };
    let fpr = key.fingerprint.clone();
    if own {
        let export = action("document-save-symbolic", &i18n("Export public key…"));
        let s = sender.clone();
        let f = fpr.clone();
        export.connect_clicked(move |_| s.input(PgpKeysInput::Export(f.clone())));
        row.add_suffix(&export);
        let delete = action("user-trash-symbolic", &i18n("Delete this key"));
        let s = sender.clone();
        delete.connect_clicked(move |_| {
            s.input(PgpKeysInput::Delete {
                fingerprint: fpr.clone(),
                secret: true,
            })
        });
        row.add_suffix(&delete);
    } else {
        if key.validity.trusted() {
            let seal = gtk::Image::from_icon_name("verified-checkmark-symbolic");
            seal.set_tooltip_text(Some(&i18n("Trusted")));
            seal.add_css_class("accent");
            row.add_suffix(&seal);
        } else if key.usable() {
            let trust = gtk::Button::with_label(&i18n("Trust…"));
            trust.set_valign(gtk::Align::Center);
            trust.add_css_class("flat");
            let s = sender.clone();
            let f = fpr.clone();
            trust.connect_clicked(move |_| s.input(PgpKeysInput::Trust(f.clone())));
            row.add_suffix(&trust);
        }
        let remove = action("user-trash-symbolic", &i18n("Remove this key"));
        let s = sender.clone();
        remove.connect_clicked(move |_| {
            s.input(PgpKeysInput::Delete {
                fingerprint: fpr.clone(),
                secret: false,
            })
        });
        row.add_suffix(&remove);
    }
    row
}
