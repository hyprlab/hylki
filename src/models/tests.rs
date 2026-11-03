//! Tests of the shared data types.

/// The numbers and tags the cache and state.toml store these as.
#[test]
fn stored_codes_stay_put() {
    use super::{GallerySort, Importance, SenderTrust};
    for (importance, n) in [
        (Importance::Normal, 0),
        (Importance::High, 1),
        (Importance::Low, -1),
    ] {
        assert_eq!(importance.to_i64(), n);
        assert_eq!(Importance::from_i64(n), importance);
    }
    assert_eq!(Importance::from_i64(7), Importance::Normal);
    for (trust, tag) in [
        (SenderTrust::Pass, "pass"),
        (SenderTrust::Unverified, "unverified"),
        (SenderTrust::Suspicious, "suspicious"),
        (SenderTrust::Fail, "fail"),
    ] {
        assert_eq!(trust.as_tag(), tag);
        assert_eq!(SenderTrust::from_tag(tag), trust);
    }
    assert_eq!(SenderTrust::from_tag("other"), SenderTrust::Unverified);
    let sorts: Vec<GallerySort> = (0..10).map(GallerySort::from_index).collect();
    assert_eq!(
        format!("{sorts:?}"),
        "[Newest, Oldest, Name, NameDesc, Sender, SenderDesc, Largest, Smallest, Type, TypeDesc]"
    );
    for (i, sort) in sorts.iter().enumerate() {
        assert_eq!(sort.index(), i as u32);
    }
    assert_eq!(GallerySort::from_index(10), GallerySort::Newest);
}
use super::*;

/// #374: tags first in Settings order, then the labels no tag stands
/// for, decoded; a label named like a tag shows as that tag, once.
#[test]
fn pills_put_tags_first_and_show_a_label_once() {
    let tag = |name: &str, keyword: &str| crate::config::Tag {
        name: name.into(),
        keyword: keyword.into(),
        color: "#1c71d8".into(),
    };
    let tags = [
        tag("Urgent", "$label1"),
        tag("Work", "Work"),
        tag("Receipts", "receipts"),
    ];
    let keywords = vec!["$Label1".to_string()];
    let labels: Vec<String> = [
        "Clients/Acme",
        "work",
        "&AOk-t&AOk-",
        "My Label",
        "Receipts",
    ]
    .iter()
    .map(|l| l.to_string())
    .collect();
    let names: Vec<String> = pills(&keywords, &labels, &tags)
        .into_iter()
        .map(|p| match p {
            Pill::Tag(t) => format!("tag:{}", t.name),
            Pill::Label(l) => format!("label:{l}"),
        })
        .collect();
    assert_eq!(
        names,
        [
            "tag:Urgent",
            "tag:Work",
            "tag:Receipts",
            "label:Clients/Acme",
            "label:été",
            "label:My Label"
        ]
    );
    assert!(pills(&[], &[], &tags).is_empty());
    assert_eq!(
        pills(&[], &["&AAA-".to_string()], &[]),
        [Pill::Label(" ".into())]
    );
}

/// #374: a label's color follows its name, whatever the case, and the
/// palette is spread over rather than one color for all.
#[test]
fn a_label_keeps_one_color() {
    assert_eq!(label_hue("Clients/Acme"), label_hue("clients/acme"));
    // Pinned: a label must not change color between releases.
    assert_eq!(["Work", "Clients", "Newsletters"].map(label_hue), [0, 7, 3]);
    let hues: std::collections::HashSet<usize> = [
        "Travel",
        "Receipts",
        "Clients",
        "Family",
        "Newsletters",
        "Bills",
        "School",
        "Friends",
    ]
    .iter()
    .map(|n| label_hue(n))
    .collect();
    assert!(hues.len() >= 4, "{hues:?}");
    assert!(hues.iter().all(|&h| h < crate::config::TAG_COLORS.len()));
}

/// #374: a Gmail label named like a tag is the tag, for every check that
/// asks; taking the tag off takes those labels too.
#[test]
fn a_label_named_like_a_tag_is_the_tag() {
    let tag = |name: &str, keyword: &str| crate::config::Tag {
        name: name.into(),
        keyword: keyword.into(),
        color: "#1c71d8".into(),
    };
    let work = tag("Work", "$label2");
    let ete = tag("Été", "ete");
    let mut m = crate::backend::MockBackend::new()
        .message(1)
        .expect("a demo message");
    m.keywords.clear();
    m.labels = vec!["work".into(), "&AMk-t&AOk-".into(), "Other".into()];
    assert!(m.has_tag(&work) && m.has_tag(&ete));
    assert!(!m.has_tag(&tag("Home", "home")));
    assert_eq!(
        m.tag_keywords(&[work.clone(), ete.clone()]),
        ["$label2", "ete"]
    );
    m.keywords = vec!["$Label2".into()];
    assert_eq!(
        m.tag_keywords(std::slice::from_ref(&work)),
        ["$Label2"],
        "no second copy of a keyword it has"
    );
    m.drop_tag(&work);
    assert!(!m.has_tag(&work) && m.keywords.is_empty());
    assert_eq!(m.labels, ["&AMk-t&AOk-", "Other"]);
}

/// #334: each priority header's spellings, and normal for anything else.
#[test]
fn importance_reads_every_priority_header() {
    assert_eq!(
        Importance::from_header("X-Priority", "1 (Highest)"),
        Importance::High
    );
    assert_eq!(
        Importance::from_header("x-priority", " 2"),
        Importance::High
    );
    assert_eq!(
        Importance::from_header("X-Priority", "3 (Normal)"),
        Importance::Normal
    );
    assert_eq!(
        Importance::from_header("X-Priority", "5 (Lowest)"),
        Importance::Low
    );
    assert_eq!(
        Importance::from_header("Importance", "High"),
        Importance::High
    );
    assert_eq!(
        Importance::from_header("Importance", "low"),
        Importance::Low
    );
    assert_eq!(
        Importance::from_header("Priority", "urgent"),
        Importance::High
    );
    assert_eq!(
        Importance::from_header("Priority", "non-urgent"),
        Importance::Low
    );
    assert_eq!(
        Importance::from_header("Importance", "whatever"),
        Importance::Normal
    );
    // The first header that says anything wins; a normal one says nothing.
    let headers = [("X-Priority", "3"), ("Importance", "high")];
    let lookup = |name: &str| headers.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
    assert_eq!(Importance::from_headers(lookup), Importance::High);
    assert_eq!(Importance::from_headers(|_| None), Importance::Normal);
    for i in [Importance::Normal, Importance::High, Importance::Low] {
        assert_eq!(Importance::from_i64(i.to_i64()), i);
    }
}

#[test]
fn a_quota_says_what_is_used_and_free() {
    let q = MailboxQuota {
        used: 250,
        limit: 1000,
    };
    assert_eq!(q.fraction(), 0.25);
    assert!(q.free_line().ends_with("(75%)"));
    let over = MailboxQuota {
        used: 1200,
        limit: 1000,
    };
    assert_eq!(over.fraction(), 1.0);
    assert!(over.free_line().ends_with("(0%)"));
}

#[test]
fn the_pgp_chip_names_its_verdict() {
    let good = |trust| PgpSignature::Good {
        signer: "A".into(),
        key_id: "1".into(),
        trust,
    };
    let status = |encrypted, signature| PgpStatus {
        encrypted,
        decrypted: encrypted,
        signature,
        ..Default::default()
    };
    assert_eq!(status(false, good(PgpTrust::Full)).short_label(), "Signed");
    assert_eq!(
        status(true, good(PgpTrust::Full)).short_label(),
        "Encrypted and signed"
    );
    assert_eq!(
        status(false, good(PgpTrust::Unknown)).short_label(),
        "Signed, key not trusted"
    );
    assert_eq!(
        status(false, PgpSignature::Bad { signer: "A".into() }).short_label(),
        "Bad signature"
    );
    assert_eq!(status(true, PgpSignature::None).short_label(), "Encrypted");
    let locked = PgpStatus {
        encrypted: true,
        decrypted: false,
        ..Default::default()
    };
    assert_eq!(locked.short_label(), "Not decrypted");
}

#[test]
fn english_folder_names_stay_as_the_server_has_them() {
    let folder = |name: &str, kind| Folder {
        id: 1,
        account_id: 1,
        name: name.into(),
        path: name.into(),
        kind,
        unread: 0,
        childless: false,
    };
    let mut folders = vec![
        folder("Deleted Items", FolderKind::Trash),
        folder("Junk Mail", FolderKind::Junk),
        folder("Old Stuff", FolderKind::Archive),
        folder("Sent", FolderKind::Custom),
    ];
    let before: Vec<String> = folders.iter().map(|f| f.name.clone()).collect();
    // The tests run without a catalogue, as English does.
    localize_special_folder_names(&mut folders);
    assert_eq!(
        folders.iter().map(|f| f.name.clone()).collect::<Vec<_>>(),
        before
    );
}

fn item() -> OutboxItem {
    OutboxItem {
        id: 7,
        account_id: 1,
        from_addr: "me@example.com".into(),
        rcpts: vec!["ada@example.com".into()],
        recipients: "Ada Lovelace <ada@example.com>".into(),
        subject: "Quarterly numbers".into(),
        preview: "Here are the figures".into(),
        raw: Vec::new(),
        sent_path: None,
        queued_at: 0,
        attempts: 1,
        send_at: None,
        last_error: String::new(),
        forwards: String::new(),
        status: String::new(),
    }
}

#[test]
fn a_queued_message_reads_as_an_ordinary_row() {
    let row = item().as_message();
    // The list's headline column is who it is going to, not who sent it: in
    // an Outbox every message is from the same person.
    assert_eq!(row.from_name, "Ada Lovelace <ada@example.com>");
    assert_eq!(row.subject, "Quarterly numbers");
    assert_eq!(row.folder_id, OUTBOX_FOLDER_ID);
    assert_eq!(row.id, 7);
    // Still waiting, so never shown as read.
    assert!(row.unread);
}

#[test]
fn a_queued_row_says_why_it_is_stuck() {
    let mut i = item();
    i.attempts = 4;
    i.last_error = "Connection refused (os error 111)".into();
    let row = i.as_message();
    assert!(row.preview.contains("4 attempts"), "{}", row.preview);
    assert!(
        row.preview.contains("Connection refused"),
        "{}",
        row.preview
    );
    // With nothing failed yet, the body stands in for the reason.
    let row = item().as_message();
    assert!(
        row.preview.contains("Here are the figures"),
        "{}",
        row.preview
    );
    assert!(!row.preview.contains("attempts"), "{}", row.preview);
}

#[test]
fn an_empty_subject_or_recipient_still_reads_sensibly() {
    let mut i = item();
    i.subject = "  ".into();
    i.recipients = String::new();
    let row = i.as_message();
    assert_eq!(row.subject, "(no subject)");
    assert_eq!(row.from_name, "(no recipients)");
}

#[test]
fn type_keywords_cover_common_kinds() {
    assert!(type_keywords("a.pdf").contains("document"));
    assert!(type_keywords("a.png").contains("image"));
    assert!(type_keywords("a.mp3").contains("audio"));
    assert!(type_keywords("a.ics").contains("calendar"));
}

#[test]
fn type_buckets_match_the_footer_dropdown_rows() {
    assert_eq!(type_bucket("photo.JPG"), 1);
    assert_eq!(type_bucket("report.pdf"), 2);
    assert_eq!(type_bucket("notes.docx"), 3);
    assert_eq!(type_bucket("backup.tar"), 4);
    assert_eq!(type_bucket("song.flac"), 5);
    assert_eq!(type_bucket("unknown.xyz"), 6);
}
#[test]
fn assigned_drafts_folder_takes_the_kind_that_counts_every_draft() {
    let f = |id: u32, path: &str, kind: FolderKind| Folder {
        id,
        account_id: 1,
        name: path.to_string(),
        path: path.to_string(),
        kind,
        unread: 0,
        childless: false,
    };
    // A laposte.net-style listing: nothing detected as Drafts, and the
    // user assigned "Brouillons" under Special Folders.
    let mut folders = vec![
        f(1, "INBOX", FolderKind::Inbox),
        f(2, "Brouillons", FolderKind::Custom),
        f(3, "Sent", FolderKind::Sent),
    ];
    let roles: std::collections::BTreeMap<String, String> =
        [("drafts".to_string(), "Brouillons".to_string())].into();
    assign_folder_roles(&roles, &mut folders);
    let drafts = folders.iter().find(|f| f.path == "Brouillons").unwrap();
    assert_eq!(drafts.kind, FolderKind::Drafts);
    assert!(
        crate::worker::chip_counts_all(drafts.kind),
        "chip counts every draft"
    );
    // Positions and ids are untouched: the worker's cache keys ids by order.
    assert_eq!(
        folders.iter().map(|f| f.id).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(folders[1].path, "Brouillons");
}

#[test]
fn templates_can_be_assigned_and_are_named_in_the_language() {
    // #360: "Vorlagen" is pointed at under Special Folders, and a
    // detected "Templates" gives way to it.
    let f = |id: u32, path: &str, kind: FolderKind| Folder {
        id,
        account_id: 1,
        name: path.to_string(),
        path: path.to_string(),
        kind,
        unread: 0,
        childless: false,
    };
    let mut folders = vec![
        f(1, "INBOX", FolderKind::Inbox),
        f(2, "Templates", FolderKind::Templates),
        f(3, "Vorlagen", FolderKind::Custom),
    ];
    let roles: std::collections::BTreeMap<String, String> =
        [("templates".to_string(), "Vorlagen".to_string())].into();
    assign_folder_roles(&roles, &mut folders);
    assert_eq!(folders[1].kind, FolderKind::Custom);
    assert_eq!(folders[2].kind, FolderKind::Templates);
    assert!(is_templates_name(" Templates "));
    assert!(!is_templates_name("My Templates"));
}

fn plain_folder(path: &str, kind: FolderKind) -> Folder {
    Folder {
        id: 0,
        account_id: 1,
        name: path.rsplit('/').next().unwrap_or(path).to_string(),
        path: path.to_string(),
        kind,
        unread: 0,
        childless: false,
    }
}

#[test]
fn a_hidden_folder_takes_its_sub_folders_with_it() {
    let hidden = vec!["Sync Issues".to_string()];
    assert!(folder_is_hidden("Sync Issues", Some("/"), &hidden));
    assert!(folder_is_hidden(
        "Sync Issues/Conflicts",
        Some("/"),
        &hidden
    ));
    assert!(!folder_is_hidden("Sync Issues Archive", Some("/"), &hidden));
    assert!(!folder_is_hidden("INBOX/Sync Issues", Some("/"), &hidden));
    // Only the server's own delimiter makes a level (#554); a flat
    // server has none.
    assert!(folder_is_hidden(
        "Sync Issues.Conflicts",
        Some("."),
        &hidden
    ));
    assert!(!folder_is_hidden(
        "Sync Issues.Conflicts",
        Some("/"),
        &hidden
    ));
    assert!(!folder_is_hidden("Sync Issues.Conflicts", None, &hidden));
    assert!(!folder_is_hidden("Sync Issues Archive", None, &hidden));
}

#[test]
fn exchange_non_mail_folders_need_an_exchange_looking_listing() {
    let exchange: Vec<Folder> = [
        ("INBOX", FolderKind::Inbox),
        ("Calendar", FolderKind::Custom),
        ("Calendar/Birthdays", FolderKind::Custom),
        ("Contacts", FolderKind::Custom),
        ("Tasks", FolderKind::Custom),
        ("Notes", FolderKind::Custom),
        ("Sync Issues", FolderKind::Custom),
        ("Sync Issues/Conflicts", FolderKind::Custom),
        ("Conversation History", FolderKind::Custom),
        ("Projects", FolderKind::Custom),
        ("Sent Items", FolderKind::Sent),
    ]
    .iter()
    .map(|(p, k)| plain_folder(p, *k))
    .collect();
    let mut hidden = exchange_non_mail_folders(&exchange);
    hidden.sort();
    assert_eq!(
        hidden,
        vec![
            "Calendar",
            "Contacts",
            "Conversation History",
            "Notes",
            "Sync Issues",
            "Tasks"
        ]
    );

    // Apple Notes keeps a "Notes" folder on ordinary IMAP servers; that
    // alone is no reason to hide anything.
    let plain: Vec<Folder> = [("INBOX", FolderKind::Inbox), ("Notes", FolderKind::Custom)]
        .iter()
        .map(|(p, k)| plain_folder(p, *k))
        .collect();
    assert!(exchange_non_mail_folders(&plain).is_empty());
}

#[test]
fn thread_ids_collect_own_and_referenced_ids_once() {
    use super::test_util::msg;
    use super::thread_ids;
    let mut a = msg(1, 1, 10);
    a.message_id = "root@x".into();
    let mut b = msg(1, 1, 11);
    b.message_id = "reply@x".into();
    b.references = "root@x parent@x".into();
    let ids = thread_ids(&[a, b]);
    assert_eq!(ids, vec!["root@x", "reply@x", "parent@x"]);
}

#[test]
fn thread_ids_skips_messages_with_no_headers() {
    use super::test_util::msg;
    use super::thread_ids;
    // Nothing to search the cache by — better an empty list than a lookup
    // that would match every message with an empty message_id.
    assert!(thread_ids(&[msg(1, 1, 10)]).is_empty());
}
