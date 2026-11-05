//! The sender checks (#133): the verify badge, the sender popover, and
//! trusting or fetching a signing key.

use super::types::MessageViewInput;
use super::MessageView;
use crate::i18n::{i18n, i18n_f};
use adw::prelude::*;
use relm4::prelude::*;

/// Ask before vouching for a signing key: the fingerprint to compare, and
/// Trust or Cancel.
#[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
pub(super) fn confirm_trust(
    parent: Option<&gtk::Window>,
    signer: &str,
    key: &crate::mail::pgp::KeyInfo,
    sender: ComponentSender<MessageView>,
    account_id: u32,
    id: u32,
) {
    let dialog = adw::MessageDialog::new(
        parent,
        Some(&i18n("Trust this key?")),
        Some(&i18n_f(
            "Compare the fingerprint with the one {uid} gives you in person or over another channel. \
             Trusting a key you have not checked lets an impostor's signature pass as theirs.",
            &[("uid", signer)],
        )),
    );
    let fpr_label = gtk::Label::new(Some(&key.fingerprint_display()));
    fpr_label.add_css_class("monospace");
    fpr_label.set_wrap(true);
    fpr_label.set_selectable(true);
    fpr_label.set_justify(gtk::Justification::Center);
    dialog.set_extra_child(Some(&fpr_label));
    dialog.add_response("cancel", &i18n("Cancel"));
    dialog.add_response("trust", &i18n("Trust"));
    dialog.set_response_appearance("trust", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("cancel"));
    let fingerprint = key.fingerprint.clone();
    dialog.connect_response(None, move |_, resp| {
        if resp == "trust" {
            sender
                .input_sender()
                .emit(MessageViewInput::PgpTrustConfirmed {
                    account_id,
                    id,
                    fingerprint: fingerprint.clone(),
                });
        }
    });
    dialog.present();
}

/// Get the sender's key (#133): the Autocrypt key the message carried
/// first, into Hylki's own keyring of found keys (it is the sender's say-so
/// alone, so it stays out of the user's keyring); then WKD and the
/// keyservers by address and by the key id the signature named, into the
/// user's keyring, as asked. Runs off the main thread (network).
pub(super) fn fetch_sender_key(pgp: &crate::models::PgpStatus) -> Result<String, String> {
    let gpg = crate::mail::pgp::Gpg::system();
    if let Some(b64) = &pgp.autocrypt {
        if let Some(bytes) = crate::encoding::base64_decode(b64) {
            if crate::mail::pgp::discovery::take_sender_key(&pgp.sender_addr, &bytes).is_ok() {
                return Ok(i18n("The sender's key was taken from the message."));
            }
        }
    }
    let summary = crate::mail::pgp::fetch_key(&gpg, &pgp.sender_addr, pgp.signing_key_id())?;
    Ok(if summary.imported > 0 {
        i18n("The sender's key was fetched and imported.")
    } else {
        i18n("The sender's key was already in your keyring.")
    })
}

impl MessageView {
    /// Light the header seal for one member with its verdict class + tooltip.
    pub(super) fn patch_verify_badge(&self, account_id: u32, id: u32) {
        let Some(check) = self.member_checks.get(&(account_id, id)) else {
            return;
        };
        // The OpenPGP chip (#133) rides the same patch: shown with its
        // lock/shield and color once a verdict exists.
        let pgp = match &check.pgp {
            Some(p) => format!(
                "var p=document.querySelector('.hylki-pgp[data-key=\"{account_id}:{id}\"]');\
                 if(p){{p.className='hylki-pgp on{enc}{sig} {cls}';p.title={title:?};\
                 var t=p.querySelector('.hylki-pgp-text');if(t)t.textContent={label:?};}}",
                enc = if p.encrypted { " enc" } else { "" },
                sig = if p.signed() { " sig" } else { "" },
                cls = p.css_class(),
                title = p.summary(),
                label = p.short_label(),
            ),
            None => String::new(),
        };
        // The sensitivity label (#411): read-only, shown as the sender's
        // organization named it, its icon in the label's color when the
        // organization's list gave one (a checked `#rrggbb`; empty clears).
        let label = match &check.label {
            Some(l) => format!(
                "var l=document.querySelector('.hylki-sens[data-key=\"{account_id}:{id}\"]');\
                 if(l){{l.className='hylki-sens on';l.title={title:?};\
                 l.style.setProperty('--hylki-sens',{color:?});\
                 var t=l.querySelector('.hylki-sens-text');if(t)t.textContent={text:?};}}",
                title = l.summary(),
                text = l.short_label(),
                color = l.color,
            ),
            None => String::new(),
        };
        let js = format!(
            "(function(){{{pgp}{label}\
             var b=document.querySelector('.hylki-verify[data-key=\"{account_id}:{id}\"]');\
             if(!b)return;b.className='hylki-verify on {cls}';b.title={title:?};}})()",
            cls = check.trust.css_class(),
            title = check.trust.label(),
        );
        self.run_js(&js);
    }

    /// The verdict-details popover, anchored where the seal was clicked.
    /// The old toolbar button's popover, reparented.
    ///
    /// The rect arrives in the page's CSS pixels with `page_width` as the
    /// page's innerWidth. With a GNOME text scaling factor ≠ 1.0, WebKit
    /// zooms the whole page, so CSS pixels differ from widget pixels by
    /// widget-width / innerWidth — scaling by the measured ratio anchors the
    /// popover correctly under any scale (and is exactly 1.0 without one).
    pub(super) fn show_sender_popover(
        &self,
        account_id: u32,
        id: u32,
        rect: (f64, f64, f64, f64),
        page_width: f64,
        sender: &ComponentSender<Self>,
    ) {
        let Some(check) = self.member_checks.get(&(account_id, id)) else {
            return;
        };
        let popover = gtk::Popover::new();
        let widget_width = self.webview.width() as f64;
        let ratio = if page_width > 0.0 && widget_width > 0.0 {
            widget_width / page_width
        } else {
            1.0
        };
        let rect = (
            rect.0 * ratio,
            rect.1 * ratio,
            rect.2 * ratio,
            rect.3 * ratio,
        );
        let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
        content.add_css_class("sender-detail");
        // The OpenPGP verdict (#133) leads when there is one: what the
        // encryption and signature say, then the sender check below it.
        if let Some(pgp) = &check.pgp {
            let head = gtk::Label::new(Some(&i18n("OpenPGP")));
            head.set_halign(gtk::Align::Start);
            head.add_css_class("heading");
            content.append(&head);
            let line = gtk::Label::new(Some(&pgp.summary()));
            line.set_halign(gtk::Align::Start);
            line.set_wrap(true);
            line.set_xalign(0.0);
            line.set_max_width_chars(44);
            content.append(&line);
            if !pgp.notes.is_empty() {
                let notes = gtk::Label::new(Some(&pgp.notes.join("\n")));
                notes.set_halign(gtk::Align::Start);
                notes.set_wrap(true);
                notes.set_xalign(0.0);
                notes.set_max_width_chars(44);
                notes.add_css_class("dim-label");
                content.append(&notes);
            }
            // The way out of the two doubts, without a terminal (#133).
            let action = if pgp.key_missing() {
                Some((
                    i18n("Fetch the sender's key"),
                    MessageViewInput::PgpFetchKey { account_id, id },
                ))
            } else if pgp.key_untrusted() {
                Some((
                    i18n("Trust this key…"),
                    MessageViewInput::PgpTrustKey { account_id, id },
                ))
            } else {
                None
            };
            if let Some((label, input)) = action {
                let button = gtk::Button::with_label(&label);
                button.set_halign(gtk::Align::Start);
                button.add_css_class("pill");
                let s = sender.clone();
                let p = popover.clone();
                let input = std::cell::RefCell::new(Some(input));
                button.connect_clicked(move |_| {
                    if let Some(i) = input.borrow_mut().take() {
                        s.input_sender().emit(i);
                    }
                    p.popdown();
                });
                content.append(&button);
            }
            content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
        let heading = gtk::Label::new(Some(&check.trust.label()));
        heading.set_halign(gtk::Align::Start);
        heading.add_css_class("heading");
        content.append(&heading);
        let summary = gtk::Label::new(Some(&check.summary));
        summary.set_halign(gtk::Align::Start);
        summary.set_wrap(true);
        summary.set_xalign(0.0);
        summary.set_max_width_chars(44);
        content.append(&summary);
        if !check.findings.is_empty() {
            content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            let findings = gtk::Label::new(Some(&check.findings.join("\n")));
            findings.set_halign(gtk::Align::Start);
            findings.set_wrap(true);
            findings.set_xalign(0.0);
            findings.set_max_width_chars(44);
            findings.add_css_class("dim-label");
            content.append(&findings);
        }
        let footnote = gtk::Label::new(Some(
            i18n("A pass proves the address wasn't forged — not that the message is safe.")
                .as_str(),
        ));
        footnote.set_halign(gtk::Align::Start);
        footnote.set_wrap(true);
        footnote.set_xalign(0.0);
        footnote.set_max_width_chars(44);
        footnote.add_css_class("dim-label");
        footnote.add_css_class("caption");
        content.append(&footnote);

        popover.set_child(Some(&content));
        popover.set_parent(&self.webview);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            rect.0 as i32,
            rect.1 as i32,
            (rect.2 as i32).max(1),
            (rect.3 as i32).max(1),
        )));
        popover.connect_closed(|p| {
            let p = p.clone();
            gtk::glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
    }

    /// The current verdict, defaulting to "unverified" before one arrives.
    fn trust(&self) -> crate::models::SenderTrust {
        self.sender_check
            .as_ref()
            .map(|c| c.trust)
            .unwrap_or(crate::models::SenderTrust::Unverified)
    }

    /// Whether the red banner is over the message. Settings can quiet it for
    /// "Check this sender", which a newsletter replying from its mailing
    /// service earns routinely; a failed check keeps it, since that verdict
    /// says the From: line is forged.
    pub(super) fn spoof_alert_shown(&self) -> bool {
        match self.trust() {
            crate::models::SenderTrust::Fail => true,
            crate::models::SenderTrust::Suspicious => self.show_spoof_banner,
            _ => false,
        }
    }
}
