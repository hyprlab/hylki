//! A manual Apply Filters run (#198) over chosen folders, its progress
//! dialog and its report.

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::ui::notifications::NotifyInput;
use crate::worker::MailRequest;

use super::model::AppModel;
use super::msg::AppMsg;

/// How long a manual "Apply Filters" run (#198) waits for the folders it asked
/// for before reporting on whatever came back. A folder that is offline, or
/// that errors, would otherwise leave the run without an answer for good.
const FILTER_RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// The one response of the manual run's progress dialog: "Run in Background"
/// while it works, "Close" once it holds the report.
const FILTER_RUN_RESPONSE: &str = "close";

/// What the rules did in one folder of a manual run.
#[derive(Default, Clone, Copy)]
pub(super) struct FolderTally {
    /// The most messages one pass over the folder was held up against. A
    /// folder answers twice (cache, then server) over the same mail, so the
    /// larger listing is the count — adding them would count it twice.
    pub(super) checked: usize,
    /// Messages at least one rule matched, whether or not anything was
    /// done about it (#201): mail tagged on an earlier sync matches again
    /// and needs nothing, and a report that only counted actions called
    /// that "none of them matched". Held the same way as `checked`.
    pub(super) matched: usize,
    /// Tags put on, and messages filed away. Both only ever happen once per
    /// message (a second pass sees the tag, or the pending move), so these
    /// add up across passes.
    pub(super) tagged: usize,
    pub(super) filed: usize,
    /// Forwards started (they finish on their own), messages marked read,
    /// and messages deleted for good. Like `filed`, once per message.
    pub(super) forwarded: usize,
    pub(super) read: usize,
    pub(super) deleted: usize,
}

/// A manual "Apply Filters" run (#198): the folders it covers, what the rules
/// have done in each so far, and the dialog watching it.
pub(super) struct FilterRun {
    /// Every folder the run covers, with its tally. A folder load answers
    /// twice (what the cache holds, then what the server sends), so the scope
    /// has to outlive the cache's answer or the server's would go unfiltered
    /// outside the Inbox. Cleared by [`FILTER_RUN_TIMEOUT`].
    pub(super) folders: std::collections::HashMap<(u32, u32), FolderTally>,
    /// The folders that have not been to the server and back yet.
    pending: std::collections::HashSet<(u32, u32)>,
    /// Whether the run has already said what it did.
    reported: bool,
    /// The progress dialog, while it is up. Gone once the run was sent to the
    /// background, in which case the report goes to the status bar instead.
    dialog: Option<FilterRunDialog>,
}

impl FilterRun {
    /// The run's totals so far.
    fn totals(&self) -> FolderTally {
        self.folders
            .values()
            .fold(FolderTally::default(), |mut t, f| {
                t.checked += f.checked;
                t.matched += f.matched;
                t.tagged += f.tagged;
                t.filed += f.filed;
                t.forwarded += f.forwarded;
                t.read += f.read;
                t.deleted += f.deleted;
                t
            })
    }

    /// How many of the run's folders are done.
    fn done_count(&self) -> usize {
        self.folders.len() - self.pending.len()
    }
}

/// The widgets of a manual run's progress dialog, kept so its live text can be
/// rewritten as folders report and so the finish can turn it into the report.
#[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
struct FilterRunDialog {
    dialog: adw::MessageDialog,
    spinner: gtk::Spinner,
    body: gtk::Label,
}

impl AppModel {
    /// The (account, folder) pairs a manual filter run should cover (#198).
    /// An empty `targets` means every account's Inbox; anything else is
    /// taken as given, minus accounts that have no rules at all.
    fn filter_run_targets(&self, targets: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
        let has_rules = |account_id: u32| {
            self.email_of(account_id).is_some_and(|email| {
                self.filters
                    .iter()
                    .any(|r| r.account_email.eq_ignore_ascii_case(&email))
            })
        };
        let targets = if targets.is_empty() {
            self.accounts
                .iter()
                .filter_map(|a| self.inbox_of(a.id).map(|f| (a.id, f.id)))
                .collect()
        } else {
            targets
        };
        targets.into_iter().filter(|(a, _)| has_rules(*a)).collect()
    }

    /// Start a manual filter run over `targets`: re-load each folder so the
    /// rules meet the mail already in it. The load's `Messages` event is what
    /// actually runs them, through [`apply_filters`]; the folder counts as
    /// done once its load has been to the server and back
    /// (`AppMsg::FolderSynced`), not when the cache answers first.
    pub(super) fn start_filter_run(
        &mut self,
        targets: Vec<(u32, u32)>,
        sender: &ComponentSender<Self>,
    ) {
        let targets = self.filter_run_targets(targets);
        if targets.is_empty() {
            self.set_filters_applying(false);
            self.notice(i18n("No filters to apply here yet"));
            return;
        }
        tracing::info!(
            "filter: manual run over {} folder(s), {} rule(s)",
            targets.len(),
            self.filters.len(),
        );
        self.set_filters_applying(true);
        self.filter_run = Some(FilterRun {
            folders: targets
                .iter()
                .map(|t| (*t, FolderTally::default()))
                .collect(),
            pending: targets.iter().copied().collect(),
            reported: false,
            dialog: None,
        });
        let dialog = self.build_filter_run_dialog(sender);
        if let Some(run) = self.filter_run.as_mut() {
            run.dialog = Some(dialog);
        }
        self.update_filter_run_dialog();
        for (account_id, folder_id) in targets {
            let path = self.folder_path(account_id, folder_id);
            match path {
                Some(path) => {
                    self.send_to(account_id, MailRequest::LoadMessages { folder_id, path })
                }
                // Nothing to load it from: count the folder as done so the
                // run can still finish.
                None => self.filter_run_folder_synced(account_id, folder_id),
            }
        }
        // A folder that never answers (offline, or an error) must not leave
        // the run hanging: report whatever landed after a decent wait.
        let sender = sender.clone();
        gtk::glib::timeout_add_local_once(FILTER_RUN_TIMEOUT, move || {
            sender.input(AppMsg::FilterRunTimeout);
        });
    }

    /// The dialog a manual run puts up: a spinner, a live count, and one
    /// button that sends the run to the background while it is working and
    /// closes the report once it is not.
    #[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
    fn build_filter_run_dialog(&self, sender: &ComponentSender<Self>) -> FilterRunDialog {
        // Parented on Settings when the run was started there, so the dialog
        // is not stranded behind it.
        let parent: Option<gtk::Window> = self
            .prefs
            .as_ref()
            .map(|p| p.widget().clone())
            .filter(|w| w.is_visible())
            .map(|w| w.upcast())
            .or_else(|| Some(self.window.clone().upcast()));
        let dialog =
            adw::MessageDialog::new(parent.as_ref(), Some(&i18n("Applying Filters")), None);
        dialog.set_body_use_markup(false);
        dialog.add_response(FILTER_RUN_RESPONSE, &i18n("Run in Background"));
        dialog.set_close_response(FILTER_RUN_RESPONSE);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_halign(gtk::Align::Center);
        let spinner = gtk::Spinner::new();
        spinner.set_size_request(32, 32);
        spinner.set_spinning(true);
        content.append(&spinner);
        let body = gtk::Label::new(None);
        body.set_justify(gtk::Justification::Center);
        body.set_wrap(true);
        body.add_css_class("dim-label");
        content.append(&body);
        dialog.set_extra_child(Some(&content));
        // Every way out of the dialog is the same one: while the run is going
        // it means "keep going without me", and afterwards it is just Close.
        // Which of the two happened is read off `reported`, not from here.
        {
            let sender = sender.clone();
            dialog.connect_response(None, move |_, _| {
                sender.input(AppMsg::FilterRunToBackground);
            });
        }
        dialog.present();
        FilterRunDialog {
            dialog,
            spinner,
            body,
        }
    }

    /// Rewrite the progress dialog's live count, if it is still up and still
    /// counting. Once the run has reported, the dialog holds that report and
    /// a later pass (the scope outlives the report, so an ordinary sync of
    /// one of these folders still runs the rules) must not write over it.
    fn update_filter_run_dialog(&self) {
        let Some(run) = self.filter_run.as_ref() else {
            return;
        };
        if run.reported {
            return;
        }
        let Some(d) = run.dialog.as_ref() else { return };
        let t = run.totals();
        let mut lines = vec![ni18n_f(
            "Looked at {n} message so far",
            "Looked at {n} messages so far",
            t.checked as u32,
            &[("n", &t.checked.to_string())],
        )];
        if t.matched > 0 {
            lines.push(ni18n_f(
                "Matched {n} message",
                "Matched {n} messages",
                t.matched as u32,
                &[("n", &t.matched.to_string())],
            ));
        }
        if t.tagged > 0 {
            lines.push(ni18n_f(
                "Tagged {n} message",
                "Tagged {n} messages",
                t.tagged as u32,
                &[("n", &t.tagged.to_string())],
            ));
        }
        if t.filed > 0 {
            lines.push(ni18n_f(
                "Filed {n} message away",
                "Filed {n} messages away",
                t.filed as u32,
                &[("n", &t.filed.to_string())],
            ));
        }
        if let Some(line) = Self::filter_extras_line(&t) {
            lines.push(line);
        }
        // Only worth saying which folder the run is on when there is more
        // than one of them.
        if run.folders.len() > 1 {
            lines.push(i18n_f(
                "Folder {done} of {total}",
                &[
                    ("done", &run.done_count().to_string()),
                    ("total", &run.folders.len().to_string()),
                ],
            ));
        }
        d.body.set_label(&lines.join("\n"));
    }

    /// "Started 2 forwards, marked 3 as read, deleted 1": what a run did
    /// beyond tagging and filing, or `None` when it did none of it.
    fn filter_extras_line(t: &FolderTally) -> Option<String> {
        let mut parts = Vec::new();
        if t.forwarded > 0 {
            parts.push(ni18n_f(
                "started {n} forward",
                "started {n} forwards",
                t.forwarded as u32,
                &[("n", &t.forwarded.to_string())],
            ));
        }
        if t.read > 0 {
            parts.push(ni18n_f(
                "marked {n} message as read",
                "marked {n} messages as read",
                t.read as u32,
                &[("n", &t.read.to_string())],
            ));
        }
        if t.deleted > 0 {
            parts.push(ni18n_f(
                "deleted {n} message",
                "deleted {n} messages",
                t.deleted as u32,
                &[("n", &t.deleted.to_string())],
            ));
        }
        if parts.is_empty() {
            return None;
        }
        let line = parts.join(", ");
        let mut chars = line.chars();
        chars
            .next()
            .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
    }

    /// Fold one pass's work into the run's tally for that folder.
    pub(super) fn filter_run_folder_pass(
        &mut self,
        account_id: u32,
        folder_id: u32,
        pass: FolderTally,
    ) {
        if let Some(run) = self.filter_run.as_mut() {
            let tally = run.folders.entry((account_id, folder_id)).or_default();
            tally.checked = tally.checked.max(pass.checked);
            tally.matched = tally.matched.max(pass.matched);
            tally.tagged += pass.tagged;
            tally.filed += pass.filed;
            tally.forwarded += pass.forwarded;
            tally.read += pass.read;
            tally.deleted += pass.deleted;
        }
        self.update_filter_run_dialog();
    }

    /// One folder of a manual run has been to the server and back.
    fn filter_run_folder_synced(&mut self, account_id: u32, folder_id: u32) {
        let Some(run) = self.filter_run.as_mut() else {
            return;
        };
        if !run.pending.remove(&(account_id, folder_id)) {
            return;
        }
        let done = run.pending.is_empty();
        self.update_filter_run_dialog();
        if done {
            self.report_filter_run(false);
        }
    }

    /// The progress dialog was dismissed. While the run is still going that
    /// means "carry on without me", and the report will go to the status bar;
    /// once it has reported, the dialog was only holding the report, so this
    /// is an ordinary close.
    fn filter_run_dismissed(&mut self) {
        let Some(run) = self.filter_run.as_mut() else {
            return;
        };
        run.dialog = None;
        if run.reported {
            self.filter_run = None;
        }
    }

    /// Say what a manual filter run did: in the dialog when it is still up
    /// (the user waited), in the status bar when it is not (they sent the run
    /// to the background). `end` (the timeout) also drops the run, so the
    /// folders stop counting as manually swept; without it the scope stands,
    /// and the server's answer to the same load is filtered too. Reports once
    /// either way.
    #[allow(deprecated)] // adw::MessageDialog: AlertDialog would change how it looks
    fn report_filter_run(&mut self, end: bool) {
        let Some(run) = self.filter_run.as_mut() else {
            return;
        };
        if std::mem::replace(&mut run.reported, true) {
            if end {
                self.filter_run = None;
            }
            return;
        }
        let t = run.totals();
        let unanswered = run.pending.len();
        tracing::info!(
            "filter: manual run done, {} checked, {} matched, {} tagged, {} filed, \
             {unanswered} folder(s) unanswered",
            t.checked,
            t.matched,
            t.tagged,
            t.filed,
        );
        self.set_filters_applying(false);
        self.notifications
            .emit(NotifyInput::SetStatus(self.copy_status()));
        // Always says how many messages the rules were held up against: with
        // nothing matching, that is the whole answer to "are my rules working"
        // (#198). The plural follows that count throughout.
        let n = t.checked.to_string();
        let matched_s = t.matched.to_string();
        let tagged_s = t.tagged.to_string();
        let filed_s = t.filed.to_string();
        let extras = t.forwarded + t.read + t.deleted;
        let text = match (t.matched, t.tagged, t.filed) {
            (0, _, _) => ni18n_f(
                "Filters looked at {n} message, and it matched nothing",
                "Filters looked at {n} messages, and none of them matched",
                t.checked as u32,
                &[("n", &n)],
            ),
            // Matches that needed nothing: the rules had already done their
            // work on an earlier sync (#201). Saying "none matched" here
            // made working rules look broken.
            (_, 0, 0) if extras > 0 => ni18n_f(
                "Filters looked at {n} message",
                "Filters looked at {n} messages",
                t.checked as u32,
                &[("n", &n)],
            ),
            (_, 0, 0) => ni18n_f(
                "Filters matched {matched} of {n} message, and it was already tagged or filed",
                "Filters matched {matched} of {n} messages, and all of them were already tagged or filed",
                t.checked as u32,
                &[("matched", &matched_s), ("n", &n)],
            ),
            (_, _, 0) => ni18n_f(
                "Filters tagged {tagged} of {n} message",
                "Filters tagged {tagged} of {n} messages",
                t.checked as u32,
                &[("tagged", &tagged_s), ("n", &n)],
            ),
            (_, 0, _) => ni18n_f(
                "Filters filed {filed} of {n} message away",
                "Filters filed {filed} of {n} messages away",
                t.checked as u32,
                &[("filed", &filed_s), ("n", &n)],
            ),
            _ => ni18n_f(
                "Filters tagged {tagged} and filed {filed} of {n} message",
                "Filters tagged {tagged} and filed {filed} of {n} messages",
                t.checked as u32,
                &[("tagged", &tagged_s), ("filed", &filed_s), ("n", &n)],
            ),
        };
        let text = match Self::filter_extras_line(&t) {
            Some(line) => format!("{text}\n{line}"),
            None => text,
        };
        // Waited on the dialog: the report belongs there, and nowhere else.
        // Sent to the background: the status bar is the only place left. A
        // dialog that went down with its parent (Settings closed mid-run)
        // counts as the second case, or the report would go nowhere.
        match self
            .filter_run
            .as_ref()
            .and_then(|r| r.dialog.as_ref())
            .filter(|d| d.dialog.is_visible())
        {
            Some(d) => {
                tracing::info!("filter: manual run reported in its dialog");
                d.spinner.set_spinning(false);
                d.spinner.set_visible(false);
                d.body.remove_css_class("dim-label");
                d.body.set_label(&text);
                d.dialog.set_heading(Some(&i18n("Filters Applied")));
                d.dialog
                    .set_response_label(FILTER_RUN_RESPONSE, &i18n("Close"));
                d.dialog.set_response_appearance(
                    FILTER_RUN_RESPONSE,
                    adw::ResponseAppearance::Suggested,
                );
            }
            None => {
                tracing::info!("filter: manual run reported in the status bar");
                self.notice(text);
            }
        }
        if end {
            self.filter_run = None;
        }
    }

    /// Tell the Settings window whether a manual filter run is under way, so
    /// its Apply Now button can spin.
    fn set_filters_applying(&self, on: bool) {
        if let Some(a) = &self.accounts_win {
            a.emit(crate::ui::accounts::AccountsInput::FiltersApplying(on));
        }
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_filter_run(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ApplyFilters(targets) => {
                self.start_filter_run(targets, &sender);
            }

            AppMsg::FilterRunTimeout => {
                self.report_filter_run(true);
            }

            AppMsg::FilterRunToBackground => {
                self.filter_run_dismissed();
            }

            AppMsg::FolderSynced {
                account_id,
                folder_id,
            } => {
                self.filter_run_folder_synced(account_id, folder_id);
            }
            _ => unreachable!("not routed to on_filter_run"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_run_totals_add_up_the_folders() {
        let mut run = FilterRun {
            folders: Default::default(),
            pending: Default::default(),
            reported: false,
            dialog: None,
        };
        run.folders.insert(
            (1, 1),
            FolderTally {
                checked: 3,
                matched: 2,
                tagged: 1,
                filed: 0,
                ..Default::default()
            },
        );
        run.folders.insert(
            (1, 2),
            FolderTally {
                checked: 4,
                matched: 1,
                tagged: 0,
                filed: 1,
                ..Default::default()
            },
        );
        run.pending.insert((1, 2));
        let t = run.totals();
        assert_eq!((t.checked, t.matched, t.tagged, t.filed), (7, 3, 1, 1));
        assert_eq!(run.done_count(), 1);
    }
}
