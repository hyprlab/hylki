//! What every settings file looks like on disk, pinned byte for byte.
//!
//! Each file is written with `toml::to_string_pretty`, so a renamed field, a
//! changed default or a new `skip_serializing_if` changes the text a user's
//! file holds. These tests compare that text with golden files in
//! `testdata/`.

use super::golden::assert_golden;
use super::*;

fn pretty<T: serde::Serialize>(value: &T) -> String {
    toml::to_string_pretty(value).expect("serializes")
}

/// Parses `input`, writes it back, and compares with `testdata/<name>`.
fn pin_round_trip<T>(name: &str, input: &str)
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let value: T = toml::from_str(input).expect("parses");
    assert_golden(&format!("config/testdata/{name}"), &pretty(&value));
}

/// Every field of an account set, the secrets included: the secrets must
/// not reach the file, and the plain-string alias comes back as a table.
const FULL_ACCOUNT: &str = r##"
[[accounts]]
name = "Ada"
email = "ada@example.org"
protocol = "imap"
imap_host = "imap.example.org"
imap_port = 143
smtp_host = "smtp.example.org"
smtp_port = 25
username = "ada"
password = "secret-password"
smtp_separate = true
jmap_token = true
jmap_generic = true
tls_accept_hostname_mismatch = true
smtp_username = "ada-smtp"
smtp_password = "secret-smtp"
color = "#1c71d8"
emoji = "🦊"
avatar = "ada.png"
gravatar = true
signature = "<p>Ada</p>"
signature_html = true
default_signature = 1
label = "Work"
aliases = [
    "Ada Alias <alias@example.org>",
    { identity = "Own <own@example.org>", smtp_host = "smtp.own.org", smtp_port = 465, smtp_username = "own", smtp_password = "secret-alias" },
    { identity = "Same <same@example.org>", smtp_port = 587 },
]
enabled = false
goa_id = "account_1"
goa_mail_disabled = true
goa_enabled_before_mail_disabled = false
oauth = true
oauth_refresh = "secret-refresh"
push = false
hidden_folders = ["Calendar", "Notes"]
folders_seeded = true
sent_copy_path = "INBOX"
server_saves_sent = true
reply_copies = "original_not_inbox"
empty_junk_days = 30
empty_trash_days = 14
clean_lists_days = 7
pgp_key = "ABCDEF"
in_unified = false
folder_sort = "name_desc"
sign_by_default = true
plain_subject = true
graph_directory = true
graph_directory_only = true
graph_label_names = true

[accounts.security]
imap_starttls = true
smtp_starttls = true
imap_accept_invalid_certs = true
smtp_accept_invalid_certs = true
smtp_no_auth = true

[[accounts.signatures]]
name = "Short"
html = "<p>A</p>"

[[accounts.signatures]]
name = "Long"
html = "<p>Ada Lovelace</p>"

[accounts.oauth_settings]
auth_url = "https://auth.example.org"
token_url = "https://token.example.org"
client_id = "client"
client_secret = "client-secret"
scopes = "mail"
redirect_uri = "https://example.org/native"

[accounts.folder_roles]
archive = "Archive"
sent = "Sent Items"

[[accounts]]
name = ""
email = "bare@example.org"
imap_host = ""
username = ""
"##;

#[test]
fn accounts_toml() {
    pin_round_trip::<ConfigFile>("accounts.toml", FULL_ACCOUNT);
    let text = pretty(&toml::from_str::<ConfigFile>(FULL_ACCOUNT).unwrap());
    for secret in [
        "secret-password",
        "secret-smtp",
        "secret-alias",
        "secret-refresh",
    ] {
        assert!(!text.contains(secret), "{secret} reached the file");
    }
}

#[test]
fn privacy_toml() {
    assert_golden(
        "config/testdata/privacy-default.toml",
        &pretty(&PrivacyFile::default()),
    );
    assert_golden(
        "config/testdata/privacy-empty-file.toml",
        &pretty(&toml::from_str::<PrivacyFile>("").unwrap()),
    );
    // The fields left out when empty.
    let text = "compose_font = \"Cantarell\"\ncompose_font_size = 11\n";
    let file: PrivacyFile = toml::from_str(text).unwrap();
    let out = pretty(&file);
    assert!(out.contains("compose_font = \"Cantarell\"\n"));
    assert!(out.contains("compose_font_size = 11\n"));
}

/// Where `PrivacyFile::default()` (a new install) and an empty file (an
/// upgrade from before every key) differ: the flags that record a default
/// already applied, the section placements that `LegacySwitches` decides
/// for an old file, and what is on for a new install only (the toolbar's
/// folded right group, single-key shortcuts). The two default sets must
/// keep exactly this difference.
#[test]
fn privacy_defaults_differ_only_in_the_applied_flags() {
    let fresh = toml::Table::try_from(PrivacyFile::default()).unwrap();
    let empty = toml::Table::try_from(toml::from_str::<PrivacyFile>("").unwrap()).unwrap();
    assert_eq!(
        fresh.keys().collect::<Vec<_>>(),
        empty.keys().collect::<Vec<_>>()
    );
    let differing: Vec<String> = fresh
        .iter()
        .filter(|(k, v)| empty.get(*k) != Some(*v))
        .map(|(k, v)| format!("{k}: default {v}, empty file {}", empty[k]))
        .collect();
    assert_golden(
        "config/testdata/privacy-default-vs-empty.txt",
        &(differing.join("\n") + "\n"),
    );
}

#[test]
fn sound_toml() {
    let sound = NewMailSound {
        enabled: true,
        sound: CUSTOM_SOUND.to_string(),
        when_focused: true,
    };
    assert_golden("config/testdata/sound.toml", &pretty(&sound));
    // Older files have no `when_focused`.
    let old: NewMailSound = toml::from_str("enabled = true\nsound = \"hum\"\n").unwrap();
    assert!(!old.when_focused);
}

#[test]
fn filters_toml() {
    pin_round_trip::<FiltersFile>(
        "filters.toml",
        r#"
[[rules]]
account_email = "ada@example.org"
field = "from_address"
matcher = "contains"
value = "news"
dest_path = "Lists"

[[rules]]
name = "Known people"
account_email = "ada@example.org"
field = "from_address"
matcher = "equals"
value = "ignored"
contacts = "in_contacts"
any = true
dest_path = ""
tag = "$label1"
count_unread = false

[[rules.more]]
field = "subject"
matcher = "starts_with"
value = "invoice, receipt"

[[rules.more]]
field = "from_address"
matcher = "ends_with"
value = "dropped"
contacts = "not_in_contacts"

[[rules.more]]
field = "body"
matcher = "contains"
value = "unsubscribe"
"#,
    );
}

#[test]
fn smart_folders_toml() {
    pin_round_trip::<SmartFoldersFile>(
        "smart_folders.toml",
        r#"
[[folders]]
id = "a1"
name = "Starred unread"

[[folders.conditions]]
field = "starred"

[[folders.conditions]]
field = "unread"

[[folders]]
id = "b2"
name = "Work"
account_email = "ada@example.org"
folder = "inbox"
any = true

[[folders.conditions]]
field = "tagged"
value = "$label1"

[[folders.conditions]]
field = "subject"
matcher = "ends_with"
value = "report"
"#,
    );
}

#[test]
fn tags_toml() {
    pin_round_trip::<TagsFile>(
        "tags.toml",
        r##"
[[tags]]
name = "Important"
keyword = "$label1"
color = "#c01c28"

[[tags]]
name = "Later"
keyword = "later"
color = "#1c71d8"
"##,
    );
}

#[test]
fn sidebar_toml() {
    assert_golden(
        "config/testdata/sidebar-default.toml",
        &pretty(&SidebarFile::default()),
    );
    assert_golden(
        "config/testdata/sidebar-empty-file.toml",
        &pretty(&toml::from_str::<SidebarFile>("").unwrap()),
    );
    pin_round_trip::<SidebarFile>(
        "sidebar.toml",
        r#"
order = ["b@x", "a@x"]
collapsed = ["a@x"]
folders_expanded = ["b@x"]
icon_only = true
tree_collapsed = ["a@x	INBOX/Sub"]
folder_order = ["a@x	Work", "a@x	Home"]
unified_expanded = false
filtered_expanded = false
tags_expanded = false
starred_expanded = true
sent_expanded = true
drafts_expanded = true
archive_expanded = true
junk_expanded = true
trash_expanded = true
snoozed_expanded = true
favorites = ["a@x	INBOX"]
favorites_expanded = false
smart_expanded = false
contacts_expanded = true
contacts_folded = ["book:1"]
filtered_expanded_accounts = ["a@x"]
tags_expanded_accounts = ["b@x"]
"#,
    );
}

/// A missing sidebar.toml and a sidebar.toml without the section keys open
/// different sections, a known quirk.
#[test]
fn sidebar_state_defaults() {
    let missing = SidebarState::default();
    assert!(!missing.unified_expanded);
    assert!(!missing.filtered_expanded);
    assert!(!missing.tags_expanded);
    assert!(!missing.favorites_expanded);
    assert!(!missing.smart_expanded);
    let empty: SidebarFile = toml::from_str("").unwrap();
    assert!(empty.unified_expanded);
    assert!(empty.filtered_expanded);
    assert!(empty.tags_expanded);
    assert!(empty.favorites_expanded);
    assert!(empty.smart_expanded);
}

#[test]
fn window_toml() {
    assert_golden(
        "config/testdata/window-default.toml",
        &pretty(&WindowFile::default()),
    );
    pin_round_trip::<WindowFile>("window.toml", "width = 900\nheight = 700\n");
}

#[test]
fn state_toml() {
    assert_golden(
        "config/testdata/state-default.toml",
        &pretty(&StateFile::default()),
    );
    assert_golden(
        "config/testdata/state-empty-file.toml",
        &pretty(&toml::from_str::<StateFile>("").unwrap()),
    );
    pin_round_trip::<StateFile>(
        "state.toml",
        r#"
mint_keyring_help_dismissed = true
drawer_collapsed = true
drawer_height = 300
drawer_list_view = true
drawer_sort_desc = true
gallery_table_view = true
gallery_thumb_width = 200
gallery_sort = 3
gallery_archive = false
gallery_other = false
gallery_folders = ["1	INBOX/Sub	1", "2	Work	0"]
list_pane_width = 420
contacts_pane_width = 380
about_height = 500
split_reply_height = 240
wizard_completed = true
web_font_fallback = "org.gnome.Platform//49"
last_account = "a@x"
app_icon_generation = 4
app_icon = "blue"
recent_fonts = [{ name = "Cantarell" }, { name = "Source Code Pro", mono = true }]

[fetch_modes]
"a@x" = "headers"
"b@x" = "no-previews"

[last_view]
email = "a@x"
path = "INBOX"

[unread_seen]
"a@x" = 12
"#,
    );
}

#[test]
fn toolbar_toml() {
    let file = ToolbarFile::of(&ReaderToolbar::default());
    assert_golden("config/testdata/toolbar-default.toml", &pretty(&file));
}

#[test]
fn focus_toml() {
    assert_golden(
        "config/testdata/focus-default.toml",
        &pretty(&FocusMode::default()),
    );
    assert_eq!(
        toml::from_str::<FocusMode>("").unwrap(),
        FocusMode::default(),
        "an empty focus.toml reads as the default"
    );
}

#[test]
fn unsubscribed_and_invites_toml() {
    pin_round_trip::<UnsubscribedFile>(
        "unsubscribed.toml",
        "[[lists]]\nkey = \"<list.example.org>\"\nname = \"News\"\nat = 1700000000\n\n\
         [[lists]]\nkey = \"bare@example.org\"\n",
    );
    pin_round_trip::<InviteAnswerFile>(
        "invites.toml",
        "[[answers]]\nkey = \"uid-1\"\nstatus = \"ACCEPTED\"\nat = 1700000000\nsummary = \"Standup\"\n\n\
         [[answers]]\nkey = \"uid-2/20240101T090000Z\"\nstatus = \"DECLINED\"\n",
    );
    pin_round_trip::<ReceiptAnswerFile>(
        "receipts.toml",
        "[[answers]]\nkey = \"abc@example.org\"\nsent = true\nat = 1700000000\n\n\
         [[answers]]\nkey = \"def@example.org\"\n",
    );
    pin_round_trip::<ReadByFile>(
        "read_by.toml",
        "asked = [\"abc@example.org\"]\n\n\
         [[read]]\nkey = \"abc@example.org\"\nby = \"ann@example.org\"\nat = 1700000000\n",
    );
}

#[test]
fn settings_bundle() {
    let bundle = SettingsBundle {
        version: 1,
        privacy: PrivacyFile::default(),
        accounts: toml::from_str::<ConfigFile>(FULL_ACCOUNT).unwrap().accounts,
        filters: Vec::new(),
        tags: vec![Tag {
            name: "Later".into(),
            keyword: "later".into(),
            color: "#1c71d8".into(),
        }],
        sidebar: Some(SidebarFile::default()),
        window: Some(WindowFile::default()),
        state: None,
        cloud: None,
        spell_words: Some(
            [("en_US".to_string(), vec!["Hylki".to_string()])]
                .into_iter()
                .collect(),
        ),
    };
    assert_golden("config/testdata/bundle.toml", &pretty(&bundle));
}

/// The keyring keys secrets are stored under. A changed key loses every
/// stored password.
#[test]
fn keyring_keys() {
    assert_eq!(smtp_key("Ada@Example.org"), "smtp:Ada@Example.org");
    assert_eq!(
        alias_smtp_key("Ada@Example.org", "Alias@Example.ORG"),
        "smtp-alias:Ada@Example.org:alias@example.org"
    );
    assert_eq!(oauth_key("Ada@Example.org"), "oauth:Ada@Example.org");
    assert_eq!(KEYRING_SERVICE, "co.hyprlab.Hylki");
}

/// state.toml's fetch modes: every combination, and its name, read back.
#[test]
fn fetch_mode_names() {
    let all = [(true, false), (false, false), (true, true), (false, true)];
    let names: Vec<_> = all.iter().map(|&(e, p)| fetch_mode_name(e, p)).collect();
    assert_eq!(
        names,
        [
            None,
            Some("headers"),
            Some("no-previews"),
            Some("headers-no-previews")
        ]
    );
    for (mode, name) in all.iter().zip(names) {
        assert_eq!(fetch_mode_from_name(name), *mode);
    }
    assert_eq!(fetch_mode_from_name(Some("newer-mode")), (true, false));
}
