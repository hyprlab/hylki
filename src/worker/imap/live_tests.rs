//! Tests against real IMAP servers, ignored by default: run one by name
//! with `--ignored` and the account in the environment.

use async_imap::types::Fetch;
use futures::TryStreamExt;

use super::*;
use crate::worker::compose::build::build_draft;
use crate::worker::ids::normalize_msgid;
use crate::worker::imap::backfill::{run_one_backfill, Backfill};
use crate::worker::imap::bodies::load_raw;
use crate::worker::imap::connect::connect;
use crate::worker::imap::folders::list_folders;
use crate::worker::imap::idle::{idle_wait, IdleOutcome};
use crate::worker::imap::mutate::{kept_flags, move_messages, purge_messages, rename_folder};
use crate::worker::imap::send::{append_draft, append_to_sent, delete_draft, file_sent_copy};
use crate::worker::imap::snooze::find_by_message_ids;
use crate::worker::imap::strip_attachment::{find_appended, strip_imap_attachment};
use crate::worker::imap::sync::load_messages;
use crate::worker::imap::wire::{
    append_dated, create_box, delete_box, fetch_uids, search_uids, sel, subscribe_box,
};
use crate::worker::render::attachments::extract_attachments;
use crate::worker::smtp::send_smtp;
use crate::worker::test_support::{sample_account, sample_outgoing};
use crate::worker::{editable_from_raw, extract_body, reply_copy_path, strip, OutgoingMessage};

/// A keyring that cannot be read at sign-in (#375), with
/// `IMAP_LIVE=host,port,user,password` and `KEYRING_LIVE=1`. Run it in a
/// throwaway session bus with scratch `XDG_DATA_HOME` and `XDG_RUNTIME_DIR`
/// under /tmp (`dbus-run-session -- cargo test ...`): it starts its own
/// gnome-keyring-daemon and stores the password there.
/// No sign-in goes out without the password, and once the keyring answers
/// the next sign-in reads it again.
#[test]
#[ignore]
fn live_keyring_read_again() {
    let (Ok(spec), Ok(_)) = (std::env::var("IMAP_LIVE"), std::env::var("KEYRING_LIVE")) else {
        return;
    };
    let scratch = std::env::var("XDG_DATA_HOME").unwrap_or_default();
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    assert!(
        scratch.starts_with("/tmp/") && runtime.starts_with("/tmp/"),
        "needs a scratch XDG_DATA_HOME and XDG_RUNTIME_DIR, not the desktop's keyring"
    );
    let p: Vec<&str> = spec.splitn(4, ',').collect();
    let account = AccountConfig {
        email: p[2].into(),
        imap_host: p[0].into(),
        imap_port: p[1].parse().expect("port"),
        username: p[2].into(),
        password: String::new(),
        security: Some(crate::config::ServerSecurity {
            imap_starttls: false,
            imap_accept_invalid_certs: true,
            ..Default::default()
        }),
        ..sample_account()
    };
    // The login keyring, once there, is the default collection entries go to.
    let keyrings = std::path::Path::new(&scratch).join("keyrings");
    std::fs::create_dir_all(&keyrings).unwrap();
    std::fs::write(keyrings.join("default"), "login").unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let err = rt
        .block_on(connect(&account))
        .expect_err("no keyring, no sign-in")
        .to_string();
    assert!(err.contains("keyring"), "{err}");

    use std::io::Write;
    let mut daemon = std::process::Command::new("gnome-keyring-daemon")
        .args(["--replace", "--unlock", "--components=secrets"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("gnome-keyring-daemon");
    daemon
        .stdin
        .take()
        .unwrap()
        .write_all(b"scratch\n")
        .unwrap();
    daemon.wait().unwrap();
    let mut stored = crate::config::store_password(&account.email, p[3]);
    for _ in 0..20 {
        if stored.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
        stored = crate::config::store_password(&account.email, p[3]);
    }
    stored.expect("store");
    rt.block_on(connect(&account))
        .expect("signs in with the keyring's password");
}

/// Removing an attachment on a real server (#289), with
/// `IMAP_LIVE=host,port,user,password` (implicit TLS, any certificate).
/// Stores a message with two files, removes one, and checks the copy
/// kept the other, the flags and the arrival date; the mailbox is left
/// as it was found.
#[test]
#[ignore]
fn live_strip_attachment() {
    let Ok(spec) = std::env::var("IMAP_LIVE") else {
        return;
    };
    let p: Vec<&str> = spec.splitn(4, ',').collect();
    let account = AccountConfig {
        imap_host: p[0].into(),
        imap_port: p[1].parse().expect("port"),
        username: p[2].into(),
        password: p[3].into(),
        security: Some(crate::config::ServerSecurity {
            imap_starttls: false,
            imap_accept_invalid_certs: true,
            ..Default::default()
        }),
        ..sample_account()
    };
    let msgid = format!("strip-{}@hylki.test", crate::datefmt::now());
    let raw = strip::MIXED.replace("r1@example.org", &msgid);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut sess = connect(&account).await.expect("connect");
        let uid_next = sel(&mut sess, "INBOX").await.expect("select").uid_next;
        append_dated(
            &mut sess,
            "INBOX",
            Some("(\\Seen \\Flagged $label1)"),
            Some("\"01-Jan-2020 10:00:00 +0000\""),
            raw.as_bytes(),
        )
        .await
        .expect("append");
        let wanted = normalize_msgid(format!("<{msgid}>").as_bytes());
        let uid = find_appended(&mut sess, 0, uid_next, &wanted)
            .await
            .expect("stored");
        let mut session = Some(sess);
        let (new_uid, _) =
            match strip_imap_attachment(&mut session, &account, "INBOX", uid, "report.pdf", 18)
                .await
            {
                Ok(done) => done,
                Err(e) => panic!("strip: {}", e.text),
            };
        let new_uid = new_uid.expect("the copy is found again");
        assert_ne!(new_uid, uid);
        let sess = session.as_mut().unwrap();
        sel(sess, "INBOX").await.expect("select");
        let left: Vec<Fetch> = fetch_uids(sess, uid.to_string(), "(UID FLAGS)")
            .await
            .expect("fetch")
            .try_collect()
            .await
            .expect("fetch");
        assert!(
            left.iter().all(|f| f.uid != Some(uid)),
            "the original is gone"
        );
        let fetches: Vec<Fetch> = fetch_uids(
            sess,
            new_uid.to_string(),
            "(UID FLAGS INTERNALDATE BODY.PEEK[])",
        )
        .await
        .expect("fetch")
        .try_collect()
        .await
        .expect("fetch");
        let f = fetches
            .iter()
            .find(|f| f.uid == Some(new_uid))
            .expect("the copy");
        let flags = kept_flags(f.flags());
        println!(
            "copy {new_uid}: flags {flags:?} date {:?}",
            f.internal_date()
        );
        assert!(flags.contains(&"\\Seen".to_string()) && flags.contains(&"\\Flagged".to_string()));
        assert!(flags.contains(&"$label1".to_string()));
        assert_eq!(
            f.internal_date().map(|d| d.timestamp()),
            Some(1_577_872_800)
        );
        let names: Vec<String> = extract_attachments(f.body().expect("body"))
            .into_iter()
            .map(|a| a.name)
            .collect();
        assert_eq!(names, vec!["data.csv"]);
        purge_messages(sess, "INBOX", &[new_uid])
            .await
            .expect("clean up");
    });
}

/// #349, #350 against a real server: a draft saved from an alias with a
/// Bcc and a file comes back with all three.
/// `IMAP_LIVE=host,port,user,password cargo test live_draft -- --ignored`
#[test]
#[ignore]
fn live_draft_comes_back_whole() {
    let Ok(spec) = std::env::var("IMAP_LIVE") else {
        return;
    };
    let p: Vec<&str> = spec.splitn(4, ',').collect();
    let account = AccountConfig {
        imap_host: p[0].into(),
        imap_port: p[1].parse().expect("port"),
        username: p[2].into(),
        password: p[3].into(),
        security: Some(crate::config::ServerSecurity {
            imap_starttls: false,
            imap_accept_invalid_certs: true,
            ..Default::default()
        }),
        ..sample_account()
    };
    let file = std::env::temp_dir().join(format!("hylki-350-{}.txt", std::process::id()));
    std::fs::write(&file, b"notes").expect("write");
    let subject = format!("hylki-350-{}", std::process::id());
    let msg = OutgoingMessage {
        from_alias: Some("Work <work@example.com>".into()),
        to: "ada@example.com".into(),
        bcc: "hidden@example.com".into(),
        subject: subject.clone(),
        html: "<p>draft text</p>".into(),
        attachments: vec![file.to_string_lossy().to_string()],
        in_reply_to: "orig@example.com".into(),
        ..sample_outgoing()
    };
    let raw = build_draft(&account, &msg).expect("builds").formatted();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut sess = connect(&account).await.expect("connect");
        append_draft(&mut sess, "Drafts", &raw, false)
            .await
            .expect("append");
        let listed = load_messages(7, &mut sess, 1, "Drafts", true, None)
            .await
            .expect("load");
        let uid = listed
            .iter()
            .find(|m| m.subject == subject)
            .expect("listed")
            .uid;
        let back = load_raw(&mut sess, "Drafts", uid).await.expect("raw");
        delete_draft(&mut sess, "Drafts", uid).await.ok();
        let e = editable_from_raw(&back, &[]);
        assert_eq!(e.from, "Work <work@example.com>");
        assert_eq!(e.bcc, "hidden@example.com");
        assert_eq!(e.in_reply_to, "orig@example.com");
        assert!(e.body_html.contains("draft text"), "{}", e.body_html);
        assert_eq!(e.attachments.len(), 1);
    });
    std::fs::remove_file(&file).ok();
}

/// #360 against a real server: a template goes into a Templates folder
/// the save makes, as read mail and no draft.
/// `IMAP_LIVE=host,port,user,password cargo test live_template -- --ignored`
#[test]
#[ignore]
fn live_template_is_saved_as_read_mail() {
    let Ok(spec) = std::env::var("IMAP_LIVE") else {
        return;
    };
    let p: Vec<&str> = spec.splitn(4, ',').collect();
    let account = AccountConfig {
        imap_host: p[0].into(),
        imap_port: p[1].parse().expect("port"),
        username: p[2].into(),
        password: p[3].into(),
        security: Some(crate::config::ServerSecurity {
            imap_starttls: false,
            imap_accept_invalid_certs: true,
            ..Default::default()
        }),
        ..sample_account()
    };
    let folder = format!("Templates-{}", std::process::id());
    let subject = format!("hylki-360-{}", std::process::id());
    let msg = OutgoingMessage {
        subject: subject.clone(),
        html: "<p>template text</p>".into(),
        ..sample_outgoing()
    };
    let raw = build_draft(&account, &msg).expect("builds").formatted();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut sess = connect(&account).await.expect("connect");
        append_draft(&mut sess, &folder, &raw, true)
            .await
            .expect("append makes the folder");
        let listed = load_messages(7, &mut sess, 1, &folder, true, None)
            .await
            .expect("load");
        let uid = listed
            .iter()
            .find(|m| m.subject == subject)
            .expect("listed")
            .uid;
        sel(&mut sess, &folder).await.expect("select");
        let fetches: Vec<Fetch> = fetch_uids(&mut sess, uid.to_string(), "(UID FLAGS)")
            .await
            .expect("fetch")
            .try_collect()
            .await
            .expect("fetch");
        let flags: Vec<String> = fetches
            .iter()
            .flat_map(|f| f.flags())
            .map(|f| format!("{f:?}"))
            .collect();
        println!("template {uid}: flags {flags:?}");
        assert!(flags.iter().any(|f| f == "Seen"), "{flags:?}");
        assert!(!flags.iter().any(|f| f == "Draft"), "{flags:?}");
        let _ = sess.close().await;
        delete_box(&mut sess, &folder).await.expect("clean up");
    });
}

/// #468 against a real server, with `IMAP_LIVE=host,port,user,password`
/// and `SMTP_LIVE=host,port` (implicit TLS): a reply goes out, and its
/// copy is filed in the folder of the message it answers, or in Sent when
/// that folder will not take it.
#[test]
#[ignore]
fn live_reply_is_filed_beside_its_original() {
    let (Ok(spec), Ok(smtp)) = (std::env::var("IMAP_LIVE"), std::env::var("SMTP_LIVE")) else {
        return;
    };
    let p: Vec<&str> = spec.splitn(4, ',').collect();
    let (smtp_host, smtp_port) = smtp.split_once(',').expect("host,port");
    let account = AccountConfig {
        email: p[2].into(),
        imap_host: p[0].into(),
        imap_port: p[1].parse().expect("port"),
        smtp_host: smtp_host.into(),
        smtp_port: smtp_port.parse().expect("port"),
        username: p[2].into(),
        password: p[3].into(),
        security: Some(crate::config::ServerSecurity {
            imap_starttls: false,
            smtp_starttls: false,
            imap_accept_invalid_certs: true,
            smtp_accept_invalid_certs: true,
            ..Default::default()
        }),
        reply_copies: crate::config::ReplyCopies::Original,
        ..sample_account()
    };
    let pid = std::process::id();
    let folder = format!("Project-{pid}");
    let parent = format!("hylki-468-{pid}@example.test");
    let original = format!(
        "From: Ada <ada@example.test>\r\nTo: {}\r\nSubject: Plans {pid}\r\nMessage-ID: <{parent}>\r\n\
         Date: Fri, 16 Oct 2026 10:00:00 +0000\r\n\r\nShall we plan?\r\n",
        account.email
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let cache = Cache::in_memory().unwrap();
        let mut sess = connect(&account).await.expect("connect");
        create_box(&mut sess, &folder).await.expect("create");
        append_to_sent(&mut sess, &folder, original.as_bytes())
            .await
            .expect("original filed");
        let folders = list_folders(1, &mut sess, &account.folder_roles, &account.hidden_folders)
            .await
            .expect("list");
        cache.save_folders(1, &folders);
        let folder_id = folders
            .iter()
            .find(|f| f.path == folder)
            .expect("listed")
            .id;
        let sent = folders
            .iter()
            .find(|f| f.kind == FolderKind::Sent)
            .expect("a Sent folder")
            .path
            .clone();
        let listed = load_messages(1, &mut sess, folder_id, &folder, true, Some(&cache))
            .await
            .expect("load");
        cache.upsert_messages(1, &folder, &listed);

        let reply = OutgoingMessage {
            to: account.email.clone(),
            subject: format!("Re: Plans {pid}"),
            body: "Yes.".into(),
            in_reply_to: parent.to_ascii_uppercase(),
            references: parent.clone(),
            ..sample_outgoing()
        };
        let raw = send_smtp(&account, &reply).await.expect("sent");
        let dest = reply_copy_path(&account, 1, Some(&cache), &reply.in_reply_to);
        assert_eq!(dest.as_deref(), Some(folder.as_str()));
        let took = file_sent_copy(&mut sess, dest.as_deref(), Some(&sent), &raw)
            .await
            .expect("filed");
        assert_eq!(took.as_deref(), Some(folder.as_str()));
        let after = load_messages(1, &mut sess, folder_id, &folder, true, None)
            .await
            .expect("load");
        let copy = after
            .iter()
            .find(|m| m.subject == reply.subject)
            .expect("the copy is beside its original");
        assert!(copy.references.contains(&parent), "{:?}", copy.references);
        println!(
            "{folder}: {:?}",
            after.iter().map(|m| &m.subject).collect::<Vec<_>>()
        );

        // A folder gone from the server since: Sent takes it.
        let gone = format!("Gone-{pid}");
        let took = file_sent_copy(&mut sess, Some(&gone), Some(&sent), &raw)
            .await
            .expect("filed");
        assert_eq!(took.as_deref(), Some(sent.as_str()));
        let sent_id = folders.iter().find(|f| f.path == sent).unwrap().id;
        let in_sent = load_messages(1, &mut sess, sent_id, &sent, true, None)
            .await
            .expect("load");
        let fallback = in_sent
            .iter()
            .find(|m| m.subject == reply.subject)
            .expect("the copy went to Sent");
        delete_draft(&mut sess, &sent, fallback.uid)
            .await
            .expect("clean up Sent");
        // The reply went to the account itself: take it out of the Inbox.
        for _ in 0..10 {
            let inbox = load_messages(1, &mut sess, 1, "INBOX", true, None)
                .await
                .expect("load");
            if let Some(m) = inbox.iter().find(|m| m.subject == reply.subject) {
                delete_draft(&mut sess, "INBOX", m.uid)
                    .await
                    .expect("clean up the Inbox");
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let _ = sess.close().await;
        delete_box(&mut sess, &folder).await.expect("clean up");
    });
}

/// #336 against a real server: mail appended while the connection is
/// busy is picked up by the next wait, not left until something else
/// happens to sync the folder.
#[test]
#[ignore]
fn live_idle_catches_mail_that_landed_between_commands() {
    let Ok(spec) = std::env::var("IMAP_LIVE") else {
        return;
    };
    let p: Vec<&str> = spec.splitn(4, ',').collect();
    let account = AccountConfig {
        imap_host: p[0].into(),
        imap_port: p[1].parse().expect("port"),
        username: p[2].into(),
        password: p[3].into(),
        security: Some(crate::config::ServerSecurity {
            imap_starttls: false,
            imap_accept_invalid_certs: true,
            ..Default::default()
        }),
        ..sample_account()
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut sess = connect(&account).await.expect("connect");
        load_messages(7, &mut sess, 1, "INBOX", true, None).await.expect("load");
        let subject = format!("hylki-336-{}", std::process::id());
        let raw = format!(
            "From: a@example.test\r\nTo: b@example.test\r\nSubject: {subject}\r\nMessage-ID: <{subject}@example.test>\r\n\r\nhello\r\n"
        );
        let mut other = connect(&account).await.expect("connect");
        append_to_sent(&mut other, "INBOX", raw.as_bytes()).await.expect("append");
        let (_tx, mut rx) = mpsc::unbounded_channel();
        let mut session = Some(sess);
        let events = std::cell::RefCell::new(Vec::new());
        let outcome = idle_wait(
            &mut session, &account, 7, 1, "INBOX", &mut rx, None, &mut true,
            &mut Default::default(), &mut Default::default(), &Default::default(),
            &|e| events.borrow_mut().push(e), 5,
        )
        .await;
        assert!(matches!(outcome, IdleOutcome::Refreshed), "the wait went quiet");
        let got = events.borrow().iter().any(|e| {
            matches!(e, WorkerEvent::Messages { messages, .. } if messages.iter().any(|m| m.subject == subject))
        });
        assert!(got, "the new message was not listed");
    });
}

/// A folder moved with a sub-folder keeps both subscribed at their new
/// names, so a client showing subscribed folders only still sees them.
/// `IMAP_LIVE=host,port,user,password cargo test --bin hylki
/// worker::imap::live_tests::live_rename_moves_subscriptions -- --ignored`
#[test]
#[ignore]
fn live_rename_moves_subscriptions() {
    let Ok(spec) = std::env::var("IMAP_LIVE") else {
        return;
    };
    let p: Vec<&str> = spec.splitn(4, ',').collect();
    let account = AccountConfig {
        imap_host: p[0].into(),
        imap_port: p[1].parse().expect("port"),
        username: p[2].into(),
        password: p[3].into(),
        security: Some(crate::config::ServerSecurity {
            imap_starttls: false,
            imap_accept_invalid_certs: true,
            ..Default::default()
        }),
        ..sample_account()
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut sess = connect(&account).await.expect("connect");
        let delim = session_delimiter(&mut sess).await;
        let (old, sub) = ("Hylki Rig".to_string(), format!("Hylki Rig{delim}Sub one"));
        let (new, new_sub) = (
            "Hylki Rig Moved".to_string(),
            format!("Hylki Rig Moved{delim}Sub one"),
        );
        for f in [&old, &sub] {
            create_box(&mut sess, f).await.expect("create");
            subscribe_box(&mut sess, f).await.expect("subscribe");
        }
        rename_folder(&mut sess, &old, &new).await.expect("rename");
        let subscribed: Vec<String> = sess
            .lsub(Some(""), Some("*"))
            .await
            .expect("lsub")
            .try_collect::<Vec<async_imap::types::Name>>()
            .await
            .expect("lsub")
            .iter()
            .map(|n| n.name().to_string())
            .collect();
        let _ = delete_box(&mut sess, &new_sub).await;
        let _ = delete_box(&mut sess, &new).await;
        println!("subscribed: {subscribed:?}");
        assert!(subscribed.contains(&new) && subscribed.contains(&new_sub));
        assert!(!subscribed.contains(&old) && !subscribed.contains(&sub));
    });
}

/// A folder the server lists but will not open (#512): Dovecot lists a
/// Sieve script's symlink as a folder and answers its SELECT with NO
/// [SERVERBUG]. The background index leaves it out after one try, on the
/// same session, instead of asking again in a loop.
/// `IMAP_LIVE=host,port,user,password IMAP_LIVE_REFUSED=dovecot.sieve
/// cargo test --bin hylki worker::imap::live_tests::live_backfill_leaves_refused_folder
/// -- --ignored`
#[test]
#[ignore]
fn live_backfill_leaves_refused_folder() {
    let (Ok(spec), Ok(path)) = (
        std::env::var("IMAP_LIVE"),
        std::env::var("IMAP_LIVE_REFUSED"),
    ) else {
        return;
    };
    let p: Vec<&str> = spec.splitn(4, ',').collect();
    let account = AccountConfig {
        imap_host: p[0].into(),
        imap_port: p[1].parse().expect("port"),
        username: p[2].into(),
        password: p[3].into(),
        security: Some(crate::config::ServerSecurity {
            imap_starttls: false,
            imap_accept_invalid_certs: true,
            ..Default::default()
        }),
        ..sample_account()
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut session = Some(connect(&account).await.expect("connect"));
        let mut queue = std::collections::VecDeque::from([Backfill {
            folder_id: 7,
            path,
            remaining: None,
            gallery: false,
            failures: 0,
        }]);
        let done = std::cell::Cell::new(false);
        let emit = |ev: WorkerEvent| {
            if matches!(ev, WorkerEvent::BackfillDone { folder_id: 7 }) {
                done.set(true);
            }
        };
        let mut use_envelope = true;
        run_one_backfill(
            &mut queue,
            &mut session,
            &account,
            1,
            None,
            &mut std::collections::VecDeque::new(),
            &mut use_envelope,
            &emit,
        )
        .await;
        assert!(queue.is_empty(), "the refused folder was queued again");
        assert!(done.get(), "the list was not told the folder is done");
        assert!(session.is_some(), "the sound session was dropped");
    });
}

/// The hierarchy delimiter the server answers `LIST "" ""` with.
async fn session_delimiter(sess: &mut ImapSession) -> String {
    sess.list(Some(""), Some("\"\""))
        .await
        .expect("list")
        .try_collect::<Vec<async_imap::types::Name>>()
        .await
        .expect("list")
        .first()
        .and_then(|n| n.delimiter().map(String::from))
        .unwrap_or_else(|| "/".into())
}

/// Where the time goes when a message is opened (#259): every step
/// `render_raw` takes, per message of a directory of saved .eml files.
/// `HYLKI_OPEN_TIMING=<dir> cargo test --release --bin hylki
/// worker::imap::live_tests::open_timing -- --ignored --nocapture`
#[test]
#[ignore]
fn open_timing() {
    use std::time::{Duration, Instant};
    let Ok(dir) = std::env::var("HYLKI_OPEN_TIMING") else {
        return;
    };
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "eml"))
        .collect();
    paths.sort();
    const STEPS: [&str; 7] = [
        "parse",
        "unsubscribe",
        "invite",
        "check",
        "pgp",
        "body",
        "attach",
    ];
    let mut total = [Duration::ZERO; 7];
    let mut whole: Vec<(Duration, [Duration; 7], String, usize)> = Vec::new();
    for p in &paths {
        let raw = std::fs::read(p).unwrap();
        let mut t = [Duration::ZERO; 7];
        let mut lap = |i: usize, f: &mut dyn FnMut()| {
            let at = Instant::now();
            f();
            t[i] = at.elapsed();
        };
        let mut parsed = None;
        lap(0, &mut || {
            parsed = mail_parser::MessageParser::default().parse(&raw[..])
        });
        if let Some(m) = parsed.as_ref() {
            lap(1, &mut || drop(crate::mail::unsubscribe::detect(m)));
            lap(2, &mut || drop(crate::mail::invite::detect(m)));
        }
        lap(3, &mut || drop(crate::mail::verify::check_sender(&raw)));
        lap(4, &mut || drop(crate::mail::pgp::detect(&raw)));
        lap(5, &mut || drop(extract_body(&raw)));
        lap(6, &mut || drop(extract_attachments(&raw)));
        for i in 0..7 {
            total[i] += t[i];
        }
        let sum: Duration = [t[3], t[4], t[5], t[6]].iter().sum();
        whole.push((
            sum,
            t,
            p.file_name().unwrap().to_string_lossy().into_owned(),
            raw.len(),
        ));
    }
    let n = paths.len().max(1) as u32;
    println!("{} messages", paths.len());
    for (i, step) in STEPS.iter().enumerate() {
        println!(
            "{step:>12}: total {:>9.1?}  mean {:>8.2?}",
            total[i],
            total[i] / n
        );
    }
    whole.sort_by_key(|w| std::cmp::Reverse(w.0));
    println!("slowest (render_raw ~ check+pgp+body+attach):");
    for (sum, t, name, len) in whole.iter().take(12) {
        println!(
            "{sum:>9.2?} {name} {len}B  parse {:.2?} unsub {:.2?} invite {:.2?} check {:.2?} pgp {:.2?} body {:.2?} attach {:.2?}",
            t[0], t[1], t[2], t[3], t[4], t[5], t[6]
        );
    }
}

/// #527 against a real server, with `IMAP_LIVE=host,port,user,password`:
/// read mail snoozed into a folder of its own comes back to the Inbox
/// unread, found by its Message-ID, and a second wake finds nothing to do.
#[test]
#[ignore]
fn live_snoozed_mail_comes_back_unread() {
    let Ok(spec) = std::env::var("IMAP_LIVE") else {
        return;
    };
    let p: Vec<&str> = spec.splitn(4, ',').collect();
    let account = AccountConfig {
        imap_host: p[0].into(),
        imap_port: p[1].parse().expect("port"),
        username: p[2].into(),
        password: p[3].into(),
        security: Some(crate::config::ServerSecurity {
            imap_starttls: false,
            imap_accept_invalid_certs: true,
            ..Default::default()
        }),
        ..sample_account()
    };
    let pid = std::process::id();
    let snoozed = format!("Snoozed-{pid}");
    let mid = format!("hylki-527-{pid}@example.test");
    let raw = format!(
        "From: Ann <ann@example.test>\r\nTo: me@example.test\r\nSubject: hylki-527-{pid}\r\n\
         Message-ID: <{mid}>\r\nDate: Sun, 25 Oct 2026 08:00:00 +0000\r\n\r\nlater\r\n"
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut sess = connect(&account).await.expect("connect");
        append_dated(&mut sess, "INBOX", Some("(\\Seen)"), None, raw.as_bytes())
            .await
            .expect("append");
        // A header search may not see it yet: the newest UID is it.
        sel(&mut sess, "INBOX").await.expect("select");
        let newest = search_uids(&mut sess, "ALL")
            .await
            .expect("search")
            .into_iter()
            .max()
            .expect("appended");
        let uids = vec![newest];
        move_messages(&mut sess, "INBOX", &uids, &snoozed)
            .await
            .expect("snooze makes the folder");

        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = events.clone();
        let emit = move |ev: WorkerEvent| {
            let line = match ev {
                WorkerEvent::Unsnoozed { message_ids, done } => {
                    format!("unsnoozed {message_ids:?} {done}")
                }
                WorkerEvent::Messages { folder_id, .. } => format!("messages {folder_id}"),
                _ => return,
            };
            log.lock().unwrap().push(line);
        };
        let (_tx, rx) = mpsc::unbounded_channel();
        let mut worker = ImapWorker {
            account_id: 1,
            account: account.clone(),
            rx,
            emit,
            cache: None,
            session: Some(sess),
            prefetch: VecDeque::new(),
            body_prefetch: VecDeque::new(),
            body_emitted: HashSet::new(),
            backfill: VecDeque::new(),
            backfill_seen: HashSet::new(),
            offline: Default::default(),
            text_search: None,
            refs_repair: VecDeque::new(),
            label_fill: VecDeque::new(),
            backlog: VecDeque::new(),
            push_enabled: false,
            idle_folder: None,
            last_unread_sweep: None,
            last_auto_empty: None,
            last_list_clean: None,
            inbox_watch_spawned: true,
            watchers: HashMap::new(),
            sweep_baseline: HashMap::new(),
            sweep_pending: Vec::new(),
            sweep_due: false,
            folder_refresh_due: false,
            pending_resync: false,
            use_envelope: true,
            outbox_flushed: true,
            local_tags_noticed: true,
        };
        worker
            .on_unsnooze(snoozed.clone(), "INBOX".into(), 1, vec![mid.clone()])
            .await;
        worker
            .on_unsnooze(snoozed.clone(), "INBOX".into(), 1, vec![mid.clone()])
            .await;
        let seen = events.lock().unwrap().clone();
        println!("events {seen:?}");
        assert_eq!(
            seen,
            vec![
                "messages 1".to_string(),
                format!("unsnoozed [{mid:?}] true"),
                format!("unsnoozed [{mid:?}] true"),
            ]
        );

        let sess = worker.session.as_mut().expect("session");
        sel(sess, "INBOX").await.expect("select");
        let back = find_by_message_ids(sess, "INBOX", std::slice::from_ref(&mid))
            .await
            .expect("find");
        assert_eq!(back.len(), 1, "back in the Inbox");
        let fetches: Vec<Fetch> = fetch_uids(sess, back[0].to_string(), "(UID FLAGS)")
            .await
            .expect("fetch")
            .try_collect()
            .await
            .expect("fetch");
        let seen = fetches
            .iter()
            .flat_map(|f| f.flags())
            .any(|f| matches!(f, async_imap::types::Flag::Seen));
        assert!(!seen, "and unread");
        assert_eq!(sel(sess, &snoozed).await.expect("select").exists, 0);

        let _ = purge_messages(sess, "INBOX", &back).await;
        let _ = sess.close().await;
        delete_box(sess, &snoozed).await.expect("clean up");
    });
}

/// Mail kept offline (#536), with `IMAP_LIVE=host,port,user,password`: of a
/// folder holding mail from two days, ten days and two months ago, a
/// "last week" period downloads the first only, attachment and all, writes
/// it to the cache, and then rests with the period complete.
#[test]
#[ignore]
fn live_offline_period_is_kept() {
    let Ok(spec) = std::env::var("IMAP_LIVE") else {
        return;
    };
    let p: Vec<&str> = spec.splitn(4, ',').collect();
    let account = AccountConfig {
        imap_host: p[0].into(),
        imap_port: p[1].parse().expect("port"),
        username: p[2].into(),
        password: p[3].into(),
        security: Some(crate::config::ServerSecurity {
            imap_starttls: false,
            imap_accept_invalid_certs: true,
            ..Default::default()
        }),
        offline_mail: crate::config::OfflineMail::Week,
        offline_attachments: true,
        ..sample_account()
    };
    let pid = std::process::id();
    let folder = format!("Offline-{pid}");
    let day = 86_400;
    let now = crate::datefmt::now();
    let mail = |n: u32, age_days: i64| {
        let date = chrono::DateTime::from_timestamp(now - age_days * day, 0)
            .unwrap()
            .to_rfc2822();
        format!(
            "From: Ann <ann@example.test>\r\nTo: me@example.test\r\nSubject: offline {n}\r\n\
             Message-ID: <hylki-536-{pid}-{n}@example.test>\r\nDate: {date}\r\n\
             MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n\
             --b\r\nContent-Type: text/plain\r\n\r\nbody {n}\r\n\
             --b\r\nContent-Type: application/octet-stream\r\n\
             Content-Disposition: attachment; filename=n{n}.bin\r\n\
             Content-Transfer-Encoding: base64\r\n\r\naGVsbG8=\r\n--b--\r\n"
        )
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut sess = connect(&account).await.expect("connect");
        create_box(&mut sess, &folder).await.expect("create");
        for (n, age) in [(1, 2), (2, 10), (3, 60)] {
            append_dated(
                &mut sess,
                &folder,
                Some("(\\Seen)"),
                None,
                mail(n, age).as_bytes(),
            )
            .await
            .expect("append");
        }
        let cache = Cache::in_memory().expect("cache");
        let listed = load_messages(1, &mut sess, 1, &folder, true, None)
            .await
            .expect("list");
        assert_eq!(listed.len(), 3);
        cache.save_messages(1, &folder, &listed);
        // As the folder listing leaves it: only listed folders are kept.
        cache.save_folders(
            1,
            &[crate::models::Folder {
                id: 1,
                account_id: 1,
                name: folder.clone(),
                path: folder.clone(),
                kind: crate::models::FolderKind::Custom,
                unread: 0,
                childless: false,
            }],
        );

        let (_tx, rx) = mpsc::unbounded_channel();
        let mut worker = ImapWorker {
            account_id: 1,
            account: account.clone(),
            rx,
            emit: |_: WorkerEvent| {},
            cache: Some(cache),
            session: Some(sess),
            prefetch: VecDeque::new(),
            body_prefetch: VecDeque::new(),
            body_emitted: HashSet::new(),
            backfill: VecDeque::new(),
            backfill_seen: HashSet::new(),
            offline: Default::default(),
            text_search: None,
            refs_repair: VecDeque::new(),
            label_fill: VecDeque::new(),
            backlog: VecDeque::new(),
            push_enabled: false,
            idle_folder: None,
            last_unread_sweep: None,
            last_auto_empty: None,
            last_list_clean: None,
            inbox_watch_spawned: true,
            watchers: HashMap::new(),
            sweep_baseline: HashMap::new(),
            sweep_pending: Vec::new(),
            sweep_due: false,
            folder_refresh_due: false,
            pending_resync: false,
            use_envelope: true,
            outbox_flushed: true,
            local_tags_noticed: true,
        };
        assert!(worker.offline.due(&worker.account));
        worker.keep_offline_batch().await;
        let uid_of = |n: u32| {
            listed
                .iter()
                .find(|m| m.subject == format!("offline {n}"))
                .expect("listed")
                .uid
        };
        let cache = worker.cache.as_ref().unwrap();
        let body = cache.load_body(1, &folder, uid_of(1)).expect("kept");
        assert!(body.contains("body 1"), "{body}");
        assert!(cache.attachments_checked(1, &folder, uid_of(1)));
        assert_eq!(cache.load_attachments(1, &folder, uid_of(1)).len(), 1);
        for n in [2, 3] {
            assert!(
                cache.load_body(1, &folder, uid_of(n)).is_none(),
                "{n} is older than a week"
            );
        }
        // Nothing left of the period: the next step finds it complete and rests.
        worker.keep_offline_batch().await;
        assert!(!worker.offline.due(&worker.account));

        let sess = worker.session.as_mut().expect("session");
        delete_box(sess, &folder).await.expect("clean up");
    });
}
