//! The Provider picker's table (shared with the first-run wizard) and the
//! rules that map an account to its entry, its brand mark and its name.

use super::*;

/// How an account signs in, chosen via the single Provider dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProviderKind {
    /// Manual IMAP/POP3 + password ("IMAP/POP3 Account").
    Manual,
    /// A known IMAP provider: password auth with auto-filled servers.
    Preset,
    /// Google OAuth (browser sign-in; falls back to GNOME Online Accounts).
    Google,
    /// Microsoft OAuth (browser sign-in).
    Microsoft,
    /// OAuth against a user-entered provider ("Custom (OAuth)…").
    CustomOAuth,
    /// A JMAP server (#245), signed in with a password: the servers are the
    /// user's own, so nothing is pre-filled but the protocol.
    Jmap,
}

/// One entry in the Provider dropdown. It selects both the sign-in method and,
/// for `Preset`, the IMAP/SMTP servers to auto-fill. `hint` is shown as the row's
/// subtitle. Server fields are empty for non-`Preset` kinds (OAuth providers get
/// their servers from `crate::services::oauth::preset`; Manual/Custom are user-entered).
pub(crate) struct Provider {
    pub(super) label: &'static str,
    /// The brand id of its mark (`brand::mark`): "mail", the IMAP tile, for
    /// manual IMAP/POP3; "mail-oauth", the OAuth tile, for custom OAuth.
    pub(super) brand: &'static str,
    pub(super) kind: ProviderKind,
    pub(super) imap_host: &'static str,
    pub(super) imap_port: u16,
    pub(super) smtp_host: &'static str,
    pub(super) smtp_port: u16,
    pub(super) hint: &'static str,
}

impl Provider {
    // Wizard accessors (src/ui/welcome.rs): the fields stay private to this
    // module, which owns the table's meaning.

    /// Google stays out of the wizard's manual form: it is imported from
    /// GNOME Online Accounts on the page before it. Microsoft signs in here
    /// too, with Hylki's own app (#329), for systems whose GNOME Online
    /// Accounts cannot add it.
    pub(crate) fn wizard_listed(&self) -> bool {
        self.kind != ProviderKind::Google
    }
    /// The entries the wizard's form signs in to in the browser: Custom
    /// OAuth, and Microsoft.
    pub(crate) fn wizard_is_oauth(&self) -> bool {
        matches!(
            self.kind,
            ProviderKind::CustomOAuth | ProviderKind::Microsoft
        )
    }
    /// Custom OAuth: its endpoints and client are typed into the form.
    pub(crate) fn wizard_is_custom_oauth(&self) -> bool {
        self.kind == ProviderKind::CustomOAuth
    }
    /// A built-in provider's OAuth client and endpoints, for the wizard's
    /// sign-in (Microsoft).
    pub(crate) fn wizard_builtin_oauth(&self) -> Option<crate::config::OAuthSettings> {
        let name = self
            .oauth_name()
            .filter(|_| self.kind == ProviderKind::Microsoft)?;
        let p = crate::services::oauth::preset(name)?;
        let (client_id, client_secret) = crate::services::oauth::provider_credentials(name);
        Some(crate::config::OAuthSettings {
            auth_url: p.auth_url.to_string(),
            token_url: p.token_url.to_string(),
            client_id,
            client_secret,
            scopes: p.scopes.to_string(),
            redirect_uri: String::new(),
        })
    }
    pub(crate) fn wizard_label(&self) -> &'static str {
        self.label
    }
    /// The plain IMAP/POP3 entry, the wizard's default.
    pub(crate) fn wizard_is_manual(&self) -> bool {
        self.kind == ProviderKind::Manual
    }
    pub(crate) fn wizard_servers(&self) -> (&'static str, u16, &'static str, u16) {
        (
            self.imap_host,
            self.imap_port,
            self.smtp_host,
            self.smtp_port,
        )
    }
    pub(crate) fn wizard_hint(&self) -> &'static str {
        self.hint
    }

    /// A JMAP entry whose server takes an API token rather than the
    /// password (#356).
    pub(crate) fn wizard_token(&self) -> bool {
        self.kind == ProviderKind::Jmap && self.brand == "fastmail"
    }
    /// The generic JMAP entry, as against Fastmail's and Stalwart's.
    pub(crate) fn wizard_jmap_generic(&self) -> bool {
        self.kind == ProviderKind::Jmap && self.brand == "mail-jmap"
    }

    pub(super) fn is_password(&self) -> bool {
        matches!(
            self.kind,
            ProviderKind::Manual | ProviderKind::Preset | ProviderKind::Jmap
        )
    }
    /// The incoming protocol the entry stands for; the manual entry and the
    /// presets leave it to the Incoming Protocol row.
    pub(crate) fn wizard_protocol(&self) -> Protocol {
        match self.kind {
            ProviderKind::Jmap => Protocol::Jmap,
            ProviderKind::Microsoft => Protocol::Graph,
            _ => Protocol::Imap,
        }
    }
    pub(super) fn is_oauth(&self) -> bool {
        !self.is_password()
    }
    /// OAuth preset key for the built-in providers.
    pub(super) fn oauth_name(&self) -> Option<&'static str> {
        match self.kind {
            ProviderKind::Google => Some("google"),
            ProviderKind::Microsoft => Some("microsoft"),
            _ => None,
        }
    }
}

const APP_PW: &str = i18n_noop("Needs an app password, not your login password.");

/// The Provider dropdown, in display order. The plain IMAP/POP3 entry first
/// (the default) with custom OAuth and any JMAP server under it, the
/// generic entries together, then the OAuth providers and the major app-password IMAP
/// providers. IMAP uses SSL/TLS on 993; SMTP uses
/// implicit TLS on 465 or STARTTLS on 587.
pub(crate) const PROVIDERS: &[Provider] = &[
    Provider { label: "IMAP/POP3 Account", brand: "mail", kind: ProviderKind::Manual, imap_host: "", imap_port: 0, smtp_host: "", smtp_port: 0, hint: i18n_noop("Enter your server details manually.") },
    Provider { label: "Custom (OAuth)…", brand: "mail-oauth", kind: ProviderKind::CustomOAuth, imap_host: "", imap_port: 0, smtp_host: "", smtp_port: 0, hint: i18n_noop("Enter your provider's OAuth endpoints, then sign in.") },
    Provider { label: "JMAP Server", brand: "mail-jmap", kind: ProviderKind::Jmap, imap_host: "", imap_port: 443, smtp_host: "", smtp_port: 0, hint: i18n_noop("Your JMAP server's address or session URL. No SMTP settings needed.") },
    Provider { label: "Google (Gmail)", brand: "gmail", kind: ProviderKind::Google, imap_host: "", imap_port: 0, smtp_host: "", smtp_port: 0, hint: i18n_noop("Signs in through your browser. No password needed.") },
    Provider { label: "Microsoft 365 / Outlook", brand: "outlook", kind: ProviderKind::Microsoft, imap_host: "", imap_port: 0, smtp_host: "", smtp_port: 0, hint: i18n_noop("Signs in through your browser. No password needed.") },
    Provider { label: "iCloud", brand: "icloud", kind: ProviderKind::Preset, imap_host: "imap.mail.me.com", imap_port: 993, smtp_host: "smtp.mail.me.com", smtp_port: 587, hint: APP_PW },
    Provider { label: "Yahoo Mail", brand: "yahoo", kind: ProviderKind::Preset, imap_host: "imap.mail.yahoo.com", imap_port: 993, smtp_host: "smtp.mail.yahoo.com", smtp_port: 465, hint: APP_PW },
    Provider { label: "Proton Mail (Bridge)", brand: "proton", kind: ProviderKind::Preset, imap_host: "127.0.0.1", imap_port: 1143, smtp_host: "127.0.0.1", smtp_port: 1025, hint: i18n_noop("Requires Proton Mail Bridge running locally.") },
    Provider { label: "Fastmail", brand: "fastmail", kind: ProviderKind::Preset, imap_host: "imap.fastmail.com", imap_port: 993, smtp_host: "smtp.fastmail.com", smtp_port: 465, hint: APP_PW },
    Provider { label: "Fastmail (JMAP)", brand: "fastmail", kind: ProviderKind::Jmap, imap_host: "https://api.fastmail.com/jmap/session", imap_port: 443, smtp_host: "", smtp_port: 0, hint: i18n_noop("Signs in with an API token made in Fastmail under Privacy & Security, with access to mail and sending.") },
    Provider { label: "Stalwart (JMAP)", brand: "stalwart", kind: ProviderKind::Jmap, imap_host: "", imap_port: 443, smtp_host: "", smtp_port: 0, hint: i18n_noop("Your Stalwart server's address. No SMTP settings needed.") },
    Provider { label: "AOL Mail", brand: "aol", kind: ProviderKind::Preset, imap_host: "imap.aol.com", imap_port: 993, smtp_host: "smtp.aol.com", smtp_port: 465, hint: APP_PW },
    Provider { label: "Zoho Mail", brand: "zoho", kind: ProviderKind::Preset, imap_host: "imap.zoho.com", imap_port: 993, smtp_host: "smtp.zoho.com", smtp_port: 465, hint: "" },
    Provider { label: "GMX", brand: "gmx", kind: ProviderKind::Preset, imap_host: "imap.gmx.com", imap_port: 993, smtp_host: "mail.gmx.com", smtp_port: 587, hint: i18n_noop("Enable POP/IMAP access in GMX settings first.") },
    Provider { label: "Yandex Mail", brand: "yandex", kind: ProviderKind::Preset, imap_host: "imap.yandex.com", imap_port: 993, smtp_host: "smtp.yandex.com", smtp_port: 465, hint: APP_PW },
    Provider { label: "Mail.com", brand: "mailcom", kind: ProviderKind::Preset, imap_host: "imap.mail.com", imap_port: 993, smtp_host: "smtp.mail.com", smtp_port: 587, hint: "" },
];

/// Dropdown index of the "IMAP/POP3 Account" manual entry (the default).
pub(super) fn manual_index() -> u32 {
    PROVIDERS
        .iter()
        .position(|p| p.kind == ProviderKind::Manual)
        .unwrap_or(0) as u32
}

/// The provider entry for a dropdown index (clamped to the manual default).
pub(super) fn provider_at(idx: u32) -> &'static Provider {
    PROVIDERS
        .get(idx as usize)
        .unwrap_or(&PROVIDERS[manual_index() as usize])
}

/// The brand id for a GNOME Online Accounts provider name ("Google",
/// "Microsoft 365"…): the two mail providers GOA offers, else the blue
/// envelope.
pub(crate) fn brand_for_goa(provider: &str) -> &'static str {
    let p = provider.to_ascii_lowercase();
    if p.contains("google") {
        "gmail"
    } else if p.contains("microsoft") || p.contains("outlook") || p.contains("365") {
        "outlook"
    } else {
        "mail"
    }
}

/// The Provider picker's rows: the provider's mark (or the generic
/// envelope) before its name, in the row and in the list that drops down.
/// Shared with the welcome wizard's picker, which lists a subset by the
/// same labels.
pub(crate) fn provider_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
            let bx = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::None);
            bx.append(&gtk::Image::new());
            bx.append(&label);
            item.set_child(Some(&bx));
        }
    });
    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        // By name, not position: the row's own selected-value slot is a
        // list item with no position.
        let name = item
            .item()
            .and_downcast::<gtk::StringObject>()
            .map(|o| o.string().to_string())
            .unwrap_or_default();
        let Some(provider) = PROVIDERS.iter().find(|p| p.label == name) else {
            return;
        };
        let Some(bx) = item.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let Some(old) = bx.first_child() else { return };
        let label = old.next_sibling().and_downcast::<gtk::Label>();
        bx.remove(&old);
        bx.prepend(&crate::desktop::brand::mark(
            provider.brand,
            20,
            crate::desktop::brand::GENERIC_MAIL,
        ));
        if let Some(label) = label {
            label.set_label(&name);
        }
    });
    factory
}

/// Dropdown index of the first provider entry of a given kind (the manual entry
/// if none — shouldn't happen for kinds present in the table).
fn kind_index(kind: ProviderKind) -> u32 {
    PROVIDERS
        .iter()
        .position(|p| p.kind == kind)
        .map(|i| i as u32)
        .unwrap_or_else(manual_index)
}

/// Dropdown index of a JMAP account's entry: Fastmail's for a Fastmail
/// address, the generic one for an account made from it, else Stalwart's.
fn jmap_index(acc: &AccountConfig) -> u32 {
    let brand = jmap_brand(acc);
    PROVIDERS
        .iter()
        .position(|p| p.kind == ProviderKind::Jmap && p.brand == brand)
        .map(|i| i as u32)
        .unwrap_or_else(|| kind_index(ProviderKind::Jmap))
}

/// The mark of a JMAP account: Fastmail's, the JMAP tile for one made from
/// the generic entry, otherwise Stalwart's.
fn jmap_brand(acc: &AccountConfig) -> &'static str {
    if acc.imap_host.to_ascii_lowercase().contains("fastmail") {
        "fastmail"
    } else if acc.jmap_generic {
        "mail-jmap"
    } else {
        "stalwart"
    }
}

/// Dropdown index of the `Preset` provider whose incoming server matches `host`,
/// or the manual entry when nothing matches.
fn preset_index_for_host(host: &str) -> u32 {
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() {
        return manual_index();
    }
    PROVIDERS
        .iter()
        .position(|p| p.kind == ProviderKind::Preset && p.imap_host.eq_ignore_ascii_case(&host))
        .map(|i| i as u32)
        .unwrap_or_else(manual_index)
}

/// Dropdown index reflecting an existing account: its OAuth provider (by token
/// endpoint) for native OAuth accounts, otherwise the matching password provider.
pub(super) fn provider_index_for_account(acc: &AccountConfig) -> u32 {
    if acc.goa_id.is_none() && acc.oauth {
        let kind = match acc.oauth_settings.as_ref() {
            Some(s) if s.token_url.contains("googleapis") => ProviderKind::Google,
            Some(s) if s.token_url.contains("microsoftonline") => ProviderKind::Microsoft,
            _ => ProviderKind::CustomOAuth,
        };
        return kind_index(kind);
    }
    if acc.protocol == Protocol::Jmap {
        return jmap_index(acc);
    }
    preset_index_for_host(&acc.imap_host)
}

/// The Incoming Protocol row's entries, in dropdown order. JMAP is not one:
/// it comes with the JMAP entries in the Provider picker.
const PROTOCOLS: [Protocol; 2] = [Protocol::Imap, Protocol::Pop3];

pub(super) fn protocol_at(idx: u32) -> Protocol {
    PROTOCOLS.get(idx as usize).copied().unwrap_or_default()
}

pub(super) fn protocol_index(protocol: Protocol) -> u32 {
    PROTOCOLS.iter().position(|p| *p == protocol).unwrap_or(0) as u32
}

/// The brand id of the service an existing account is on: Microsoft 365
/// over Graph, an OAuth account by its token endpoint, otherwise by its
/// incoming server (the well-known hosts, then the provider table, whose
/// manual entry gives anything else the blue envelope).
pub(super) fn brand_for_account(acc: &AccountConfig) -> &'static str {
    if acc.is_local() {
        return "local";
    }
    if acc.protocol == Protocol::Graph {
        return "outlook";
    }
    if acc.protocol == Protocol::Jmap {
        return jmap_brand(acc);
    }
    if let Some(s) = acc.oauth_settings.as_ref().filter(|_| acc.oauth) {
        if s.token_url.contains("googleapis") {
            return "gmail";
        }
        if s.token_url.contains("microsoftonline") {
            return "outlook";
        }
    }
    // Native OAuth against anything else: the custom-OAuth envelope.
    if acc.oauth && acc.goa_id.is_none() {
        return "mail-oauth";
    }
    let host = acc.imap_host.trim().to_ascii_lowercase();
    if host.contains("gmail") || host.contains("googlemail") {
        return "gmail";
    }
    if host.contains("outlook")
        || host.contains("office365")
        || host.contains("hotmail")
        || host.contains("live.com")
    {
        return "outlook";
    }
    if host.contains("proton") {
        return "proton";
    }
    match provider_at(preset_index_for_host(&host)).brand {
        "mail" => manual_brand(acc.protocol),
        brand => brand,
    }
}

/// The mark for an account whose servers were entered by hand: its
/// protocol's tile.
pub(super) fn manual_brand(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Jmap => "mail-jmap",
        Protocol::Pop3 => "mail-pop3",
        _ => "mail",
    }
}

/// What the account list's provider mark says on hover: the provider's
/// name as the picker lists it, without the picker's own hints ("sign in",
/// "Bridge"); a plain IMAP/POP3 account names its server instead.
pub(super) fn provider_name(brand: &str, acc: &AccountConfig) -> String {
    match brand {
        "gmail" => i18n("Google (Gmail)"),
        "outlook" => i18n("Microsoft 365 / Outlook"),
        "proton" => i18n("Proton Mail"),
        "mail-oauth" => i18n("Custom OAuth"),
        "local" => i18n("Folders on this computer"),
        "mail" | "mail-pop3" => {
            let host = acc.imap_host.trim();
            if host.is_empty() {
                i18n("IMAP/POP3 account")
            } else {
                i18n_f("IMAP/POP3 account on {host}", &[("host", host)])
            }
        }
        other => PROVIDERS
            .iter()
            .find(|p| p.brand == other)
            .map(|p| i18n(p.label))
            .unwrap_or_else(|| i18n("Mail account")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_host_maps_to_its_own_entry() {
        let idx = preset_index_for_host("imap.mail.me.com");
        assert_eq!(provider_at(idx).label, "iCloud");
        // Case-insensitive.
        assert_eq!(
            preset_index_for_host("IMAP.FASTMAIL.COM"),
            preset_index_for_host("imap.fastmail.com")
        );
        assert_eq!(
            provider_at(preset_index_for_host("imap.fastmail.com")).label,
            "Fastmail"
        );
    }

    #[test]
    fn unknown_or_empty_host_falls_back_to_manual() {
        assert_eq!(preset_index_for_host("mail.example.org"), manual_index());
        assert_eq!(preset_index_for_host(""), manual_index());
        assert_eq!(preset_index_for_host("  "), manual_index());
        assert_eq!(provider_at(manual_index()).kind, ProviderKind::Manual);
    }

    #[test]
    fn removed_password_providers_are_gone() {
        // Gmail/Hotmail no longer work with a password — only via OAuth.
        for p in PROVIDERS {
            if p.kind == ProviderKind::Preset {
                assert_ne!(p.imap_host, "imap.gmail.com");
                assert_ne!(p.imap_host, "outlook.office365.com");
            }
        }
    }

    #[test]
    fn jmap_provider_sets_the_protocol() {
        let p = PROVIDERS
            .iter()
            .find(|p| p.kind == ProviderKind::Jmap && p.brand == "stalwart")
            .expect("a JMAP entry");
        assert!(p.is_password());
        assert!(!p.wizard_token());
        assert_eq!(p.wizard_protocol(), Protocol::Jmap);
        assert!(
            p.imap_host.is_empty(),
            "a self-hosted server has no preset host"
        );
        // The Incoming Protocol row offers IMAP and POP3 only; a JMAP
        // account's comes from its provider.
        assert_eq!(protocol_at(protocol_index(Protocol::Pop3)), Protocol::Pop3);
        assert_eq!(protocol_index(Protocol::Jmap), 0);
        assert_eq!(protocol_at(99), Protocol::Imap);
        let acc = AccountConfig {
            protocol: Protocol::Jmap,
            imap_host: "mail.example.org".into(),
            ..crate::config::blank_account()
        };
        assert_eq!(provider_at(provider_index_for_account(&acc)).label, p.label);
        assert_eq!(brand_for_account(&acc), "stalwart");
        // One made from the generic entry keeps to it, under the JMAP tile.
        let generic = AccountConfig {
            jmap_generic: true,
            ..acc.clone()
        };
        let gp = provider_at(provider_index_for_account(&generic));
        assert!(gp.wizard_jmap_generic());
        assert_eq!(brand_for_account(&generic), "mail-jmap");
        let fm = AccountConfig {
            imap_host: "api.fastmail.com".into(),
            ..acc
        };
        assert_eq!(brand_for_account(&fm), "fastmail");
        // Fastmail's own entry: its session URL, and a token sign-in (#356).
        let fp = provider_at(provider_index_for_account(&fm));
        assert_eq!(fp.label, "Fastmail (JMAP)");
        assert!(fp.wizard_token());
        assert_eq!(fp.wizard_protocol(), Protocol::Jmap);
        assert!(fp.imap_host.starts_with("https://api.fastmail.com/"));
    }

    #[test]
    fn provider_table_is_well_formed() {
        // Distinct labels; presets have sane servers; exactly one Manual entry.
        let mut labels: Vec<&str> = PROVIDERS.iter().map(|p| p.label).collect();
        let n = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), n, "provider labels must be unique");
        assert_eq!(
            PROVIDERS
                .iter()
                .filter(|p| p.kind == ProviderKind::Manual)
                .count(),
            1
        );
        for p in PROVIDERS {
            if p.kind == ProviderKind::Preset {
                assert!(
                    !p.imap_host.is_empty() && !p.smtp_host.is_empty(),
                    "{}",
                    p.label
                );
                assert!(p.imap_port > 0 && p.smtp_port > 0, "{}", p.label);
                // Each preset's host round-trips to its own entry.
                assert_eq!(
                    provider_at(preset_index_for_host(p.imap_host)).label,
                    p.label
                );
            }
        }
    }

    #[test]
    fn brands_for_accounts_and_goa_providers() {
        let blank = crate::config::blank_account();
        let with = |f: &dyn Fn(&mut AccountConfig)| {
            let mut a = blank.clone();
            f(&mut a);
            brand_for_account(&a)
        };
        assert_eq!(with(&|a| a.protocol = Protocol::Graph), "outlook");
        let oauth = |url: &str| OAuthSettings {
            auth_url: String::new(),
            token_url: url.into(),
            client_id: String::new(),
            client_secret: String::new(),
            scopes: String::new(),
            redirect_uri: String::new(),
        };
        assert_eq!(
            with(&|a| {
                a.oauth = true;
                a.oauth_settings = Some(oauth("https://oauth2.googleapis.com/token"));
            }),
            "gmail"
        );
        assert_eq!(
            with(&|a| {
                a.oauth = true;
                a.oauth_settings = Some(oauth("https://login.microsoftonline.com/x"));
            }),
            "outlook"
        );
        assert_eq!(with(&|a| a.oauth = true), "mail-oauth");
        // A GOA OAuth account goes by its host.
        assert_eq!(
            with(&|a| {
                a.oauth = true;
                a.goa_id = Some("g".into());
                a.imap_host = "imap.gmail.com".into();
            }),
            "gmail"
        );
        assert_eq!(
            with(&|a| a.imap_host = "outlook.office365.com".into()),
            "outlook"
        );
        assert_eq!(with(&|a| a.imap_host = "imap.live.com".into()), "outlook");
        assert_eq!(with(&|a| a.imap_host = "proton.local".into()), "proton");
        assert_eq!(
            with(&|a| a.imap_host = " IMAP.mail.me.com ".into()),
            "icloud"
        );
        assert_eq!(with(&|a| a.imap_host = "mail.example.org".into()), "mail");
        assert_eq!(
            with(&|a| {
                a.imap_host = "mail.example.org".into();
                a.protocol = Protocol::Pop3;
            }),
            "mail-pop3"
        );
        assert_eq!(manual_brand(Protocol::Jmap), "mail-jmap");
        assert_eq!(brand_for_goa("Google"), "gmail");
        assert_eq!(brand_for_goa("Microsoft 365"), "outlook");
        assert_eq!(brand_for_goa("Outlook.com"), "outlook");
        assert_eq!(brand_for_goa("IMAP and SMTP"), "mail");
    }

    #[test]
    fn provider_names_for_the_account_list() {
        let blank = crate::config::blank_account();
        assert_eq!(provider_name("gmail", &blank), "Google (Gmail)");
        assert_eq!(provider_name("mail", &blank), "IMAP/POP3 account");
        let hosted = AccountConfig {
            imap_host: " mail.example.org ".into(),
            ..blank.clone()
        };
        assert_eq!(
            provider_name("mail-pop3", &hosted),
            "IMAP/POP3 account on mail.example.org"
        );
        assert_eq!(provider_name("icloud", &blank), "iCloud");
        // The first entry with the brand wins, so a JMAP Fastmail account is
        // named after the IMAP entry (see Suspected bugs).
        assert_eq!(provider_name("fastmail", &blank), "Fastmail");
        assert_eq!(provider_name("nope", &blank), "Mail account");
    }

    #[test]
    fn provider_kinds_and_oauth_accounts_find_their_entries() {
        assert_eq!(kind_index(ProviderKind::Manual), manual_index());
        assert_eq!(
            provider_at(kind_index(ProviderKind::Jmap)).label,
            "JMAP Server"
        );
        assert_eq!(provider_at(999).kind, ProviderKind::Manual);
        let oauth = AccountConfig {
            oauth: true,
            ..crate::config::blank_account()
        };
        assert_eq!(
            provider_at(provider_index_for_account(&oauth)).kind,
            ProviderKind::CustomOAuth
        );
        let goa = AccountConfig {
            goa_id: Some("g".into()),
            imap_host: "imap.mail.yahoo.com".into(),
            ..oauth
        };
        assert_eq!(
            provider_at(provider_index_for_account(&goa)).label,
            "Yahoo Mail"
        );
    }
}
