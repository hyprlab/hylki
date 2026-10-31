//! Tests of filter matching and of filters.toml.

use super::*;

fn smart_input<'a>(
    starred: bool,
    unread: bool,
    keywords: &'a [String],
    tags: &'a [String],
) -> SmartInput<'a> {
    SmartInput {
        text: FilterInput {
            subject: "Quarterly invoice",
            from_addr: "billing@shop.example",
            ..Default::default()
        },
        starred,
        unread,
        attachment: false,
        keywords,
        tags,
    }
}

fn smart(any: bool, conditions: Vec<SmartCondition>) -> SmartFolder {
    SmartFolder {
        id: "x".into(),
        name: "Test".into(),
        account_email: String::new(),
        folder: Some(SmartFolderKind::Sent),
        any,
        conditions,
    }
}

fn smart_cond(field: SmartField, value: &str) -> SmartCondition {
    SmartCondition {
        field,
        matcher: FilterMatch::Contains,
        value: value.into(),
    }
}

/// #417's own example: sent mail that is starred or carries a tag.
#[test]
fn smart_folder_starred_or_tagged() {
    let f = smart(
        true,
        vec![
            smart_cond(SmartField::Starred, ""),
            smart_cond(SmartField::Tagged, ""),
        ],
    );
    let tags = vec!["$label1".to_string()];
    let none: Vec<String> = Vec::new();
    let tagged = vec!["$Label1".to_string()];
    let server_only = vec!["$Forwarded".to_string()];
    assert!(f.matches(&smart_input(true, false, &none, &tags)));
    assert!(f.matches(&smart_input(false, false, &tagged, &tags)));
    // A keyword no tag names is not a tag.
    assert!(!f.matches(&smart_input(false, false, &server_only, &tags)));
    assert!(!f.matches(&smart_input(false, false, &none, &tags)));
    // Nothing can be ruled out ahead when any one condition will do.
    assert_eq!(f.prefilter(), SmartPrefilter::default());
}

#[test]
fn smart_folder_every_condition() {
    let f = smart(
        false,
        vec![
            smart_cond(SmartField::Unread, ""),
            smart_cond(SmartField::Subject, "invoice, receipt"),
        ],
    );
    let none: Vec<String> = Vec::new();
    assert!(f.matches(&smart_input(false, true, &none, &none)));
    assert!(!f.matches(&smart_input(false, false, &none, &none)));
    assert_eq!(
        f.prefilter(),
        SmartPrefilter {
            unread: Some(true),
            ..Default::default()
        }
    );
    let one_tag = smart(false, vec![smart_cond(SmartField::Tagged, "work")]);
    let work = vec!["Work".to_string()];
    assert!(one_tag.matches(&smart_input(false, false, &work, &none)));
    assert!(!one_tag.matches(&smart_input(false, false, &none, &none)));
}

#[test]
fn smart_folders_file_round_trips() {
    let f = smart(
        true,
        vec![
            smart_cond(SmartField::Starred, ""),
            smart_cond(SmartField::FromAddress, "shop"),
        ],
    );
    let text = toml::to_string_pretty(&SmartFoldersFile {
        folders: vec![f.clone()],
    })
    .unwrap();
    assert!(text.contains("folder = \"sent\""), "{text}");
    let back: SmartFoldersFile = toml::from_str(&text).unwrap();
    assert_eq!(back.folders, vec![f]);
    // Only what a folder needs has to be written.
    let short: SmartFoldersFile =
        toml::from_str("[[folders]]\nid = \"a\"\nname = \"Starred\"\n[[folders.conditions]]\nfield = \"starred\"\n").unwrap();
    assert_eq!(
        short.folders[0].conditions[0].matcher,
        FilterMatch::Contains
    );
    assert!(short.folders[0].folder.is_none() && !short.folders[0].any);
}

pub(super) fn rule(field: FilterField, matcher: FilterMatch, value: &str) -> FilterRule {
    FilterRule {
        name: String::new(),
        account_email: "a@b.c".into(),
        field,
        matcher,
        value: value.into(),
        more: Vec::new(),
        any: false,
        dest_path: "Archive".into(),
        tag: String::new(),
        count_unread: true,
        forward_to: Vec::new(),
        forward_since: 0,
        mark_read: false,
        delete: false,
    }
}

fn cond(field: FilterField, matcher: FilterMatch, value: &str) -> FilterCondition {
    FilterCondition {
        field,
        matcher,
        value: value.into(),
    }
}

/// The four header fields the original rules looked at.
fn headers<'a>(
    from_addr: &'a str,
    from_name: &'a str,
    subject: &'a str,
    recipients: &'a str,
) -> FilterInput<'a> {
    FilterInput {
        from_addr,
        from_name,
        subject,
        recipients,
        ..Default::default()
    }
}

#[test]
fn draft_filter_matches_sender_and_subject() {
    let rule = FilterRule::draft_from_message("me@x.org", "billing@shop.example", "Your invoice");
    assert_eq!(rule.account_email, "me@x.org");
    assert!(!rule.any, "both conditions have to match");
    assert!(
        rule.dest_path.is_empty() && rule.tag.is_empty(),
        "the user picks what the rule does"
    );
    assert_eq!(
        rule.conditions(),
        vec![
            FilterCondition {
                field: FilterField::FromAddress,
                matcher: FilterMatch::Equals,
                value: "billing@shop.example".into(),
            },
            FilterCondition {
                field: FilterField::Subject,
                matcher: FilterMatch::Contains,
                value: "Your invoice".into(),
            },
        ]
    );
}

#[test]
fn draft_filter_strips_reply_and_forward_prefixes() {
    let subject_of = |subject| {
        FilterRule::draft_from_message("me@x.org", "a@b.org", subject).conditions()[1]
            .value
            .clone()
    };
    assert_eq!(subject_of("Re: Invoice"), "Invoice");
    assert_eq!(subject_of("RE: Fwd: re:  Invoice"), "Invoice");
    assert_eq!(subject_of("Fw: Invoice"), "Invoice");
    // Only a leading prefix goes; one inside the subject is the subject's own.
    assert_eq!(subject_of("Invoice re: March"), "Invoice re: March");
    assert_eq!(subject_of("Reminder: Invoice"), "Reminder: Invoice");
}

#[test]
fn draft_filter_skips_an_empty_subject() {
    for subject in ["", "   ", "Re: ", "Fwd:"] {
        let rule = FilterRule::draft_from_message("me@x.org", "a@b.org", subject);
        assert_eq!(
            rule.conditions().len(),
            1,
            "subject {subject:?} leaves the sender alone"
        );
        assert_eq!(rule.field, FilterField::FromAddress);
    }
}

#[test]
fn found_keywords_get_names_and_colors() {
    // Thunderbird's built-ins keep Thunderbird's names and colors.
    assert_eq!(Tag::name_for_keyword("$label1"), "Important");
    assert_eq!(Tag::name_for_keyword("$LABEL4"), "To Do");
    assert_eq!(Tag::color_for_keyword("$label1", &[]), TAG_COLORS[4]);
    // Anything else reads as words.
    assert_eq!(Tag::name_for_keyword("travel_plans"), "Travel Plans");
    assert_eq!(Tag::name_for_keyword("$Receipts"), "Receipts");
    assert_eq!(Tag::name_for_keyword("to-do"), "To Do");
    assert_eq!(Tag::name_for_keyword("$"), "$");
    // Colors skip what is taken, then cycle.
    let taken: Vec<String> = TAG_COLORS[..2].iter().map(|c| c.to_string()).collect();
    assert_eq!(Tag::color_for_keyword("Receipts", &taken), TAG_COLORS[2]);
    let all: Vec<String> = TAG_COLORS.iter().map(|c| c.to_string()).collect();
    assert_eq!(Tag::color_for_keyword("Receipts", &all), TAG_COLORS[0]);
    // Bookkeeping keywords are not tags.
    assert!(crate::worker::is_system_keyword("$Forwarded"));
    assert!(crate::worker::is_system_keyword("NonJunk"));
    assert!(crate::worker::is_system_keyword("$MailFlagBit1"));
    assert!(!crate::worker::is_system_keyword("$label2"));
    assert!(!crate::worker::is_system_keyword("Receipts"));
}

#[test]
fn tag_keywords_derive_from_names() {
    assert_eq!(Tag::keyword_for("To Do"), "To_Do");
    assert_eq!(Tag::keyword_for("  Work "), "Work");
    assert_eq!(Tag::keyword_for("Réunion (lundi)"), "Runion_lundi");
    assert!(Tag::valid_keyword("$label1"));
    assert!(!Tag::valid_keyword("has space"));
    assert!(!Tag::valid_keyword(""));
    assert_eq!(tag_css_class("$label1"), "tag-_24label1");
    assert_eq!(tag_css_class("Work"), "tag-work");
}

#[test]
fn filters_match_case_insensitively_per_field() {
    let r = rule(FilterField::FromAddress, FilterMatch::Contains, "NEWS@");
    assert!(r.matches(&headers("news@example.com", "", "", "")));
    assert!(!r.matches(&headers("other@example.com", "News", "News", "News")));

    let r = rule(FilterField::Subject, FilterMatch::StartsWith, "[list]");
    assert!(r.matches(&headers("", "", "[LIST] hello", "")));
    assert!(!r.matches(&headers("", "", "re: [list] hello", "")));

    let r = rule(FilterField::Recipients, FilterMatch::Contains, "team@");
    assert!(r.matches(&headers("", "", "", "me@x.org team@x.org")));

    let r = rule(FilterField::FromName, FilterMatch::Equals, "Bank");
    assert!(r.matches(&headers("", "bank", "", "")));
    assert!(!r.matches(&headers("", "bankster", "", "")));

    // An empty needle can never match (a half-filled rule stays inert).
    let r = rule(FilterField::Subject, FilterMatch::Contains, "");
    assert!(!r.matches(&headers("x", "x", "x", "x")));
    let r = rule(FilterField::Subject, FilterMatch::Contains, " , ,");
    assert!(!r.matches(&headers("x", "x", "x", "x")));
}

#[test]
fn contact_conditions_reach_disk_in_a_form_older_versions_read() {
    let mut r = rule(FilterField::FromNotInContacts, FilterMatch::Contains, "");
    r.more
        .push(cond(FilterField::FromInContacts, FilterMatch::Contains, ""));
    r.more
        .push(cond(FilterField::Subject, FilterMatch::Contains, "sale"));
    let text = toml::to_string_pretty(&FiltersFile {
        rules: vec![r.clone()],
    })
    .unwrap();
    assert!(
        !text.contains("from_in_contacts") && !text.contains("from_not_in_contacts"),
        "{text}"
    );
    assert!(text.contains("contacts = \"not_in_contacts\""), "{text}");
    let back: FiltersFile = toml::from_str(&text).unwrap();
    assert_eq!(back.rules, vec![r]);

    // What a version without the conditions reads: the same file, with
    // `contacts` an unknown key it skips, is a From match on nothing.
    #[derive(serde::Deserialize)]
    struct OldCondition {
        field: String,
        value: String,
    }
    #[derive(serde::Deserialize)]
    struct OldRule {
        field: String,
        value: String,
        more: Vec<OldCondition>,
    }
    #[derive(serde::Deserialize)]
    struct OldFile {
        rules: Vec<OldRule>,
    }
    let old: OldFile = toml::from_str(&text).unwrap();
    assert_eq!(
        (old.rules[0].field.as_str(), old.rules[0].value.as_str()),
        ("from_address", "")
    );
    assert_eq!(old.rules[0].more[0].field, "from_address");
    assert_eq!(old.rules[0].more[0].value, "");
    assert_eq!(old.rules[0].more[1].field, "subject");
}

#[test]
fn filters_check_the_sender_against_the_address_books() {
    let book: std::collections::HashSet<String> = ["ann@x.org".to_string()].into_iter().collect();
    let with_book = |from_addr| FilterInput {
        from_addr,
        contacts: Some(&book),
        ..Default::default()
    };

    let inside = rule(FilterField::FromInContacts, FilterMatch::Contains, "");
    let outside = rule(FilterField::FromNotInContacts, FilterMatch::Contains, "");
    assert!(inside.matches(&with_book("Ann@X.org ")));
    assert!(!inside.matches(&with_book("bob@x.org")));
    assert!(outside.matches(&with_book("bob@x.org")));
    assert!(!outside.matches(&with_book("ann@x.org")));

    // Unknown address books match neither way, so a book that failed to
    // load cannot make every sender look unknown.
    let none = FilterInput {
        from_addr: "bob@x.org",
        ..Default::default()
    };
    assert!(!inside.matches(&none));
    assert!(!outside.matches(&none));

    // Combined with another condition, and reported to the caller.
    let mut mixed = rule(FilterField::FromNotInContacts, FilterMatch::Contains, "");
    mixed
        .more
        .push(cond(FilterField::Subject, FilterMatch::Contains, "sale"));
    let input = FilterInput {
        from_addr: "bob@x.org",
        subject: "Big SALE",
        contacts: Some(&book),
        ..Default::default()
    };
    assert!(mixed.matches(&input));
    assert!(mixed.needs_contacts());
    assert!(!rule(FilterField::Subject, FilterMatch::Contains, "x").needs_contacts());
}

#[test]
fn filter_values_hold_comma_separated_alternatives() {
    // #192: "invoice, receipt" matches either; spaces around the commas
    // and empty pieces are ignored; a comma-free value is one piece.
    assert_eq!(
        FilterCondition::alternatives(" Invoice ,receipt,, "),
        ["invoice", "receipt"]
    );
    assert_eq!(FilterCondition::alternatives("plain"), ["plain"]);
    let r = rule(
        FilterField::Subject,
        FilterMatch::Contains,
        "invoice, receipt",
    );
    assert!(r.matches(&headers("", "", "Your RECEIPT", "")));
    assert!(r.matches(&headers("", "", "invoice #12", "")));
    assert!(!r.matches(&headers("", "", "hello", "")));
    // Each alternative gets the whole matcher, not just "contains".
    let r = rule(
        FilterField::FromAddress,
        FilterMatch::EndsWith,
        "@a.org, @b.org",
    );
    assert!(r.matches(&headers("x@B.org", "", "", "")));
    assert!(!r.matches(&headers("x@b.org.evil", "", "", "")));
}

#[test]
fn filter_rules_combine_conditions_all_or_any() {
    // #192: a second condition narrows by default…
    let mut r = rule(
        FilterField::FromAddress,
        FilterMatch::EndsWith,
        "@shop.example",
    );
    r.more
        .push(cond(FilterField::Subject, FilterMatch::Contains, "order"));
    assert!(r.matches(&headers("a@shop.example", "", "Order shipped", "")));
    assert!(!r.matches(&headers("a@shop.example", "", "Newsletter", "")));
    assert!(!r.matches(&headers("a@other.example", "", "Order shipped", "")));
    // …and widens with "any".
    r.any = true;
    assert!(r.matches(&headers("a@shop.example", "", "Newsletter", "")));
    assert!(r.matches(&headers("a@other.example", "", "Order shipped", "")));
    assert!(!r.matches(&headers("a@other.example", "", "Newsletter", "")));
    // A condition with nothing to match holds nothing up in "any" mode
    // and blocks an "all" rule, as an inert rule should.
    r.more
        .push(cond(FilterField::Subject, FilterMatch::Contains, ""));
    assert!(r.matches(&headers("a@shop.example", "", "", "")));
    r.any = false;
    assert!(!r.matches(&headers("a@shop.example", "", "Order shipped", "")));

    // The conditions round-trip through the accessors, first one included.
    let conds = r.conditions();
    assert_eq!(conds.len(), 3);
    assert_eq!(
        conds[0],
        cond(
            FilterField::FromAddress,
            FilterMatch::EndsWith,
            "@shop.example"
        )
    );
    assert!(!r.set_conditions(Vec::new()), "a rule needs a condition");
    assert!(r.set_conditions(vec![cond(
        FilterField::FromName,
        FilterMatch::Equals,
        "Bank"
    )]));
    assert_eq!(r.field, FilterField::FromName);
    assert_eq!(r.value, "Bank");
    assert!(r.more.is_empty());
}

#[test]
fn filter_reply_to_falls_back_to_from() {
    // #191: Reply-To when the sender set one, From otherwise.
    let r = rule(FilterField::ReplyTo, FilterMatch::Contains, "sales@");
    let mut input = headers("noreply@shop.example", "", "", "");
    input.reply_to = "Sales@shop.example";
    assert!(r.matches(&input));
    input.reply_to = "";
    assert!(!r.matches(&input));
    input.from_addr = "sales@shop.example";
    assert!(r.matches(&input));
}

#[test]
fn filter_recipient_matchers_look_at_each_recipient() {
    // #201: a rule on the recipients compared the whole To and Cc list
    // as one string, so "is x@y" never matched mail with a second
    // recipient, and "ends with @y" only ever saw the last one.
    let list = "Ann <ann@shop.example>, bob@shop.example, \"Cy, Jr\" <cy@other.example>";
    let input = headers("", "", "", list);
    let r = |m, v| rule(FilterField::Recipients, m, v);
    assert!(r(FilterMatch::Equals, "bob@shop.example").matches(&input));
    assert!(r(FilterMatch::Equals, "ANN@shop.example").matches(&input));
    assert!(r(FilterMatch::Equals, "cy@other.example").matches(&input));
    assert!(!r(FilterMatch::Equals, "shop.example").matches(&input));
    assert!(r(FilterMatch::EndsWith, "@shop.example").matches(&input));
    assert!(!r(FilterMatch::EndsWith, "@nowhere.example").matches(&input));
    assert!(r(FilterMatch::StartsWith, "cy@").matches(&input));
    assert!(r(FilterMatch::Contains, "other").matches(&input));
    // A display name counts on its own too.
    assert!(r(FilterMatch::Equals, "ann").matches(&input));
    // The single bare address most mail carries.
    assert!(r(FilterMatch::Equals, "me@shop.example").matches(&headers(
        "",
        "",
        "",
        "me@shop.example"
    )));
    assert!(!r(FilterMatch::Equals, "me@shop.example").matches(&headers("", "", "", "")));
}

#[test]
fn filter_body_matches_hits_or_text_on_hand() {
    // #191: the server's search result counts, and so does the text the
    // list carries; the matcher is always "contains".
    let r = rule(
        FilterField::Body,
        FilterMatch::Equals,
        "unsubscribe, opt out",
    );
    let hits = vec!["unsubscribe".to_string()];
    let mut input = headers("", "", "", "");
    input.body_hits = &hits;
    assert!(r.matches(&input));
    input.body_hits = &[];
    assert!(!r.matches(&input));
    input.body = "Click here to OPT OUT of these mails";
    assert!(r.matches(&input));
    input.body = "nothing of the sort";
    assert!(!r.matches(&input));
    assert_eq!(r.body_needles(), ["unsubscribe", "opt out"]);
    assert!(rule(FilterField::Subject, FilterMatch::Contains, "x")
        .body_needles()
        .is_empty());
}

#[test]
fn filter_rules_read_the_single_condition_files() {
    // A file from before #192 names one condition per rule and none of
    // the new keys; it must load as a one-condition, all-must-match rule.
    let text = r#"
[[rules]]
account_email = "a@b.c"
field = "subject"
matcher = "contains"
value = "digest"
dest_path = "Lists"
"#;
    let back: FiltersFile = toml::from_str(text).unwrap();
    assert_eq!(back.rules.len(), 1);
    let r = &back.rules[0];
    assert!(r.more.is_empty());
    assert!(!r.any);
    assert_eq!(
        r.conditions(),
        vec![cond(FilterField::Subject, FilterMatch::Contains, "digest")]
    );
    // And a one-condition rule written now carries none of the new keys,
    // so those versions read it back too.
    let text = toml::to_string_pretty(&FiltersFile {
        rules: back.rules.clone(),
    })
    .unwrap();
    assert!(!text.contains("more"), "{text}");
    assert!(!text.contains("any"), "{text}");
}

/// Grouping by date is on for a new install and for one whose file
/// predates it, once; switched off afterwards, it stays off.
#[test]
fn date_groups_default_reaches_existing_installs_once() {
    let mut old: PrivacyFile = toml::from_str("list_group_by_date = false\n").unwrap();
    assert!(apply_new_defaults(&mut old));
    assert!(old.list_group_by_date);
    old.list_group_by_date = false;
    let mut again: PrivacyFile = toml::from_str(&toml::to_string_pretty(&old).unwrap()).unwrap();
    assert!(!apply_new_defaults(&mut again));
    assert!(!again.list_group_by_date);
    let mut fresh = PrivacyFile::default();
    assert!(fresh.list_group_by_date);
    fresh.list_group_by_date = false;
    apply_new_defaults(&mut fresh);
    assert!(!fresh.list_group_by_date);
}

/// An account from before #445 has one `signature`, which reads as a
/// list of one that new messages start with; a list written now keeps
/// `signature` on its default for an older Hylki, and no default
/// leaves it empty.
#[test]
fn signatures_read_old_configs_and_write_for_them() {
    let old: AccountConfig = toml::from_str(
        "name = \"A\"\nemail = \"a@b.c\"\nimap_host = \"h\"\nusername = \"a\"\n\
         signature = \"<b>Ada</b>\"\nsignature_html = true\n",
    )
    .unwrap();
    let (list, default) = old.signature_set();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].html, "<b>Ada</b>");
    assert_eq!(default, Some(0));

    let mut acc = old.clone();
    let two = vec![
        NamedSignature {
            name: "English".into(),
            html: "Regards".into(),
        },
        NamedSignature {
            name: "Deutsch".into(),
            html: "Grüße".into(),
        },
    ];
    acc.set_signatures(two.clone(), Some(1));
    assert_eq!(acc.signature.as_deref(), Some("Grüße"));
    let back: AccountConfig = toml::from_str(&toml::to_string(&acc).unwrap()).unwrap();
    assert_eq!(back.signature_set(), (two.clone(), Some(1)));

    acc.set_signatures(two.clone(), None);
    assert!(acc.signature.is_none());
    let text = toml::to_string(&acc).unwrap();
    assert!(!text.contains("default_signature"), "{text}");
    let back: AccountConfig = toml::from_str(&text).unwrap();
    assert_eq!(back.signature_set(), (two, None));

    acc.set_signatures(Vec::new(), None);
    assert!(!toml::to_string(&acc).unwrap().contains("signatures"));
}

#[test]
fn settings_bundle_roundtrip_parses() {
    // Parse-and-serialize only (no disk, no keyring): the wire format
    // itself must round-trip, secrets must never appear.
    let mut acc = AccountConfig {
        name: "A".into(),
        email: "a@b.c".into(),
        protocol: Default::default(),
        imap_host: "imap.b.c".into(),
        imap_port: 993,
        smtp_host: String::new(),
        smtp_port: 587,
        username: "a@b.c".into(),
        password: "SECRET".into(),
        smtp_separate: false,
        jmap_token: false,
        jmap_generic: false,
        tls_accept_hostname_mismatch: false,
        security: None,
        smtp_username: String::new(),
        smtp_password: String::new(),
        color: None,
        emoji: None,
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
        oauth_refresh: "TOKEN".into(),
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
        copy_to_local: false,
        pgp_key: None,
        in_unified: true,
        folder_sort: None,
        sign_by_default: false,
        plain_subject: false,
        graph_directory: false,
        graph_directory_only: false,
        graph_label_names: false,
    };
    acc.aliases = Vec::new();
    let bundle = SettingsBundle {
        version: 1,
        privacy: PrivacyFile::default(),
        accounts: vec![acc],
        filters: Vec::new(),
        tags: Vec::new(),
        sidebar: None,
        window: None,
        state: None,
        cloud: Some(vec![crate::services::cloud::CloudAccount::empty()]),
        spell_words: Some(std::collections::BTreeMap::from([(
            "en_US".to_string(),
            vec!["Hylki".to_string()],
        )])),
    };
    let text = toml::to_string_pretty(&bundle).unwrap();
    assert!(!text.contains("SECRET"));
    assert!(!text.contains("TOKEN"));
    let back: SettingsBundle = toml::from_str(&text).unwrap();
    assert_eq!(back.accounts[0].email, "a@b.c");
    assert!(back.accounts[0].password.is_empty());
    // The later sections ride along, and a bundle without them (from
    // before they existed) still parses, leaving them unset.
    assert_eq!(back.cloud.as_ref().map(Vec::len), Some(1));
    assert_eq!(
        back.spell_words
            .as_ref()
            .and_then(|w| w.get("en_US"))
            .map(Vec::len),
        Some(1)
    );
    let old: SettingsBundle = toml::from_str("version = 1\n[privacy]\n").unwrap();
    assert!(old.cloud.is_none() && old.spell_words.is_none());
    // Auto-empty (#140): "never" is left out of the file, an age is kept.
    assert!(!text.contains("empty_junk_days"), "{text}");
    assert_eq!(back.accounts[0].empty_trash_days, 0);
    let mut aged = bundle;
    aged.accounts[0].empty_trash_days = 30;
    let text = toml::to_string_pretty(&aged).unwrap();
    assert!(text.contains("empty_trash_days = 30"), "{text}");
    let back: SettingsBundle = toml::from_str(&text).unwrap();
    assert_eq!(back.accounts[0].empty_trash_days, 30);
    assert_eq!(back.accounts[0].empty_junk_days, 0);
    // Left out of the unified section (#267): only "out" is written,
    // and a file without the key keeps the account in.
    assert!(!text.contains("in_unified"), "{text}");
    assert!(back.accounts[0].in_unified);
    let mut apart = aged;
    apart.accounts[0].in_unified = false;
    let text = toml::to_string_pretty(&apart).unwrap();
    assert!(text.contains("in_unified = false"), "{text}");
    let back: SettingsBundle = toml::from_str(&text).unwrap();
    assert!(!back.accounts[0].in_unified);
    // Signing by default (#267) is written only when on.
    assert!(!text.contains("sign_by_default"), "{text}");
    let mut signing = apart;
    signing.accounts[0].sign_by_default = true;
    let text = toml::to_string_pretty(&signing).unwrap();
    assert!(text.contains("sign_by_default = true"), "{text}");
    let back: SettingsBundle = toml::from_str(&text).unwrap();
    assert!(back.accounts[0].sign_by_default);
    // Hiding the subject of encrypted mail (#493) is the default, so
    // only turning it off is written.
    assert!(!text.contains("plain_subject"), "{text}");
    let mut plain = signing;
    plain.accounts[0].plain_subject = true;
    let text = toml::to_string_pretty(&plain).unwrap();
    assert!(text.contains("plain_subject = true"), "{text}");
    let back: SettingsBundle = toml::from_str(&text).unwrap();
    assert!(back.accounts[0].plain_subject);
}

#[test]
fn filter_names_are_optional_and_round_trip() {
    // A rule with no name is written without the key, so a version from
    // before #197 reads the file back unchanged.
    let plain = rule(FilterField::Subject, FilterMatch::Contains, "digest");
    assert_eq!(plain.name, "");
    let text = toml::to_string_pretty(&FiltersFile {
        rules: vec![plain.clone()],
    })
    .unwrap();
    assert!(!text.contains("name"), "{text}");
    // A named one keeps its name across a round trip, and an unknown
    // `name` key in a file from a newer version is simply carried.
    let mut named = plain.clone();
    named.name = "Mailing lists".into();
    let text = toml::to_string_pretty(&FiltersFile {
        rules: vec![named.clone()],
    })
    .unwrap();
    let back: FiltersFile = toml::from_str(&text).unwrap();
    assert_eq!(back.rules, vec![named.clone()]);
    // The label falls back to the conditions when there is no name.
    assert_eq!(named.label(), "Mailing lists");
    assert!(plain.label().contains("digest"));
}

#[test]
fn filter_rules_roundtrip_through_toml() {
    let mut multi = rule(FilterField::Body, FilterMatch::Contains, "unsubscribe");
    multi.more.push(cond(
        FilterField::ReplyTo,
        FilterMatch::EndsWith,
        "@list.example",
    ));
    multi.any = true;
    let rules = vec![
        rule(FilterField::Subject, FilterMatch::EndsWith, "digest"),
        multi,
    ];
    let text = toml::to_string_pretty(&FiltersFile {
        rules: rules.clone(),
    })
    .unwrap();
    let back: FiltersFile = toml::from_str(&text).unwrap();
    assert_eq!(back.rules, rules);
}

#[test]
fn new_action_keys_round_trip() {
    let mut r = rule(FilterField::Subject, FilterMatch::Contains, "invoice");
    r.dest_path = String::new();
    r.forward_to = vec!["admin@corp.example".into(), "books@corp.example".into()];
    r.forward_since = 1_700_000_000;
    r.mark_read = true;
    r.delete = true;
    let file = FiltersFile {
        rules: vec![r.clone()],
    };
    let text = toml::to_string_pretty(&file).unwrap();
    let back: FiltersFile = toml::from_str(&text).unwrap();
    assert_eq!(back.rules, vec![r]);
}

#[test]
fn move_only_rule_writes_no_new_keys() {
    let r = rule(FilterField::Subject, FilterMatch::Contains, "invoice");
    let text = toml::to_string_pretty(&FiltersFile { rules: vec![r] }).unwrap();
    for key in ["forward_to", "forward_since", "mark_read", "delete"] {
        assert!(!text.contains(key), "{key} leaked into {text}");
    }
}

#[test]
fn old_file_loads_with_actions_off() {
    let old: FiltersFile = toml::from_str(
        "[[rules]]\naccount_email = \"a@b.c\"\nfield = \"subject\"\nmatcher = \"contains\"\nvalue = \"x\"\ndest_path = \"Lists\"\n",
    )
    .unwrap();
    let r = &old.rules[0];
    assert!(r.forward_to.is_empty() && r.forward_since == 0);
    assert!(!r.mark_read && !r.delete);
    assert_eq!(r.dest_path, "Lists");
}

#[test]
fn delete_with_a_destination_loses_the_delete() {
    let both: FiltersFile = toml::from_str(
        "[[rules]]\naccount_email = \"a@b.c\"\nfield = \"subject\"\nmatcher = \"contains\"\nvalue = \"x\"\ndest_path = \"Lists\"\ndelete = true\n",
    )
    .unwrap();
    assert!(!both.rules[0].delete);
    assert_eq!(both.rules[0].dest_path, "Lists");
}

#[test]
fn parse_forward_addresses_normalises() {
    assert_eq!(
        parse_forward_addresses(" A@x.org, b@y.org ,a@X.org,").unwrap(),
        vec!["a@x.org".to_string(), "b@y.org".to_string()]
    );
    assert!(parse_forward_addresses("").unwrap().is_empty());
}

#[test]
fn parse_forward_addresses_rejects_bad() {
    assert_eq!(parse_forward_addresses("nobody").unwrap_err(), "nobody");
    assert_eq!(
        parse_forward_addresses("a@x.org, bad address@y").unwrap_err(),
        "bad address@y"
    );
    assert!(parse_forward_addresses("@x.org").is_err());
    assert!(parse_forward_addresses("a@").is_err());
}

#[test]
fn has_action_counts_every_kind() {
    let mut r = rule(FilterField::Subject, FilterMatch::Contains, "x");
    r.dest_path = String::new();
    assert!(!r.has_action());
    let mut t = r.clone();
    t.tag = "$label1".into();
    assert!(t.has_action());
    let mut f = r.clone();
    f.forward_to = vec!["a@x.org".into()];
    assert!(f.has_action());
    let mut m = r.clone();
    m.mark_read = true;
    assert!(m.has_action());
    let mut d = r.clone();
    d.delete = true;
    assert!(d.has_action());
}

#[test]
fn forward_start_resets_when_the_destinations_change() {
    let old = vec!["a@x.org".to_string()];
    // No forward before: starts now.
    assert_eq!(forward_start(None, &old, 500), 500);
    // Same destinations in another order or case keep the old start.
    let same = vec!["A@x.org".to_string()];
    assert_eq!(forward_start(Some((100, &old)), &same, 500), 100);
    // A new address starts again from now, so old mail is not sent on.
    let more = vec!["a@x.org".to_string(), "b@x.org".to_string()];
    assert_eq!(forward_start(Some((100, &old)), &more, 500), 500);
    // An unset start counts as none; no destinations means no start.
    assert_eq!(forward_start(Some((0, &old)), &old, 500), 500);
    assert_eq!(forward_start(Some((100, &old)), &[], 500), 0);
}
