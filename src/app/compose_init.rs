//! Setting up a composer: the From identities it offers and which one it opens
//! with.

use std::collections::HashMap;

use adw::prelude::*;
use relm4::prelude::*;

use crate::config;
use crate::config::{split_identity, AccountConfig};
use crate::i18n::i18n;
use crate::ui::compose::{Compose, ComposeInit, ComposeOutput};
use crate::ui::compose::{ComposeAccount, ComposePrefill};
use crate::ui::preferences::PrefOutput;

use super::model::AppModel;
use super::msg::AppMsg;
use super::palette::palette_key;

/// Selectable "from" identities, in display order: each account, then
/// one entry per send-as alias it defines (#34) — same transport and
/// signature, a different From on the wire. Built from the CONFIG, not
/// the live account list: launched cold by a mailto/file hand-off
/// (Nautilus's "Send by email", #105) the composer opens before any
/// worker has connected, and the live list is still empty — which
/// hid the From row entirely.
/// The demo's stand-in accounts count here too, so a demo reply
/// carries a signature like a real one.
fn compose_identities(
    account_order: &[String],
    config: &[AccountConfig],
    server_identities: &HashMap<u32, Vec<(String, String)>>,
) -> Vec<ComposeAccount> {
    accounts_in_order(account_order, config)
        .iter()
        .flat_map(|email| account_identities(email, config, server_identities))
        .collect()
}

/// The enabled accounts' addresses, in sidebar order, then any the order
/// does not list. On This Computer (#552) sends nothing and is left out.
fn accounts_in_order(account_order: &[String], config: &[AccountConfig]) -> Vec<String> {
    let sends = |c: &&AccountConfig| c.enabled && !c.is_local();
    let mut emails: Vec<String> = Vec::new();
    for email in account_order {
        if config.iter().filter(sends).any(|c| &c.email == email) && !emails.contains(email) {
            emails.push(email.clone());
        }
    }
    for c in config.iter().filter(sends) {
        if !emails.contains(&c.email) {
            emails.push(c.email.clone());
        }
    }
    emails
}

/// One account's identities in the From list: its own address, its
/// aliases, then its server's own identities. None for an address no
/// account has.
fn account_identities(
    email: &str,
    config: &[AccountConfig],
    server_identities: &HashMap<u32, Vec<(String, String)>>,
) -> Vec<ComposeAccount> {
    let Some((idx, cfg)) = config.iter().enumerate().find(|(_, c)| c.email == email) else {
        return Vec::new();
    };
    let id = idx as u32 + 1;
    let label = if cfg.name.trim().is_empty() {
        cfg.email.clone()
    } else {
        format!("{} <{}>", cfg.name, cfg.email)
    };
    let cfg = Some(cfg);
    let (signatures, default) = cfg.map(|c| c.signature_set()).unwrap_or_default();
    let signature = default
        .map(|i| signatures[i].html.clone())
        .unwrap_or_default();
    let pgp_key = cfg.and_then(|c| c.pgp_key.clone());
    let sign_default = cfg.is_some_and(|c| c.sign_by_default);
    let mut identities = vec![ComposeAccount {
        id,
        label,
        signature: signature.clone(),
        signatures: signatures.clone(),
        email: email.to_string(),
        pgp_key: pgp_key.clone(),
        sign_default,
        alias_from: None,
    }];
    for alias in cfg.map(|c| c.aliases.as_slice()).unwrap_or_default() {
        let (name, addr) = split_identity(&alias.identity);
        if addr.is_empty() {
            continue;
        }
        let display = if name.is_empty() {
            addr.clone()
        } else {
            format!("{name} <{addr}>")
        };
        identities.push(ComposeAccount {
            id,
            label: display.clone(),
            signature: signature.clone(),
            signatures: signatures.clone(),
            email: addr,
            pgp_key: pgp_key.clone(),
            sign_default,
            alias_from: Some(display),
        });
    }
    // A JMAP server's own identities (#346), unless the account
    // already lists the address. A catch-all (`*@domain`) is no
    // address to send from; typing one into From uses it.
    for (name, addr) in server_identities
        .get(&id)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if addr.starts_with("*@")
            || identities
                .iter()
                .any(|i| i.email.eq_ignore_ascii_case(addr))
        {
            continue;
        }
        let display = if name.is_empty() || name.eq_ignore_ascii_case(addr) {
            addr.clone()
        } else {
            format!("{name} <{addr}>")
        };
        identities.push(ComposeAccount {
            id,
            label: display.clone(),
            signature: signature.clone(),
            signatures: signatures.clone(),
            email: addr.clone(),
            pgp_key: pgp_key.clone(),
            sign_default,
            alias_from: Some(display),
        });
    }
    identities
}

/// Default identity: for a reply, whichever of the account's addresses
/// the original was sent to — mail to an alias is answered as the alias.
/// For a new message, the default sender chosen in Settings (#157), if
/// it is one of this account's addresses. Otherwise (and when nothing
/// matches) the account's own address.
/// A reply to a `+tag` address of one of the account's identities
/// (#347) answers from that address, as typed, through the identity.
///
/// Returns the index into `accounts`; a `+tag` reply also sets
/// `prefill.custom_from`.
fn choose_identity(
    accounts: &[ComposeAccount],
    account_id: u32,
    prefill: &mut ComposePrefill,
) -> usize {
    let plus = (prefill.custom_from.trim().is_empty())
        .then(|| {
            crate::worker::parse_recipients(&prefill.reply_addressed_to)
                .into_iter()
                .find_map(|(_, addr)| {
                    let (user, host) = addr.split_once('@')?;
                    let (base, _tag) = user.split_once('+')?;
                    let plain = format!("{base}@{host}");
                    let i = accounts
                        .iter()
                        .position(|c| c.id == account_id && c.email.eq_ignore_ascii_case(&plain))?;
                    let (name, _) = crate::config::split_identity(
                        &accounts[i]
                            .alias_from
                            .clone()
                            .unwrap_or_else(|| accounts[i].label.clone()),
                    );
                    Some((i, crate::worker::format_recipient(&name, &addr)))
                })
        })
        .flatten();
    if let Some((_, from)) = &plus {
        prefill.custom_from = from.clone();
    }
    let hay = prefill.reply_addressed_to.to_lowercase();
    let want = prefill.from_address.trim().to_lowercase();
    // An account with no From of its own (On This Computer, #552) answers
    // from whichever account the original was addressed to.
    let own = accounts.iter().any(|c| c.id == account_id);
    let by_reply = || {
        (!hay.is_empty())
            .then(|| {
                accounts.iter().position(|c| {
                    (c.id == account_id || !own) && hay.contains(c.email.to_lowercase().as_str())
                })
            })
            .flatten()
    };
    plus.map(|(i, _)| i)
        .or_else(by_reply)
        .or_else(|| {
            (!want.is_empty())
                .then(|| {
                    accounts
                        .iter()
                        .position(|c| c.id == account_id && c.email.to_lowercase() == want)
                })
                .flatten()
        })
        .or_else(|| {
            accounts
                .iter()
                .position(|c| c.id == account_id && c.alias_from.is_none())
        })
        .unwrap_or(0)
}

impl AppModel {
    /// The account and identity a NEW message opens from (#157). With a
    /// default sender chosen in Settings → Composing, that identity — an
    /// account's own address or one of its aliases — as long as its account
    /// is still enabled; otherwise `fallback` (the open folder's or open
    /// message's account, as before) from its own address. Replies never
    /// come through here: they answer from the address written to.
    pub(super) fn new_message_from(
        &self,
        fallback: u32,
        mut prefill: ComposePrefill,
    ) -> (u32, ComposePrefill) {
        let want = self.compose_default_from.trim();
        if want.is_empty() {
            return (fallback, prefill);
        }
        let owner = self.config.iter().enumerate().find(|(_, c)| {
            c.enabled
                && (c.email.eq_ignore_ascii_case(want)
                    || c.aliases.iter().any(|al| {
                        crate::config::split_identity(&al.identity)
                            .1
                            .eq_ignore_ascii_case(want)
                    }))
        });
        match owner {
            Some((idx, _)) => {
                prefill.from_address = want.to_string();
                (idx as u32 + 1, prefill)
            }
            None => (fallback, prefill),
        }
    }

    /// Assemble the `ComposeInit` for a composer (from-accounts + signatures,
    /// autocomplete suggestions, a fresh id, host mode).
    pub(super) fn build_compose_init(
        &mut self,
        account_id: u32,
        prefill: ComposePrefill,
        windowed: bool,
        can_toggle: bool,
    ) -> (u32, ComposeInit) {
        let accounts = compose_identities(
            &self.account_order,
            self.effective_config(),
            &self.server_identities,
        );
        let mut prefill = prefill;
        let selected = choose_identity(&accounts, account_id, &mut prefill);
        prefill.receipt.get_or_insert(self.request_receipts);

        // A reply or forward to plain-text mail answers in kind when the
        // setting says so (#436); anything else starts in the chosen format.
        let format = if self.follow_plain && prefill.original_plain {
            config::ComposeFormat::Plain
        } else {
            self.compose_format
        };
        // The user's own addresses go last in the recipient suggestions.
        let own: Vec<(String, String)> = self
            .config
            .iter()
            .map(|c| (c.name.clone(), c.email.clone()))
            .collect();
        let id = self.next_compose_id;
        self.next_compose_id += 1;
        let init = ComposeInit {
            compose_id: id,
            prefill,
            accounts,
            selected,
            own_addresses: own,
            windowed,
            can_toggle,
            compact: false,
            decorations: true,
            format,
            signature_position: self.signature_position,
        };
        (id, init)
    }

    /// Launch a `Compose` component, forwarding its outputs into `AppMsg`.
    pub(super) fn spawn_compose(
        &self,
        init: ComposeInit,
        sender: &ComponentSender<Self>,
    ) -> Controller<Compose> {
        Compose::builder()
            .launch(init)
            .forward(sender.input_sender(), |out| match out {
                ComposeOutput::Send { id, message } => AppMsg::SendMessage {
                    from: id,
                    out: message,
                },
                ComposeOutput::SaveDraft(msg) => AppMsg::SaveDraftMessage(msg),
                ComposeOutput::SaveTemplate(msg) => AppMsg::SaveTemplateMessage(msg),
                ComposeOutput::AutoSave { id, message } => AppMsg::AutoSaveDraft { id, message },
                ComposeOutput::DiscardAutosave { id, origin } => {
                    AppMsg::DiscardAutosave { id, origin }
                }
                ComposeOutput::DeleteDraft { id, origin } => AppMsg::DeleteDraft { id, origin },
                ComposeOutput::ToggleWindow(id) => AppMsg::ComposeToggleWindow(id),
                ComposeOutput::Close(id) => AppMsg::ComposeClosed(id),
                ComposeOutput::History { id, undo, redo } => {
                    AppMsg::ComposeHistory { id, undo, redo }
                }
                ComposeOutput::SetSpellcheckLangs(langs) => {
                    AppMsg::Pref(PrefOutput::SetSpellcheckLangs(langs))
                }
                ComposeOutput::DirectoryError(text) => AppMsg::DirectoryError(text),
                ComposeOutput::ShowContacts => AppMsg::ShowContactsPage,
            })
    }

    /// Host a compose pane in a fresh standalone window, transient for the app.
    pub(super) fn compose_window_host(
        &self,
        content: &impl IsA<gtk::Widget>,
        id: u32,
        sender: &ComponentSender<Self>,
    ) -> adw::Window {
        let win = adw::Window::builder()
            .modal(false)
            // Room for the full compose toolbar (Cancel, Save Draft, the
            // action icons, Send) before it folds into its ⋯ menu.
            .default_width(720)
            .default_height(760)
            .title(i18n("New Message"))
            .transient_for(&self.window)
            .build();
        win.set_content(Some(content));
        win.add_controller(palette_key(sender));
        win.add_controller(crate::ui::close_key());
        let s = sender.input_sender().clone();
        win.connect_close_request(move |_| {
            let _ = s.send(AppMsg::ComposeCloseRequested(id));
            gtk::glib::Propagation::Stop
        });
        win.connect_is_active_notify(|w| {
            if w.is_active() {
                crate::desktop::notify::withdraw_compose_ready();
            }
        });
        win.present();
        win
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::demo::demo_account_configs;

    fn identities_fixture() -> Vec<ComposeAccount> {
        let mut config = demo_account_configs();
        config.truncate(2);
        config[0].email = "a@x.com".into();
        config[0].name = "Ann".into();
        config[0].aliases = vec![
            toml::from_str("identity = \"Bea <ba@x.com>\"").unwrap(),
            toml::from_str("identity = \"Nobody <>\"").unwrap(),
        ];
        config[1].email = "c@y.org".into();
        config[1].name = " ".into();
        let mut server = HashMap::new();
        server.insert(
            1,
            vec![
                ("".to_string(), "*@x.com".to_string()),
                ("".to_string(), "BA@x.com".to_string()),
                ("Ann".to_string(), "info@x.com".to_string()),
            ],
        );
        server.insert(2, vec![("c@y.org".to_string(), "c2@y.org".to_string())]);
        compose_identities(&["c@y.org".to_string()], &config, &server)
    }

    #[test]
    fn compose_identities_follow_the_sidebar_order_then_aliases() {
        let ids = identities_fixture();
        let rows: Vec<(u32, &str, &str, Option<&str>)> = ids
            .iter()
            .map(|c| {
                (
                    c.id,
                    c.label.as_str(),
                    c.email.as_str(),
                    c.alias_from.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                (2, "c@y.org", "c@y.org", None),
                (
                    2,
                    "c@y.org <c2@y.org>",
                    "c2@y.org",
                    Some("c@y.org <c2@y.org>")
                ),
                (1, "Ann <a@x.com>", "a@x.com", None),
                (1, "Bea <ba@x.com>", "ba@x.com", Some("Bea <ba@x.com>")),
                (
                    1,
                    "Ann <info@x.com>",
                    "info@x.com",
                    Some("Ann <info@x.com>")
                ),
            ]
        );
    }

    /// B5 in the map: the reply identity is found by substring, so mail to
    /// the alias ba@x.com is answered from the account a@x.com, which comes
    /// first and is contained in it.
    #[test]
    fn reply_identity_is_chosen_by_substring_of_the_recipients() {
        let ids = identities_fixture();
        let mut reply = ComposePrefill {
            reply_addressed_to: "Bea <ba@x.com>, ".into(),
            ..Default::default()
        };
        assert_eq!(choose_identity(&ids, 1, &mut reply), 2);
        assert!(reply.custom_from.is_empty());
        let mut info = ComposePrefill {
            reply_addressed_to: "info@x.com".into(),
            ..Default::default()
        };
        assert_eq!(choose_identity(&ids, 1, &mut info), 4);
        let mut chosen = ComposePrefill {
            from_address: " BA@x.com ".into(),
            ..Default::default()
        };
        assert_eq!(choose_identity(&ids, 1, &mut chosen), 3);
        let mut plain = ComposePrefill::default();
        assert_eq!(choose_identity(&ids, 1, &mut plain), 2);
        assert_eq!(choose_identity(&ids, 9, &mut plain), 0);
    }

    #[test]
    fn a_reply_to_a_plus_address_keeps_it_as_the_from() {
        let ids = identities_fixture();
        let mut reply = ComposePrefill {
            reply_addressed_to: "ba+lists@x.com".into(),
            ..Default::default()
        };
        assert_eq!(choose_identity(&ids, 1, &mut reply), 3);
        assert_eq!(reply.custom_from, "Bea <ba+lists@x.com>");
        let mut typed = ComposePrefill {
            reply_addressed_to: "ba+lists@x.com".into(),
            custom_from: "Me <me@x.com>".into(),
            ..Default::default()
        };
        assert_eq!(choose_identity(&ids, 1, &mut typed), 2);
        assert_eq!(typed.custom_from, "Me <me@x.com>");
    }

    /// On This Computer (#552) sends nothing: it offers no From, and a
    /// reply to mail filed there answers from the account it was sent to.
    #[test]
    fn mail_on_this_computer_is_answered_from_the_account_it_came_to() {
        let mut config = vec![
            crate::config::local_account(),
            crate::config::blank_account(),
            crate::config::blank_account(),
        ];
        config[1].email = "a@x.com".into();
        config[2].email = "c@y.org".into();
        let ids = compose_identities(&[], &config, &HashMap::new());
        let emails: Vec<(u32, &str)> = ids.iter().map(|c| (c.id, c.email.as_str())).collect();
        assert_eq!(emails, [(2, "a@x.com"), (3, "c@y.org")]);
        let mut reply = ComposePrefill {
            reply_addressed_to: "C <c@y.org>".into(),
            ..Default::default()
        };
        assert_eq!(choose_identity(&ids, 1, &mut reply), 1);
        let mut fresh = ComposePrefill::default();
        assert_eq!(choose_identity(&ids, 1, &mut fresh), 0);
    }
}
