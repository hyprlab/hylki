//! Opening a composer on a draft, a reply, a forward or an edited copy,
//! fetching the original's body first when it is not cached.

use relm4::prelude::*;

use crate::config;
use crate::models::{FolderKind, Message};
use crate::ui::compose::{ComposeInput, ComposePrefill};
use crate::worker::MailRequest;

use super::conversation::body_key;
use super::hand_off::{FileHandOff, HandOffFiles};
use super::model::AppModel;
use super::msg::AppMsg;
use super::prefill::{
    first_recipient, forward_prefill, reply_all_prefill, reply_prefill, reply_subject,
    stage_attachments,
};
use super::uri_queue::parse_mailto;
use crate::mail::quote::editable_copy_html;
use crate::ui::keymap::WindowCompose;

impl AppModel {
    /// Where a just-saved draft is, by its Message-ID, among the Drafts
    /// folder's messages the save has reloaded.
    pub(super) fn find_draft(
        &self,
        account_id: u32,
        message_id: &str,
    ) -> Option<crate::models::DraftOrigin> {
        let (folder_id, path) = self.drafts_target(account_id)?;
        let want = message_id
            .trim()
            .trim_start_matches('<')
            .trim_end_matches('>');
        let m = self
            .message_cache
            .get(&(account_id, folder_id))?
            .iter()
            .find(|m| {
                m.message_id
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .eq_ignore_ascii_case(want)
            })?;
        Some(crate::models::DraftOrigin {
            account_id,
            folder_id,
            path,
            uid: m.uid,
        })
    }

    /// Take a draft out of its folder for good, and list the folder again.
    pub(super) fn purge_draft(&self, origin: &crate::models::DraftOrigin) {
        self.send_to(
            origin.account_id,
            MailRequest::PurgeMessages {
                path: origin.path.clone(),
                uids: vec![origin.uid],
            },
        );
        self.send_to(
            origin.account_id,
            MailRequest::LoadMessages {
                folder_id: origin.folder_id,
                path: origin.path.clone(),
            },
        );
    }

    pub(super) fn composer_exists(&self, id: u32) -> bool {
        self.composers.iter().any(|h| h.id == id)
            || self.reader_compose.as_ref().is_some_and(|r| r.id == id)
    }

    pub(super) fn emit_to_composer(&self, id: u32, input: ComposeInput) {
        if let Some(h) = self.composers.iter().find(|h| h.id == id) {
            h.controller.emit(input);
        } else if let Some(r) = self.reader_compose.as_ref().filter(|r| r.id == id) {
            r.controller.emit(input);
        }
    }

    /// Open a draft for editing, from its own bytes (#349, #350): the body
    /// the reader shows has lost the From, the Bcc, the threading headers
    /// and the files. The editor opens once they arrive (see the
    /// `RawExported` handler). `inline` puts the editor in the reading pane
    /// (a selected draft); otherwise it gets a window (a draft opened by
    /// double-click or Enter). `extra` carries files handed in from GNOME
    /// Files to attach (or upload) on top of the draft's own.
    pub(super) fn open_draft(
        &mut self,
        m: Message,
        inline: bool,
        extra: HandOffFiles,
        sender: &ComponentSender<Self>,
    ) {
        let Some(path) = self.resolve_folder_path(&m) else {
            self.open_draft_body(m, inline, extra, sender);
            return;
        };
        let token = self.next_transfer;
        self.next_transfer += 1;
        self.pending_draft_token = Some(token);
        self.send_to(
            m.account_id,
            MailRequest::ExportRaw {
                token,
                path,
                uid: m.uid,
                for_reader: true,
            },
        );
        self.pending_draft = Some((m, inline, extra));
    }

    /// Open a draft from its body alone, when its bytes can't be had: a
    /// cached body if there is one, otherwise fetched, with the editor
    /// opening once it arrives (see the `Body` handler).
    pub(super) fn open_draft_body(
        &mut self,
        m: Message,
        inline: bool,
        extra: HandOffFiles,
        sender: &ComponentSender<Self>,
    ) {
        let body = if !m.body.is_empty() {
            Some(m.body.clone())
        } else {
            self.body_cache.get(&body_key(&m)).cloned()
        };
        match body {
            Some(html) => self.compose_from_draft(m, html, inline, extra, sender),
            None => {
                if let Some(path) = self.resolve_folder_path(&m) {
                    self.send_to(
                        m.account_id,
                        MailRequest::LoadBody {
                            message_id: m.id,
                            path,
                            uid: m.uid,
                        },
                    );
                }
                self.pending_draft = Some((m, inline, extra));
            }
        }
    }

    /// Open the compose editor pre-filled from a draft, remembering its origin so
    /// saving/sending replaces it. Inline, the editor covers the reading pane
    /// the way a new message does, with the pane cleared beneath it: the draft
    /// is what is selected, so nothing older should reappear when the editor
    /// closes.
    pub(super) fn compose_from_draft(
        &mut self,
        m: Message,
        body_html: String,
        inline: bool,
        extra: HandOffFiles,
        sender: &ComponentSender<Self>,
    ) {
        let prefill = ComposePrefill {
            to: m.to.clone(),
            cc: m.cc.clone(),
            subject: m.subject.clone(),
            body_html,
            from_address: m.from_addr.clone(),
            attachments: extra.attach,
            cloud_uploads: extra.cloud,
            ..Default::default()
        };
        self.show_draft_composer(m, prefill, inline, sender);
    }

    /// Open the compose editor on a draft's own bytes: everything it was
    /// saved with, its files written out for the composer to attach.
    pub(super) fn compose_from_draft_raw(
        &mut self,
        m: Message,
        raw: &[u8],
        inline: bool,
        extra: HandOffFiles,
        sender: &ComponentSender<Self>,
    ) {
        let e = crate::worker::editable_from_raw(raw, &[]);
        let mut attachments = stage_attachments(
            &format!("hylki-draft-{}-{}", m.account_id, m.uid),
            &e.attachments,
        );
        attachments.extend(extra.attach);
        let prefill = ComposePrefill {
            to: e.to,
            cc: e.cc,
            bcc: e.bcc,
            subject: e.subject,
            body_html: e.body_html,
            in_reply_to: e.in_reply_to,
            references: e.references,
            receipt: Some(e.receipt),
            from_address: first_recipient(&e.from)
                .map(|(_, addr)| addr)
                .unwrap_or_default(),
            // Saved from an address that is no identity (#347): it opens
            // as typed.
            custom_from: e.from,
            attachments,
            cloud_uploads: extra.cloud,
            ..Default::default()
        };
        self.show_draft_composer(m, prefill, inline, sender);
    }

    /// The editor for a draft, tied to it so saving or sending replaces it.
    fn show_draft_composer(
        &mut self,
        m: Message,
        mut prefill: ComposePrefill,
        inline: bool,
        sender: &ComponentSender<Self>,
    ) {
        prefill.draft_origin = Some(crate::models::DraftOrigin {
            account_id: m.account_id,
            folder_id: m.folder_id,
            path: self.resolve_folder_path(&m).unwrap_or_default(),
            uid: m.uid,
        });
        // A template opened this way is being edited (#360): saving puts it
        // back in Templates, and sending leaves it there.
        prefill.template =
            self.folder_kind(m.account_id, m.folder_id) == Some(FolderKind::Templates);
        if inline && self.reading_pane == config::ReadingPane::Right {
            self.attachments.clear();
            self.attachments_loading = false;
            self.sync_attachment_drawer();
            self.current = None;
            self.current_thread.clear();
            self.show_message(None, false);
            self.open_inline_reply(m.account_id, prefill, None, sender);
        } else {
            self.open_compose(m.account_id, prefill, sender);
        }
    }

    /// Asks for what a copy of `m` (a forward, an edit as new) still
    /// lacks: its body first, then its attachments. True when a request
    /// went out; the caller holds `m` until the answer re-enters.
    pub(super) fn fetch_for_copy(&self, m: &Message) -> bool {
        if m.body.is_empty() {
            if let Some(path) = self.resolve_folder_path(m) {
                self.send_to(
                    m.account_id,
                    MailRequest::LoadBody {
                        message_id: m.id,
                        path,
                        uid: m.uid,
                    },
                );
                return true;
            }
        }
        if m.has_attachment && !self.attachment_cache.contains_key(&body_key(m)) {
            if let Some(path) = self.resolve_folder_path(m) {
                self.send_to(
                    m.account_id,
                    MailRequest::LoadAttachments {
                        message_id: m.id,
                        path,
                        uid: m.uid,
                        download: true,
                    },
                );
                return true;
            }
        }
        false
    }

    /// Forward a message with its attachments (#240): the quoted original
    /// in the body, and every file that came with it attached to the new
    /// message, as Thunderbird, Apple Mail and Gmail do. Much of what gets
    /// forwarded is forwarded for the file. A reply deliberately does not
    /// do this: the sender already has what they sent.
    ///
    /// Built like `edit_as_new`: the body and the attachments may still be
    /// on the server, so each is fetched at most once with the message held
    /// in `pending_forward`, and the reply re-enters here. `inline` says
    /// the composer opens where a reply does; otherwise in a window.
    pub(super) fn forward(&mut self, m: Message, inline: bool, sender: &ComponentSender<Self>) {
        let mut m = self.with_cached_body(m);
        if let Some(subject) = self.decrypted_subject(&m) {
            m.subject = subject;
        }
        if self.fetch_for_copy(&m) {
            self.pending_forward = Some((m, inline));
            return;
        }
        let attachments = self
            .attachment_cache
            .get(&body_key(&m))
            .map(|items| {
                stage_attachments(&format!("hylki-forward-{}-{}", m.account_id, m.id), items)
            })
            .unwrap_or_default();
        tracing::info!(
            "forward: {}:{} with {} attachment(s)",
            m.account_id,
            m.id,
            attachments.len()
        );
        let mut prefill = self.quoting(&m, forward_prefill(&m));
        prefill.attachments = attachments;
        if inline {
            self.open_reply(m.account_id, prefill, (m.account_id, m.id), sender);
        } else {
            self.open_compose(m.account_id, prefill, sender);
        }
    }

    /// "Edit as New Message" (#232): the message opened in the composer as a
    /// message of its own — the same recipients, subject, body and
    /// attachments, with none of the threading headers and no tie to what it
    /// was copied from. Sending it sends a new message; the original is left
    /// exactly where it was.
    ///
    /// The body and the attachments may still be on the server. Each is
    /// fetched at most once, and the reply re-enters here: the message is
    /// held in `pending_edit_as_new` meanwhile, so nothing opens until what
    /// is being copied is actually in hand.
    pub(super) fn edit_as_new(&mut self, m: Message, sender: &ComponentSender<Self>) {
        let m = self.with_cached_body(m);
        if self.fetch_for_copy(&m) {
            self.pending_edit_as_new = Some(m);
            return;
        }
        let attachments = self
            .attachment_cache
            .get(&body_key(&m))
            .map(|items| stage_attachments(&format!("hylki-copy-{}-{}", m.account_id, m.id), items))
            .unwrap_or_default();
        tracing::info!(
            "edit as new: copying {}:{} ({} attachment(s))",
            m.account_id,
            m.id,
            attachments.len()
        );
        // A template (#360) is used as it was written: from the address it
        // was saved with, and with the signature it already carries, none
        // put above it again.
        let template = self.folder_kind(m.account_id, m.folder_id) == Some(FolderKind::Templates);
        let prefill = self.quoting(
            &m,
            ComposePrefill {
                to: m.to.clone(),
                cc: m.cc.clone(),
                subject: m.subject.clone(),
                body_html: editable_copy_html(&m.body),
                attachments,
                from_address: if template {
                    m.from_addr.clone()
                } else {
                    String::new()
                },
                resumed: template,
                no_signature: true,
                ..Default::default()
            },
        );
        // A new message, so it opens where a new message opens.
        if self.compose_inline {
            self.open_inline_reply(m.account_id, prefill, None, sender);
        } else {
            self.open_compose(m.account_id, prefill, sender);
        }
    }

    /// What a reply takes over from how the reader showed the original. A
    /// reply to an encrypted message starts with Encrypt on (#133): the
    /// verdict the reader had for the original says whether it was.
    pub(super) fn reply_context(&self, m: &Message, mut prefill: ComposePrefill) -> ComposePrefill {
        prefill.encrypt = self
            .sender_cache
            .get(&body_key(m))
            .and_then(|c| c.pgp.as_ref())
            .is_some_and(|p| p.encrypted);
        if let Some(subject) = self.decrypted_subject(m) {
            prefill.subject = reply_subject(&subject);
        }
        self.quoting(m, prefill)
    }

    /// The subject inside `m` once it has been decrypted (#493), where its
    /// own is a placeholder. Only answers use it: a reply to an encrypted
    /// message starts encrypted, and its subject travels inside again.
    fn decrypted_subject(&self, m: &Message) -> Option<String> {
        self.sender_cache
            .get(&body_key(m))?
            .pgp
            .as_ref()?
            .subject
            .clone()
    }

    /// A composer quoting or copying `m` loads its remote pictures only
    /// when the reader would (#295).
    fn quoting(&self, m: &Message, mut prefill: ComposePrefill) -> ComposePrefill {
        prefill.block_remote_images = !self.remote_allowed(m);
        prefill
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_compose_open(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::Compose => {
                let (account, prefill) =
                    self.new_message_from(self.active_account(), ComposePrefill::default());
                if self.compose_inline {
                    // The new-message pane slides down over the reader,
                    // exactly like an inline reply — same composer, same
                    // pop-out-to-window toggle in its header.
                    self.open_inline_reply(account, prefill, None, &sender);
                } else {
                    self.open_compose(account, prefill, &sender);
                }
            }

            AppMsg::Reply => {
                if let Some(m) = self.compose_target() {
                    self.open_reply(
                        m.account_id,
                        self.reply_context(&m, reply_prefill(&m, &self.reply_header)),
                        (m.account_id, m.id),
                        &sender,
                    );
                }
            }

            AppMsg::ReplyAll => {
                if let Some(m) = self.compose_target() {
                    let own = self.own_identities(m.account_id);
                    self.open_reply(
                        m.account_id,
                        self.reply_context(&m, reply_all_prefill(&m, &own, &self.reply_header)),
                        (m.account_id, m.id),
                        &sender,
                    );
                }
            }

            AppMsg::Forward => {
                if let Some(m) = self.compose_target() {
                    self.forward(m, true, &sender);
                }
            }

            AppMsg::ComposeInWindow(what) => match what {
                WindowCompose::New => {
                    let (account, prefill) =
                        self.new_message_from(self.active_account(), ComposePrefill::default());
                    self.open_compose(account, prefill, &sender);
                }
                WindowCompose::Forward => {
                    if let Some(m) = self.compose_target() {
                        self.forward(m, false, &sender);
                    }
                }
                WindowCompose::Reply | WindowCompose::ReplyAll => {
                    if let Some(m) = self.compose_target() {
                        let prefill = if what == WindowCompose::Reply {
                            reply_prefill(&m, &self.reply_header)
                        } else {
                            reply_all_prefill(
                                &m,
                                &self.own_identities(m.account_id),
                                &self.reply_header,
                            )
                        };
                        self.open_compose(m.account_id, self.reply_context(&m, prefill), &sender);
                    }
                }
            },

            AppMsg::EditAsNewCurrent => {
                if let Some(m) = self.compose_target() {
                    self.edit_as_new(m, &sender);
                }
            }

            AppMsg::ComposeTo(to) => {
                self.compose_addressed(
                    ComposePrefill {
                        to,
                        ..Default::default()
                    },
                    &sender,
                );
            }

            AppMsg::ComposeBcc(bcc) => {
                self.compose_addressed(
                    ComposePrefill {
                        bcc,
                        ..Default::default()
                    },
                    &sender,
                );
            }

            AppMsg::OpenMailto(uri) => self.open_mailto(uri, sender),

            AppMsg::PresentComposers => {
                crate::desktop::session::present_window(&self.window);
                // The newest standalone composer is the one the alert was
                // about; transient for the main window, it sits above it.
                if let Some(h) = self.composers.last() {
                    crate::desktop::session::present_window(&h.window);
                }
            }
            _ => unreachable!("not routed to on_compose_open"),
        }
    }

    /// A `mailto:` link handed to the app: a composer with what it carries.
    fn open_mailto(&mut self, uri: String, sender: ComponentSender<Self>) {
        // A mailto: link from anywhere on the system (the desktop file
        // registers the scheme). Same open-composer flow as ComposeTo,
        // with the URI's subject/cc/bcc/body carried along.
        let Some(mut prefill) = parse_mailto(&uri) else {
            return;
        };
        // attach= names files (#90): keep only the ones that really
        // exist as regular files. Whatever attaches shows up as a
        // normal removable chip in the composer, so nothing rides
        // along invisibly if a web link (rather than Nautilus)
        // carried the parameter. The ones that don't are reported,
        // not dropped in silence: from inside the Flatpak sandbox a
        // path the file manager can see may be one Hylki cannot.
        let (attachments, dropped): (Vec<_>, Vec<_>) = std::mem::take(&mut prefill.attachments)
            .into_iter()
            .partition(|p| p.is_file());
        for p in &dropped {
            tracing::warn!(
                "mailto attach ignored (not a readable file): {}",
                p.display()
            );
        }
        tracing::info!(
            "mailto hand-off: {} attachment(s) named, {} readable",
            attachments.len() + dropped.len(),
            attachments.len()
        );
        if !attachments.is_empty() {
            // Files came along (Files' own "Email…" entry): the same
            // choice of destination as "Send with Hylki".
            self.begin_hand_off(
                FileHandOff {
                    base: prefill,
                    files: attachments,
                    dropped,
                },
                &sender,
            );
            return;
        }
        // None of the named files could be read (#339): asking where
        // no files should go helps nobody, so the message opens with
        // the error beside it.
        self.compose_addressed(prefill, &sender);
        self.after_hand_off(0, dropped);
    }
}
