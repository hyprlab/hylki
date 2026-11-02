//! The folder context menu's dialogs: new, rename, delete, empty.

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::{i18n, i18n_f};
use crate::models::FolderKind;
use crate::ui::sidebar::CtxAction;
use crate::worker::MailRequest;

use super::model::AppModel;
use super::msg::AppMsg;
use super::sidebar::folder_key;
use super::undo::UndoStep;
use crate::ui::dialog::confirm_destructive;

impl AppModel {
    /// Prompt for a new custom folder name and create it under `account_id`,
    /// inside `parent` when there is one (#554).
    #[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
    fn prompt_new_folder(&self, account_id: u32, parent: String, sender: &ComponentSender<Self>) {
        let body = match self.folder_by_path(account_id, &parent) {
            Some(f) if !parent.is_empty() => {
                i18n_f("Create a folder inside {name}.", &[("name", &f.name)])
            }
            _ => i18n("Create a new folder for this account."),
        };
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(i18n("New Folder").as_str()),
            Some(body.as_str()),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("ok", &i18n("Create"));
        dialog.set_default_response(Some("ok"));
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some(i18n("Folder name").as_str()));
        entry.set_activates_default(true);
        dialog.set_extra_child(Some(&entry));
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "ok" {
                let name = entry.text().to_string();
                if !name.trim().is_empty() {
                    s.input(AppMsg::CreateFolder {
                        account_id,
                        parent: parent.clone(),
                        name,
                    });
                }
            }
        });
        dialog.present();
    }

    /// Prompt for a folder's new name and rename it in place.
    #[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
    fn prompt_rename_folder(
        &self,
        account_id: u32,
        name: String,
        path: String,
        sender: &ComponentSender<Self>,
    ) {
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(&format!("Rename {name:?}")),
            Some(i18n("Sub-folders keep their place under the new name.").as_str()),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("ok", &i18n("Rename"));
        dialog.set_default_response(Some("ok"));
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        let entry = gtk::Entry::new();
        entry.set_text(&name);
        entry.select_region(0, -1);
        entry.set_activates_default(true);
        dialog.set_extra_child(Some(&entry));
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "ok" {
                let new_name = entry.text().to_string();
                if !new_name.trim().is_empty() {
                    s.input(AppMsg::RenameFolderTo {
                        account_id,
                        path: path.clone(),
                        new_name,
                    });
                }
            }
        });
        dialog.present();
    }

    /// Rename a custom folder's leaf in place: same parent, new (encoded)
    /// name, the same optimistic machinery as a drag-and-drop move.
    fn rename_folder_to(&mut self, account_id: u32, path: String, new_name: String) {
        let Some(h) = self.hierarchy_for_change(account_id) else {
            return;
        };
        let leaf = match h.leaf_for(&new_name) {
            Ok(leaf) => leaf,
            Err(why) => return self.notice_error(why),
        };
        let Some(new_path) = h.child(&h.parent(&path), &leaf) else {
            return;
        };
        if new_path == path {
            return;
        }
        if self
            .folders
            .get(&account_id)
            .is_some_and(|fs| fs.iter().any(|f| f.path == new_path))
        {
            self.notice_error(i18n_f(
                "A folder named {new_name} is already there.",
                &[("new_name", &format!("{:?}", new_name.trim()))],
            ));
            return;
        }
        self.apply_folder_rename(account_id, path, new_path, Some(i18n("Rename Folder")));
    }

    /// Confirm deleting a custom folder (contents moved to Trash). Deliberately
    /// outside the undo history (#200): the mailbox itself is gone from the
    /// server, so the dialog says as much instead of promising a way back.
    #[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
    fn confirm_delete_folder(
        &self,
        account_id: u32,
        name: String,
        path: String,
        sender: &ComponentSender<Self>,
    ) {
        let dialog = adw::MessageDialog::new(
            Some(&self.window),
            Some(&format!("Delete “{name}”?")),
            Some(
                i18n(
                    "Its messages are moved to Trash and the folder is removed from the \
                      server. This cannot be undone.",
                )
                .as_str(),
            ),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("delete", &i18n("Delete"));
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let s = sender.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "delete" {
                s.input(AppMsg::DeleteFolder {
                    account_id,
                    path: path.clone(),
                });
            }
        });
        dialog.present();
    }

    /// Empty Trash / Empty Junk (#152): there is no undo, so always ask.
    fn confirm_empty_folder(
        &self,
        account_id: u32,
        folder_id: u32,
        name: String,
        path: String,
        sender: &ComponentSender<Self>,
    ) {
        let s = sender.clone();
        confirm_destructive(
            Some(self.window.upcast_ref()),
            &i18n_f("Empty “{name}”?", &[("name", &name)]),
            &i18n("Every message in it will be erased from the server. This can’t be undone."),
            "empty",
            &i18n("Empty"),
            move || {
                s.input(AppMsg::EmptyFolder {
                    account_id,
                    folder_id,
                    path,
                })
            },
        );
    }

    /// Empty every unified account's Trash or Junk (#413). One folder asks
    /// the way its own row does; more name each account they belong to.
    fn confirm_empty_all(&self, kind: FolderKind, sender: &ComponentSender<Self>) {
        let targets: Vec<(u32, u32, String, String, String)> = self
            .accounts
            .iter()
            .filter(|a| self.in_unified(a.id))
            .filter_map(|a| {
                let f = self.folder_of_kind(a.id, kind)?;
                Some((a.id, f.id, f.name.clone(), f.path.clone(), a.email.clone()))
            })
            .collect();
        match targets.as_slice() {
            [] => {}
            [(account_id, folder_id, name, path, _)] => {
                self.confirm_empty_folder(
                    *account_id,
                    *folder_id,
                    name.clone(),
                    path.clone(),
                    sender,
                );
            }
            _ => {
                let heading = if kind == FolderKind::Trash {
                    i18n("Empty Trash in Every Account?")
                } else {
                    i18n("Empty Junk in Every Account?")
                };
                let accounts: Vec<&str> = targets.iter().map(|t| t.4.as_str()).collect();
                let body = format!(
                    "{}\n\n{}",
                    i18n("Every message in them will be erased from the server. This can’t be undone."),
                    accounts.join("\n")
                );
                let s = sender.clone();
                confirm_destructive(
                    Some(self.window.upcast_ref()),
                    &heading,
                    &body,
                    "empty",
                    &i18n("Empty"),
                    move || {
                        for (account_id, folder_id, _, path, _) in targets {
                            s.input(AppMsg::EmptyFolder {
                                account_id,
                                folder_id,
                                path,
                            });
                        }
                    },
                );
            }
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_folder_menu(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::SidebarContext(action) => self.on_sidebar_context(action, sender),

            AppMsg::DropMoveMessages {
                dest_account,
                dest,
                items,
            } => {
                self.drop_move(dest_account, dest, items);
            }

            AppMsg::CreateFolder {
                account_id,
                parent,
                name,
            } => {
                let Some(h) = self.hierarchy_for_change(account_id) else {
                    return;
                };
                // Encoded as the account names folders: modified UTF-7 on
                // IMAP (issue #1, in the other direction), the delimiter
                // refused where a name can't hold it (#554).
                let leaf = match h.leaf_for(&name) {
                    Ok(leaf) => leaf,
                    Err(why) => return self.notice_error(why),
                };
                if let Some(f) = self
                    .folder_by_path(account_id, &parent)
                    .filter(|f| f.childless)
                {
                    return self.notice_error(super::folders::cannot_hold(&f.name));
                }
                let Some(path) = h.child(&parent, &leaf) else {
                    return self.notice_error(if parent.is_empty() {
                        i18n("This account can't have folders at the top level.")
                    } else {
                        i18n("Folders on this account can't hold other folders.")
                    });
                };
                if self
                    .folders
                    .get(&account_id)
                    .is_some_and(|fs| fs.iter().any(|f| f.path == path))
                {
                    return self.notice_error(i18n_f(
                        "A folder named {new_name} is already there.",
                        &[("new_name", &format!("{:?}", name.trim()))],
                    ));
                }
                // Its parent opens, or the new folder would be made out of
                // sight.
                if !parent.is_empty() {
                    self.expand_folder(account_id, &parent);
                }
                self.create_folder(account_id, path.clone());
                self.push_undo(
                    account_id,
                    i18n("Create Folder"),
                    UndoStep::DeleteFolder { path },
                );
            }

            AppMsg::MoveFolder {
                account_id,
                path,
                dest,
            } => {
                let _ = self.move_folder(account_id, path, dest);
            }

            AppMsg::RenameFolderTo {
                account_id,
                path,
                new_name,
            } => {
                self.rename_folder_to(account_id, path, new_name);
            }

            AppMsg::EmptyFolder {
                account_id,
                folder_id,
                path,
            } => {
                // Gone locally at once; the worker's empty message list and
                // zero count confirm it, or an error says why not.
                self.message_cache.remove(&(account_id, folder_id));
                self.folder_unread.insert((account_id, folder_id), 0);
                if self
                    .selected
                    .as_ref()
                    .is_some_and(|s| s.account_id == account_id && s.folder_id == folder_id)
                {
                    self.blank_for_folder_change();
                }
                self.push_unread_counts();
                self.send_to(account_id, MailRequest::EmptyFolder { folder_id, path });
            }

            // Not recorded: deleting a folder cannot be taken back (#200).
            // The confirmation says so before it happens.
            AppMsg::DeleteFolder { account_id, path } => {
                self.delete_folder(account_id, path);
            }
            _ => unreachable!("not routed to on_folder_menu"),
        }
    }

    /// What a sidebar row's context menu asked for.
    fn on_sidebar_context(&mut self, action: CtxAction, sender: ComponentSender<Self>) {
        match action {
            CtxAction::MarkFolderRead {
                account_id,
                folder_id,
            } => {
                self.mark_folder_read(account_id, folder_id);
            }
            CtxAction::RefreshFolder {
                account_id,
                folder_id,
            } => {
                if let Some(path) = self.folder_path(account_id, folder_id) {
                    self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
                }
            }
            CtxAction::MarkAllInboxesRead => {
                // The inboxes the row merges, as its chip counts them.
                let inboxes: Vec<(u32, u32)> = self
                    .accounts
                    .iter()
                    .filter(|a| self.in_unified(a.id))
                    .filter_map(|a| self.inbox_of(a.id).map(|f| (a.id, f.id)))
                    .collect();
                for (account_id, folder_id) in inboxes {
                    self.mark_folder_read(account_id, folder_id);
                }
            }
            CtxAction::RefreshAllInboxes => {
                let reqs: Vec<(u32, u32, String)> = self
                    .accounts
                    .iter()
                    .filter(|a| self.in_unified(a.id))
                    .filter_map(|a| self.inbox_of(a.id).map(|f| (a.id, f.id, f.path.clone())))
                    .collect();
                for (account_id, folder_id, path) in reqs {
                    self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
                }
            }
            CtxAction::EmptyAll(kind) => {
                self.confirm_empty_all(kind, &sender);
            }
            CtxAction::ApplyFilters {
                account_id,
                folder_id,
            } => {
                self.start_filter_run(vec![(account_id, folder_id)], &sender);
            }
            CtxAction::EditFilter { account_id, path } => {
                // The first rule filing into this folder, in the
                // Filters page's editor.
                let email = self.email_of(account_id);
                let index = email.and_then(|email| {
                    self.filters.iter().position(|r| {
                        r.account_email.eq_ignore_ascii_case(&email) && r.dest_path == path
                    })
                });
                if let Some(i) = index {
                    self.open_settings_at(
                        &sender,
                        "filters",
                        crate::ui::accounts::AccountsInput::EditFilter(i),
                    );
                }
            }
            CtxAction::NewSmartFolder => {
                self.open_settings_at(
                    &sender,
                    "smart",
                    crate::ui::accounts::AccountsInput::AddSmartFolder,
                );
            }
            CtxAction::EditSmartFolder(id) => {
                if let Some(i) = self.smart_folders.iter().position(|f| f.id == id) {
                    self.open_settings_at(
                        &sender,
                        "smart",
                        crate::ui::accounts::AccountsInput::EditSmartFolder(i),
                    );
                }
            }
            CtxAction::DeleteSmartFolder(id) => {
                self.confirm_delete_smart_folder(id, &sender);
            }
            CtxAction::EditTag(keyword) => {
                if let Some(i) = self.tags.iter().position(|t| t.keyword == keyword) {
                    self.open_settings_at(
                        &sender,
                        "tags",
                        crate::ui::accounts::AccountsInput::EditTag(i),
                    );
                }
            }
            CtxAction::OpenAccountSettings(account_id) => {
                // Straight to this account's editor, not the accounts list.
                let email = self
                    .accounts
                    .iter()
                    .find(|a| a.id == account_id)
                    .map(|a| a.email.clone());
                self.open_settings_window(&sender, true, false);
                if let (Some(email), Some(acc)) = (email, &self.accounts_win) {
                    acc.emit(crate::ui::accounts::AccountsInput::EditAccountByEmail(
                        email,
                    ));
                }
            }
            CtxAction::RemoveAccount(account_id) => {
                self.confirm_remove_account(account_id, &sender);
            }
            CtxAction::NewFolder { account_id, parent } => {
                self.prompt_new_folder(account_id, parent, &sender);
            }
            CtxAction::DeleteFolder {
                account_id,
                name,
                path,
            } => {
                self.confirm_delete_folder(account_id, name, path, &sender);
            }
            CtxAction::RenameFolder {
                account_id,
                name,
                path,
            } => {
                self.prompt_rename_folder(account_id, name, path, &sender);
            }
            CtxAction::EmptyFolder {
                account_id,
                folder_id,
                name,
                path,
            } => {
                self.confirm_empty_folder(account_id, folder_id, name, path, &sender);
            }
            CtxAction::HideFolder { account_id, path } => {
                self.hide_folders(account_id, vec![path]);
            }
            CtxAction::AddFavorite { account_id, path } => {
                if let Some(email) = self.email_of(account_id) {
                    let key = folder_key(&email, &path);
                    if !self.favorites.contains(&key) {
                        // The first favorite shows its list open.
                        if self.favorites.is_empty() {
                            self.favorites_expanded = true;
                        }
                        self.favorites.push(key);
                        self.save_sidebar_state();
                        self.rebuild_sidebar();
                    }
                }
            }
            CtxAction::RemoveFavorite { account_id, path } => {
                if let Some(email) = self.email_of(account_id) {
                    let key = folder_key(&email, &path);
                    self.favorites.retain(|k| *k != key);
                    self.save_sidebar_state();
                    self.rebuild_sidebar();
                }
            }
            CtxAction::ResetFolderOrder(account_id) => {
                if let Some(email) = self.email_of(account_id) {
                    let prefix = format!("{email}\t");
                    self.folder_order.retain(|k| !k.starts_with(&prefix));
                    self.save_sidebar_state();
                    self.rebuild_sidebar();
                }
            }
        }
    }
}
