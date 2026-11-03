//! Attachments: saving them to disk, and pruning old downloads.

use std::collections::{HashMap, HashSet};

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::i18n::{i18n, i18n_f};
use crate::models::Attachment;
use crate::ui::attachment_drawer::AttachmentDrawerInput;
use crate::ui::attachment_io::save_attachment_as;
use crate::ui::attachments_gallery::GalleryInput;
use crate::ui::message_list::MessageListInput;
use crate::ui::message_view::MessageViewInput;
use crate::ui::message_window::MessageWindowInput;
use crate::worker::MailRequest;

use super::conversation::{body_key, BodyKey};
use super::model::AppModel;
use super::msg::AppMsg;
use crate::ui::dialog::confirm_destructive;

/// Remove downloaded mail older than `days` (#394), off the main thread;
/// 0 keeps it all. An account keeping a period offline (#536) keeps that
/// period, or the prune would remove what its download brings back, and
/// On This Computer (#552) keeps it all, for the same reason.
pub(super) fn prune_downloads(days: u32) {
    if days == 0 {
        return;
    }
    let now = crate::datefmt::now();
    let before = now - i64::from(days) * 86_400;
    let keep: Vec<(u32, i64)> = config::load()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .filter_map(|(i, a)| {
            let since = if a.is_local() {
                Some(i64::MIN)
            } else {
                a.offline_mail.since(now)
            };
            Some((i as u32 + 1, since?))
        })
        .collect();
    std::thread::spawn(move || {
        match crate::cache::Cache::open().and_then(|c| c.remove_downloaded_sparing(before, &keep)) {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!("storage: removed {n} downloaded message(s) older than {days} days")
            }
            Err(e) => tracing::warn!("storage: could not remove old downloaded mail: {e}"),
        }
    });
}

/// One attachment of one message on the server, to be removed (#289).
#[derive(Debug, Clone)]
pub(crate) struct AttachmentTarget {
    pub(super) account_id: u32,
    pub(super) message_id: u32,
    pub(super) path: String,
    pub(super) uid: u32,
    pub(super) name: String,
    pub(super) size: u64,
}

impl AttachmentTarget {
    fn gallery_key(&self) -> crate::ui::attachments_gallery::FileKey {
        (
            self.account_id,
            self.path.clone(),
            self.uid,
            self.name.clone(),
        )
    }
}

impl AppModel {
    pub(super) fn sync_attachment_drawer(&self) {
        // Switched off (#213), the drawer is simply never given anything:
        // empty hides it, seam and all.
        let items = if self.drawer_enabled {
            self.attachments.clone()
        } else {
            Vec::new()
        };
        self.attachment_drawer
            .emit(AttachmentDrawerInput::SetItems(items));
        self.push_card_attachments();
    }

    /// The messages on screen: the open conversation, or the lone message.
    fn conversation_members(&self) -> Vec<(u32, u32)> {
        if self.current_thread.is_empty() {
            self.current.iter().map(|c| (c.account_id, c.id)).collect()
        } else {
            self.current_thread
                .iter()
                .map(|m| (m.account_id, m.id))
                .collect()
        }
    }

    /// Where the message on screen that goes by `(account, id)` lives, as
    /// the caches key it (see [`body_key`]).
    pub(super) fn member_key(&self, (account_id, id): (u32, u32)) -> Option<BodyKey> {
        self.current_thread
            .iter()
            .chain(self.current.iter())
            .find(|m| m.account_id == account_id && m.id == id)
            .map(body_key)
    }

    /// The message a worker event is about, as the caches key it: `message_id`
    /// in the folder at `path`. An event names a message by the id it was
    /// asked for under, which for a member opened out from another folder is
    /// the id it goes by here, so that is taken back to its UID. `None` for a
    /// folder the app does not know.
    pub(super) fn event_key(
        &self,
        account_id: u32,
        path: &str,
        message_id: u32,
    ) -> Option<BodyKey> {
        let fid = self.folder_by_path(account_id, path)?.id;
        let uid = self
            .related_ids
            .iter()
            .find(|((a, f, _), id)| *a == account_id && *f == fid && **id == message_id)
            .map_or(message_id, |((_, _, uid), _)| *uid);
        Some((account_id, fid, uid))
    }

    /// Give the message at `key` its paperclip or take it away, in its own
    /// folder only, where the list keeps it and on its row.
    pub(super) fn set_has_attachment(&mut self, key: Option<BodyKey>, has: bool) {
        let Some((account_id, folder_id, uid)) = key else {
            return;
        };
        if let Some(msgs) = self.message_cache.get_mut(&(account_id, folder_id)) {
            for m in msgs.iter_mut().filter(|m| m.uid == uid) {
                m.has_attachment = has;
            }
        }
        self.message_list.emit(MessageListInput::SetHasAttachment {
            account_id,
            folder_id,
            uid,
            has,
        });
    }

    /// Hand the reader what each message on screen has attached (#213), as
    /// far as the cache knows: names and sizes only, never the bytes — the
    /// document lists them, and the app opens or saves them on request.
    pub(super) fn push_card_attachments(&self) {
        use crate::ui::message_view::CardAttachment;
        let mut map: HashMap<(u32, u32), Vec<CardAttachment>> = HashMap::new();
        for key in self.conversation_members() {
            let atts: Vec<CardAttachment> = match self
                .member_key(key)
                .and_then(|k| self.attachment_cache.get(&k))
            {
                Some(items) => items
                    .iter()
                    .map(|a| CardAttachment {
                        name: a.name.clone(),
                        size: a.data.len() as u64,
                    })
                    .collect(),
                None => self.sent_card_files(key).unwrap_or_default(),
            };
            if !atts.is_empty() {
                map.insert(key, atts);
            }
        }
        self.message_view
            .emit(MessageViewInput::SetCardAttachments(map));
    }

    /// Fill the drawer for a whole conversation: ask the disk cache for every
    /// member's attachments (never the network) and show what's already in
    /// hand. Opening a thread used to load nothing at all — the drawer only
    /// appeared after clicking a member, which runs the single-message path.
    pub(super) fn load_thread_attachments(&mut self) {
        let wanted: Vec<(u32, u32, u32, String)> = self
            .current_thread
            .iter()
            .filter(|tm| tm.has_attachment)
            // A reply's stand-in card is on no server yet.
            .filter(|tm| !self.provisionals.contains_key(&(tm.account_id, tm.id)))
            .filter(|tm| !self.attachment_cache.contains_key(&body_key(tm)))
            .filter_map(|tm| {
                self.resolve_folder_path(tm)
                    .map(|p| (tm.account_id, tm.id, tm.uid, p))
            })
            .collect();
        if !wanted.is_empty() {
            self.attachments_loading = true;
        }
        for (account_id, message_id, uid, path) in wanted {
            self.send_to(
                account_id,
                MailRequest::LoadAttachments {
                    message_id,
                    path,
                    uid,
                    download: true,
                },
            );
        }
        self.refresh_thread_attachments();
    }

    /// Point the drawer at the union of cached attachments across the open
    /// conversation. A reply pulled in from Sent can duplicate what it was
    /// sent with, so identical (name, size) pairs are shown once.
    pub(super) fn refresh_thread_attachments(&mut self) {
        let mut seen = HashSet::new();
        let mut merged = Vec::new();
        for tm in &self.current_thread {
            if let Some(items) = self.attachment_cache.get(&body_key(tm)) {
                for a in items {
                    if seen.insert((a.name.clone(), a.data.len())) {
                        merged.push(a.clone());
                    }
                }
            }
        }
        self.attachments = merged;
        self.sync_attachment_drawer();
    }

    /// The message on screen that carries the attachment `name` (of `len`
    /// bytes, when known), as the drawer's Show in Message finds it.
    pub(super) fn attachment_target(
        &self,
        name: &str,
        len: Option<usize>,
    ) -> Option<AttachmentTarget> {
        let (m, size) = self
            .current_thread
            .iter()
            .chain(self.current.iter())
            .find_map(|m| {
                let items = self.attachment_cache.get(&body_key(m))?;
                let a = items
                    .iter()
                    .find(|a| a.name == name && len.is_none_or(|l| a.data.len() == l))?;
                Some((m, a.data.len() as u64))
            })?;
        Some(AttachmentTarget {
            account_id: m.account_id,
            message_id: m.id,
            path: self.resolve_folder_path(m)?,
            uid: m.uid,
            name: name.to_string(),
            size,
        })
    }

    /// Forget a deletion the worker has answered, handing back what it was.
    fn take_deleting(
        &mut self,
        account_id: u32,
        message_id: u32,
        name: &str,
    ) -> Option<AttachmentTarget> {
        let at = self.deleting_attachments.iter().position(|t| {
            t.account_id == account_id && t.message_id == message_id && t.name == name
        })?;
        Some(self.deleting_attachments.remove(at))
    }

    /// Removing an attachment cannot be undone, and reaches every device
    /// that reads the account: ask (#289).
    fn confirm_delete_attachment(
        &self,
        target: AttachmentTarget,
        parent: &gtk::Window,
        sender: &ComponentSender<Self>,
    ) {
        let heading = i18n("Delete the attachment from the server?");
        let body = i18n_f(
            "“{name}” will be removed from the message on the server. The rest of the message stays. This can’t be undone.",
            &[("name", &target.name)],
        );
        let s = sender.clone();
        confirm_destructive(
            Some(parent),
            &heading,
            &body,
            "delete",
            &i18n("Delete"),
            move || s.input(AppMsg::DeleteAttachment(target)),
        );
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_attachments(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::LoadAttachmentsFor(m) => {
                let m = *m;
                if let Some(path) = self.resolve_folder_path(&m) {
                    self.send_to(
                        m.account_id,
                        MailRequest::LoadAttachments {
                            message_id: m.id,
                            path,
                            uid: m.uid,
                            download: true,
                        },
                    );
                }
            }

            AppMsg::DeletePopoutAttachment {
                key,
                message,
                attachment,
            } => {
                let Some(window) = self.popouts.get(&key).map(|p| p.window.clone()) else {
                    return;
                };
                let Some(path) = self.resolve_folder_path(&message) else {
                    return;
                };
                let target = AttachmentTarget {
                    account_id: message.account_id,
                    message_id: message.id,
                    path,
                    uid: message.uid,
                    name: attachment.name.clone(),
                    size: attachment.data.len() as u64,
                };
                self.confirm_delete_attachment(target, window.upcast_ref(), &sender);
            }

            AppMsg::ShowLightbox { items, start } => {
                self.lightbox
                    .emit(crate::ui::lightbox::LightboxInput::Show { items, start });
            }
            AppMsg::CardAttachment {
                account_id,
                id,
                index,
                save,
            } => {
                let Some(items) = self
                    .member_key((account_id, id))
                    .and_then(|k| self.attachment_cache.get(&k))
                    .cloned()
                    .or_else(|| self.sent_file_bytes((account_id, id)))
                else {
                    return;
                };
                let Some(att) = items.get(index).cloned() else {
                    return;
                };
                if save {
                    // A card chip's save button (#213); a failed write is dropped.
                    save_attachment_as(&att.name, att.data, Some(self.window.upcast_ref()), |_| {});
                } else if crate::ui::attachment_drawer::previewable(&att) {
                    // The lightbox pages through this message's previewable
                    // files, starting at the one clicked.
                    let previewable: Vec<Attachment> = items
                        .iter()
                        .filter(|a| crate::ui::attachment_drawer::previewable(a))
                        .cloned()
                        .collect();
                    let start = items
                        .iter()
                        .take(index + 1)
                        .filter(|a| crate::ui::attachment_drawer::previewable(a))
                        .count()
                        .saturating_sub(1);
                    sender.input(AppMsg::ShowLightbox {
                        items: previewable,
                        start,
                    });
                } else {
                    crate::ui::attachment_io::open_bytes(
                        &att.name,
                        &att.data,
                        Some(self.window.upcast_ref::<gtk::Window>()),
                    );
                }
            }
            AppMsg::ShowAttachmentInMessage(att) => {
                // The drawer shows identical (name, size) pairs once, so the
                // first message carrying this one is the one to find.
                let owner = self.conversation_members().into_iter().find(|key| {
                    self.member_key(*key)
                        .and_then(|k| self.attachment_cache.get(&k))
                        .is_some_and(|items| {
                            items
                                .iter()
                                .any(|a| a.name == att.name && a.data.len() == att.data.len())
                        })
                });
                if let Some((account_id, id)) = owner {
                    self.message_view
                        .emit(MessageViewInput::ScrollToAttachments { account_id, id });
                }
            }
            AppMsg::DeleteDrawerAttachment(att) => {
                if let Some(target) = self.attachment_target(&att.name, Some(att.data.len())) {
                    sender.input(AppMsg::AskDeleteAttachment(target));
                }
            }
            AppMsg::AskDeleteAttachment(target) => {
                self.confirm_delete_attachment(target, self.window.upcast_ref(), &sender)
            }
            AppMsg::DeleteAttachment(t) => {
                self.attachment_drawer
                    .emit(AttachmentDrawerInput::MarkDeleting(
                        t.name.clone(),
                        t.size as usize,
                    ));
                if let Some(p) = self.popouts.get(&(t.account_id, t.message_id)) {
                    p.controller.emit(MessageWindowInput::Drawer(
                        AttachmentDrawerInput::MarkDeleting(t.name.clone(), t.size as usize),
                    ));
                }
                self.gallery
                    .emit(GalleryInput::SetDeleting(t.gallery_key(), true));
                self.deleting_attachments.push(t.clone());
                // HYLKI_SHOWCASE_DELETE_HOLD=<seconds> holds the request back,
                // so the file can be captured while it is being deleted.
                if let Some(secs) = std::env::var("HYLKI_SHOWCASE_DELETE_HOLD")
                    .ok()
                    .and_then(|v| v.parse().ok())
                {
                    let s = sender.clone();
                    gtk::glib::timeout_add_seconds_local_once(secs, move || {
                        s.input(AppMsg::SendDeleteAttachment(t));
                    });
                    return;
                }
                sender.input(AppMsg::SendDeleteAttachment(t));
            }
            AppMsg::SendDeleteAttachment(t) => {
                self.send_to(
                    t.account_id,
                    MailRequest::DeleteAttachment {
                        message_id: t.message_id,
                        path: t.path,
                        uid: t.uid,
                        name: t.name,
                        size: t.size,
                    },
                );
            }
            AppMsg::AttachmentNotDeleted {
                account_id,
                message_id,
                name,
                size,
            } => {
                if let Some(t) = self.take_deleting(account_id, message_id, &name) {
                    self.gallery
                        .emit(GalleryInput::SetDeleting(t.gallery_key(), false));
                }
                if let Some(p) = self.popouts.get(&(account_id, message_id)) {
                    p.controller.emit(MessageWindowInput::Drawer(
                        AttachmentDrawerInput::NotDeleted(name.clone(), size as usize),
                    ));
                }
                self.attachment_drawer
                    .emit(AttachmentDrawerInput::NotDeleted(name, size as usize));
            }
            msg @ AppMsg::AttachmentDeleted { .. } => self.on_attachment_deleted(msg, sender),
            _ => unreachable!("not routed to on_attachments"),
        }
    }

    /// The worker did it: the message is `new_uid` now (the same UID on
    /// Microsoft 365, a new one elsewhere, `None` if it was not found again).
    fn on_attachment_deleted(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        let AppMsg::AttachmentDeleted {
            account_id,
            message_id,
            path,
            uid,
            new_uid,
            name,
            size,
        } = msg
        else {
            unreachable!("only AttachmentDeleted comes here")
        };
        // The file fades out where it is shown; the lists that
        // follow wait for it.
        let t = self.take_deleting(account_id, message_id, &name);
        self.gallery.emit(GalleryInput::Removed((
            account_id,
            path.clone(),
            uid,
            name.clone(),
        )));
        let shown = t.map_or(size, |t| t.size) as usize;
        self.attachment_drawer
            .emit(AttachmentDrawerInput::Deleted(name.clone(), shown));
        if let Some(p) = self.popouts.get(&(account_id, message_id)) {
            p.controller
                .emit(MessageWindowInput::Drawer(AttachmentDrawerInput::Deleted(
                    name.clone(),
                    shown,
                )));
        }
        let key = (account_id, message_id);
        let members = self.conversation_members();
        self.thread_cache
            .retain(|_, members| !members.iter().any(|m| (m.account_id, m.id) == key));
        let folder = self.folder_by_path(account_id, &path).map(|f| f.id);
        if let Some(fid) = folder {
            self.body_cache.remove(&(account_id, fid, uid));
        }
        let held = folder.map(|fid| (account_id, fid, uid));
        if new_uid == Some(uid) {
            // The same message, one file fewer (Microsoft 365): the
            // reader drops the file where it stands.
            if let Some(mut items) = held.and_then(|k| self.attachment_cache.remove(&k)) {
                if let Some(at) = items
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| a.name == name)
                    .min_by_key(|(_, a)| (a.data.len() as u64).abs_diff(size))
                    .map(|(i, _)| i)
                {
                    items.remove(at);
                }
                sender.input(AppMsg::Attachments {
                    account_id,
                    path,
                    message_id,
                    items,
                });
            }
            return;
        }
        // A copy took the message's place under a new UID. The list,
        // loaded again first, may already have carried the selection
        // over to it by Message-ID, the files it held with it; if not,
        // the copy is opened here.
        if let Some(k) = held {
            self.attachment_cache.remove(&k);
        }
        let Some(new_uid) = new_uid else { return };
        let now = (account_id, new_uid);
        if let Some(fid) = folder {
            self.attachment_cache.remove(&(account_id, fid, new_uid));
        }
        if members.contains(&now) {
            self.send_to(
                account_id,
                MailRequest::LoadAttachments {
                    message_id: new_uid,
                    path,
                    uid: new_uid,
                    download: true,
                },
            );
        } else if members.contains(&key) {
            self.message_list.emit(MessageListInput::SelectAndLoad(now));
        }
    }
}

/// Downloaded mail past Settings → System → Keep downloaded mail goes
/// a little after start, once the first syncs are under way, and
/// every six hours after (#394).
pub(super) fn schedule_download_prune() {
    gtk::glib::timeout_add_seconds_local_once(120, || {
        prune_downloads(config::load_privacy().keep_downloads_days);
        gtk::glib::timeout_add_seconds_local(6 * 3600, || {
            prune_downloads(config::load_privacy().keep_downloads_days);
            gtk::glib::ControlFlow::Continue
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_targets_key_the_gallery_by_path_uid_and_name() {
        let t = AttachmentTarget {
            account_id: 1,
            message_id: 2,
            path: "INBOX".into(),
            uid: 3,
            name: "a.pdf".into(),
            size: 4,
        };
        assert_eq!(
            t.gallery_key(),
            (1, "INBOX".to_string(), 3, "a.pdf".to_string())
        );
    }
}
