use super::*;

const RAW: &[u8] = b"From: a@example.org\r\nSubject: Hi\r\n\r\nbody\r\n";

/// A directory of its own for one test, gone when it ends.
pub(in crate::worker::local) struct Scratch(PathBuf);

impl Scratch {
    pub(in crate::worker::local) fn new() -> Scratch {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hylki-local-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    pub(in crate::worker::local) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tree() -> (Scratch, Maildir) {
    let dir = Scratch::new();
    let md = Maildir::open(dir.path().join("local")).expect("open");
    (dir, md)
}

fn cur_names(md: &Maildir, path: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(md.dir(path).join("cur"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

#[test]
fn a_new_tree_has_an_inbox_and_a_trash() {
    let (_d, md) = tree();
    assert_eq!(md.folder_paths(), ["INBOX", "Trash"]);
    assert!(md.root().join("cur").is_dir());
    assert!(md.root().join(".Trash/new").is_dir());
}

#[test]
fn folder_names_with_dots_and_levels_round_trip() {
    let (_d, md) = tree();
    md.create_folder("Work/v1.2_final").unwrap();
    assert!(md.root().join(".Work.v1_2E2_5Ffinal/cur").is_dir());
    assert!(md.folder_paths().contains(&"Work/v1.2_final".to_string()));
    assert_eq!(decode_name("plain_name"), "plain_name");
    assert!(md.create_folder("Work/v1.2_final").is_err());
    assert!(md.create_folder("a//b").is_err());
    assert!(md.create_folder("INBOX").is_err());
}

#[test]
fn delivered_mail_is_numbered_in_order_and_kept_across_a_restart() {
    let (d, mut md) = tree();
    let a = md.deliver("INBOX", RAW, "S", &[]).unwrap();
    let b = md.deliver("INBOX", RAW, "", &[]).unwrap();
    assert_eq!((a, b), (1, 2));
    assert_eq!(md.read("INBOX", a).unwrap(), RAW);
    assert!(md.root().join("tmp").read_dir().unwrap().next().is_none());

    let mut again = Maildir::open(d.path().join("local")).unwrap();
    let entries = again.scan("INBOX").unwrap();
    let got: Vec<(u32, &str)> = entries.iter().map(|e| (e.uid, e.flags.as_str())).collect();
    assert_eq!(got, [(1, "S"), (2, "")]);
    assert_eq!(again.deliver("INBOX", RAW, "", &[]).unwrap(), 3);
    assert_eq!(again.counts("INBOX"), (3, 2));
}

#[test]
fn mail_put_in_by_hand_is_found_and_numbered_after_the_rest() {
    let (_d, mut md) = tree();
    md.deliver("INBOX", RAW, "S", &[]).unwrap();
    std::fs::write(md.root().join("new/9999.hand.host"), RAW).unwrap();
    std::fs::write(md.root().join("cur/1.old.host:2,FS"), RAW).unwrap();
    let entries = md.scan("INBOX").unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].uid, 1);
    let by_uid: Vec<(u32, String)> = entries.iter().map(|e| (e.uid, e.flags.clone())).collect();
    // Found together, they are numbered by name: the older one first.
    assert_eq!(by_uid[1], (2, "FS".to_string()));
    assert_eq!(by_uid[2], (3, String::new()));
    assert!(cur_names(&md, INBOX).contains(&"9999.hand.host:2,".to_string()));
}

#[test]
fn flags_change_the_file_name_and_stay_sorted() {
    let (_d, mut md) = tree();
    let uid = md.deliver("INBOX", RAW, "", &[]).unwrap();
    md.set_flags("INBOX", uid, "SF", "").unwrap();
    md.set_flags("INBOX", uid, "R", "F").unwrap();
    let e = md.entry("INBOX", uid).unwrap();
    assert_eq!(e.flags, "RS");
    assert!(cur_names(&md, INBOX)[0].ends_with(":2,RS"));
    assert!(e.has('S') && !e.has('F'));
}

#[test]
fn tags_are_letters_the_keywords_file_names() {
    let (_d, mut md) = tree();
    let uid = md
        .deliver("INBOX", RAW, "S", &["$label1".to_string()])
        .unwrap();
    assert!(md.set_keyword("INBOX", uid, "Work", true).unwrap());
    let e = md.entry("INBOX", uid).unwrap();
    assert_eq!(e.keywords, ["$label1", "Work"]);
    assert_eq!(
        std::fs::read_to_string(md.root().join(KEYWORDS_FILE)).unwrap(),
        "0 $label1\n1 Work\n"
    );
    assert!(cur_names(&md, INBOX)[0].ends_with(":2,Sab"));
    md.set_keyword("INBOX", uid, "$label1", false).unwrap();
    assert_eq!(md.entry("INBOX", uid).unwrap().keywords, ["Work"]);
}

#[test]
fn a_move_keeps_flags_and_tags_whatever_letters_they_have_there() {
    let (_d, mut md) = tree();
    md.create_folder("Archive").unwrap();
    // The destination already gives `a` to another tag.
    let other = md
        .deliver("Archive", RAW, "", &["Other".to_string()])
        .unwrap();
    let uid = md
        .deliver("INBOX", RAW, "FS", &["Work".to_string()])
        .unwrap();
    let moved = md.move_to("INBOX", uid, "Archive").unwrap();
    assert_eq!((other, moved), (1, 2));
    assert!(md.scan("INBOX").unwrap().is_empty());
    let e = md.entry("Archive", moved).unwrap();
    assert_eq!(
        (e.flags.as_str(), e.keywords.clone()),
        ("FS", vec!["Work".to_string()])
    );
    assert_eq!(md.read("Archive", moved).unwrap(), RAW);
    // A move to a folder that does not exist makes it.
    let back = md.move_to("Archive", moved, "Snoozed").unwrap();
    assert_eq!(back, 1);
    assert!(md.has_folder("Snoozed"));
}

#[test]
fn a_file_renamed_by_another_program_is_found_again() {
    let (_d, mut md) = tree();
    let uid = md.deliver("INBOX", RAW, "", &[]).unwrap();
    let name = cur_names(&md, INBOX).remove(0);
    let cur = md.root().join("cur");
    std::fs::rename(cur.join(&name), cur.join(format!("{name}S"))).unwrap();
    assert_eq!(md.entry("INBOX", uid).unwrap().flags, "S");
    assert_eq!(md.read("INBOX", uid).unwrap(), RAW);
}

#[test]
fn deleting_removes_the_file_and_an_unknown_message_is_an_error() {
    let (_d, mut md) = tree();
    let uid = md.deliver("INBOX", RAW, "", &[]).unwrap();
    md.delete("INBOX", uid).unwrap();
    assert!(cur_names(&md, INBOX).is_empty());
    assert!(md.read("INBOX", uid).is_err());
    // Numbers are not given again.
    assert_eq!(md.deliver("INBOX", RAW, "", &[]).unwrap(), 2);
}

#[test]
fn renaming_a_folder_takes_its_sub_folders_along() {
    let (_d, mut md) = tree();
    md.create_folder("Work").unwrap();
    md.create_folder("Work/2025").unwrap();
    md.create_folder("Workshop").unwrap();
    let uid = md.deliver("Work/2025", RAW, "", &[]).unwrap();
    md.rename_folder("Work", "Jobs").unwrap();
    let paths = md.folder_paths();
    assert!(paths.contains(&"Jobs".to_string()));
    assert!(paths.contains(&"Jobs/2025".to_string()));
    assert!(paths.contains(&"Workshop".to_string()));
    assert!(!paths.contains(&"Work".to_string()));
    assert_eq!(md.read("Jobs/2025", uid).unwrap(), RAW);
    assert!(md.rename_folder("INBOX", "Other").is_err());
}

#[test]
fn only_an_empty_folder_is_deleted() {
    let (_d, mut md) = tree();
    md.create_folder("Old").unwrap();
    let uid = md.deliver("Old", RAW, "", &[]).unwrap();
    assert!(md.delete_folder("Old").is_err());
    md.move_to("Old", uid, "Trash").unwrap();
    md.delete_folder("Old").unwrap();
    assert!(!md.has_folder("Old"));
    assert!(md.delete_folder("INBOX").is_err());
}

#[test]
fn age_in_a_folder_counts_from_when_mail_came_in() {
    let (_d, mut md) = tree();
    let uid = md.deliver("INBOX", RAW, "", &[]).unwrap();
    let now = crate::datefmt::now();
    assert!(md.older_than("INBOX", now - 3600).is_empty());
    assert_eq!(md.older_than("INBOX", now + 3600), [uid]);
    // A move is a new arrival in the Trash.
    let moved = md.move_to("INBOX", uid, "Trash").unwrap();
    assert!(md.older_than("Trash", now - 3600).is_empty());
    assert_eq!(md.older_than("Trash", now + 3600), [moved]);
}
