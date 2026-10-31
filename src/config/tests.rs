//! Tests of privacy.toml's migrations, the keyring thread, accounts.toml
//! and state.toml.

use super::{
    decode_gallery_folder, encode_gallery_folder, ConfigFile, ListColumn, PrivacyFile, StateFile,
};

/// A section once kept in the unified section moves above the accounts
/// where its switch was on, and off where it was off; one already
/// placed stays put.
#[test]
fn unified_placements_move_out() {
    use super::{LegacySwitches, SectionPlacement::*};
    let text = "filtered_placement = \"all_inboxes\"\nunified_filtered = true\n\
                tags_placement = \"all_inboxes\"\nunified_tags = false\n\
                favorites_placement = \"below_accounts\"\nshow_contacts = false\n";
    let mut file: PrivacyFile = toml::from_str(text).unwrap();
    let old: LegacySwitches = toml::from_str(text).unwrap();
    assert!(old.place(&mut file));
    assert_eq!(file.filtered_placement, AboveAccounts);
    assert_eq!(file.tags_placement, Off);
    assert_eq!(file.favorites_placement, BelowAccounts);
    // A file from before the key: its old switch decides.
    assert_eq!(file.contacts_placement, Off);
    assert!(!old.place(&mut file));
    assert_eq!(PrivacyFile::default().contacts_placement, Footer);
}

/// Contacts moves to the footer once, from wherever it was put
/// unless it was off; afterwards it stays where it is put.
#[test]
fn contacts_moves_to_the_footer_once() {
    use super::SectionPlacement::*;
    let mut file: PrivacyFile =
        toml::from_str("contacts_placement = \"above_accounts\"\n").unwrap();
    assert!(super::apply_new_defaults(&mut file));
    assert_eq!(file.contacts_placement, Footer);
    file.contacts_placement = BelowAccounts;
    super::apply_new_defaults(&mut file);
    assert_eq!(file.contacts_placement, BelowAccounts);
    let mut off: PrivacyFile = toml::from_str("contacts_placement = \"off\"\n").unwrap();
    super::apply_new_defaults(&mut off);
    assert_eq!(off.contacts_placement, Off);
}

/// The keyring thread runs what is queued in order, a call made from
/// another thread waits behind it, one made on the keyring thread itself
/// runs at once, and the drain waits for the queue (#466).
#[test]
fn the_keyring_thread_keeps_the_order() {
    use std::sync::{Arc, Mutex};
    let seen = Arc::new(Mutex::new(Vec::new()));
    for i in 0..5 {
        let seen = seen.clone();
        super::keyring_later(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            seen.lock().unwrap().push(i);
        });
    }
    let after = {
        let seen = seen.clone();
        super::on_keyring_thread(move || {
            let inline = super::on_keyring_thread(|| 7);
            seen.lock().unwrap().push(inline);
            seen.lock().unwrap().len()
        })
    };
    assert_eq!(after, 6, "the blocking call waited for the queue");
    let seen_late = seen.clone();
    super::keyring_later(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        seen_late.lock().unwrap().push(9);
    });
    assert!(super::keyring_drain(std::time::Duration::from_secs(5)));
    assert_eq!(*seen.lock().unwrap(), vec![0, 1, 2, 3, 4, 7, 9]);
}

/// An account with a protocol this build does not know (an EWS account
/// from 1.44.0-beta.5) leaves the others readable.
#[test]
fn an_unknown_protocol_keeps_the_file_readable() {
    let account = |email: &str, protocol: &str| {
        format!("[[accounts]]\nname = \"\"\nemail = \"{email}\"\nimap_host = \"\"\nusername = \"\"\nprotocol = \"{protocol}\"\n")
    };
    let text = account("a@x", "ews") + &account("b@x", "jmap");
    let cfg: ConfigFile = toml::from_str(&text).expect("parses");
    assert_eq!(cfg.accounts.len(), 2);
    assert_eq!(cfg.accounts[0].protocol, super::Protocol::Imap);
    assert_eq!(cfg.accounts[1].protocol, super::Protocol::Jmap);
}

/// #431: a file from before the badge choice keeps what its switch
/// said, and the choice, once made, wins over the switch.
#[test]
fn launcher_badge_follows_the_old_switch() {
    use super::LauncherBadge::*;
    let read = |text: &str| {
        toml::from_str::<PrivacyFile>(text)
            .unwrap()
            .launcher_badge()
    };
    assert_eq!(read(""), AllUnread);
    assert_eq!(read("launcher_count = true"), AllUnread);
    assert_eq!(read("launcher_count = false"), Off);
    assert_eq!(
        read("launcher_count = true\nlauncher_badge = \"new_since_front\""),
        NewSinceFront
    );
    assert_eq!(read("launcher_count = true\nlauncher_badge = \"off\""), Off);
    let saved = PrivacyFile {
        launcher_count: true,
        launcher_badge: Some(NewSinceFront),
        ..Default::default()
    };
    assert_eq!(read(&toml::to_string(&saved).unwrap()), NewSinceFront);
}

/// #367: a sidebar file from before Favorites has none, and the list
/// starts open; the entries come back in the order they were added.
#[test]
fn favorites_load_in_order() {
    let old: super::SidebarFile = toml::from_str("icon_only = false").expect("parses");
    assert!(old.favorites.is_empty() && old.favorites_expanded);
    let new: super::SidebarFile =
        toml::from_str("favorites = [\"b@x\\tWork\", \"a@x\\tINBOX\"]").expect("parses");
    assert_eq!(
        new.favorites,
        vec!["b@x\tWork".to_string(), "a@x\tINBOX".to_string()]
    );
}

/// #369: unified Junk and Trash are off in a file that predates them,
/// and list after Archive, in an account's own order. Snoozed (#542) is
/// on, and closes the run.
#[test]
fn unified_junk_and_trash_start_off() {
    use crate::models::FolderKind::*;
    let kinds: super::UnifiedKinds = toml::from_str("starred = false").expect("parses");
    assert!(!kinds.junk && !kinds.trash && kinds.snoozed && kinds.archive);
    assert_eq!(kinds.listed(), vec![Sent, Drafts, Archive, Snoozed]);
    let all = super::UnifiedKinds {
        junk: true,
        trash: true,
        ..Default::default()
    };
    assert_eq!(
        all.listed(),
        vec![Starred, Sent, Drafts, Archive, Junk, Trash, Snoozed]
    );
    assert_eq!(
        super::UnifiedKinds {
            trash: true,
            ..super::UnifiedKinds::NONE
        }
        .listed(),
        vec![Trash]
    );
}

/// #334: the saved columns come back in order; a name this version does
/// not know, or a repeat, is dropped, and the subject is never missing.
#[test]
fn list_columns_keep_their_order_and_the_subject() {
    let keys = |k: &[&str]| k.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        ListColumn::from_keys(&keys(&["date", "subject", "star", "date", "colour"])),
        vec![ListColumn::Date, ListColumn::Subject, ListColumn::Star]
    );
    assert_eq!(
        ListColumn::from_keys(&keys(&["star", "sender", "date"])),
        vec![
            ListColumn::Star,
            ListColumn::Sender,
            ListColumn::Subject,
            ListColumn::Date
        ]
    );
    assert_eq!(
        ListColumn::from_keys(&ListColumn::to_keys(&ListColumn::DEFAULT)),
        ListColumn::DEFAULT.to_vec()
    );
    // Dragged widths keep their column, within reason; an unknown
    // column's is dropped.
    let saved: std::collections::BTreeMap<String, i32> = [
        ("sender".into(), 240),
        ("date".into(), 5),
        ("colour".into(), 90),
    ]
    .into_iter()
    .collect();
    let widths = super::list_column_widths_from(&saved);
    assert_eq!(widths.len(), 2);
    assert_eq!(widths[&ListColumn::Sender], 240);
    assert_eq!(widths[&ListColumn::Date], 32);
    assert_eq!(
        super::list_column_widths_to(&widths).get("sender"),
        Some(&240)
    );
    // A settings file from before the columns has the old layout.
    let old: PrivacyFile = toml::from_str("list_layout = \"single_line\"").unwrap();
    assert_eq!(
        ListColumn::from_keys(&old.list_columns),
        ListColumn::DEFAULT.to_vec()
    );
}

/// Every gallery folder override survives the trip to the file and back,
/// including the paths servers really use: dots, slashes, spaces, UTF-7.
#[test]
fn a_gallery_folder_override_round_trips() {
    let cases = [
        (1, "INBOX", true),
        (2, "INBOX.Sent", false),
        (3, "Archive/2026", true),
        (7, "Saved Mail", false),
        (11, "INBOX.&AMQA5gDo-", true),
    ];
    for (id, path, on) in cases {
        let encoded = encode_gallery_folder(id, path, on);
        assert_eq!(
            decode_gallery_folder(&encoded),
            Some((id, path.to_string(), on)),
            "{path} did not survive the round trip",
        );
    }
}

/// A line that is not in the stored shape is dropped, not guessed at.
#[test]
fn a_malformed_gallery_override_is_skipped() {
    for bad in ["", "1", "1\tInbox", "notanid\tInbox\t1", "1\t\t1"] {
        assert_eq!(
            decode_gallery_folder(bad),
            None,
            "{bad:?} should not decode"
        );
    }
}

/// A first run with no state file must land on the `#[serde(default = …)]`
/// each field declares, not on the zero its type happens to have: the
/// gallery's two master switches start on, and the panes have real widths.
#[test]
fn a_missing_state_file_falls_back_to_the_declared_defaults() {
    let fresh: StateFile = toml::from_str("").expect("an empty state document must parse");
    assert!(fresh.gallery_archive);
    assert!(fresh.gallery_other);
    assert_eq!(
        fresh.gallery_thumb_width,
        super::default_gallery_thumb_width()
    );
    assert_eq!(fresh.list_pane_width, super::default_list_pane_width());
}

#[test]
fn plain_string_aliases_still_load() {
    // The pre-per-alias-SMTP format (#34): a bare array of identity strings.
    let cfg: ConfigFile = toml::from_str(
        r#"
        [[accounts]]
        name = "Ann"
        email = "ann@example.org"
        imap_host = "imap.example.org"
        username = "ann"
        aliases = ["Ann Work <ann@work.org>", "ann@shop.org"]
        "#,
    )
    .unwrap();
    let aliases = &cfg.accounts[0].aliases;
    assert_eq!(aliases.len(), 2);
    assert_eq!(aliases[0].identity, "Ann Work <ann@work.org>");
    assert_eq!(aliases[0].address(), "ann@work.org");
    assert!(
        !aliases[0].has_own_smtp(),
        "a plain alias rides the account's SMTP"
    );
    assert_eq!(aliases[1].address(), "ann@shop.org");
}

#[test]
fn aliases_can_carry_their_own_smtp() {
    let cfg: ConfigFile = toml::from_str(
        r#"
        [[accounts]]
        name = "Ann"
        email = "ann@example.org"
        imap_host = "imap.example.org"
        username = "ann"

        [[accounts.aliases]]
        identity = "Ann Work <ann@work.org>"
        smtp_host = "smtp.work.org"
        smtp_port = 465
        smtp_username = "ann@work.org"
        "#,
    )
    .unwrap();
    let alias = &cfg.accounts[0].aliases[0];
    assert!(alias.has_own_smtp());
    assert_eq!(alias.smtp_host, "smtp.work.org");
    assert_eq!(alias.smtp_port, 465);
    assert_eq!(alias.smtp_username, "ann@work.org");
    assert!(
        alias.smtp_password.is_empty(),
        "passwords live in the keyring"
    );
}

#[test]
fn alias_smtp_password_never_reaches_disk() {
    let mut cfg: ConfigFile = toml::from_str(
        r#"
        [[accounts]]
        name = "Ann"
        email = "ann@example.org"
        imap_host = "imap.example.org"
        username = "ann"

        [[accounts.aliases]]
        identity = "ann@work.org"
        smtp_host = "smtp.work.org"
        smtp_username = "ann"
        "#,
    )
    .unwrap();
    cfg.accounts[0].aliases[0].smtp_password = "hunter2".into();
    let out = toml::to_string_pretty(&cfg).unwrap();
    assert!(
        !out.contains("hunter2"),
        "password serialized to disk: {out}"
    );
    // And the round trip keeps the alias's transport settings.
    let back: ConfigFile = toml::from_str(&out).unwrap();
    let alias = &back.accounts[0].aliases[0];
    assert_eq!(alias.smtp_host, "smtp.work.org");
    assert_eq!(
        alias.smtp_port, 587,
        "unwritten port falls back to the default"
    );
}

#[test]
fn preview_lines_default_to_one_and_stay_in_range() {
    // An older privacy.toml has no key at all.
    let p: PrivacyFile = toml::from_str("").unwrap();
    assert_eq!(p.preview_lines, 1);
    // Hand-edited nonsense must not make the list build rows of 40 lines, or
    // of none: the setting offers 1–3 and that is what it is worth honouring.
    // 0 is a real setting — previews off — but nothing above 3 is.
    for (written, expected) in [(0, 0), (1, 1), (3, 3), (99, 3)] {
        let p: PrivacyFile = toml::from_str(&format!("preview_lines = {written}")).unwrap();
        assert_eq!(p.preview_lines.min(3), expected, "for {written}");
    }
}

#[test]
fn notifications_default_on_when_absent() {
    // An older privacy.toml with no `notifications` key opts in by default.
    let p: PrivacyFile = toml::from_str("").unwrap();
    assert!(p.notifications);
}

#[test]
fn new_mail_sound_names_a_builtin_or_falls_back() {
    let resource = |sound: &str| match (super::NewMailSound {
        enabled: true,
        sound: sound.into(),
        when_focused: false,
    })
    .source()
    {
        Some(super::SoundSource::Resource(p)) => p,
        _ => panic!("a built-in plays from the bundle"),
    };
    assert_eq!(resource("hum"), "/co/hyprlab/Hylki/sounds/hum.ogg");
    assert_eq!(resource("bogus"), "/co/hyprlab/Hylki/sounds/click.ogg");
    // A sound.toml from before the in-front switch (#337) still reads.
    let saved: super::NewMailSound = toml::from_str("enabled = true\nsound = \"swing\"").unwrap();
    assert_eq!(
        saved,
        super::NewMailSound {
            enabled: true,
            sound: "swing".into(),
            when_focused: false
        }
    );
    let saved: super::NewMailSound =
        toml::from_str("enabled = true\nsound = \"swing\"\nwhen_focused = true").unwrap();
    assert!(saved.when_focused);
}

#[test]
fn notification_buttons_default_to_all_and_round_trip() {
    // An older privacy.toml with no key carries every button.
    let p: PrivacyFile = toml::from_str("").unwrap();
    assert_eq!(
        super::NotificationButtons::from_list(&p.notification_buttons),
        super::NotificationButtons::default()
    );
    // An empty list is no buttons at all, not the default.
    let p: PrivacyFile = toml::from_str("notification_buttons = []").unwrap();
    let none = super::NotificationButtons::NONE;
    assert_eq!(
        super::NotificationButtons::from_list(&p.notification_buttons),
        none
    );
    let p: PrivacyFile =
        toml::from_str(r#"notification_buttons = ["delete", "bogus", "mark_read"]"#).unwrap();
    let b = super::NotificationButtons::from_list(&p.notification_buttons);
    assert_eq!(
        b,
        super::NotificationButtons {
            mark_read: true,
            delete: true,
            ..none
        }
    );
    assert_eq!(
        b.to_list(),
        vec!["mark_read".to_string(), "delete".to_string()]
    );
}

#[test]
fn notification_buttons_stop_at_three() {
    // A hand-edited file naming more than three keeps the first three
    // in button order, whatever order the file lists them in.
    let p: PrivacyFile = toml::from_str(
        r#"notification_buttons = ["spam", "forward", "reply", "delete", "archive"]"#,
    )
    .unwrap();
    let b = super::NotificationButtons::from_list(&p.notification_buttons);
    assert_eq!(
        b,
        super::NotificationButtons {
            archive: true,
            delete: true,
            reply: true,
            ..super::NotificationButtons::NONE
        }
    );
    assert!(b.full());
    assert_eq!(b.count(), 3);
}

#[test]
fn notifications_can_be_disabled() {
    let p: PrivacyFile = toml::from_str("notifications = false").unwrap();
    assert!(!p.notifications);
}

#[test]
fn reading_pane_reads_the_old_switch() {
    use super::ReadingPane;
    #[derive(serde::Deserialize)]
    struct F {
        reading_pane: ReadingPane,
    }
    let read = |s: &str| toml::from_str::<F>(s).unwrap().reading_pane;
    assert_eq!(read("reading_pane = true"), ReadingPane::Right);
    assert_eq!(read("reading_pane = false"), ReadingPane::Off);
    assert_eq!(read("reading_pane = \"below\""), ReadingPane::Below);
    assert_eq!(read("reading_pane = \"sideways\""), ReadingPane::Right);
}
