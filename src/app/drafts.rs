//! Drafts and templates: their folders, and the pickers that choose one.

use std::collections::HashSet;

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::i18n;
use crate::models::{Folder, FolderKind, Message};
use crate::ui::compose::ComposeInput;
use crate::ui::message_list::MessageListInput;
use crate::ui::message_view::MessageViewInput;
use crate::worker::MailRequest;

use super::hand_off::FileHandOff;
use super::model::AppModel;
use super::msg::AppMsg;
use super::picker::show_message_picker;

impl AppModel {
    /// Every template the app knows, under its account's name.
    fn template_choices(&self) -> Vec<(Message, String)> {
        let mut templates = Vec::new();
        for (account_id, folder) in self.templates_folders() {
            let list = self.listed_or_cached(account_id, &folder);
            let name = self.account_name(account_id);
            templates.extend(list.into_iter().map(|m| (m, name.clone())));
        }
        templates.sort_by_key(|(m, _)| m.subject.to_lowercase());
        templates
    }

    /// The template picker: to start a new message from (#360), or with
    /// `into`, to put into that composer's message. As the draft picker
    /// does, Templates folders not listed this run are fetched first, and a
    /// silent worker gets three seconds.
    fn pick_template(&mut self, into: Option<u32>, sender: &ComponentSender<Self>) {
        let mut waiting = HashSet::new();
        for (account_id, folder) in self.templates_folders() {
            if !self.message_cache.contains_key(&(account_id, folder.id)) {
                waiting.insert((account_id, folder.id));
                self.send_to(
                    account_id,
                    MailRequest::SyncFolder {
                        folder_id: folder.id,
                        path: folder.path.clone(),
                    },
                );
            }
        }
        if waiting.is_empty() {
            self.show_template_picker(into, sender);
        } else {
            self.pending_template_pick = Some((into, waiting));
            let s = sender.clone();
            gtk::glib::timeout_add_seconds_local_once(3, move || s.input(AppMsg::TemplatePickNow));
        }
    }

    fn show_template_picker(&self, into: Option<u32>, sender: &ComponentSender<Self>) {
        let s = sender.clone();
        let empty =
            i18n("No templates yet. Save one from the composer's ⋯ menu with Save as Template.");
        match into {
            None => show_message_picker(
                self.window.upcast_ref(),
                &i18n("New Message from Template"),
                &i18n("The new message starts as a copy of the template you pick."),
                &empty,
                &i18n("Use Template"),
                self.template_choices(),
                (),
                true,
                move |(), template| s.input(AppMsg::TemplatePicked(template)),
            ),
            Some(id) => show_message_picker(
                self.window.upcast_ref(),
                &i18n("Insert Template"),
                &i18n("The template goes in where the cursor is."),
                &empty,
                &i18n("Insert Template"),
                self.template_choices(),
                id,
                false,
                move |id, template| {
                    if let Some(template) = template {
                        s.input(AppMsg::TemplateInsertPicked { id, template });
                    }
                },
            ),
        }
    }

    /// Fetch a picked template's body, then put it into composer `id`.
    fn insert_template(&mut self, id: u32, template: Message) {
        let template = self.with_cached_body(template);
        if !template.body.is_empty() {
            self.emit_to_composer(id, ComposeInput::TemplateBody(template.body));
            return;
        }
        let Some(path) = self.resolve_folder_path(&template) else {
            self.notice_error(i18n("Could not load the template."));
            return;
        };
        self.send_to(
            template.account_id,
            MailRequest::LoadBody {
                message_id: template.id,
                path,
                uid: template.uid,
            },
        );
        self.pending_template_insert = Some((id, template));
    }

    /// Each account's Templates folder (#360), in account order.
    pub(super) fn templates_folders(&self) -> Vec<(u32, Folder)> {
        self.folders_of_kind(FolderKind::Templates)
    }

    /// Each account's folder of one kind, in account order.
    fn folders_of_kind(&self, kind: FolderKind) -> Vec<(u32, Folder)> {
        let mut ids: Vec<u32> = self.folders.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
            .filter_map(|id| Some((id, self.folder_of_kind(id, kind)?.clone())))
            .collect()
    }

    /// A folder's messages: the ones listed this session, else what the
    /// cache on disk holds.
    fn listed_or_cached(&self, account_id: u32, folder: &Folder) -> Vec<Message> {
        match self.message_cache.get(&(account_id, folder.id)) {
            Some(l) => l.to_vec(),
            None => self
                .cache
                .as_ref()
                .map(|c| c.load_messages(account_id, &folder.path, folder.id))
                .unwrap_or_default(),
        }
    }

    /// The "Continue a Draft" picker over every draft the app knows.
    pub(super) fn show_draft_picker(&self, hand_off: FileHandOff, sender: &ComponentSender<Self>) {
        let drafts = self.drafts_for_pick();
        let s = sender.clone();
        show_message_picker(
            self.window.upcast_ref(),
            &i18n("Continue a Draft"),
            &i18n("The files go into the draft you pick."),
            &i18n("No drafts to continue."),
            &i18n("Attach to This Draft"),
            drafts,
            hand_off,
            true,
            move |hand_off, draft| s.input(AppMsg::HandOffDraft { hand_off, draft }),
        );
    }

    /// Tell the reader which folders hold templates (#360), so a template's
    /// card wears its banner.
    pub(super) fn push_template_folders(&self) {
        let folders = self
            .folders
            .iter()
            .flat_map(|(account_id, fs)| {
                fs.iter()
                    .filter(|f| f.kind == FolderKind::Templates)
                    .map(move |f| (*account_id, f.id))
            })
            .collect();
        self.message_view
            .emit(MessageViewInput::SetTemplateFolders(folders));
    }

    /// Each account's Drafts folder, in account order.
    pub(super) fn drafts_folders(&self) -> Vec<(u32, Folder)> {
        self.folders_of_kind(FolderKind::Drafts)
    }

    /// Every account's saved drafts, newest first, each with its account's
    /// name for the picker.
    fn drafts_for_pick(&self) -> Vec<(Message, String)> {
        let mut out = Vec::new();
        for (account_id, drafts) in self.drafts_folders() {
            let list = self.listed_or_cached(account_id, &drafts);
            let name = self.account_name(account_id);
            out.extend(list.into_iter().map(|m| (m, name.clone())));
        }
        out.sort_by_key(|(m, _)| std::cmp::Reverse(m.timestamp));
        out
    }

    /// Messages a reply could answer: what the reading pane shows first,
    /// then every account's Inbox (and the other folders listed this
    /// session, Drafts, Templates, Sent, Junk and Trash left out), newest
    /// first.
    pub(super) fn messages_for_pick(&self) -> Vec<(Message, String)> {
        let mut seen: HashSet<(u32, u32)> = HashSet::new();
        let mut out = Vec::new();
        for m in self.current.iter().chain(self.current_thread.iter()) {
            if seen.insert((m.account_id, m.id)) {
                out.push((m.clone(), self.account_name(m.account_id)));
            }
        }
        let mut rest = Vec::new();
        let mut ids: Vec<u32> = self.folders.keys().copied().collect();
        ids.sort_unstable();
        for account_id in ids {
            let Some(folders) = self.folders.get(&account_id) else {
                continue;
            };
            let name = self.account_name(account_id);
            for f in folders {
                if matches!(
                    f.kind,
                    FolderKind::Drafts
                        | FolderKind::Templates
                        | FolderKind::Sent
                        | FolderKind::Junk
                        | FolderKind::Trash
                ) {
                    continue;
                }
                let list = match self.message_cache.get(&(account_id, f.id)) {
                    Some(l) => l.to_vec(),
                    None if f.kind == FolderKind::Inbox => self
                        .cache
                        .as_ref()
                        .map(|c| c.load_messages(account_id, &f.path, f.id))
                        .unwrap_or_default(),
                    None => Vec::new(),
                };
                for m in list {
                    if seen.insert((m.account_id, m.id)) {
                        rest.push((m, name.clone()));
                    }
                }
            }
        }
        rest.sort_by_key(|(m, _)| std::cmp::Reverse(m.timestamp));
        rest.truncate(500);
        out.extend(rest);
        out
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_drafts(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::NewFromTemplate => self.pick_template(None, &sender),
            AppMsg::PickTemplate { id } => self.pick_template(Some(id), &sender),

            AppMsg::TemplatePickNow => {
                if let Some((into, _)) = self.pending_template_pick.take() {
                    self.show_template_picker(into, &sender);
                }
            }

            AppMsg::TemplatePicked(picked) => match picked {
                Some(m) => self.edit_as_new(m, &sender),
                None => sender.input(AppMsg::Compose),
            },

            AppMsg::TemplateInsertPicked { id, template } => self.insert_template(id, template),

            AppMsg::SaveDraftMessage(out) => {
                let account_id = out.from_account_id;
                let Some((folder_id, path)) = self.drafts_target(account_id) else {
                    self.notice_error(i18n("No Drafts folder available for this account"));
                    return;
                };
                self.send_to(
                    account_id,
                    MailRequest::SaveDraft {
                        message: out,
                        folder_id,
                        path,
                        autosave: None,
                        template: false,
                    },
                );
            }

            AppMsg::SaveTemplateMessage(out) => {
                let account_id = out.from_account_id;
                // An edited template goes back to the folder it was kept in;
                // a new one, or one now sent from another account, to the
                // account's Templates folder, which saving makes if need be.
                let target = out
                    .draft_origin
                    .as_ref()
                    .filter(|o| o.account_id == account_id)
                    .map(|o| (o.folder_id, o.path.clone()))
                    .or_else(|| self.templates_target(account_id));
                let Some((folder_id, path)) = target else {
                    self.notice_error(i18n("No Templates folder available for this account"));
                    return;
                };
                self.send_to(
                    account_id,
                    MailRequest::SaveDraft {
                        message: out,
                        folder_id,
                        path,
                        autosave: None,
                        template: true,
                    },
                );
            }

            AppMsg::AutoSaveDraft { id, message } => {
                let account_id = message.from_account_id;
                match self.drafts_target(account_id) {
                    Some((folder_id, path)) => self.send_to(
                        account_id,
                        MailRequest::SaveDraft {
                            message,
                            folder_id,
                            path,
                            autosave: Some(id),
                            template: false,
                        },
                    ),
                    None => self.emit_to_composer(
                        id,
                        ComposeInput::Autosaved {
                            saved: false,
                            origin: None,
                        },
                    ),
                }
            }

            AppMsg::DiscardAutosave { id, origin } => {
                self.purge_draft(&origin);
                self.close_compose(id);
                self.message_list.emit(MessageListInput::ReclaimFocus);
            }

            AppMsg::DeleteDraft { id, origin } => {
                // The same move the list's trash button makes for a draft, so
                // it is undoable and the Drafts list updates in place. A draft
                // the list no longer holds (a stale window) is erased by uid.
                let cached = self
                    .message_cache
                    .get(&(origin.account_id, origin.folder_id))
                    .and_then(|msgs| msgs.iter().find(|m| m.uid == origin.uid).cloned());
                match cached {
                    Some(m) => self.move_to(m, FolderKind::Trash),
                    None => {
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
                                path: origin.path,
                            },
                        );
                    }
                }
                self.pending_draft = None;
                self.pending_draft_token = None;
                self.close_compose(id);
                self.message_list.emit(MessageListInput::ReclaimFocus);
            }

            AppMsg::DraftSaved {
                account_id,
                autosave,
                message_id,
            } => {
                // The Drafts folder reload already reflects the saved draft; an
                // explicit save's composer has closed. No notification (mirrors
                // silent send). An automatic save tells its composer where the
                // copy is now, so the next save replaces it (#340).
                let Some(id) = autosave else { return };
                let origin = message_id
                    .as_deref()
                    .and_then(|mid| self.find_draft(account_id, mid));
                if !self.composer_exists(id) {
                    // Sent, saved or closed while this save was on its way:
                    // the copy it left is not wanted.
                    if let Some(origin) = &origin {
                        self.purge_draft(origin);
                    }
                    return;
                }
                self.emit_to_composer(
                    id,
                    ComposeInput::Autosaved {
                        saved: message_id.is_some(),
                        origin,
                    },
                );
            }
            _ => unreachable!("not routed to on_drafts"),
        }
    }
}
