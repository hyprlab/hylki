//! The OpenPGP check before a send (#133): a key of the user's own, a key
//! for every recipient, and the question about keys nobody vouched for.

use super::messages::ComposeInput;
use super::Compose;
use super::ComposeWidgets;
use crate::i18n::{i18n, i18n_f};
use adw::prelude::*;
use relm4::prelude::*;

/// Whether an OpenPGP send can go ahead (#133): a key of the user's own
/// for the From address (or the account's chosen key), and, to encrypt, a
/// key for every recipient: in the user's keyring, or one Hylki found
/// (Autocrypt, gossip, WKD, a keyserver). The message names the first gap.
/// What comes back are the keys to ask about before sending, which the
/// user has not yet said to use: one in the user's keyring nobody has
/// vouched for, or one passed on by someone else (gossip). A key the
/// recipient sent or their domain publishes is not asked about, as asking
/// at every send would undo what Autocrypt is for. With them, the keys from
/// Hylki's own keyring, which go with the message.
pub(super) fn pgp_send_check(
    gpg: &crate::mail::pgp::Gpg,
    peers: Option<&crate::cache::Cache>,
    accepted: &[crate::config::AcceptedKey],
    from: &str,
    chosen_key: Option<&str>,
    fields: &[&str],
    encrypt: bool,
) -> Result<PgpCheck, String> {
    let own = chosen_key
        .and_then(|f| crate::mail::pgp::secret_key_by_fingerprint(gpg, f))
        .or_else(|| crate::mail::pgp::secret_key_for(gpg, from));
    if own.is_none() {
        return Err(crate::i18n::i18n_f(
            "There is no OpenPGP key of your own for {addr}. Generate one under Settings, OpenPGP, \
             or choose a key in the account's settings.",
            &[("addr", from)],
        ));
    }
    let mut check = PgpCheck::default();
    if !encrypt {
        return Ok(check);
    }
    let mut addrs: Vec<String> = Vec::new();
    for field in fields {
        for (_, addr) in crate::worker::parse_recipients(field) {
            let addr = addr.trim().to_ascii_lowercase();
            if !addr.is_empty() && !addrs.contains(&addr) {
                addrs.push(addr);
            }
        }
    }
    use crate::mail::pgp::discovery::{statuses_with, KeySource};
    let statuses = statuses_with(
        gpg,
        peers,
        &addrs,
        crate::mail::pgp::autocrypt::PreferEncrypt::NoPreference,
        false,
    );
    for s in statuses {
        let no_key = || {
            crate::i18n::i18n_f(
                "There is no OpenPGP key for {addr}. Ask them for their public key, or fetch it \
                 under Settings, OpenPGP.",
                &[("addr", &s.addr)],
            )
        };
        let (fingerprint, ask) = match (s.source, &s.fingerprint) {
            (None, _) => return Err(no_key()),
            (Some(KeySource::Keyring), _) => {
                let key = crate::mail::pgp::public_key_for(gpg, &s.addr).ok_or_else(no_key)?;
                let ask = !key.vouched_for(&s.addr);
                (key.fingerprint, ask)
            }
            (Some(source), Some(fpr)) => {
                check.found.push((s.addr.clone(), fpr.clone()));
                (fpr.clone(), source == KeySource::Gossip)
            }
            (Some(_), None) => return Err(no_key()),
        };
        let this = crate::config::AcceptedKey {
            addr: s.addr.clone(),
            fingerprint,
        };
        if ask && !accepted.contains(&this) && !check.unvouched.contains(&this) {
            check.unvouched.push(this);
        }
    }
    Ok(check)
}

/// What a passing OpenPGP send check found (see `pgp_send_check`).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct PgpCheck {
    /// Keys to ask about before sending (see `pgp_send_check`).
    pub(crate) unvouched: Vec<crate::config::AcceptedKey>,
    /// Keys from Hylki's own keyring: (address, fingerprint).
    pub(crate) found: Vec<(String, String)>,
}

/// Ask before encrypting to keys the keyring does not vouch for. Anyone can
/// publish a key with any address on it, so an unchecked key may belong to
/// somebody else. Most people never check keys, and asking at every send would
/// teach them to click through it, so Send Anyway is remembered for the key:
/// the question comes back only when an address gets a different one.
#[allow(deprecated)] // adw::MessageDialog, as in crate::ui::dialog
pub(super) fn ask_unvouched(
    check: PgpCheck,
    checked: Vec<String>,
    widgets: &ComposeWidgets,
    sender: &ComponentSender<Compose>,
) {
    let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
    let addresses: Vec<&str> = check.unvouched.iter().map(|k| k.addr.as_str()).collect();
    let dialog = adw::MessageDialog::new(
        parent.as_ref(),
        Some(&i18n("Encrypt to keys you have not checked?")),
        Some(&i18n_f(
            "Nothing confirms that the keys for {addresses} belong to them. If one is someone else's, that person can read this message and the recipient cannot. To check a key, compare its fingerprint with its owner and trust it under Settings, OpenPGP. Send Anyway does not ask about these keys again.",
            &[("addresses", &addresses.join(", "))],
        )),
    );
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("send", &i18n("Send Anyway"));
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let unvouched = check.unvouched;
    let pending = std::cell::RefCell::new(Some((checked, check.found)));
    let s = sender.input_sender().clone();
    dialog.connect_response(None, move |_, resp| {
        if resp != "send" {
            return;
        }
        if let Some((checked, found)) = pending.borrow_mut().take() {
            crate::config::accept_keys(&unvouched);
            s.emit(ComposeInput::PgpChecked {
                checked,
                result: Ok(PgpCheck {
                    unvouched: Vec::new(),
                    found,
                }),
            });
        }
    });
    dialog.present();
}

#[cfg(test)]
mod tests {
    use super::pgp_send_check;
    use crate::config::AcceptedKey;
    use crate::mail::pgp::{export_public, generate_key, import_keys, trust_key, Gpg};

    /// What Send asks about: an unchecked key until Send Anyway accepts it,
    /// again when the address gets a different key, and never one trusted.
    #[test]
    fn the_send_check_asks_once_for_each_unchecked_key() {
        if !crate::mail::pgp::available() {
            eprintln!("gpg not installed; skipping");
            return;
        }
        let root = std::env::temp_dir().join(format!("hylki-gpg-ask-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let keyring = |name: &str| {
            let home = root.join(name);
            std::fs::create_dir_all(&home).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            Gpg::at(home)
        };
        let me = keyring("me");
        let peer = keyring("peer");
        generate_key(&me, "Me", "me@hylki.invalid", "never", "").expect("own key");
        let fpr = generate_key(&peer, "Ada", "ada@hylki.invalid", "never", "").expect("key");
        import_keys(&me, &export_public(&peer, &fpr).expect("exported")).expect("imported");
        let check = |accepted: &[AcceptedKey]| {
            pgp_send_check(
                &me,
                None,
                accepted,
                "me@hylki.invalid",
                None,
                &["Ada <ADA@hylki.invalid>, ada@hylki.invalid"],
                true,
            )
            .expect("can send")
        };
        let this_key = AcceptedKey {
            addr: "ada@hylki.invalid".into(),
            fingerprint: fpr.clone(),
        };
        let other_key = AcceptedKey {
            addr: "ada@hylki.invalid".into(),
            fingerprint: "0".repeat(40),
        };
        assert_eq!(check(&[]).unvouched, vec![this_key.clone()], "named once");
        assert!(check(std::slice::from_ref(&this_key)).unvouched.is_empty());
        assert_eq!(check(&[other_key]).unvouched, vec![this_key.clone()]);
        // Signing only encrypts to nobody, so there is nothing to ask.
        let signing = pgp_send_check(
            &me,
            None,
            &[],
            "me@hylki.invalid",
            None,
            &["ada@hylki.invalid"],
            false,
        );
        assert_eq!(signing, Ok(super::PgpCheck::default()));
        trust_key(&me, &fpr, None).expect("trusted");
        assert!(check(&[]).unvouched.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Keys Hylki found: one a recipient sent with their mail goes without
    /// asking, one passed on by somebody else (gossip) is asked about
    /// until Send Anyway accepts it. Both go with the message.
    #[test]
    fn the_send_check_asks_about_gossip_keys_only() {
        use crate::mail::pgp::autocrypt::PeerState;
        use crate::mail::pgp::autocrypt::PreferEncrypt;
        use crate::mail::pgp::discovery::{export_minimal, import_one};
        if !crate::mail::pgp::available() {
            eprintln!("gpg not installed; skipping");
            return;
        }
        let root = std::env::temp_dir().join(format!("hylki-gpg-found-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let me = Gpg::at(root.join("me"));
        let them = Gpg::at(root.join("them"));
        generate_key(&me, "Me", "me@hylki.invalid", "never", "").expect("own key");
        let found = me.found_keyring().expect("found keyring");
        let cache = crate::cache::Cache::in_memory().expect("cache");
        let take = |addr: &str, gossip: bool| {
            let fpr = generate_key(&them, "", addr, "never", "").expect("key");
            let key = export_minimal(&them, &fpr, addr).expect("exported");
            assert_eq!(import_one(&found, &key, addr).expect("imported"), fpr);
            let mut peer = PeerState::new(addr);
            let now = crate::datefmt::now();
            if gossip {
                peer.take_gossip(now, &fpr, "g");
            } else {
                peer.take_autocrypt(now, &fpr, "a", PreferEncrypt::Mutual);
            }
            cache.save_pgp_peer(&peer);
            fpr
        };
        let carol = take("carol@hylki.invalid", false);
        let dave = take("dave@hylki.invalid", true);
        let check = |accepted: &[AcceptedKey]| {
            pgp_send_check(
                &me,
                Some(&cache),
                accepted,
                "me@hylki.invalid",
                None,
                &["carol@hylki.invalid, dave@hylki.invalid"],
                true,
            )
            .expect("can send")
        };
        let dave_key = AcceptedKey {
            addr: "dave@hylki.invalid".into(),
            fingerprint: dave.clone(),
        };
        let first = check(&[]);
        assert_eq!(first.unvouched, vec![dave_key.clone()]);
        assert_eq!(
            first.found,
            vec![
                ("carol@hylki.invalid".to_string(), carol),
                ("dave@hylki.invalid".to_string(), dave),
            ]
        );
        assert!(check(&[dave_key]).unvouched.is_empty());
        crate::mail::pgp::drop_test_keyring(&root.join("me").join("hylki-discovered"));
        crate::mail::pgp::drop_test_keyring(&root.join("me"));
        crate::mail::pgp::drop_test_keyring(&root.join("them"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
