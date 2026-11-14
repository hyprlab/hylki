//! Each recipient's OpenPGP key, on their pill (Autocrypt Level 1's
//! recommendation, plus keys from the user's keyring and WKD): looked up in
//! the background once the recipients stop changing, and Encrypt turned on
//! when every recipient and the account agree to it.

use super::messages::ComposeInput;
use super::Compose;
use super::ComposeWidgets;
use crate::i18n::i18n;
use crate::mail::pgp::autocrypt::{PreferEncrypt, Recommendation};
use crate::mail::pgp::discovery::{KeySource, KeyStatus};
use crate::ui::recipient_row::KeyBadge;
use adw::prelude::*;
use relm4::prelude::*;

/// How long the recipients have to stay as they are before their keys are
/// looked up: typing an address makes a pill of each word that looks like
/// one, and each of those is not worth a lookup.
const SETTLE_MS: u64 = 600;

/// The badge for one recipient's key. `encrypting` makes a missing key an
/// error rather than a fact.
fn badge_for(status: &KeyStatus, encrypting: bool) -> KeyBadge {
    let found = |class, tooltip: String| KeyBadge {
        icon: "channel-secure-symbolic",
        class,
        tooltip,
    };
    let prefers = if status.recommendation == Recommendation::Encrypt {
        format!(" {}", i18n("They prefer encrypted mail."))
    } else {
        String::new()
    };
    match status.source {
        None => KeyBadge {
            icon: "channel-insecure-symbolic",
            class: if encrypting { "key-missing" } else { "key-none" },
            tooltip: i18n("No OpenPGP key: mail to this recipient cannot be encrypted."),
        },
        Some(_) if status.recommendation == Recommendation::Discourage => found(
            "key-weak",
            i18n("OpenPGP key from older mail: their recent mail came without it, so they may no longer read encrypted mail."),
        ),
        Some(KeySource::Keyring) => found(
            "key-keyring",
            format!("{}{prefers}", i18n("OpenPGP key in your keyring.")),
        ),
        Some(KeySource::Autocrypt) => found(
            "key-found",
            format!(
                "{}{prefers}",
                i18n("OpenPGP key sent with their mail (Autocrypt).")
            ),
        ),
        Some(KeySource::Gossip) => found(
            "key-weak",
            i18n("OpenPGP key passed on in someone else's encrypted mail."),
        ),
        Some(KeySource::Wkd) => found(
            "key-found",
            format!(
                "{}{prefers}",
                i18n("OpenPGP key published by their mail domain (WKD).")
            ),
        ),
        Some(KeySource::Keyserver) => found(
            "key-found",
            format!(
                "{}{prefers}",
                i18n("OpenPGP key from the keyserver keys.openpgp.org.")
            ),
        ),
    }
}

/// The demo's stand-in lookup: one recipient of each kind in turn, so the
/// badges can be seen without keys or a network.
fn demo_statuses(addrs: &[String]) -> Vec<KeyStatus> {
    addrs
        .iter()
        .enumerate()
        .map(|(i, addr)| {
            let (source, recommendation) = match i % 4 {
                0 => (Some(KeySource::Keyring), Recommendation::Available),
                1 => (Some(KeySource::Autocrypt), Recommendation::Encrypt),
                2 => (Some(KeySource::Wkd), Recommendation::Available),
                _ => (None, Recommendation::Disable),
            };
            KeyStatus {
                addr: addr.clone(),
                source,
                fingerprint: None,
                recommendation,
            }
        })
        .collect()
}

/// What the lookup needs from the composer, taken on the main thread.
struct Lookup {
    generation: u64,
    addrs: Vec<String>,
    from: String,
    chosen_key: Option<String>,
    own: PreferEncrypt,
    reply_to_encrypted: bool,
}

/// Look the recipients up off the main thread: what both keyrings and the
/// cache say at once, then, for those still without a key, WKD (and a
/// keyserver when allowed), answering again if anything was asked.
fn look_up(job: Lookup, out: relm4::Sender<ComposeInput>) {
    let answer = |badges, statuses| {
        let _ = out.send(ComposeInput::KeysFound {
            generation: job.generation,
            badges,
            statuses,
        });
    };
    if std::env::var_os("HYLKI_DEMO").is_some() {
        answer(true, demo_statuses(&job.addrs));
        return;
    }
    use crate::mail::pgp::{self, discovery, Gpg};
    // Badges, and lookups on the network, only for someone who uses
    // OpenPGP: a key of their own for this From.
    let main = Gpg::system();
    let mine = pgp::available()
        && (job
            .chosen_key
            .as_deref()
            .and_then(|f| pgp::secret_key_by_fingerprint(&main, f))
            .is_some()
            || pgp::secret_key_for(&main, &job.from).is_some());
    if !mine {
        answer(false, Vec::new());
        return;
    }
    let statuses = discovery::statuses(&job.addrs, job.own, job.reply_to_encrypted);
    answer(true, statuses.clone());
    let prefs = crate::config::load_privacy();
    if !prefs.key_lookup {
        return;
    }
    let (Some(found), Ok(cache)) = (Gpg::discovered(), crate::cache::Cache::open()) else {
        return;
    };
    let now = crate::datefmt::now();
    let mut asked = false;
    for s in statuses.iter().filter(|s| s.source.is_none()) {
        if discovery::lookup_due(cache.pgp_peer(&s.addr).as_ref(), now) {
            discovery::discover(
                &discovery::Https,
                &found,
                &cache,
                &s.addr,
                prefs.keyserver_lookup,
                now,
            );
            asked = true;
        }
    }
    if asked {
        answer(
            true,
            discovery::statuses(&job.addrs, job.own, job.reply_to_encrypted),
        );
    }
}

impl Compose {
    /// Handles [`ComposeInput::RecipientsChanged`]: the badges known
    /// already go on new pills at once, and a lookup is due once the
    /// recipients settle.
    pub(super) fn on_recipients_changed(
        &mut self,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        self.key_generation += 1;
        self.show_key_badges(widgets);
        if !crate::mail::pgp::available() && std::env::var_os("HYLKI_DEMO").is_none() {
            return;
        }
        let generation = self.key_generation;
        let s = sender.input_sender().clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(SETTLE_MS), move || {
            let _ = s.send(ComposeInput::LookUpKeys(generation));
        });
    }

    /// The recipients of every field, lowercased, To first.
    fn recipient_addresses(widgets: &ComposeWidgets) -> Vec<String> {
        let mut addrs: Vec<String> = Vec::new();
        for row in [&widgets.to_row, &widgets.cc_row, &widgets.bcc_row] {
            for a in row.addresses() {
                if !addrs.contains(&a) {
                    addrs.push(a);
                }
            }
        }
        addrs
    }

    /// Handles [`ComposeInput::LookUpKeys`].
    pub(super) fn on_look_up_keys(
        &mut self,
        generation: u64,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        if generation != self.key_generation {
            return;
        }
        let addrs = Self::recipient_addresses(widgets);
        if addrs.is_empty() {
            return;
        }
        let idx = widgets.from_row.selected() as usize;
        let Some(account) = self.accounts.get(idx) else {
            return;
        };
        let job = Lookup {
            generation,
            addrs,
            from: account.email.clone(),
            chosen_key: account.pgp_key.clone(),
            own: PreferEncrypt::from_mutual(account.autocrypt_mutual),
            reply_to_encrypted: self.reply_to_encrypted,
        };
        let out = sender.input_sender().clone();
        std::thread::spawn(move || look_up(job, out));
    }

    /// Handles [`ComposeInput::KeysFound`]: the badges, and Encrypt turned
    /// on when Autocrypt recommends it for every recipient (which takes
    /// both sides preferring it) and the user has not chosen otherwise.
    pub(super) fn on_keys_found(
        &mut self,
        generation: u64,
        badges: bool,
        statuses: Vec<KeyStatus>,
        widgets: &ComposeWidgets,
    ) {
        self.key_badges = badges;
        for s in statuses {
            self.key_statuses.insert(s.addr.clone(), s);
        }
        self.show_key_badges(widgets);
        if generation != self.key_generation || self.encrypt_touched || self.encrypt || !badges {
            return;
        }
        let current: Option<Vec<KeyStatus>> = Self::recipient_addresses(widgets)
            .iter()
            .map(|a| self.key_statuses.get(a).cloned())
            .collect();
        let Some(current) = current else {
            return;
        };
        if crate::mail::pgp::discovery::overall(&current) == Recommendation::Encrypt {
            self.encrypt_expected = Some(true);
            widgets.encrypt_btn.set_active(true);
        }
    }

    /// Put each recipient's badge on their pill, from what is known.
    pub(super) fn show_key_badges(&self, widgets: &ComposeWidgets) {
        let badge = |addr: &str| -> Option<KeyBadge> {
            if !self.key_badges {
                return None;
            }
            self.key_statuses
                .get(addr)
                .map(|s| badge_for(s, self.encrypt))
        };
        for row in [&widgets.to_row, &widgets.cc_row, &widgets.bcc_row] {
            row.set_key_badges(&badge);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(source: Option<KeySource>, recommendation: Recommendation) -> KeyStatus {
        KeyStatus {
            addr: "a@b.c".into(),
            source,
            fingerprint: None,
            recommendation,
        }
    }

    #[test]
    fn badges_say_where_the_key_is_from() {
        use Recommendation::*;
        let b = badge_for(&status(Some(KeySource::Keyring), Available), false);
        assert_eq!(
            (b.icon, b.class),
            ("channel-secure-symbolic", "key-keyring")
        );
        let b = badge_for(&status(Some(KeySource::Autocrypt), Encrypt), false);
        assert_eq!(b.class, "key-found");
        assert!(b.tooltip.contains("Autocrypt") && b.tooltip.contains("prefer"));
        assert_eq!(
            badge_for(&status(Some(KeySource::Wkd), Available), false).class,
            "key-found"
        );
        assert_eq!(
            badge_for(&status(Some(KeySource::Autocrypt), Discourage), false).class,
            "key-weak"
        );
        let none = status(None, Disable);
        assert_eq!(badge_for(&none, false).class, "key-none");
        assert_eq!(badge_for(&none, true).class, "key-missing");
        assert_eq!(badge_for(&none, true).icon, "channel-insecure-symbolic");
    }

    #[test]
    fn the_demo_shows_each_kind() {
        let addrs: Vec<String> = (0..4).map(|i| format!("{i}@demo.invalid")).collect();
        let kinds: Vec<_> = demo_statuses(&addrs)
            .into_iter()
            .map(|s| s.source)
            .collect();
        assert_eq!(
            kinds,
            vec![
                Some(KeySource::Keyring),
                Some(KeySource::Autocrypt),
                Some(KeySource::Wkd),
                None
            ]
        );
    }
}
