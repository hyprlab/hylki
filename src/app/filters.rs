//! Message filters: which rules match a message, and running them over a
//! folder as it syncs.

use crate::config;
use crate::models::{FolderKind, Message};
use crate::worker::MailRequest;

use super::filter_actions::{plan_actions, ForwardJob, PlanContext};
use super::filter_run::FolderTally;
use super::model::AppModel;
use super::msg::AppMsg;

/// The rules among `rules` that match `m`, as indexes in order. The list
/// rarely carries a body (the demo's does); the preview is what a sync
/// brings for every message, so it stands in for an empty body.
fn rules_matching(
    rules: &[&config::FilterRule],
    m: &Message,
    body_hits: &[String],
    contacts: Option<&std::collections::HashSet<String>>,
) -> Vec<usize> {
    let recipients = [m.to.as_str(), m.cc.as_str()]
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let input = config::FilterInput {
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
        body_hits,
        contacts,
    };
    rules
        .iter()
        .enumerate()
        .filter(|(_, r)| r.matches(&input))
        .map(|(i, _)| i)
        .collect()
}

impl AppModel {
    /// Whether a sender address matches the blacklist (exact address, or a bare
    /// domain entry matching the sender's domain or any subdomain of it).
    fn is_blacklisted(&self, addr: &str) -> bool {
        let a = addr.trim().to_lowercase();
        if a.is_empty() {
            return false;
        }
        let domain = a.rsplit('@').next().unwrap_or("");
        self.blacklist.iter().any(|entry| {
            if entry.contains('@') {
                a == *entry
            } else {
                domain == entry.as_str() || domain.ends_with(&format!(".{entry}"))
            }
        })
    }

    /// Move any blacklisted senders in an inbox sync to Trash, returning the rest.
    pub(super) fn apply_blacklist(
        &self,
        account_id: u32,
        folder_id: u32,
        messages: Vec<Message>,
    ) -> Vec<Message> {
        if self.blacklist.is_empty() || self.inbox_of(account_id).map(|f| f.id) != Some(folder_id) {
            return messages;
        }
        let folders = self.folders.get(&account_id);
        let trash = folders
            .and_then(|fs| fs.iter().find(|f| f.kind == FolderKind::Trash))
            .map(|f| f.path.clone());
        let src = self.folder_path(account_id, folder_id);
        let mut kept = Vec::with_capacity(messages.len());
        for m in messages {
            if self.is_blacklisted(&m.from_addr) {
                if let (Some(trash), Some(src)) = (&trash, &src) {
                    self.send_to(
                        account_id,
                        MailRequest::MoveMessage {
                            path: src.clone(),
                            uid: m.uid,
                            dest: trash.clone(),
                        },
                    );
                }
            } else {
                kept.push(m);
            }
        }
        kept
    }

    /// Apply the mail filter rules (#47) to an inbox sync, Evolution-style:
    /// the first matching rule files the message into its folder; everything
    /// else passes through. On-sight like the blacklist, so mail that arrived
    /// while Hylki was closed still gets filed on the next sync.
    ///
    /// Returns the messages staying in the inbox, plus the ones filed away on
    /// this sync (paired with their destination path) so the caller can still
    /// count them as new mail when notifying.
    ///
    /// A manual run (#198) adds the folder it was pointed at to the pass, even
    /// when that is not the Inbox, and tallies what the rules did there so the
    /// run can report it.
    pub(super) fn apply_filters(
        &mut self,
        account_id: u32,
        folder_id: u32,
        messages: Vec<Message>,
    ) -> (Vec<Message>, Vec<(Message, String)>) {
        let manual = self
            .filter_run
            .as_ref()
            .is_some_and(|r| r.folders.contains_key(&(account_id, folder_id)));
        let followup = self.filter_followups.remove(&(account_id, folder_id));
        let (kept, filed, pass) =
            self.filter_pass(account_id, folder_id, messages, manual || followup);
        if manual {
            self.filter_run_folder_pass(account_id, folder_id, pass);
        }
        (kept, filed)
    }

    /// The addresses whose mail a pass leaves alone in this folder. When
    /// the account files copies of what it sends into it (#199), the
    /// user's own mail sitting here is one of those copies, not an
    /// arrival. Rules are written about incoming mail, so a rule on a
    /// subject or a recipient would otherwise pick up your own reply and
    /// move it away from the thread it was filed beside. The Sent folder
    /// is the exception: everything in it is your own mail, so running
    /// rules over it (#198) plainly means to act on exactly that. Replies
    /// filed beside what they answer (#468) can be in any folder.
    fn own_mail_to_skip(&self, account_id: u32, src: Option<&str>) -> Vec<String> {
        let files_replies = self.account_config(account_id).is_some_and(|c| {
            c.reply_copies != crate::config::ReplyCopies::WithSent && c.files_some_sent()
        });
        let diverted = (files_replies || self.sent_copy_path(account_id).as_deref() == src)
            && self
                .folder_of_kind(account_id, FolderKind::Sent)
                .map(|f| f.path.as_str())
                != src;
        if diverted {
            self.own_identities(account_id)
        } else {
            Vec::new()
        }
    }

    /// Tags first (#71), from every matching rule, and only where the
    /// message lacks the tag: this runs on every sync, and the flag comes
    /// back down with the next one. Tagged before any move, so the move
    /// carries the keyword along. A tag that no longer exists tags
    /// nothing. Returns how many tags were put on.
    fn tag_by_rules(
        &self,
        account_id: u32,
        src: &str,
        m: &mut Message,
        matching: &[&&config::FilterRule],
    ) -> usize {
        let mut tagged = 0;
        let wanted: Vec<(String, String)> = matching
            .iter()
            .filter(|r| !r.tag.is_empty() && !self.carries_tag(m, &r.tag))
            .filter(|r| {
                self.tags
                    .iter()
                    .any(|t| t.keyword.eq_ignore_ascii_case(&r.tag))
            })
            .map(|r| (r.tag.clone(), r.label()))
            .collect();
        for (tag, rule) in wanted {
            if self.carries_tag(m, &tag) {
                continue; // two rules naming one tag
            }
            tagged += 1;
            tracing::info!("filter: {} tagged {tag} [{rule}]", m.from_addr);
            self.send_to(
                account_id,
                MailRequest::SetKeyword {
                    path: src.to_string(),
                    uid: m.uid,
                    message_id: m.message_id.clone(),
                    keyword: tag.clone(),
                    add: true,
                },
            );
            m.keywords.push(tag);
        }
        tagged
    }

    /// One pass of the rules over a folder's messages. Returns what stays,
    /// what was filed elsewhere, and (for a manual run) the pass's count:
    /// how many messages were looked at, how many a rule matched, and how
    /// many tags were put on and messages filed.
    fn filter_pass(
        &mut self,
        account_id: u32,
        folder_id: u32,
        messages: Vec<Message>,
        manual: bool,
    ) -> (Vec<Message>, Vec<(Message, String)>, FolderTally) {
        let Some(email) = self.filtered_account(account_id, folder_id, manual) else {
            return (messages, Vec::new(), FolderTally::default());
        };
        self.purge_stale_auto_forwards();
        let rules: Vec<&config::FilterRule> = self
            .filters
            .iter()
            .filter(|r| r.account_email.eq_ignore_ascii_case(&email))
            .collect();
        if rules.is_empty() {
            return (messages, Vec::new(), FolderTally::default());
        }
        let checked = messages.len();
        let mut matched = 0usize;
        let mut tagged = 0usize;
        let mut read = 0usize;
        let mut deleted = 0usize;
        // Forwards to start once the loop is done with its borrows of the
        // rules: a forward is asynchronous (see `start_auto_forward`).
        let mut to_forward: Vec<(Message, ForwardJob)> = Vec::new();
        let own_addrs = self.own_identities(account_id);
        let now = crate::datefmt::now();
        let outbox = self.outbox_by_account.get(&account_id);
        let in_outbox = |m: &Message| {
            !m.message_id.is_empty()
                && outbox.is_some_and(|items| {
                    items.iter().any(|i| {
                        i.forwards
                            .trim()
                            .trim_matches(['<', '>'])
                            .eq_ignore_ascii_case(&m.message_id)
                    })
                })
        };
        // How many messages each rule matched, said in the log at the end of
        // a manual run: the one place a rule that matches nothing can be
        // told apart from one whose matches needed nothing done (#201).
        let mut per_rule = vec![0usize; rules.len()];
        let folders = self.folders.get(&account_id);
        let src = self.folder_path(account_id, folder_id);
        let known = |path: &str| folders.is_some_and(|fs| fs.iter().any(|f| f.path == path));
        let pending = self
            .filter_moved
            .remove(&(account_id, folder_id))
            .unwrap_or_default();
        let mut still_pending = std::collections::HashSet::new();
        let mut kept = Vec::with_capacity(messages.len());
        let mut filed = Vec::new();
        let own = self.own_mail_to_skip(account_id, src.as_deref());
        let hits = self.body_hits.get(&(account_id, folder_id));
        // The address books are read once per pass, and only when a rule asks
        // for them. An empty read counts as unknown: see FilterField::FromInContacts.
        let contacts = if rules.iter().any(|r| r.needs_contacts()) {
            self.saved_addresses.get()
        } else {
            None
        };
        for mut m in messages {
            if own.iter().any(|a| a.eq_ignore_ascii_case(&m.from_addr)) {
                kept.push(m);
                continue;
            }
            let body_hits = hits
                .and_then(|h| h.get(&m.uid))
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let matching: Vec<&&config::FilterRule> =
                rules_matching(&rules, &m, body_hits, contacts.as_deref())
                    .into_iter()
                    .map(|i| {
                        per_rule[i] += 1;
                        &rules[i]
                    })
                    .collect();
            if !matching.is_empty() {
                matched += 1;
            }
            if let Some(src) = &src {
                tagged += self.tag_by_rules(account_id, src, &mut m, &matching);
            }
            // A destination that vanished from the server keeps the mail in
            // the inbox rather than erroring it into limbo.
            let matching_rules: Vec<&config::FilterRule> = matching.iter().map(|r| **r).collect();
            let cx = PlanContext {
                src_path: src.as_deref(),
                own: &own_addrs,
                log: &self.forwarded_log,
                dest_ok: &known,
                in_outbox: &in_outbox,
                now,
            };
            let plan = plan_actions(&matching_rules, &m, &cx);
            // A forward not yet on its way holds the message where it is:
            // nothing else a rule does to it may run before the forward is
            // recorded, or a failed forward would lose the mail.
            if plan.held {
                tracing::info!(
                    "filter: {} held: encrypted, or its forward is in the Outbox",
                    m.from_addr
                );
                kept.push(m);
                continue;
            }
            if !plan.forwards.is_empty() {
                for job in plan.forwards {
                    if !self.auto_forward_inflight.contains(&job.key) {
                        to_forward.push((m.clone(), job));
                    }
                }
                kept.push(m);
                continue;
            }
            let Some(src) = &src else {
                kept.push(m);
                continue;
            };
            if plan.mark_read && !pending.contains(&m.uid) {
                self.send_to(
                    account_id,
                    MailRequest::SetSeen {
                        path: src.clone(),
                        uid: m.uid,
                        seen: true,
                    },
                );
                m.unread = false;
                read += 1;
            }
            if plan.delete {
                still_pending.insert(m.uid);
                // Requested on an earlier sync that raced the server.
                if pending.contains(&m.uid) {
                    continue;
                }
                tracing::info!("filter: {} deleted", m.from_addr);
                self.send_to(
                    account_id,
                    MailRequest::PurgeMessages {
                        path: src.clone(),
                        uids: vec![m.uid],
                    },
                );
                deleted += 1;
                continue;
            }
            match plan.move_to {
                Some(dest) => {
                    still_pending.insert(m.uid);
                    if pending.contains(&m.uid) {
                        // The move was already requested on an earlier sync;
                        // this one raced the server. Requesting again would
                        // error once the first move lands, and re-notifying
                        // would repeat the toast.
                        continue;
                    }
                    self.request_filter_move(account_id, src, &m, &dest, &matching_rules);
                    filed.push((m, dest));
                }
                None => kept.push(m),
            }
        }
        // UIDs absent from this sync have completed their move server-side;
        // dropping them keeps the set from growing without bound.
        if !still_pending.is_empty() {
            self.filter_moved
                .insert((account_id, folder_id), still_pending);
        }
        if manual {
            log_rule_counts(&rules, &per_rule, checked, src.as_deref());
        }
        // The worker answers each purge with a `BulkComplete`, which would
        // otherwise end the count of somebody else's bulk action early.
        if deleted > 0 {
            self.bulk_pending += deleted;
            self.update_busy_indicator();
        }
        let forwarded = to_forward.len();
        for (m, job) in to_forward {
            self.start_auto_forward(m, job);
        }
        let pass = FolderTally {
            checked,
            matched,
            tagged,
            filed: filed.len(),
            forwarded,
            read,
            deleted,
        };
        (kept, filed, pass)
    }

    /// The address of `account_id` when this pass runs the filters at all:
    /// there are rules, and the folder is the account's Inbox or the run
    /// was asked for by hand.
    fn filtered_account(&self, account_id: u32, folder_id: u32, manual: bool) -> Option<String> {
        if self.filters.is_empty()
            || (!manual && self.inbox_of(account_id).map(|f| f.id) != Some(folder_id))
        {
            return None;
        }
        self.email_of(account_id)
    }

    /// Ask the server to move `m` from `src` to `dest`, where the first of
    /// the `matching` rules with that destination files it.
    fn request_filter_move(
        &self,
        account_id: u32,
        src: &str,
        m: &Message,
        dest: &str,
        matching: &[&config::FilterRule],
    ) {
        let label = matching
            .iter()
            .find(|r| r.dest_path == dest)
            .map(|r| r.label())
            .unwrap_or_default();
        tracing::info!("filter: {} → {dest} [{label}]", m.from_addr);
        self.send_to(
            account_id,
            MailRequest::MoveMessage {
                path: src.to_string(),
                uid: m.uid,
                dest: dest.to_string(),
            },
        );
    }

    /// Re-sync every inbox so a newly-blacklisted sender's existing mail is
    /// caught and deleted by [`apply_blacklist`].
    pub(super) fn sweep_blacklisted(&self) {
        let reqs: Vec<(u32, u32, String)> = self
            .accounts
            .iter()
            .filter_map(|a| self.inbox_of(a.id).map(|f| (a.id, f.id, f.path.clone())))
            .collect();
        for (account_id, folder_id, path) in reqs {
            self.send_to(account_id, MailRequest::LoadMessages { folder_id, path });
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_filters(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::AllowSender(addr) | AppMsg::AddSender(addr) => {
                let addr = addr.trim().to_lowercase();
                if !addr.is_empty() && !self.allowed_senders.contains(&addr) {
                    self.allowed_senders.push(addr);
                    self.save_settings();
                }
            }

            AppMsg::RemoveSender(addr) => {
                let addr = addr.to_lowercase();
                self.allowed_senders.retain(|s| *s != addr);
                self.save_settings();
            }

            AppMsg::AddBlacklist(addr) => {
                let addr = addr.trim().trim_start_matches('@').to_lowercase();
                if !addr.is_empty() && !self.blacklist.contains(&addr) {
                    self.blacklist.push(addr);
                    self.save_settings();
                    // Sweep mail already in view from the newly-blocked sender.
                    self.sweep_blacklisted();
                }
            }

            AppMsg::RemoveBlacklist(addr) => {
                let addr = addr.to_lowercase();
                self.blacklist.retain(|s| *s != addr);
                self.save_settings();
            }

            AppMsg::SetFilters(rules) => {
                config::save_filters(&rules);
                let listed_before = self.unified_folder_keys();
                if rules.iter().any(|r| r.needs_contacts()) {
                    self.saved_addresses.refresh();
                }
                self.filters = rules;
                // A rule opting its folder in or out of All Inboxes changes
                // the sidebar's Filtered Folders section; nothing else
                // about the rules is drawn there.
                if self.unified_folder_keys() != listed_before {
                    self.rebuild_sidebar();
                }
                // File whatever already sits in the inboxes under the new
                // rules; reuses the blacklist's re-sync sweep.
                self.sweep_blacklisted();
                // The counted folders may have changed with the rules (#116):
                // the total follows at once, and a newly counted folder's
                // list is fetched for the tray menu.
                self.push_unread_counts();
                self.sync_unloaded_counted_folders();
            }
            _ => unreachable!("not routed to on_filters"),
        }
    }
}

/// Log how many of the `checked` messages in `folder` each rule matched,
/// at the end of a manual run (see `per_rule` in `filter_pass`).
fn log_rule_counts(
    rules: &[&config::FilterRule],
    per_rule: &[usize],
    checked: usize,
    folder: Option<&str>,
) {
    let folder = folder.unwrap_or("?");
    for (rule, n) in rules.iter().zip(per_rule) {
        tracing::info!(
            "filter: [{}] matched {n} of {checked} in {folder}",
            rule.label()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::demo::demo_filters;
    use crate::app::test_support::msg;

    #[test]
    fn filter_rules_read_recipients_and_the_preview() {
        use crate::config::{FilterField, FilterMatch};
        let mut rules = demo_filters();
        rules[0].field = FilterField::Recipients;
        rules[0].matcher = FilterMatch::Contains;
        rules[0].value = "carol".into();
        rules[1].field = FilterField::Body;
        rules[1].value = "invoice".into();
        let rules: Vec<&config::FilterRule> = rules.iter().collect();
        let mut m = msg(1, 1, 1);
        m.cc = "Carol <carol@x>".into();
        m.preview = "Your invoice".into();
        assert_eq!(rules_matching(&rules, &m, &[], None), vec![0, 1]);
        m.body = "nothing here".into();
        assert_eq!(rules_matching(&rules, &m, &[], None), vec![0]);
        m.subject = "Your order".into();
        assert_eq!(rules_matching(&rules, &m, &[], None), vec![0, 2]);
    }
}
