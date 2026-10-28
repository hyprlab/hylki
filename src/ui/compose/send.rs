//! Sending and saving: the From, the outgoing message, OpenPGP checks before a
//! send, and Send Later.

use super::messages::ComposeOutput;
use super::messages::{ComposeInput, OutgoingFields};
use super::types::ComposeAccount;
use super::types::ComposePrefill;
use super::Compose;
use super::ComposeWidgets;
use crate::config::ComposeFormat;
use crate::i18n::{i18n, i18n_f};
use crate::models::DraftOrigin;
use crate::ui::dialog::alert;
use crate::worker::OutgoingMessage;
use adw::prelude::*;
use relm4::prelude::*;

/// How often an edited message is saved to Drafts on its own (#340).
const AUTOSAVE_SECS: u32 = 30;

/// An identity's From as the composer writes it out: "Name <address>".
fn identity_display(a: &ComposeAccount) -> String {
    a.alias_from
        .clone()
        .unwrap_or_else(|| a.label.clone())
        .trim()
        .to_string()
}

/// Whether two Froms name the same person at the same address, however
/// each is quoted or spaced.
pub(super) fn same_from(a: &str, b: &str) -> bool {
    let one = |s: &str| {
        crate::worker::parse_recipients(s)
            .into_iter()
            .next()
            .map(|(n, e)| (n.trim().trim_matches('"').to_string(), e.to_lowercase()))
    };
    matches!((one(a), one(b)), (Some(x), Some(y)) if x == y)
}

/// Whether an OpenPGP send can go ahead (#133): a key of the user's own
/// for the From address (or the account's chosen key), and, to encrypt, a
/// public key for every recipient. The message names the first gap.
fn pgp_send_check(
    from: &str,
    chosen_key: Option<&str>,
    fields: &[&str],
    encrypt: bool,
) -> Result<(), String> {
    let gpg = crate::mail::pgp::Gpg::system();
    let own = chosen_key
        .and_then(|f| crate::mail::pgp::secret_key_by_fingerprint(&gpg, f))
        .or_else(|| crate::mail::pgp::secret_key_for(&gpg, from));
    if own.is_none() {
        return Err(crate::i18n::i18n_f(
            "There is no OpenPGP key of your own for {addr}. Generate one under Settings, OpenPGP, \
             or choose a key in the account's settings.",
            &[("addr", from)],
        ));
    }
    if encrypt {
        for field in fields {
            for (_, addr) in crate::worker::parse_recipients(field) {
                let addr = addr.as_str();
                if addr.is_empty() {
                    continue;
                }
                if crate::mail::pgp::public_key_for(&gpg, addr).is_none() {
                    return Err(crate::i18n::i18n_f(
                        "There is no OpenPGP key for {addr}. Ask them for their public key, or fetch it \
                         under Settings, OpenPGP.",
                        &[("addr", addr)],
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The Send Later picker (#145), starting from the scheduled time if there
/// is one.
pub(super) fn pick_send_time(
    parent: Option<&gtk::Window>,
    current: Option<i64>,
    sender: relm4::Sender<ComposeInput>,
) {
    crate::ui::later::pick_later_time(
        parent,
        current,
        &i18n("Send later"),
        &i18n("Schedule"),
        move |t| {
            let _ = sender.send(ComposeInput::SendAt(t));
        },
    );
}

impl Compose {
    /// The From to put on the wire: the picked identity's (`None` for the
    /// account's own), or the one typed in (#347), which has to hold one
    /// address.
    fn chosen_from(&self, widgets: &ComposeWidgets) -> Result<Option<String>, String> {
        let idx = widgets.from_row.selected() as usize;
        let picked = self.accounts.get(idx).and_then(|a| a.alias_from.clone());
        if !self.custom_from {
            return Ok(picked);
        }
        let typed = widgets.custom_from_row.text().trim().to_string();
        let found = crate::worker::parse_recipients(&typed);
        let [(name, addr)] = found.as_slice() else {
            return Err(i18n(
                "The From has to be one address, such as Ann <ann@example.com>.",
            ));
        };
        let at = addr.split_once('@');
        if at.is_none_or(|(user, host)| {
            user.is_empty() || !host.contains('.') || addr.contains(char::is_whitespace)
        }) {
            return Err(i18n_f(
                "“{address}” is not an email address.",
                &[("address", addr)],
            ));
        }
        // Left as the picked identity reads: send as that.
        if self
            .accounts
            .get(idx)
            .is_some_and(|a| same_from(&identity_display(a), &typed))
        {
            return Ok(picked);
        }
        Ok(Some(if name.trim().is_empty() {
            addr.clone()
        } else {
            format!("{} <{addr}>", name.trim())
        }))
    }

    /// Read the body out of the editor in `format`'s own terms: the source
    /// text for a source format (with no plain-text rendering, since the
    /// source *is* text), the document's HTML and text otherwise.
    ///
    /// `for_send` picks the reader that recuts pictures armed for it; a
    /// draft save must never cost quality, so it passes false.
    pub(super) fn read_body_as(
        &self,
        format: ComposeFormat,
        for_send: bool,
        cb: impl FnOnce(String, String) + 'static,
    ) {
        if format.is_source() {
            self.editor
                .extract_source(move |src| cb(src, String::new()));
        } else if for_send {
            self.editor.extract_for_send(cb);
        } else {
            self.editor.extract(cb);
        }
    }

    /// The `(html, text)` parts of the outgoing message, from what the
    /// editor handed back. Markdown is rendered and keeps its source as the
    /// text part — Markdown being, deliberately, plain text that reads as
    /// itself. Hand-written HTML is sanitized and gets a Markdown rendering
    /// as its text part. Plain text sends no HTML part at all (#180).
    fn outgoing_body(&self, body: String, text: String) -> (String, String) {
        match self.format {
            ComposeFormat::Rich => (body, text),
            ComposeFormat::Plain => (String::new(), text),
            ComposeFormat::Markdown => {
                let html = crate::mail::markdown::sanitize_outgoing(
                    &crate::mail::markdown::to_html(&body),
                );
                (html, body)
            }
            ComposeFormat::Html => {
                let text = crate::mail::markdown::html_to_text(&body);
                (crate::mail::markdown::sanitize_outgoing(&body), text)
            }
        }
    }

    /// The fields as a draft or the automatic save takes them, before the
    /// body is read. A draft can be saved without recipients, and keeps even
    /// a From still being typed: it is checked when the message is sent.
    fn draft_fields(&self, widgets: &ComposeWidgets) -> OutgoingFields {
        let to = widgets.to_row.text().trim().to_string();
        let cc = widgets.cc_row.text().trim().to_string();
        let bcc = widgets.bcc_row.text().trim().to_string();
        let reply_to = widgets.reply_to_row.text().trim().to_string();
        let subject = widgets.subject_row.text().to_string();
        let idx = widgets.from_row.selected() as usize;
        let from_account_id = self.accounts.get(idx).map(|a| a.id).unwrap_or(1);
        let from_alias = self
            .chosen_from(widgets)
            .unwrap_or_else(|_| self.accounts.get(idx).and_then(|a| a.alias_from.clone()));
        OutgoingFields {
            html: String::new(),
            text: String::new(),
            to,
            cc,
            bcc,
            reply_to,
            subject,
            from_account_id,
            from_alias,
        }
    }

    /// The message to hand over: the body converted out of its composing
    /// format, the fields and the attachments, with the draft origin so a
    /// saved or sent draft replaces its predecessor.
    fn outgoing_message(&self, fields: OutgoingFields) -> OutgoingMessage {
        let OutgoingFields {
            html,
            text,
            to,
            cc,
            bcc,
            reply_to,
            subject,
            from_account_id,
            from_alias,
        } = fields;
        let (html, text) = self.outgoing_body(html, text);
        OutgoingMessage {
            from_account_id,
            from_alias,
            to,
            cc,
            bcc,
            reply_to,
            subject,
            body: text,
            html,
            attachments: self
                .attachments
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect(),
            in_reply_to: self.in_reply_to.clone(),
            references: self.references.clone(),
            forwards: self.forwards.clone(),
            draft_origin: self.draft_origin.clone(),
            outbox_origin: self.outbox_origin,
            sign: self.sign,
            encrypt: self.encrypt,
            calendar: None,
            request_receipt: self.request_receipt,
            receipt_report: None,
            send_at: self.send_at,
        }
    }

    /// Handles [`ComposeInput::SaveDraftIfDirty`].
    pub(super) fn on_save_draft_if_dirty(&mut self, sender: &ComponentSender<Self>) {
        // Save only if the user actually edited something, so navigating
        // away from a pristine quote-only reply doesn't litter Drafts.
        if self.fields_dirty {
            sender.input_sender().emit(ComposeInput::SaveDraft);
        } else {
            let s = sender.clone();
            let id = self.compose_id;
            self.editor.is_dirty(move |body_dirty| {
                if body_dirty {
                    s.input_sender().emit(ComposeInput::SaveDraft);
                } else {
                    let _ = s.output(ComposeOutput::Close(id));
                }
            });
        }
    }

    /// Handles [`ComposeInput::CustomFrom`].
    pub(super) fn on_custom_from(&mut self, on: bool, widgets: &ComposeWidgets) {
        self.custom_from = on;
        if on {
            // Start from the address that was picked, to edit.
            let idx = widgets.from_row.selected() as usize;
            let start = self
                .accounts
                .get(idx)
                .map(identity_display)
                .unwrap_or_default();
            if widgets.custom_from_row.text().trim().is_empty() {
                widgets.custom_from_row.set_text(&start);
            }
            widgets.from_row.set_visible(false);
            widgets.custom_from_row.set_visible(true);
            widgets.custom_from_row.grab_focus();
        } else {
            widgets.custom_from_row.set_visible(false);
            widgets.from_row.set_visible(true);
        }
    }

    /// Handles [`ComposeInput::AccountChanged`].
    pub(super) fn on_account_changed(&mut self, widgets: &ComposeWidgets) {
        self.follow_account_signing(widgets);
        let idx = widgets.from_row.selected() as usize;
        // Another account may suggest from elsewhere: what a
        // directory answered for the last one is not kept.
        let account_changed = self
            .accounts
            .get(idx)
            .is_some_and(|a| a.id != self.from_account);
        if let Some(id) = self
            .accounts
            .get(idx)
            .map(|a| a.id)
            .filter(|id| *id != self.from_account)
        {
            self.from_account = id;
            self.directory_generation = self.directory_generation.wrapping_add(1);
            self.directory_matches.clear();
        }
        // Swap the editor's signature block for the new account's.
        // An alias or another identity of the same account shares
        // its signatures, so the one picked from them stays.
        let Some(account) = self.accounts.get(idx) else {
            return;
        };
        self.signature_btn
            .set_visible(!account.signatures.is_empty());
        let new_sig = account.signature.clone();
        if !account_changed || new_sig == self.current_sig {
            return;
        }
        self.put_signature(new_sig);
    }

    /// Handles [`ComposeInput::ToggleSign`].
    pub(super) fn on_toggle_sign(&mut self, on: bool) {
        self.sign = on;
        // Anything but the composer's own setting is a choice for
        // this message, which a later From change must not undo.
        if self.sign_expected.take() != Some(on) {
            self.sign_touched = true;
        }
    }

    /// Handles [`ComposeInput::PgpChecked`].
    pub(super) fn on_pgp_checked(
        &mut self,
        checked: Vec<String>,
        result: Result<(), String>,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        self.pgp_checking = false;
        match result {
            Ok(()) => {
                self.pgp_cleared = Some(checked);
                sender.input_sender().emit(ComposeInput::Send);
            }
            Err(problem) => {
                let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
                alert(parent.as_ref(), &i18n("Cannot send with OpenPGP"), &problem);
            }
        }
    }

    /// Handles [`ComposeInput::Send`].
    pub(super) fn on_send(&mut self, widgets: &ComposeWidgets, sender: &ComponentSender<Self>) {
        let to = widgets.to_row.text().trim().to_string();
        if to.is_empty() {
            widgets.to_row.add_css_class("error");
            return;
        }
        if !recipients_ok(widgets) {
            return;
        }
        let to = widgets.to_row.text().trim().to_string();
        if !self.attachments_present(widgets, sender) {
            return;
        }
        let cc = widgets.cc_row.text().trim().to_string();
        let bcc = widgets.bcc_row.text().trim().to_string();
        let reply_to = widgets.reply_to_row.text().trim().to_string();
        let subject = widgets.subject_row.text().to_string();
        let idx = widgets.from_row.selected() as usize;
        if self.pgp_check_pending(idx, [&to, &cc, &bcc], sender) {
            return;
        }
        let from_account_id = self.accounts.get(idx).map(|a| a.id).unwrap_or(1);
        let from_alias = match self.chosen_from(widgets) {
            Ok(from) => from,
            Err(why) => {
                let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
                alert(parent.as_ref(), &i18n("Check the From address"), &why);
                return;
            }
        };

        // Pull the body out of the editor (async), then finish
        // sending via SendBody. The send-time reader also recuts any
        // picture armed for it; a draft save below does not. In a
        // source format `html` carries the source itself, which
        // `outgoing_body` converts.
        let s = sender.clone();
        self.read_body_as(self.format, true, move |html, text| {
            s.input_sender()
                .emit(ComposeInput::SendBody(OutgoingFields {
                    html,
                    text,
                    to: to.clone(),
                    cc: cc.clone(),
                    bcc: bcc.clone(),
                    reply_to: reply_to.clone(),
                    subject: subject.clone(),
                    from_account_id,
                    from_alias: from_alias.clone(),
                }));
        });
    }

    /// Whether every attached file can still be read. One moved or deleted
    /// since it was attached (#340) is named, and the send stops: the
    /// composer closes on Send, so this is the last moment the message can
    /// still be fixed rather than lost.
    fn attachments_present(
        &mut self,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) -> bool {
        let missing: Vec<String> = self
            .attachments
            .iter()
            .filter(|p| std::fs::File::open(p).is_err())
            .map(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            })
            .collect();
        if !missing.is_empty() {
            self.rebuild_attachments(&widgets.attach_box, sender);
            let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
            alert(parent.as_ref(), &i18n("An attachment is missing"), &i18n_f(
                "Hylki cannot read {names}. It was moved or deleted after it was attached. Remove it, or attach it again from where it is now.",
                &[("names", &missing.join(", "))],
            ));
            return false;
        }
        true
    }

    /// OpenPGP (#133): say what is missing before anything leaves. gpg
    /// answers on a worker thread, since a stale keyring lock makes it wait
    /// about ten seconds (#316); its answer sends again, and passes here
    /// when nothing has changed since. True while that check is out, and
    /// the send waits for it.
    fn pgp_check_pending(
        &mut self,
        idx: usize,
        fields: [&str; 3],
        sender: &ComponentSender<Self>,
    ) -> bool {
        if !(self.sign || self.encrypt) {
            return false;
        }
        let fields = fields.map(str::to_string);
        let from = self.accounts.get(idx);
        let from_email = from.map(|a| a.email.clone()).unwrap_or_default();
        let key = from.and_then(|a| a.pgp_key.clone());
        let encrypt = self.encrypt;
        let mut checked = vec![
            from_email.clone(),
            key.clone().unwrap_or_default(),
            encrypt.to_string(),
        ];
        checked.extend(fields.iter().cloned());
        if self.pgp_cleared.take().as_ref() != Some(&checked) {
            if !self.pgp_checking {
                self.pgp_checking = true;
                let s = sender.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
                        pgp_send_check(&from_email, key.as_deref(), &fields, encrypt)
                    })
                    .await
                    .unwrap_or_else(|_| Err("task failed".into()));
                    s.input_sender()
                        .emit(ComposeInput::PgpChecked { checked, result });
                });
            }
            return true;
        }
        false
    }

    /// Handles [`ComposeInput::SendBody`].
    pub(super) fn on_send_body(&mut self, fields: OutgoingFields, sender: &ComponentSender<Self>) {
        let mut out = self.outgoing_message(fields);
        // The direction of each line (#524) is written in only here, on
        // the way out: a draft goes back into the editor, which finds it
        // itself, and a `dir` kept there would outlast a line rewritten in
        // another language, or be copied onto each paragraph split from
        // it. HTML written by hand keeps every `dir` it gives.
        if self.format != ComposeFormat::Plain {
            out.html = crate::mail::direction::stamp(&out.html);
        }
        // A draft goes once it is sent; a template stays to be used
        // again.
        if self.template {
            out.draft_origin = None;
        }
        let _ = sender.output(ComposeOutput::Send {
            id: self.compose_id,
            message: Box::new(out),
        });
        let _ = sender.output(ComposeOutput::Close(self.compose_id));
    }

    /// Handles [`ComposeInput::SaveDraftBody`].
    pub(super) fn on_save_draft_body(
        &mut self,
        fields: OutgoingFields,
        as_template: bool,
        sender: &ComponentSender<Self>,
    ) {
        let mut out = self.outgoing_message(fields);
        if as_template {
            // A copy, cut loose from wherever this message came
            // from: saving it replaces no draft and no queued
            // message, and a message started from it answers
            // nothing and waits for no time.
            out.draft_origin = None;
            out.outbox_origin = None;
            out.in_reply_to.clear();
            out.references.clear();
            out.forwards.clear();
            out.send_at = None;
            let _ = sender.output(ComposeOutput::SaveTemplate(Box::new(out)));
            return;
        }
        if self.template {
            let _ = sender.output(ComposeOutput::SaveTemplate(Box::new(out)));
        } else {
            let _ = sender.output(ComposeOutput::SaveDraft(Box::new(out)));
        }
        let _ = sender.output(ComposeOutput::Close(self.compose_id));
    }

    /// Handles [`ComposeInput::AutoSave`].
    pub(super) fn on_auto_save(&mut self, sender: &ComponentSender<Self>) {
        if self.autosave_off || self.autosaving || self.asking_discard || self.translating {
            return;
        }
        // Not before anything was written: an untouched reply is
        // no draft.
        if self.fields_dirty {
            sender.input_sender().emit(ComposeInput::AutoSaveRead);
        } else {
            let s = sender.clone();
            self.editor.is_dirty(move |dirty| {
                if dirty {
                    s.input_sender().emit(ComposeInput::AutoSaveRead);
                }
            });
        }
    }

    /// Handles [`ComposeInput::AutoSaveRead`].
    pub(super) fn on_auto_save_read(
        &mut self,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        if self.autosaving {
            return;
        }
        let fields = self.draft_fields(widgets);
        let s = sender.clone();
        self.read_body_as(self.format, false, move |html, text| {
            s.input_sender()
                .emit(ComposeInput::AutoSaveBody(OutgoingFields {
                    html,
                    text,
                    ..fields.clone()
                }));
        });
    }

    /// Handles [`ComposeInput::AutoSaveBody`].
    pub(super) fn on_auto_save_body(
        &mut self,
        fields: OutgoingFields,
        sender: &ComponentSender<Self>,
    ) {
        let OutgoingFields {
            html,
            text,
            to,
            cc,
            bcc,
            reply_to,
            subject,
            from_account_id,
            from_alias,
        } = fields;
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (
            &html,
            &text,
            &to,
            &cc,
            &bcc,
            &reply_to,
            &subject,
            from_account_id,
            &from_alias,
            &self.attachments,
        )
            .hash(&mut h);
        let hash = h.finish();
        // Unchanged since the last save, or nothing in it at all.
        let empty = [&to, &cc, &bcc, &subject]
            .iter()
            .all(|f| f.trim().is_empty())
            && text.trim().is_empty()
            && self.attachments.is_empty();
        if self.autosaved == Some(hash) || empty || self.autosaving {
            return;
        }
        let out = self.outgoing_message(OutgoingFields {
            html,
            text,
            to,
            cc,
            bcc,
            reply_to,
            subject,
            from_account_id,
            from_alias,
        });
        self.autosaving = true;
        self.autosave_pending = Some(hash);
        let _ = sender.output(ComposeOutput::AutoSave {
            id: self.compose_id,
            message: Box::new(out),
        });
    }

    /// Handles [`ComposeInput::Autosaved`].
    pub(super) fn on_autosaved(&mut self, saved: bool, origin: Option<DraftOrigin>) {
        self.autosaving = false;
        let pending = self.autosave_pending.take();
        if !saved {
            // Offline, say: the next tick tries again.
            return;
        }
        match origin {
            Some(origin) => {
                if self.draft_origin.is_none() {
                    self.autosave_made = true;
                }
                self.draft_origin = Some(origin);
                self.autosaved = pending;
            }
            // Saved, but not found again to be replaced next time:
            // one more save would leave a second copy behind.
            None => {
                tracing::info!("automatic draft save stopped: the saved copy was not found");
                self.autosave_off = true;
            }
        }
    }

    /// The From row: the identities, the pencil for a From of one's own, and
    /// a prefilled From that is none of the identities.
    pub(super) fn fill_from_row(
        &mut self,
        widgets: &ComposeWidgets,
        prefill: &ComposePrefill,
        selected: usize,
        sender: &ComponentSender<Self>,
    ) {
        // Populate the From dropdown.
        let labels: Vec<&str> = self.accounts.iter().map(|a| a.label.as_str()).collect();
        let strings = gtk::StringList::new(&labels);
        widgets.from_row.set_model(Some(&strings));
        // Custom factory so the selected account isn't needlessly ellipsized.
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                let label = gtk::Label::new(None);
                label.set_xalign(0.0);
                label.set_ellipsize(gtk::pango::EllipsizeMode::None);
                item.set_child(Some(&label));
            }
        });
        factory.connect_bind(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                let text = item
                    .item()
                    .and_downcast::<gtk::StringObject>()
                    .map(|s| s.string().to_string())
                    .unwrap_or_default();
                if let Some(label) = item.child().and_downcast::<gtk::Label>() {
                    label.set_label(&text);
                }
            }
        });
        widgets.from_row.set_factory(Some(&factory));
        widgets.from_row.set_selected(selected as u32);
        widgets.from_row.set_visible(self.accounts.len() > 1);
        {
            // The pencil types a From of your own (#347); the custom row's
            // button goes back to the list.
            let edit = gtk::Button::from_icon_name("document-edit-symbolic");
            edit.add_css_class("flat");
            edit.set_valign(gtk::Align::Center);
            edit.set_tooltip_text(Some(i18n("Type a From address").as_str()));
            let s = sender.input_sender().clone();
            edit.connect_clicked(move |_| {
                let _ = s.send(ComposeInput::CustomFrom(true));
            });
            widgets.from_row.add_suffix(&edit);
            let back = gtk::Button::from_icon_name("pan-down-symbolic");
            back.add_css_class("flat");
            back.set_valign(gtk::Align::Center);
            back.set_tooltip_text(Some(i18n("Choose from your addresses").as_str()));
            let s = sender.input_sender().clone();
            back.connect_clicked(move |_| {
                let _ = s.send(ComposeInput::CustomFrom(false));
            });
            widgets.custom_from_row.add_suffix(&back);
        }
        // A From that is none of the identities opens as text (#347).
        let custom = prefill.custom_from.trim();
        if !custom.is_empty() {
            let addr = crate::worker::parse_recipients(custom)
                .into_iter()
                .next()
                .map(|(_, a)| a)
                .unwrap_or_default();
            let same = self
                .accounts
                .get(selected)
                .is_some_and(|a| same_from(&identity_display(a), custom));
            // A bare address that is the identity's counts as the identity.
            let bare = !custom.contains('<')
                && self
                    .accounts
                    .get(selected)
                    .is_some_and(|a| a.email.eq_ignore_ascii_case(&addr));
            if !same && !bare {
                self.custom_from = true;
                widgets.from_row.set_visible(false);
                widgets.custom_from_row.set_text(custom);
                widgets.custom_from_row.set_visible(true);
            }
        }
    }

    /// Save Draft and Save as Template: the fields now, the body once the
    /// editor has given it.
    pub(super) fn on_save_draft(
        &mut self,
        as_template: bool,
        widgets: &ComposeWidgets,
        sender: &ComponentSender<Self>,
    ) {
        // A draft can be saved without recipients; just capture the fields.
        let fields = self.draft_fields(widgets);
        let s = sender.clone();
        self.read_body_as(self.format, false, move |html, text| {
            s.input_sender().emit(ComposeInput::SaveDraftBody {
                fields: OutgoingFields {
                    html,
                    text,
                    ..fields.clone()
                },
                as_template,
            });
        });
    }
}

pub(super) fn start_autosave(sender: &ComponentSender<Compose>) {
    // The message is saved to Drafts on its own every so often while it
    // changes (#340), so a crash, a lost connection or a closed laptop
    // costs at most that much. The timer stops with the composer.
    let s = sender.input_sender().clone();
    gtk::glib::timeout_add_seconds_local(AUTOSAVE_SECS, move || {
        if s.send(ComposeInput::AutoSave).is_err() {
            return gtk::glib::ControlFlow::Break;
        }
        gtk::glib::ControlFlow::Continue
    });
}

/// The recipient fields cleaned of invisible characters pasted along with
/// an address, then checked: an address the message cannot go to is
/// marked and explained, and the send stops. The composer closes on Send,
/// so this is where it can still be fixed (#368).
fn recipients_ok(widgets: &ComposeWidgets) -> bool {
    for row in [&widgets.to_row, &widgets.cc_row, &widgets.bcc_row] {
        if let Some(clean) = crate::worker::clean_recipient_field(&row.text()) {
            row.set_text(&clean);
        }
    }
    if let Some(clean) = crate::worker::clean_recipient_field(&widgets.reply_to_row.text()) {
        widgets.reply_to_row.set_text(&clean);
    }
    let rows: [(&gtk::Widget, String); 4] = [
        (widgets.to_row.upcast_ref(), widgets.to_row.text()),
        (widgets.cc_row.upcast_ref(), widgets.cc_row.text()),
        (widgets.bcc_row.upcast_ref(), widgets.bcc_row.text()),
        (
            widgets.reply_to_row.upcast_ref(),
            widgets.reply_to_row.text().to_string(),
        ),
    ];
    if let Some((row, addr)) = rows
        .iter()
        .find_map(|(row, text)| crate::worker::invalid_recipient(text).map(|addr| (row, addr)))
    {
        row.add_css_class("error");
        let parent = widgets.to_row.root().and_downcast::<gtk::Window>();
        alert(
            parent.as_ref(),
            &i18n("Check the address"),
            &i18n_f("“{addr}” is not an email address.", &[("addr", &addr)]),
        );
        return false;
    }
    true
}
