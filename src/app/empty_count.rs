//! Empty Trash / Empty Junk ask first, and say how much will go (#676): the
//! servers are asked for each folder's count before the question opens, and
//! a server that is slow to answer only costs the number, not the question.

use adw::prelude::*;
use relm4::prelude::*;

use crate::i18n::{i18n, i18n_f, ni18n_f};
use crate::models::FolderKind;
use crate::ui::dialog::confirm_destructive;
use crate::worker::MailRequest;

use super::model::AppModel;
use super::msg::AppMsg;

/// How long the question waits for the counts. Long enough for a server
/// round trip, short enough that the menu still feels like it answered.
const COUNT_WAIT_MS: u64 = 1500;

/// One folder an empty would erase.
pub(super) struct EmptyTarget {
    account_id: u32,
    folder_id: u32,
    name: String,
    path: String,
    /// How the multi-account question names the account.
    account: String,
    /// `None` while the server has yet to answer; `Some(None)` when it gave
    /// no number.
    count: Option<Option<u32>>,
}

/// An empty waiting for its counts before it asks.
pub(super) struct EmptyAsk {
    gen: u32,
    /// The unified row's kind, when the question covers every account.
    every: Option<FolderKind>,
    targets: Vec<EmptyTarget>,
}

impl EmptyAsk {
    fn answered(&self) -> bool {
        self.targets.iter().all(|t| t.count.is_some())
    }
}

impl AppModel {
    /// Empty one Trash or Junk folder, once asked (#152).
    pub(super) fn confirm_empty_folder(
        &mut self,
        account_id: u32,
        folder_id: u32,
        name: String,
        path: String,
        sender: &ComponentSender<Self>,
    ) {
        let account = self.account_label(account_id);
        self.ask_empty(
            None,
            vec![EmptyTarget {
                account_id,
                folder_id,
                name,
                path,
                account,
                count: None,
            }],
            sender,
        );
    }

    /// Empty every unified account's Trash or Junk (#413). One folder asks
    /// the way its own row does; more name each account they belong to.
    pub(super) fn confirm_empty_all(&mut self, kind: FolderKind, sender: &ComponentSender<Self>) {
        let targets: Vec<EmptyTarget> = self
            .accounts
            .iter()
            .filter(|a| self.in_unified(a.id))
            .filter_map(|a| {
                let f = self.folder_of_kind(a.id, kind)?;
                Some(EmptyTarget {
                    account_id: a.id,
                    folder_id: f.id,
                    name: f.name.clone(),
                    path: f.path.clone(),
                    account: self.account_label(a.id),
                    count: None,
                })
            })
            .collect();
        let every = (targets.len() > 1).then_some(kind);
        self.ask_empty(every, targets, sender);
    }

    /// Ask each folder's server for its count, then the user.
    fn ask_empty(
        &mut self,
        every: Option<FolderKind>,
        mut targets: Vec<EmptyTarget>,
        sender: &ComponentSender<Self>,
    ) {
        if targets.is_empty() {
            return;
        }
        for t in &mut targets {
            let asked = self.workers.get(&t.account_id).is_some_and(|w| {
                w.send(MailRequest::CountFolder {
                    path: t.path.clone(),
                })
                .is_ok()
            });
            if !asked {
                t.count = Some(None);
            }
        }
        self.empty_ask_gen = self.empty_ask_gen.wrapping_add(1);
        let gen = self.empty_ask_gen;
        self.empty_ask = Some(EmptyAsk {
            gen,
            every,
            targets,
        });
        if self.empty_ask.as_ref().is_some_and(EmptyAsk::answered) {
            self.show_empty_ask(sender);
            return;
        }
        let s = sender.clone();
        gtk::glib::timeout_add_local_once(
            std::time::Duration::from_millis(COUNT_WAIT_MS),
            move || s.input(AppMsg::EmptyCountWait(gen)),
        );
    }

    /// The messages the router sends here (see `AppModel::route`).
    pub(super) fn on_empty_count(&mut self, msg: AppMsg, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::FolderCounted {
                account_id,
                path,
                count,
            } => {
                let Some(ask) = self.empty_ask.as_mut() else {
                    return;
                };
                if let Some(t) = ask
                    .targets
                    .iter_mut()
                    .find(|t| t.account_id == account_id && t.path == path && t.count.is_none())
                {
                    t.count = Some(count);
                }
                if ask.answered() {
                    self.show_empty_ask(&sender);
                }
            }
            AppMsg::EmptyCountWait(gen) => {
                if self.empty_ask.as_ref().is_some_and(|a| a.gen == gen) {
                    self.show_empty_ask(&sender);
                }
            }
            _ => unreachable!("not routed to on_empty_count"),
        }
    }

    /// Ask, with whatever counts came back. There is no undo, so always ask.
    fn show_empty_ask(&mut self, sender: &ComponentSender<Self>) {
        let Some(ask) = self.empty_ask.take() else {
            return;
        };
        let (heading, body) = match ask.every {
            None => {
                let t = &ask.targets[0];
                (
                    i18n_f("Empty “{name}”?", &[("name", &t.name)]),
                    one_folder_body(t.count.flatten()),
                )
            }
            Some(kind) => every_account_text(kind, &ask.targets),
        };
        let erase: Vec<(u32, u32, String)> = ask
            .targets
            .into_iter()
            .map(|t| (t.account_id, t.folder_id, t.path))
            .collect();
        let s = sender.clone();
        confirm_destructive(
            Some(self.window.upcast_ref()),
            &heading,
            &body,
            "empty",
            &i18n("Empty"),
            move || {
                for (account_id, folder_id, path) in erase {
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

/// What emptying one folder erases.
fn one_folder_body(count: Option<u32>) -> String {
    match count {
        Some(n) => ni18n_f(
            "{count} message will be erased from the server. This can’t be undone.",
            "{count} messages will be erased from the server. This can’t be undone.",
            n,
            &[("count", &n.to_string())],
        ),
        None => i18n("Every message in it will be erased from the server. This can’t be undone."),
    }
}

/// The heading and body for emptying a kind of folder in every account:
/// the total when every server answered, and each account's own count.
fn every_account_text(kind: FolderKind, targets: &[EmptyTarget]) -> (String, String) {
    let heading = if kind == FolderKind::Trash {
        i18n("Empty Trash in Every Account?")
    } else {
        i18n("Empty Junk in Every Account?")
    };
    let counts: Option<Vec<u32>> = targets.iter().map(|t| t.count.flatten()).collect();
    let lead = match counts {
        Some(counts) => {
            let n = counts.iter().fold(0u32, |sum, c| sum.saturating_add(*c));
            ni18n_f(
                "{count} message will be erased from the server. This can’t be undone.",
                "{count} messages will be erased from the server. This can’t be undone.",
                n,
                &[("count", &n.to_string())],
            )
        }
        None => {
            i18n("Every message in them will be erased from the server. This can’t be undone.")
        }
    };
    let lines: Vec<String> = targets
        .iter()
        .map(|t| match t.count.flatten() {
            Some(n) => ni18n_f(
                "{account}: {count} message",
                "{account}: {count} messages",
                n,
                &[("account", &t.account), ("count", &n.to_string())],
            ),
            None => t.account.clone(),
        })
        .collect();
    (heading, format!("{lead}\n\n{}", lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(account: &str, count: Option<Option<u32>>) -> EmptyTarget {
        EmptyTarget {
            account_id: 1,
            folder_id: 1,
            name: "Trash".into(),
            path: "Trash".into(),
            account: account.into(),
            count,
        }
    }

    #[test]
    fn one_folder_says_how_many() {
        assert!(one_folder_body(Some(47)).starts_with("47 messages will be erased"));
        assert!(one_folder_body(Some(1)).starts_with("1 message will be erased"));
        assert!(one_folder_body(None).starts_with("Every message in it"));
    }

    #[test]
    fn every_account_totals_when_all_answered() {
        let (heading, body) = every_account_text(
            FolderKind::Trash,
            &[target("a@x", Some(Some(40))), target("b@x", Some(Some(7)))],
        );
        assert_eq!(heading, "Empty Trash in Every Account?");
        assert!(body.starts_with("47 messages will be erased"));
        assert!(body.ends_with("a@x: 40 messages\nb@x: 7 messages"));
    }

    #[test]
    fn every_account_without_a_total_when_one_is_missing() {
        let (_, body) = every_account_text(
            FolderKind::Junk,
            &[target("a@x", Some(Some(1))), target("b@x", Some(None))],
        );
        assert!(body.starts_with("Every message in them"));
        assert!(body.ends_with("a@x: 1 message\nb@x"));
    }

    #[test]
    fn answered_waits_for_every_target() {
        let mut ask = EmptyAsk {
            gen: 1,
            every: None,
            targets: vec![target("a@x", None), target("b@x", Some(None))],
        };
        assert!(!ask.answered());
        ask.targets[0].count = Some(Some(3));
        assert!(ask.answered());
    }
}
