//! Tags: their colors, the tag views, and the tag finder.

use relm4::prelude::*;

use crate::config;
use crate::models::{Folder, FolderKind, KeywordFinding, Message};
use crate::ui::message_list::MessageListInput;
use crate::ui::message_view::MessageViewInput;
use crate::ui::message_window::MessageWindowInput;
use crate::ui::preferences::PrefInput;
use crate::ui::sidebar::TagUnread;
use crate::worker::MailRequest;

use super::demo::demo_mode;
use super::model::AppModel;
use super::msg::AppMsg;

/// What one pass over the tag rows' unread chips reads (#717), gathered on
/// the main thread: every tag with the name a Gmail label goes by, and each
/// account in turn.
pub(super) struct TagCountJob {
    tags: Vec<(String, String)>,
    accounts: Vec<TagCountAccount>,
}

/// One account of a [`TagCountJob`].
struct TagCountAccount {
    id: u32,
    /// The folders whose mail a tag view lists: all but Trash and Junk,
    /// which keep their tags but stay out.
    listed: std::collections::HashSet<String>,
    /// Whether the unified Tags rows cover the account.
    unified: bool,
    /// On Gmail, the label folder each tag stands for, by keyword.
    labels: std::collections::HashMap<String, String>,
}

impl TagCountJob {
    /// Each tag's unread mail per account and for the unified rows, counted
    /// as its view lists it: a message Gmail files under several labels is
    /// one.
    pub(super) fn run(&self, cache: &crate::cache::Cache) -> TagUnread {
        let mut out = TagUnread::new();
        for (kw, _) in &self.tags {
            out.insert((None, kw.clone()), 0);
        }
        for a in &self.accounts {
            let marks: Vec<crate::cache::TagMarks> = cache
                .unread_tag_marks(a.id)
                .into_iter()
                .filter(|m| a.listed.contains(&m.path))
                .collect();
            for (kw, name) in &self.tags {
                let label = a.labels.get(kw).map(String::as_str);
                let n = count_tagged(&marks, kw, name, label);
                out.insert((Some(a.id), kw.clone()), n);
                if a.unified {
                    *out.entry((None, kw.clone())).or_default() += n;
                }
            }
        }
        out
    }
}

/// The messages among `marks` carrying the tag, or filed in the Gmail label
/// it stands for, each Message-ID once.
fn count_tagged(
    marks: &[crate::cache::TagMarks],
    keyword: &str,
    name: &str,
    label: Option<&str>,
) -> u32 {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    marks
        .iter()
        .filter(|m| m.carries(keyword, name) || label == Some(m.path.as_str()))
        .filter(|m| m.message_id.is_empty() || seen.insert(m.message_id.as_str()))
        .count() as u32
}

/// A running tag-finder scan (see `AppMsg::FindTags`).
pub(super) struct TagScan {
    gen: u32,
    remaining: usize,
    found: Vec<(u32, Vec<KeywordFinding>)>,
}

impl AppModel {
    /// With a tag view open, ask the accounts in its scope to bring their
    /// cached keywords in line with the server (#166). An account scanned
    /// within the last few seconds is left alone: reopening a view, or a
    /// refresh right after opening one, must not walk every folder twice.
    pub(super) fn sync_tag_keywords(&mut self) {
        const PAUSE: std::time::Duration = std::time::Duration::from_secs(15);
        let Some((scope, _)) = self.tag_view.clone() else {
            return;
        };
        let keywords: Vec<String> = self.tags.iter().map(|t| t.keyword.clone()).collect();
        if keywords.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        let ids: Vec<u32> = self
            .accounts
            .iter()
            .map(|a| a.id)
            .filter(|id| self.in_tag_scope(scope, *id))
            .collect();
        for id in ids {
            if self
                .keyword_sync_at
                .get(&id)
                .is_some_and(|t| now.duration_since(*t) < PAUSE)
            {
                continue;
            }
            self.keyword_sync_at.insert(id, now);
            self.send_to(
                id,
                MailRequest::RefreshKeywords {
                    keywords: keywords.clone(),
                },
            );
        }
    }

    /// Whether a tag view covers an account: its own, or for the unified
    /// Tags row every account merged into the unified section (#267), all
    /// of them when none is (#714).
    fn in_tag_scope(&self, scope: Option<u32>, account_id: u32) -> bool {
        scope.map_or_else(
            || self.in_every_account_view(account_id),
            |id| id == account_id,
        )
    }

    /// What the next count of the tag rows' unread mail reads.
    pub(super) fn tag_count_job(&self) -> TagCountJob {
        let tags = self
            .tags
            .iter()
            .map(|t| (t.keyword.clone(), t.name.clone()))
            .collect();
        let accounts = self
            .accounts
            .iter()
            .filter_map(|a| {
                let folders = self.folders.get(&a.id)?;
                let listed = folders
                    .iter()
                    .filter(|f| !matches!(f.kind, FolderKind::Trash | FolderKind::Junk))
                    .map(|f| f.path.clone())
                    .collect();
                let labels = self
                    .tag_label_folders(a.id)
                    .into_iter()
                    .map(|(kw, f)| (kw, f.path.clone()))
                    .collect();
                Some(TagCountAccount {
                    id: a.id,
                    listed,
                    unified: self.in_tag_scope(None, a.id),
                    labels,
                })
            })
            .collect();
        TagCountJob { tags, accounts }
    }

    /// On a Gmail account, the label each tag stands for (#374), by the
    /// tag's keyword: the folder named like its keyword or its name. Mail
    /// labelled in Gmail itself carries the tag, but reaches the cache only
    /// through that folder, whose rows leave out the label they are read
    /// under (#717).
    pub(super) fn tag_label_folders(&self, account_id: u32) -> Vec<(String, &Folder)> {
        let Some(folders) = self.folders.get(&account_id) else {
            return Vec::new();
        };
        if self.tags.is_empty() || !self.gmail_labels(account_id) {
            return Vec::new();
        }
        self.tags
            .iter()
            .filter_map(|t| {
                let names = [&t.keyword, &t.name].map(|n| crate::mail::mutf7::encode(n));
                let f = folders.iter().find(|f| {
                    f.kind == FolderKind::Custom
                        && names.iter().any(|n| n.eq_ignore_ascii_case(&f.path))
                })?;
                Some((t.keyword.clone(), f))
            })
            .collect()
    }

    /// The demo's tag counts, from the views it assembles out of the folders
    /// loaded so far: its mail never reaches the database.
    pub(super) fn demo_tag_counts(&self) -> TagUnread {
        let scopes = std::iter::once(None).chain(self.accounts.iter().map(|a| Some(a.id)));
        let mut out = TagUnread::new();
        for scope in scopes {
            for t in &self.tags {
                let key = (scope, Some(t.keyword.clone()));
                let n = self
                    .assemble_tag_view(&key, Vec::new())
                    .iter()
                    .filter(|m| m.unread)
                    .count() as u32;
                out.insert((scope, t.keyword.clone()), n);
            }
        }
        out
    }

    /// The keywords a tag view answers: one, or — the unified Tags row —
    /// every configured tag.
    fn tag_view_keywords(&self, kw: &Option<String>) -> Vec<String> {
        match kw {
            Some(k) => vec![k.clone()],
            None => self.tags.iter().map(|t| t.keyword.clone()).collect(),
        }
    }

    /// Show the open tag view from its last result at once (an empty
    /// loading list if it never ran); [`refresh_tag_view`] brings it up to
    /// date behind.
    pub(super) fn show_tag_view(&self) {
        let Some(key) = self.tag_view.as_ref() else {
            return;
        };
        self.show_view_rows(self.tag_view_cache.get(key));
    }

    /// Read the open tag view's rows from the on-disk index off the main
    /// thread (reading tens of thousands of rows per account is what made
    /// a tag click pause), answering with [`AppMsg::TagViewLoaded`]. The
    /// demo's mail never reaches the database, so its view is assembled
    /// here from the folders loaded so far.
    pub(super) fn refresh_tag_view(&mut self, sender: &ComponentSender<Self>) {
        let Some(key) = self.tag_view.clone() else {
            return;
        };
        if demo_mode() && self.config.is_empty() {
            let out = self.assemble_tag_view(&key, Vec::new());
            self.tag_view_cache.insert(key, out.clone());
            self.message_list
                .emit(MessageListInput::SetMessages { messages: out });
            return;
        }
        if self.tag_view_loading {
            self.tag_view_dirty = true;
            return;
        }
        self.tag_view_loading = true;
        let (scope, kw) = key.clone();
        // Each keyword with its tag's name, which a Gmail label goes by.
        let keywords: Vec<(String, String)> = self
            .tag_view_keywords(&kw)
            .into_iter()
            .map(|k| {
                let name = self
                    .tags
                    .iter()
                    .find(|t| t.keyword.eq_ignore_ascii_case(&k))
                    .map(|t| t.name.clone());
                let name = name.unwrap_or_else(|| k.clone());
                (k, name)
            })
            .collect();
        // Each account with the Gmail labels its keywords stand for.
        let accounts: Vec<(u32, Vec<String>)> = self
            .accounts
            .iter()
            .map(|a| a.id)
            .filter(|id| self.in_tag_scope(scope, *id))
            .map(|id| {
                let labels = self
                    .tag_label_folders(id)
                    .into_iter()
                    .filter(|(kw, _)| keywords.iter().any(|(k, _)| k == kw))
                    .map(|(_, f)| f.path.clone())
                    .collect();
                (id, labels)
            })
            .collect();
        let s = sender.clone();
        std::thread::spawn(move || {
            let mut rows: Vec<(u32, String, Message)> = Vec::new();
            if let Ok(cache) = crate::cache::Cache::open() {
                for (account_id, labels) in accounts {
                    for (kw, name) in &keywords {
                        for (path, m) in cache.messages_with_keyword(account_id, kw, name) {
                            rows.push((account_id, path, m));
                        }
                    }
                    for label in &labels {
                        for (path, m) in cache.messages_in_label(account_id, label) {
                            rows.push((account_id, path, m));
                        }
                    }
                }
            }
            s.input(AppMsg::TagViewLoaded { key, rows });
        });
    }

    /// The tag view (#71) from its rows: every message of every account in
    /// scope carrying the tag, newest first. Trash and Junk keep their tags
    /// but stay out; Gmail's per-label copies of one message collapse to one
    /// row, the inbox copy where there is one (its actions land where
    /// expected).
    fn assemble_tag_view(
        &self,
        key: &(Option<u32>, Option<String>),
        rows: Vec<(u32, String, Message)>,
    ) -> Vec<Message> {
        let (scope, kw) = key;
        let in_scope = |account_id: u32| self.in_tag_scope(*scope, account_id);
        let keywords = self.tag_view_keywords(kw);
        let mut out: Vec<Message> = Vec::new();
        let mut seen_rows: std::collections::HashSet<(u32, u32, u32)> =
            std::collections::HashSet::new();
        for (account_id, path, mut m) in rows {
            let Some(folders) = self.folders.get(&account_id) else {
                continue;
            };
            let Some(f) = folders.iter().find(|f| f.path == path) else {
                continue;
            };
            if matches!(f.kind, FolderKind::Trash | FolderKind::Junk) {
                continue;
            }
            m.folder_id = f.id;
            // A message with several tags answers several keywords.
            if seen_rows.insert((m.account_id, m.folder_id, m.uid)) {
                out.push(m);
            }
        }
        // The demo's mail never reaches the database, so its tag view reads
        // the folders loaded so far instead.
        if out.is_empty() && demo_mode() && self.config.is_empty() {
            for ((account_id, folder_id), messages) in &self.message_cache {
                if !in_scope(*account_id)
                    || matches!(
                        self.folder_kind(*account_id, *folder_id),
                        Some(FolderKind::Trash | FolderKind::Junk)
                    )
                {
                    continue;
                }
                out.extend(
                    messages
                        .iter()
                        .filter(|m| keywords.iter().any(|k| m.has_keyword(k)))
                        .cloned(),
                );
            }
        }
        self.newest_once(&mut out);
        out
    }

    /// The tag colors as CSS: `.tag-<keyword>` fills (chips), the same
    /// class on a `.tag-tint` widget colors its glyph instead, and with the
    /// setting on, `.rowtint-tag-<keyword>` washes a message row.
    pub(super) fn refresh_tag_css(&self) {
        let mut css = String::new();
        for t in &self.tags {
            let class = t.css_class();
            let text = crate::color::readable_text(&t.color);
            css.push_str(&format!(
                ".{class} {{ background-color: {color}; color: {text}; }} \
                 .{class} label {{ color: {text}; }} \
                 .tag-tint.{class} {{ color: {color}; background-color: transparent; }}\n",
                color = t.color,
            ));
            // Rows carry their `rowtint-` class whatever the setting; only
            // the rules come and go, so switching it restyles no row by hand.
            if self.tag_row_tint {
                css.push_str(&format!(
                    ".message-list > row:not(:selected) > .message-item.rowtint-{class}:not(.swiping) .message-row {{ \
                       background-color: alpha({color}, 0.18); }} \
                     .message-list > row:hover:not(:selected) > .message-item.rowtint-{class}:not(.swiping) .message-row {{ \
                       background-color: alpha({color}, 0.28); }}\n",
                    color = t.color,
                ));
            }
        }
        // The colors a Gmail label no tag stands for can wear (#374).
        for (hue, color) in crate::config::TAG_COLORS.iter().enumerate() {
            let text = crate::color::readable_text(color);
            css.push_str(&format!(
                ".tag-chip.label-chip.label-hue-{hue} {{ background-color: {color}; color: {text}; }}\n"
            ));
        }
        self.tag_provider.load(css);
    }

    /// The tag finder's scan is over: fold every account's findings into one
    /// list (a keyword seen on two accounts is one entry), drop the keywords
    /// that are tags already, propose a name and color for each, and hand
    /// the report to the Tags page — bringing Settings back if it was closed
    /// meanwhile.
    fn finish_tag_scan(&mut self, sender: &ComponentSender<Self>) {
        let Some(scan) = self.tag_scan.take() else {
            return;
        };
        let many = scan.found.len() > 1;
        let mut merged: Vec<(KeywordFinding, Vec<String>)> = Vec::new();
        for (account_id, findings) in scan.found {
            let label = self
                .accounts
                .iter()
                .find(|a| a.id == account_id)
                .map(|a| {
                    if a.name.trim().is_empty() {
                        a.email.clone()
                    } else {
                        a.name.clone()
                    }
                })
                .unwrap_or_default();
            for f in findings {
                if crate::worker::is_system_keyword(&f.keyword)
                    || self
                        .tags
                        .iter()
                        .any(|t| t.keyword.eq_ignore_ascii_case(&f.keyword))
                {
                    continue;
                }
                // One entry per account: its folders, named by the account
                // when more than one took part.
                let places: Vec<String> = if f.folders.is_empty() {
                    Vec::new()
                } else if many {
                    vec![format!("{label}: {}", f.folders.join(", "))]
                } else {
                    vec![f.folders.join(", ")]
                };
                match merged
                    .iter_mut()
                    .find(|(m, _)| m.keyword.eq_ignore_ascii_case(&f.keyword))
                {
                    Some((m, p)) => {
                        m.count += f.count;
                        if m.name.is_none() {
                            m.name = f.name.clone();
                        }
                        if m.color.is_none() {
                            m.color = f.color.clone();
                        }
                        p.extend(places);
                    }
                    None => merged.push((f, places)),
                }
            }
        }
        let mut taken: Vec<String> = self.tags.iter().map(|t| t.color.clone()).collect();
        let proposals: Vec<crate::ui::accounts::TagProposal> = merged
            .into_iter()
            .map(|(f, places)| {
                let name = f
                    .name
                    .clone()
                    .unwrap_or_else(|| config::Tag::name_for_keyword(&f.keyword));
                let color = f
                    .color
                    .clone()
                    .unwrap_or_else(|| config::Tag::color_for_keyword(&f.keyword, &taken));
                taken.push(color.clone());
                crate::ui::accounts::TagProposal {
                    tag: config::Tag {
                        name,
                        keyword: f.keyword,
                        color,
                    },
                    count: f.count,
                    places,
                }
            })
            .collect();
        self.open_settings_window(sender, true, false);
        if let Some(p) = &self.prefs {
            p.emit(PrefInput::ShowPageById("tags".to_string()));
        }
        if let Some(a) = &self.accounts_win {
            a.emit(crate::ui::accounts::AccountsInput::TagFindings(proposals));
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_tags(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::FindTags => {
                if self.tag_scan.is_some() {
                    return;
                }
                if self.workers.is_empty() {
                    if let Some(a) = &self.accounts_win {
                        a.emit(crate::ui::accounts::AccountsInput::TagFindings(Vec::new()));
                    }
                    return;
                }
                self.tag_scan_gen = self.tag_scan_gen.wrapping_add(1);
                let gen = self.tag_scan_gen;
                self.tag_scan = Some(TagScan {
                    gen,
                    remaining: self.workers.len(),
                    found: Vec::new(),
                });
                for w in self.workers.values() {
                    let _ = w.send(MailRequest::FindKeywords);
                }
                if let Some(a) = &self.accounts_win {
                    a.emit(crate::ui::accounts::AccountsInput::TagScanning(true));
                }
                // An account that is offline never answers: report what the
                // others found rather than searching forever.
                let s = sender.clone();
                gtk::glib::timeout_add_seconds_local_once(120, move || {
                    s.input(AppMsg::TagScanTimeout(gen));
                });
            }

            AppMsg::WantQuota(email) => {
                let worker = self
                    .accounts
                    .iter()
                    .find(|a| a.email.eq_ignore_ascii_case(&email))
                    .and_then(|a| self.workers.get(&a.id));
                if let Some(w) = worker {
                    let _ = w.send(MailRequest::Quota);
                }
            }

            AppMsg::QuotaFound { account_id, quota } => {
                if let (Some(email), Some(a)) = (self.email_of(account_id), &self.accounts_win) {
                    a.emit(crate::ui::accounts::AccountsInput::Quota { email, quota });
                }
            }

            AppMsg::KeywordsFound {
                account_id,
                findings,
            } => {
                let Some(scan) = self.tag_scan.as_mut() else {
                    return;
                };
                scan.found.push((account_id, findings));
                scan.remaining = scan.remaining.saturating_sub(1);
                if scan.remaining == 0 {
                    self.finish_tag_scan(&sender);
                }
            }

            AppMsg::TagScanTimeout(gen) => {
                if self.tag_scan.as_ref().is_some_and(|s| s.gen == gen) {
                    tracing::warn!("find tags: not every account answered in time");
                    self.finish_tag_scan(&sender);
                }
            }

            AppMsg::SetTags(tags) => {
                config::save_tags(&tags);
                self.tags = tags;
                // The unified Tags view spans every tag: what it holds is
                // stale once the set changes.
                self.tag_view_cache.retain(|(_, kw), _| kw.is_some());
                self.refresh_tag_css();
                self.message_list
                    .emit(MessageListInput::SetTags(self.tags.clone()));
                self.message_view
                    .emit(MessageViewInput::SetTags(self.tags.clone()));
                self.popouts_emit(|| MessageWindowInput::SetTags(self.tags.clone()));
                // The sidebar lists the tags; a removed one that was the open
                // view falls back to All Inboxes there (SetContents).
                self.rebuild_sidebar();
                self.schedule_smart_counts(&sender);
                // A new tag may stand for a Gmail label nothing fetched yet.
                self.sync_unloaded_counted_folders();
                // Rules may name a tag that now exists: tag what already sits
                // in the inboxes, through the same sweep the filters use.
                self.sweep_blacklisted();
            }

            AppMsg::TagSelected { keyword, account } => {
                let sel = match keyword.clone() {
                    Some(k) => crate::ui::sidebar::Sel::Tag(account, k),
                    None => crate::ui::sidebar::Sel::UnifiedTags,
                };
                self.enter_index_view(sel, Some((account, keyword)), None);
                self.show_tag_view();
                self.refresh_tag_view(&sender);
                self.sync_tag_keywords();
                self.push_index_complete();
            }

            AppMsg::KeywordsSynced { account_id, paths } => {
                if paths.is_empty() {
                    return;
                }
                self.schedule_smart_counts(&sender);
                // The index changed under the open views: the tag view
                // re-reads it, and an open folder among the changed ones
                // re-syncs so its rows' chips follow.
                if self.tag_view.is_some() {
                    self.refresh_tag_view(&sender);
                }
                if self.smart_view.is_some() {
                    self.refresh_smart_view(&sender);
                }
                if let Some(sel) = self.selected.clone() {
                    if sel.account_id == account_id && paths.contains(&sel.path) {
                        self.send_to(
                            account_id,
                            MailRequest::LoadMessages {
                                folder_id: sel.folder_id,
                                path: sel.path,
                            },
                        );
                    }
                }
            }

            AppMsg::TagViewLoaded { key, rows } => {
                self.tag_view_loading = false;
                let out = self.assemble_tag_view(&key, rows);
                let changed = self.tag_view_cache.get(&key) != Some(&out);
                if changed {
                    self.tag_view_cache.insert(key.clone(), out.clone());
                }
                if self.tag_view.as_ref() == Some(&key) && changed {
                    self.message_list
                        .emit(MessageListInput::SetMessages { messages: out });
                }
                if std::mem::take(&mut self.tag_view_dirty) {
                    self.refresh_tag_view(&sender);
                }
            }
            _ => unreachable!("not routed to on_tags"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::msg;

    /// Mail labelled in Gmail itself sits in the label's folder without the
    /// label on its row (#717): the chip counts it there, once beside its
    /// All Mail copy, and leaves Trash out.
    #[test]
    fn a_tags_gmail_label_folder_counts_toward_its_chip() {
        let c = crate::cache::Cache::in_memory().unwrap();
        let unread = |uid: u32, id: &str| {
            let mut m = msg(5, 1, uid);
            m.unread = true;
            m.message_id = id.to_string();
            m
        };
        c.save_messages(5, "Vireo", &[unread(1, "a@x"), unread(2, "b@x")]);
        let mut copy = unread(9, "a@x");
        copy.labels = vec!["Vireo".into()];
        c.save_messages(5, "[Gmail]/All Mail", &[copy]);
        c.save_messages(5, "[Gmail]/Trash", &[unread(3, "c@x")]);
        let job = |label: Option<&str>| TagCountJob {
            tags: vec![("Vireo".into(), "Vireo".into())],
            accounts: vec![TagCountAccount {
                id: 5,
                listed: ["Vireo", "[Gmail]/All Mail"]
                    .map(String::from)
                    .into_iter()
                    .collect(),
                unified: true,
                labels: label
                    .map(|l| ("Vireo".to_string(), l.to_string()))
                    .into_iter()
                    .collect(),
            }],
        };
        let counts = job(Some("Vireo")).run(&c);
        assert_eq!(counts[&(Some(5), "Vireo".to_string())], 2);
        assert_eq!(counts[&(None, "Vireo".to_string())], 2);
        // Without the label folder, only the copy that lists the label.
        let counts = job(None).run(&c);
        assert_eq!(counts[&(Some(5), "Vireo".to_string())], 1);
    }
}
