//! Copying an IMAP account's mail to On This Computer (#552), against a real
//! server: `IMAP_LIVE=host,port,user,password cargo test live_copy -- --ignored`.

use super::bodies::load_raw;
use super::connect::connect;
use super::sync::load_messages;
use super::wire::{append_dated, create_box, delete_box};
use crate::cache::Cache;
use crate::config::AccountConfig;
use crate::worker::offline::{keep, Offline};
use crate::worker::test_support::sample_account;

/// With the setting on and no offline period, every message is queued,
/// downloaded and stored once under the account's local folder, flags
/// and all; then the queue rests with nothing left.
#[test]
#[ignore]
fn live_copy_to_this_computer() {
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
        label: Some("Live".into()),
        copy_to_local: true,
        ..sample_account()
    };
    let pid = std::process::id();
    let folder = format!("Copy-{pid}");
    let root = std::env::temp_dir().join(format!("hylki-live-copy-{pid}"));
    let _ = std::fs::remove_dir_all(&root);
    let mail = |n: u32| {
        format!(
            "From: Ann <ann@example.test>\r\nTo: me@example.test\r\nSubject: copy {n}\r\n\
             Message-ID: <hylki-552-{pid}-{n}@example.test>\r\n\r\nbody {n}\r\n"
        )
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut sess = connect(&account).await.expect("connect");
        create_box(&mut sess, &folder).await.expect("create");
        append_dated(
            &mut sess,
            &folder,
            Some("(\\Seen \\Flagged)"),
            None,
            mail(1).as_bytes(),
        )
        .await
        .expect("append");
        append_dated(&mut sess, &folder, None, None, mail(2).as_bytes())
            .await
            .expect("append");
        let cache = Cache::in_memory().expect("cache");
        let listed = load_messages(1, &mut sess, 1, &folder, true, None)
            .await
            .expect("list");
        assert_eq!(listed.len(), 2);
        cache.save_messages(1, &folder, &listed);
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

        crate::worker::local::copy::register_at(1, &account, root.clone());
        let mut offline = Offline::default();
        assert!(Offline::wanted(&account));
        assert!(offline.fill(&cache, 1, &account));
        let (path, uids) = offline.next_batch(10).expect("queued");
        assert_eq!(uids.len(), 2);
        for uid in &uids {
            let raw = load_raw(&mut sess, &path, *uid).await.expect("raw");
            keep(&cache, 1, &path, *uid, &raw, false);
            // Downloaded again for its body: not copied twice.
            keep(&cache, 1, &path, *uid, &raw, false);
        }
        crate::worker::local::copy::forget(1);
        assert!(!offline.fill(&cache, 1, &account), "everything is copied");

        let cur = root.join(format!(".Live.{folder}/cur"));
        let mut names: Vec<String> = std::fs::read_dir(&cur)
            .expect("copied")
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names.len(), 2, "{names:?}");
        let flags: Vec<&str> = names
            .iter()
            .map(|n| n.rsplit(":2,").next().unwrap())
            .collect();
        assert!(flags.contains(&"FS") && flags.contains(&""), "{flags:?}");
        assert!(root
            .join("hylki-copied")
            .join("me@example.com.txt")
            .is_file());

        delete_box(&mut sess, &folder).await.expect("clean up");
    });
    let _ = std::fs::remove_dir_all(&root);
}
