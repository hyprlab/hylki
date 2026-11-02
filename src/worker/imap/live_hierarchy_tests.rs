//! The folder hierarchy against real IMAP servers (#554), ignored by
//! default: `IMAP_LIVE=host,port,user,password` (implicit TLS, any
//! certificate) and `--ignored`. Each server layout (a '/' or '.'
//! delimiter, an `INBOX.` namespace, NAMESPACE or not, mbox files that
//! hold no folders) is one run.

use super::*;
use crate::models::FolderHierarchy;
use crate::worker::imap::connect::connect;
use crate::worker::imap::folders::list_folders;
use crate::worker::imap::mutate::{create_folder, delete_folder, rename_folder};
use crate::worker::imap::namespace::{inside_inbox, read_hierarchy};
use crate::worker::test_support::sample_account;

/// What the server says, and the paths built from it doing what the app
/// does: a folder at the top, one inside it, that one moved to the top and
/// renamed, each checked against the server's own listing.
#[test]
#[ignore]
fn live_folders_go_where_the_server_puts_them() {
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
    let roles = Default::default();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut sess = connect(&account).await.expect("connect");
        let listed = list_folders(1, &mut sess, &roles, &[]).await.expect("list");
        let mut h: FolderHierarchy = read_hierarchy(&mut sess, &listed)
            .await
            .expect("the server says how its folders nest");
        eprintln!("hierarchy: {h:?}");
        // The session carries on after NAMESPACE.
        let again = list_folders(1, &mut sess, &roles, &[])
            .await
            .expect("list again");
        assert_eq!(listed.len(), again.len());

        let paths = |fs: &[crate::models::Folder]| -> Vec<(String, bool)> {
            fs.iter().map(|f| (f.path.clone(), f.childless)).collect()
        };
        let mut top = h
            .child("", &h.leaf_for("h554 Café").unwrap())
            .expect("a top-level path");
        // As the worker does: a server that didn't say where its folders go
        // and refuses this one at the top takes it inside the Inbox.
        match create_folder(&mut sess, &top).await {
            Ok(()) => {}
            Err(async_imap::error::Error::No(why)) => {
                eprintln!("refused at the top: {why}");
                let cache = crate::cache::Cache::in_memory().unwrap();
                cache.save_imap_hierarchy(1, &h);
                let (_, inside) = inside_inbox(Some(&cache), 1, &top)
                    .expect("only a server that didn't say is asked again");
                create_folder(&mut sess, &inside)
                    .await
                    .expect("create inside the Inbox");
                h = FolderHierarchy::imap(
                    h.delimiter,
                    Some(inside[..inside.len() - top.len()].to_string()),
                );
                eprintln!("learned: {h:?}");
                top = inside;
            }
            Err(e) => panic!("create top: {e}"),
        }
        let fs = list_folders(1, &mut sess, &roles, &[]).await.unwrap();
        eprintln!("after create: {:?}", paths(&fs));
        let made = fs
            .iter()
            .find(|f| f.path == top)
            .expect("listed where built");
        assert_eq!(made.name, "h554 Café");
        assert_eq!(h.parent(&top), "");

        if !h.nests() || made.childless {
            eprintln!("this server makes no folder inside {top:?}: the app offers none");
            delete_folder(&mut sess, &top, None).await.expect("delete");
            return;
        }
        let child = h
            .child(
                &top,
                &h.leaf_for("v1.2").or_else(|_| h.leaf_for("v1_2")).unwrap(),
            )
            .unwrap();
        create_folder(&mut sess, &child)
            .await
            .expect("create child");
        let fs = list_folders(1, &mut sess, &roles, &[]).await.unwrap();
        eprintln!("after child: {:?}", paths(&fs));
        assert!(fs.iter().any(|f| f.path == child), "{child:?} listed");
        assert!(h.is_under(&child, &top));
        assert_eq!(h.parent(&child), top);

        // Out to the top under a new name, as a drag and a rename do.
        let moved = h.child("", &h.leaf_for("h554 moved").unwrap()).unwrap();
        rename_folder(&mut sess, &child, &moved)
            .await
            .expect("move to top");
        let fs = list_folders(1, &mut sess, &roles, &[]).await.unwrap();
        eprintln!("after move: {:?}", paths(&fs));
        assert!(fs.iter().any(|f| f.path == moved));
        assert!(!fs.iter().any(|f| f.path == child));
        assert_eq!(h.parent(&moved), "");

        delete_folder(&mut sess, &moved, None)
            .await
            .expect("delete moved");
        delete_folder(&mut sess, &top, None)
            .await
            .expect("delete top");
        let fs = list_folders(1, &mut sess, &roles, &[]).await.unwrap();
        assert!(!fs.iter().any(|f| f.path == top || f.path == moved));
    });
}
