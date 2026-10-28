//! The demo mode's stand-ins (HYLKI_DEMO): accounts, tags and filters for
//! screenshots, with no server behind them.

use crate::config::{self, AccountConfig};

/// Whether to serve the built-in sample/demo data (for screenshots). Off unless
/// `HYLKI_DEMO` is set, so removing all real accounts leaves the app blank.
pub(super) fn demo_mode() -> bool {
    std::env::var_os("HYLKI_DEMO").is_some()
}

/// Stand-in [`AccountConfig`]s mirroring the demo backend's three accounts
/// (same names, colors and emoji), so the Accounts window has something to
/// show in demo screenshots.
pub(super) fn demo_account_configs() -> Vec<AccountConfig> {
    let mk = |name: &str, email: &str, color: &str, emoji: Option<&str>| AccountConfig {
        name: name.into(),
        email: email.into(),
        protocol: Default::default(),
        imap_host: format!("imap.{}", email.split('@').nth(1).unwrap_or("example.com")),
        imap_port: 993,
        smtp_host: format!("smtp.{}", email.split('@').nth(1).unwrap_or("example.com")),
        smtp_port: 587,
        username: email.into(),
        // A stand-in secret: the account editor refuses to save an account
        // without one, and the demo's private bus has no keyring to ask.
        password: "demo".into(),
        smtp_separate: false,
        jmap_token: false,
        jmap_generic: false,
        tls_accept_hostname_mismatch: false,
        security: None,
        smtp_username: String::new(),
        smtp_password: String::new(),
        color: Some(color.into()),
        emoji: emoji.map(Into::into),
        avatar: None,
        gravatar: false,
        signature: None,
        signature_html: false,
        signatures: Vec::new(),
        default_signature: None,
        label: None,
        aliases: Vec::new(),
        enabled: true,
        goa_id: None,
        goa_mail_disabled: false,
        goa_enabled_before_mail_disabled: true,
        oauth: false,
        oauth_settings: None,
        oauth_refresh: String::new(),
        push: None,
        folder_roles: Default::default(),
        hidden_folders: Vec::new(),
        folders_seeded: false,
        sent_copy_path: None,
        server_saves_sent: false,
        reply_copies: Default::default(),
        empty_junk_days: 0,
        empty_trash_days: 0,
        clean_lists_days: 0,
        offline_mail: Default::default(),
        offline_attachments: false,
        pgp_key: None,
        in_unified: true,
        folder_sort: None,
        sign_by_default: false,
        plain_subject: false,
        graph_directory: false,
        graph_directory_only: false,
        graph_label_names: false,
    };
    vec![
        mk("Jason M.", "jason@hylki.hyprlab.co", "#3584e4", Some("🚀")),
        mk("Hyprlab", "hello@hyprlab.dev", "#f6d32d", None),
        mk(
            "Jason (Personal)",
            "jason.m@fastmail.com",
            "#f66151",
            Some("🌎"),
        ),
    ]
}

/// The demo's stand-in accounts as last edited in the Accounts panel, or
/// the stock ones. The stand-in secret is not serialised, so it is put
/// back on load (the editor will not save an account without one).
pub(super) fn demo_account_configs_saved() -> Vec<AccountConfig> {
    match config::load_demo_accounts() {
        Some(mut saved) => {
            for a in saved.iter_mut() {
                if a.password.is_empty() {
                    a.password = "demo".into();
                }
            }
            saved
        }
        None => demo_account_configs(),
    }
}

/// The demo's tags (#71): the keywords its sample messages carry, so the
/// Tags section has rows and the row chips have colors.
pub(super) fn demo_tags() -> Vec<config::Tag> {
    let mk = |name: &str, keyword: &str, color: &str| config::Tag {
        name: name.into(),
        keyword: keyword.into(),
        color: color.into(),
    };
    vec![
        mk("Work", "Work", "#1c71d8"),
        mk("To Do", "To_Do", "#e66100"),
        mk("Personal", "Personal", "#2ec27e"),
    ]
}

/// The demo's filter rules, one per account, each filing into that
/// account's custom folder (backend.rs) and listed under All Inboxes. Their
/// matches are chosen to hit only the mail already in those folders: the
/// mock backend never moves anything, so a rule catching inbox mail would
/// only make it vanish from the list.
pub(super) fn demo_filters() -> Vec<config::FilterRule> {
    use config::{FilterField, FilterMatch, FilterRule};
    let mk = |name: &str,
              email: &str,
              field: FilterField,
              matcher: FilterMatch,
              value: &str,
              dest: &str| FilterRule {
        name: name.into(),
        account_email: email.into(),
        field,
        matcher,
        value: value.into(),
        more: Vec::new(),
        any: false,
        dest_path: dest.into(),
        tag: String::new(),
        count_unread: true,
        forward_to: Vec::new(),
        forward_since: 0,
        mark_read: false,
        delete: false,
    };
    vec![
        mk(
            "Substack",
            "jason@hylki.hyprlab.co",
            FilterField::FromAddress,
            FilterMatch::EndsWith,
            "substack.com",
            "Newsletters",
        ),
        mk(
            "Invoices",
            "hello@hyprlab.dev",
            FilterField::Subject,
            FilterMatch::Contains,
            "invoice",
            "Invoices",
        ),
        mk(
            "",
            "jason.m@fastmail.com",
            FilterField::Subject,
            FilterMatch::Contains,
            "order",
            "Orders",
        ),
    ]
}
