//! Smart folders (#417): saved searches over the index, shown as folders.

use std::collections::HashMap;

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::i18n::{i18n, i18n_f};
use crate::models::{FolderKind, Message};
use crate::ui::message_list::{MessageListInput, SearchScope};
use crate::ui::message_view::MessageViewInput;
use crate::ui::sidebar::SidebarInput;

use super::demo::demo_mode;
use super::model::AppModel;
use super::msg::AppMsg;

/// Most messages one smart folder (#417) lists per account.
pub(super) const SMART_VIEW_LIMIT: usize = 5000;

/// What a smart folder's conditions need besides the message itself, made
/// on the main thread and taken to the one that reads the index.
struct SmartContext {
    /// The tags' keywords, lower case.
    tags: Vec<String>,
    /// The tags themselves, for the Gmail labels that stand for them (#374).
    tag_list: Vec<config::Tag>,
    contacts: Option<std::sync::Arc<std::collections::HashSet<String>>>,
}

impl SmartContext {
    pub(super) fn matches(&self, folder: &config::SmartFolder, m: &Message) -> bool {
        let recipients = [m.to.as_str(), m.cc.as_str()]
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        folder.matches(&config::SmartInput {
            text: config::FilterInput {
                from_addr: &m.from_addr,
                from_name: &m.from_name,
                subject: &m.subject,
                recipients: &recipients,
                reply_to: &m.reply_to,
                body: if m.body.is_empty() {
                    &m.preview
                } else {
                    &m.body
                },
                body_hits: &[],
                contacts: self.contacts.as_deref(),
            },
            starred: m.starred,
            unread: m.unread,
            attachment: m.has_attachment,
            keywords: &m.tag_keywords(&self.tag_list),
            tags: &self.tags,
        })
    }
}

/// Where a smart folder looks: per account, the folders it is held to
/// (`None` for all) and the ones it leaves out.
pub(super) type SmartScope = Vec<(u32, Option<Vec<String>>, Vec<String>)>;

impl AppModel {
    pub(super) fn open_smart_folder(&self) -> Option<&config::SmartFolder> {
        let id = self.smart_view.as_ref()?;
        self.smart_folders.iter().find(|f| &f.id == id)
    }

    /// Switches to a view the index assembles, a tag (`tag_view`) or a
    /// smart folder (`smart_view`): no folder selected, the reader blank,
    /// the list reset to a colorized view whose rows cannot be restored,
    /// junked or edited as drafts. Recipients are named in a smart folder
    /// held to Sent, as Sent does; a tag view never is.
    pub(super) fn enter_index_view(
        &mut self,
        sel: crate::ui::sidebar::Sel,
        tag_view: Option<(Option<u32>, Option<String>)>,
        smart_view: Option<String>,
    ) {
        self.close_sidebar_peek();
        self.mirror_selection(sel);
        self.leave_gallery();
        self.showing_contacts = false;
        self.showing_outbox = false;
        self.unified = false;
        self.selected = None;
        self.tag_view = tag_view;
        self.smart_view = smart_view;
        self.blank_reader();
        self.message_list.emit(MessageListInput::SetSelected(None));
        self.message_list.emit(MessageListInput::ResetPaging);
        self.message_list
            .emit(MessageListInput::SetScope(SearchScope::ThisFolder));
        self.message_list.emit(MessageListInput::SetColorize(true));
        let sent = self
            .open_smart_folder()
            .is_some_and(|f| f.folder == Some(config::SmartFolderKind::Sent));
        self.message_list
            .emit(MessageListInput::SetShowRecipient(sent));
        self.message_list
            .emit(MessageListInput::SetRestorable(false));
        self.message_list.emit(MessageListInput::SetInJunk(false));
        self.message_list.emit(MessageListInput::SetInDrafts(false));
        self.message_view
            .emit(MessageViewInput::SetDraftsView(false));
    }

    /// Where a smart folder (#417) looks, per account: the folders it is
    /// held to (`None` for all) and the ones it leaves out. One account
    /// when it names one, else every account in the unified section; one
    /// kind of folder when it names one, else all but Trash and Junk.
    pub(super) fn smart_scope(&self, f: &config::SmartFolder) -> SmartScope {
        self.accounts
            .iter()
            .filter(|a| {
                if f.account_email.is_empty() {
                    self.in_unified(a.id)
                } else {
                    a.email.eq_ignore_ascii_case(&f.account_email)
                }
            })
            .map(|a| {
                let folders = self.folders.get(&a.id).map(Vec::as_slice).unwrap_or(&[]);
                let paths_of = |kinds: &[FolderKind]| -> Vec<String> {
                    folders
                        .iter()
                        .filter(|x| kinds.contains(&x.kind))
                        .map(|x| x.path.clone())
                        .collect()
                };
                match f.folder {
                    Some(k) => (a.id, Some(paths_of(&[k.kind()])), Vec::new()),
                    None => (a.id, None, paths_of(&[FolderKind::Trash, FolderKind::Junk])),
                }
            })
            .collect()
    }

    /// What a smart folder's matcher needs from here, to take to a thread:
    /// the tags' keywords, and the address books when a condition asks.
    fn smart_context(&self, f: &config::SmartFolder) -> SmartContext {
        SmartContext {
            tags: self.tags.iter().map(|t| t.keyword.to_lowercase()).collect(),
            tag_list: self.tags.clone(),
            contacts: if f.needs_contacts() {
                self.saved_addresses.get()
            } else {
                None
            },
        }
    }

    /// Lists a tag or smart folder view's rows as last assembled, or shows
    /// the list loading while there are none yet.
    pub(super) fn show_view_rows(&self, cached: Option<&Vec<Message>>) {
        match cached {
            Some(cached) => self.message_list.emit(MessageListInput::SetMessages {
                messages: cached.clone(),
            }),
            None => self.message_list.emit(MessageListInput::SetLoading),
        }
    }

    /// Show the open smart folder from its last result at once (a loading
    /// list if it never ran); [`refresh_smart_view`] brings it up to date.
    pub(super) fn show_smart_view(&self) {
        let Some(id) = self.smart_view.as_ref() else {
            return;
        };
        self.show_view_rows(self.smart_view_cache.get(id));
    }

    /// Read the open smart folder's rows from the on-disk index off the main
    /// thread, answering with [`AppMsg::SmartViewLoaded`]. The demo's mail
    /// never reaches the database, so its view is matched here from the
    /// folders loaded so far.
    pub(super) fn refresh_smart_view(&mut self, sender: &ComponentSender<Self>) {
        let Some(folder) = self.open_smart_folder().cloned() else {
            return;
        };
        if demo_mode() && self.config.is_empty() {
            let out = self.assemble_smart_view(&folder, Vec::new());
            self.smart_view_cache.insert(folder.id.clone(), out.clone());
            self.message_list
                .emit(MessageListInput::SetMessages { messages: out });
            return;
        }
        if self.smart_view_loading {
            self.smart_view_dirty = true;
            return;
        }
        self.smart_view_loading = true;
        let scope = self.smart_scope(&folder);
        let ctx = self.smart_context(&folder);
        let s = sender.clone();
        std::thread::spawn(move || {
            let mut rows: Vec<(u32, String, Message)> = Vec::new();
            if let Ok(cache) = crate::cache::Cache::open() {
                for (account_id, include, exclude) in scope {
                    let found = cache.smart_scan(
                        account_id,
                        include.as_deref(),
                        &exclude,
                        folder.prefilter(),
                        SMART_VIEW_LIMIT,
                        |m| ctx.matches(&folder, m),
                    );
                    rows.extend(found.into_iter().map(|(path, m)| (account_id, path, m)));
                }
            }
            s.input(AppMsg::SmartViewLoaded {
                id: folder.id,
                rows,
            });
        });
    }

    /// Newest first, and one row per message filed in several folders
    /// (Gmail's labels): the Inbox copy where there is one. Messages with
    /// no Message-ID are all kept.
    pub(super) fn newest_once(&self, out: &mut Vec<Message>) {
        let inbox_first =
            |m: &Message| self.folder_kind(m.account_id, m.folder_id) != Some(FolderKind::Inbox);
        out.sort_by(|a, b| {
            b.timestamp
                .cmp(&a.timestamp)
                .then_with(|| inbox_first(a).cmp(&inbox_first(b)))
        });
        let mut seen: std::collections::HashSet<(u32, String)> = std::collections::HashSet::new();
        out.retain(|m| {
            m.message_id.is_empty() || seen.insert((m.account_id, m.message_id.clone()))
        });
    }

    /// A smart folder from its rows: newest first, Gmail's per-label copies
    /// of one message as one row, the inbox copy where there is one.
    fn assemble_smart_view(
        &self,
        folder: &config::SmartFolder,
        rows: Vec<(u32, String, Message)>,
    ) -> Vec<Message> {
        let mut out: Vec<Message> = Vec::new();
        for (account_id, path, mut m) in rows {
            let Some(f) = self.folder_by_path(account_id, &path) else {
                continue;
            };
            m.folder_id = f.id;
            out.push(m);
        }
        if out.is_empty() && demo_mode() && self.config.is_empty() {
            let ctx = self.smart_context(folder);
            for (account_id, include, exclude) in self.smart_scope(folder) {
                for f in self.folders.get(&account_id).into_iter().flatten() {
                    let looked_in = include.as_ref().is_none_or(|p| p.contains(&f.path))
                        && !exclude.contains(&f.path);
                    if !looked_in {
                        continue;
                    }
                    if let Some(messages) = self.message_cache.get(&(account_id, f.id)) {
                        out.extend(messages.iter().filter(|m| ctx.matches(folder, m)).cloned());
                    }
                }
            }
        }
        self.newest_once(&mut out);
        out
    }

    /// Count the smart folders' unread mail a few seconds from now, once for
    /// however many syncs land meanwhile.
    pub(super) fn schedule_smart_counts(&mut self, sender: &ComponentSender<Self>) {
        if self.smart_folders.is_empty() || self.smart_counts_due {
            return;
        }
        self.smart_counts_due = true;
        let s = sender.clone();
        gtk::glib::timeout_add_seconds_local_once(3, move || s.input(AppMsg::CountSmartFolders));
    }

    /// Each smart folder's unread mail for its sidebar row, read off the
    /// main thread: only the unread rows are looked at.
    fn count_smart_folders(&mut self, sender: &ComponentSender<Self>) {
        if self.smart_counts_running {
            // A count in flight answers for what it read; this one comes
            // after it.
            self.schedule_smart_counts(sender);
            return;
        }
        let demo = demo_mode() && self.config.is_empty();
        let jobs: Vec<(config::SmartFolder, SmartScope, SmartContext)> = self
            .smart_folders
            .iter()
            .map(|f| (f.clone(), self.smart_scope(f), self.smart_context(f)))
            .collect();
        if demo {
            let counts = jobs
                .iter()
                .map(|(f, _, _)| {
                    let n = self
                        .assemble_smart_view(f, Vec::new())
                        .iter()
                        .filter(|m| m.unread)
                        .count() as u32;
                    (f.id.clone(), n)
                })
                .collect();
            sender.input(AppMsg::SmartCounts(counts));
            return;
        }
        self.smart_counts_running = true;
        let s = sender.clone();
        std::thread::spawn(move || {
            let mut counts: HashMap<String, u32> = HashMap::new();
            if let Ok(cache) = crate::cache::Cache::open() {
                for (folder, scope, ctx) in jobs {
                    let mut pre = folder.prefilter();
                    // Read mail is never counted, so a folder of read mail
                    // counts nothing.
                    if pre.unread == Some(false) {
                        counts.insert(folder.id, 0);
                        continue;
                    }
                    pre.unread = Some(true);
                    let mut seen: std::collections::HashSet<(u32, String)> =
                        std::collections::HashSet::new();
                    let mut n = 0u32;
                    for (account_id, include, exclude) in scope {
                        for (_, m) in cache.smart_scan(
                            account_id,
                            include.as_deref(),
                            &exclude,
                            pre,
                            usize::MAX,
                            |m| ctx.matches(&folder, m),
                        ) {
                            if m.message_id.is_empty() || seen.insert((account_id, m.message_id)) {
                                n += 1;
                            }
                        }
                    }
                    counts.insert(folder.id, n);
                }
            }
            s.input(AppMsg::SmartCounts(counts));
        });
    }

    /// Delete a smart folder from the sidebar's menu: it is only a search,
    /// so nothing else goes with it, but it still asks.
    pub(super) fn confirm_delete_smart_folder(&self, id: String, sender: &ComponentSender<Self>) {
        let Some(folder) = self.smart_folders.iter().find(|f| f.id == id) else {
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some(&i18n_f("Delete “{name}”?", &[("name", &folder.name)])),
            Some(&i18n(
                "The smart folder goes from the sidebar. The mail it shows stays where it is.",
            )),
        );
        dialog.add_response("cancel", &i18n("Cancel"));
        dialog.add_response("delete", &i18n("Delete"));
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        let s = sender.clone();
        let rest: Vec<config::SmartFolder> = self
            .smart_folders
            .iter()
            .filter(|f| f.id != id)
            .cloned()
            .collect();
        dialog.connect_response(None, move |_, resp| {
            if resp == "delete" {
                s.input(AppMsg::SetSmartFolders(rest.clone()));
            }
        });
        dialog.present(Some(&self.window));
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_smart(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::SmartSelected(id) => {
                if !self.smart_folders.iter().any(|f| f.id == id) {
                    return;
                }
                self.enter_index_view(crate::ui::sidebar::Sel::Smart(id.clone()), None, Some(id));
                self.show_smart_view();
                self.refresh_smart_view(&sender);
                self.sync_view_flags();
                self.push_index_complete();
            }

            AppMsg::FlagsSynced { account_id, paths } => {
                self.on_flags_synced(account_id, &paths, &sender);
            }

            AppMsg::SmartViewLoaded { id, rows } => {
                self.smart_view_loading = false;
                if let Some(folder) = self.smart_folders.iter().find(|f| f.id == id).cloned() {
                    let out = self.assemble_smart_view(&folder, rows);
                    let changed = self.smart_view_cache.get(&id) != Some(&out);
                    if changed {
                        self.smart_view_cache.insert(id.clone(), out.clone());
                    }
                    if self.smart_view.as_ref() == Some(&id) && changed {
                        self.message_list
                            .emit(MessageListInput::SetMessages { messages: out });
                    }
                }
                if std::mem::take(&mut self.smart_view_dirty) {
                    self.refresh_smart_view(&sender);
                }
            }

            AppMsg::CountSmartFolders => {
                self.smart_counts_due = false;
                self.count_smart_folders(&sender);
            }

            AppMsg::SmartCounts(counts) => {
                self.smart_counts_running = false;
                self.sidebars_emit(SidebarInput::SetSmartUnread(counts));
            }

            AppMsg::SetSmartFolders(folders) => {
                config::save_smart_folders(&folders);
                // What each one keeps may have changed with its conditions.
                self.smart_view_cache.clear();
                self.smart_folders = folders;
                if let Some(acc) = &self.accounts_win {
                    acc.emit(crate::ui::accounts::AccountsInput::SetSmartFolders(
                        self.smart_folders.clone(),
                    ));
                }
                self.rebuild_sidebar();
                if self
                    .smart_view
                    .as_ref()
                    .is_some_and(|id| self.smart_folders.iter().any(|f| &f.id == id))
                {
                    self.show_smart_view();
                    self.refresh_smart_view(&sender);
                }
                self.schedule_smart_counts(&sender);
            }
            _ => unreachable!("not routed to on_smart"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::msg;

    #[test]
    fn smart_folders_read_recipients_and_fall_back_to_the_preview() {
        use crate::config::{FilterMatch, SmartCondition, SmartField, SmartFolder};
        let ctx = SmartContext {
            tags: Vec::new(),
            tag_list: Vec::new(),
            contacts: None,
        };
        let folder = |field, value: &str| SmartFolder {
            id: "s".into(),
            name: "S".into(),
            account_email: String::new(),
            folder: None,
            any: false,
            conditions: vec![SmartCondition {
                field,
                matcher: FilterMatch::Contains,
                value: value.into(),
            }],
        };
        let mut m = msg(1, 1, 1);
        m.cc = "carol@x".into();
        m.preview = "quarterly numbers".into();
        assert!(ctx.matches(&folder(SmartField::Recipients, "carol"), &m));
        assert!(ctx.matches(&folder(SmartField::Body, "quarterly"), &m));
        m.body = "something else".into();
        assert!(!ctx.matches(&folder(SmartField::Body, "quarterly"), &m));
    }
}
