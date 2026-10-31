//! The mail of an On This Computer account (#552), as a Maildir++ tree: the
//! Inbox is the root's `cur`, `new` and `tmp`, every other folder a `.Name`
//! directory beside them, a sub-folder `.Parent.Child`. Dovecot keeps the
//! same layout, which lets its tree be copied in, and any Maildir reader
//! read this one.
//!
//! A message is one file, named `<unique>:2,<flags>`; its read, starred,
//! answered and forwarded state are letters of the name, its tags lower
//! case letters that `dovecot-keywords` spells out. Each folder keeps the
//! number the app knows each message by in `hylki-uids`, given in order
//! as files arrive, so the cache and the list key the mail as they key a
//! server's.
//!
//! Only this account's worker writes the tree. A file put in by hand is
//! found the next time its folder is read.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The file that keeps a folder's message numbers.
const UIDS_FILE: &str = "hylki-uids";
/// The file that names a folder's tag letters, as Dovecot writes it.
const KEYWORDS_FILE: &str = "dovecot-keywords";
/// The Inbox, the tree's root.
pub(super) const INBOX: &str = "INBOX";

/// One message as its folder lists it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Entry {
    pub(super) uid: u32,
    /// The system flags, Maildir's capital letters.
    pub(super) flags: String,
    /// The tags its lower case letters stand for.
    pub(super) keywords: Vec<String>,
}

impl Entry {
    pub(super) fn has(&self, flag: char) -> bool {
        self.flags.contains(flag)
    }
}

/// What the app knows of one folder on disk: each message's number and
/// file name in `cur`, and the next number to give.
#[derive(Default)]
struct FolderIndex {
    files: BTreeMap<u32, String>,
    next_uid: u32,
    /// Numbers given since the uid file was last written.
    dirty: bool,
}

/// The tree, and the folders read so far.
pub(super) struct Maildir {
    root: PathBuf,
    folders: HashMap<String, FolderIndex>,
    /// Uid files are written when this is lifted, not at each message: a
    /// batch of moves or an import rewrote a large folder's file each time.
    holding: bool,
}

impl Maildir {
    /// The tree at `root`, with its Inbox and Trash made if missing.
    pub(super) fn open(root: PathBuf) -> std::io::Result<Maildir> {
        let md = Maildir {
            root,
            folders: HashMap::new(),
            holding: false,
        };
        md.make_folder_dirs(INBOX)?;
        md.make_folder_dirs("Trash")?;
        Ok(md)
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// The directory of the folder at `path` (`/` between levels).
    fn dir(&self, path: &str) -> PathBuf {
        folder_dir(&self.root, path)
    }

    fn make_folder_dirs(&self, path: &str) -> std::io::Result<()> {
        make_folder_dirs(&self.root, path)
    }

    /// Every folder's path, the Inbox first.
    pub(super) fn folder_paths(&self) -> Vec<String> {
        let mut out = vec![INBOX.to_string()];
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return out;
        };
        let mut rest: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().join("cur").is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|name| {
                let name = name.strip_prefix('.')?;
                (!name.is_empty()).then(|| name.split('.').map(decode_name).collect::<Vec<_>>())
            })
            .map(|parts| parts.join("/"))
            .collect();
        rest.sort();
        out.extend(rest);
        out
    }

    pub(super) fn has_folder(&self, path: &str) -> bool {
        self.dir(path).join("cur").is_dir()
    }

    pub(super) fn create_folder(&self, path: &str) -> Result<(), String> {
        check_path(path)?;
        if self.has_folder(path) {
            return Err(crate::i18n::i18n("A folder by that name already exists"));
        }
        self.make_folder_dirs(path).map_err(|e| e.to_string())
    }

    /// Rename a folder, its sub-folders with it.
    pub(super) fn rename_folder(&mut self, old: &str, new: &str) -> Result<(), String> {
        check_path(new)?;
        if old == INBOX || !self.has_folder(old) {
            return Err(crate::i18n::i18n("unknown folder"));
        }
        if self.has_folder(new) {
            return Err(crate::i18n::i18n("A folder by that name already exists"));
        }
        let below = format!("{old}/");
        let moves: Vec<(String, String)> = self
            .folder_paths()
            .into_iter()
            .filter_map(|p| {
                let to = if p == old {
                    new.to_string()
                } else {
                    format!("{new}/{}", p.strip_prefix(&below)?)
                };
                Some((p, to))
            })
            .collect();
        for (from, to) in &moves {
            std::fs::rename(self.dir(from), self.dir(to)).map_err(|e| e.to_string())?;
            self.folders.remove(from);
        }
        Ok(())
    }

    /// Delete an empty folder; what is in it has to be moved or deleted
    /// first, and a folder still holding mail is refused. Its sub-folders
    /// stay.
    pub(super) fn delete_folder(&mut self, path: &str) -> Result<(), String> {
        if path == INBOX || !self.has_folder(path) {
            return Err(crate::i18n::i18n("unknown folder"));
        }
        if self.counts(path).0 > 0 {
            return Err(crate::i18n::i18n("The folder still holds mail"));
        }
        let dir = self.dir(path);
        for sub in ["cur", "new", "tmp"] {
            std::fs::remove_dir_all(dir.join(sub)).map_err(|e| e.to_string())?;
        }
        for file in [UIDS_FILE, KEYWORDS_FILE] {
            let _ = std::fs::remove_file(dir.join(file));
        }
        // Anything else in it (a client's index) goes with it.
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
        self.folders.remove(path);
        Ok(())
    }

    /// The folder's messages, read from disk: files that arrived in `new`
    /// move to `cur`, as a mail reader takes them, and new files are
    /// numbered after the ones already known.
    pub(super) fn scan(&mut self, path: &str) -> std::io::Result<Vec<Entry>> {
        let dir = self.dir(path);
        if !dir.join("cur").is_dir() {
            return Ok(Vec::new());
        }
        // Mail dropped in `new` is taken in as unread.
        if let Ok(new) = std::fs::read_dir(dir.join("new")) {
            for e in new.flatten() {
                let Ok(name) = e.file_name().into_string() else {
                    continue;
                };
                if name.starts_with('.') {
                    continue;
                }
                let unique = unique_of(&name);
                let _ = std::fs::rename(e.path(), dir.join("cur").join(format!("{unique}:2,")));
            }
        }
        let mut names: Vec<String> = std::fs::read_dir(dir.join("cur"))?
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.'))
            .collect();
        // Maildir names start with the time of delivery: mail found for the
        // first time is numbered oldest first.
        names.sort();
        let (known, mut next) = read_uids(&dir);
        let mut by_unique: HashMap<&str, &String> = HashMap::new();
        for n in &names {
            by_unique.insert(unique_of(n), n);
        }
        let mut files = BTreeMap::new();
        for (uid, unique) in &known {
            if let Some(name) = by_unique.remove(unique.as_str()) {
                files.insert(*uid, name.clone());
            }
        }
        let mut dirty = files.len() != known.len();
        for n in &names {
            if by_unique.remove(unique_of(n)).is_some() {
                files.insert(next, n.clone());
                next += 1;
                dirty = true;
            }
        }
        let index = FolderIndex {
            files,
            next_uid: next,
            dirty,
        };
        let letters = read_keywords(&dir);
        let entries = index
            .files
            .iter()
            .map(|(uid, name)| entry(*uid, name, &letters))
            .collect();
        self.folders.insert(path.to_string(), index);
        self.flush_folder(path);
        Ok(entries)
    }

    /// The folder's index, read from disk the first time it is asked for.
    fn index(&mut self, path: &str) -> std::io::Result<&mut FolderIndex> {
        if !self.folders.contains_key(path) {
            self.scan(path)?;
        }
        Ok(self.folders.entry(path.to_string()).or_default())
    }

    /// The file of message `uid` in `path`, read again from disk if it is
    /// not where the index last saw it (renamed by another program).
    fn file(&mut self, path: &str, uid: u32) -> std::io::Result<PathBuf> {
        let cur = self.dir(path).join("cur");
        if let Some(name) = self.index(path)?.files.get(&uid) {
            let file = cur.join(name);
            if file.is_file() {
                return Ok(file);
            }
        }
        self.scan(path)?;
        self.folders
            .get(path)
            .and_then(|i| i.files.get(&uid))
            .map(|name| cur.join(name))
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
    }

    /// The message's bytes.
    pub(super) fn read(&mut self, path: &str, uid: u32) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.file(path, uid)?)
    }

    /// The message's flags and tags as its file name says.
    pub(super) fn entry(&mut self, path: &str, uid: u32) -> std::io::Result<Entry> {
        let file = self.file(path, uid)?;
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        Ok(entry(uid, &name, &read_keywords(&self.dir(path))))
    }

    /// Store a message in `path` with `flags` and `keywords`, written in
    /// full and synced before it shows up in the folder. Returns its number.
    pub(super) fn deliver(
        &mut self,
        path: &str,
        raw: &[u8],
        flags: &str,
        keywords: &[String],
    ) -> std::io::Result<u32> {
        if !self.has_folder(path) {
            self.make_folder_dirs(path)?;
        }
        // Read before the file lands, or the reading would number it too.
        self.index(path)?;
        let dir = self.dir(path);
        let letters = keyword_letters(&dir, keywords, true);
        let name = write_message(&dir, raw, &format!("{flags}{letters}"))?;
        let index = self.index(path)?;
        let uid = index.next_uid.max(1);
        index.next_uid = uid + 1;
        index.files.insert(uid, name);
        index.dirty = true;
        self.flush_folder(path);
        Ok(uid)
    }

    /// Change a message's flags: `add` and `remove` are Maildir letters.
    pub(super) fn set_flags(
        &mut self,
        path: &str,
        uid: u32,
        add: &str,
        remove: &str,
    ) -> std::io::Result<()> {
        let file = self.file(path, uid)?;
        let name = file_name_of(&file);
        let (unique, flags) = split_name(&name);
        let mut kept: Vec<char> = flags
            .chars()
            .filter(|c| !remove.contains(*c) && !add.contains(*c))
            .collect();
        kept.extend(add.chars());
        let renamed = format!("{unique}:2,{}", sorted(&kept));
        self.rename_in_place(path, uid, &file, renamed)
    }

    /// Put a tag on a message or take it off. False when the folder has
    /// no letter left for a new tag (Maildir has 26).
    pub(super) fn set_keyword(
        &mut self,
        path: &str,
        uid: u32,
        keyword: &str,
        add: bool,
    ) -> std::io::Result<bool> {
        let dir = self.dir(path);
        let letters = keyword_letters(&dir, &[keyword.to_string()], add);
        let Some(letter) = letters.chars().next() else {
            return Ok(!add);
        };
        let (add, remove) = if add {
            (letter.to_string(), String::new())
        } else {
            (String::new(), letter.to_string())
        };
        self.set_flags(path, uid, &add, &remove)?;
        Ok(true)
    }

    fn rename_in_place(
        &mut self,
        path: &str,
        uid: u32,
        file: &Path,
        renamed: String,
    ) -> std::io::Result<()> {
        if file_name_of(file) != renamed {
            std::fs::rename(file, file.with_file_name(&renamed))?;
        }
        if let Some(index) = self.folders.get_mut(path) {
            index.files.insert(uid, renamed);
        }
        Ok(())
    }

    /// Move a message to `dest` (made if missing), its flags and tags with
    /// it. Returns its number there.
    pub(super) fn move_to(&mut self, path: &str, uid: u32, dest: &str) -> std::io::Result<u32> {
        if path == dest {
            return Ok(uid);
        }
        if !self.has_folder(dest) {
            self.make_folder_dirs(dest)?;
        }
        // Read before the file lands, or the reading would number it too.
        self.index(dest)?;
        let file = self.file(path, uid)?;
        let name = file_name_of(&file);
        let (unique, flags) = split_name(&name);
        // The two folders may give one tag different letters.
        let keywords = keywords_of(flags, &read_keywords(&self.dir(path)));
        let system: String = flags.chars().filter(|c| c.is_ascii_uppercase()).collect();
        let dest_dir = self.dir(dest);
        let letters = keyword_letters(&dest_dir, &keywords, true);
        let moved = file_name(unique, &system, &letters);
        std::fs::rename(&file, dest_dir.join("cur").join(&moved))?;
        if let Some(index) = self.folders.get_mut(path) {
            index.files.remove(&uid);
        }
        let index = self.index(dest)?;
        let new_uid = index.next_uid.max(1);
        index.next_uid = new_uid + 1;
        index.files.insert(new_uid, moved);
        index.dirty = true;
        self.flush_folder(dest);
        Ok(new_uid)
    }

    /// Delete a message for good.
    pub(super) fn delete(&mut self, path: &str, uid: u32) -> std::io::Result<()> {
        let file = self.file(path, uid)?;
        std::fs::remove_file(file)?;
        if let Some(index) = self.folders.get_mut(path) {
            index.files.remove(&uid);
        }
        Ok(())
    }

    /// The messages that came into the folder before `cutoff` (a Unix
    /// time): a file's change time is when it was last delivered or moved,
    /// which is how long it has been in the folder.
    pub(super) fn older_than(&mut self, path: &str, cutoff: i64) -> Vec<u32> {
        use std::os::unix::fs::MetadataExt;
        if self.scan(path).is_err() {
            return Vec::new();
        }
        let cur = self.dir(path).join("cur");
        self.folders
            .get(path)
            .map(|index| {
                index
                    .files
                    .iter()
                    .filter(|(_, name)| {
                        std::fs::metadata(cur.join(name.as_str())).is_ok_and(|m| m.ctime() < cutoff)
                    })
                    .map(|(uid, _)| *uid)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// How many messages the folder holds, and how many of them are
    /// unread, from the file names alone.
    pub(super) fn counts(&self, path: &str) -> (u32, u32) {
        let dir = self.dir(path);
        let mut total = 0;
        let mut unread = 0;
        for sub in ["cur", "new"] {
            let Ok(entries) = std::fs::read_dir(dir.join(sub)) else {
                continue;
            };
            for e in entries.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') {
                    continue;
                }
                total += 1;
                if sub == "new" || !split_name(&name).1.contains('S') {
                    unread += 1;
                }
            }
        }
        (total, unread)
    }

    /// Hold the uid files back while `hold`, for a batch; lifting it
    /// writes every one that changed.
    pub(super) fn hold_uid_files(&mut self, hold: bool) {
        self.holding = hold;
        if !hold {
            let dirty: Vec<String> = self
                .folders
                .iter()
                .filter(|(_, i)| i.dirty)
                .map(|(p, _)| p.clone())
                .collect();
            for path in dirty {
                self.flush_folder(&path);
            }
        }
    }

    /// Write the folder's uid file if numbers were given since.
    fn flush_folder(&mut self, path: &str) {
        if self.holding {
            return;
        }
        let dir = self.dir(path);
        let Some(index) = self.folders.get_mut(path).filter(|i| i.dirty) else {
            return;
        };
        let mut text = format!("next {}\n", index.next_uid.max(1));
        for (uid, name) in &index.files {
            text.push_str(&format!("{uid} {}\n", unique_of(name)));
        }
        match write_atomic(&dir.join(UIDS_FILE), text.as_bytes()) {
            Ok(()) => index.dirty = false,
            Err(e) => tracing::warn!("local mail: could not write the uid file of {path}: {e}"),
        }
    }
}

/// The directory of the folder at `path` (`/` between levels) in the tree
/// at `root`.
fn folder_dir(root: &Path, path: &str) -> PathBuf {
    if path == INBOX {
        return root.to_path_buf();
    }
    let name: Vec<String> = path.split('/').map(encode_name).collect();
    root.join(format!(".{}", name.join(".")))
}

fn make_folder_dirs(root: &Path, path: &str) -> std::io::Result<()> {
    let dir = folder_dir(root, path);
    for sub in ["cur", "new", "tmp"] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    Ok(())
}

/// Write a message into the folder at `dir`: in full under `tmp`, synced,
/// then renamed into `cur` with `flags` (letters in any order). Returns
/// the file's name.
fn write_message(dir: &Path, raw: &[u8], flags: &str) -> std::io::Result<String> {
    let unique = unique_name();
    let tmp = dir.join("tmp").join(&unique);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(raw)?;
        f.sync_all()?;
    }
    let name = file_name(&unique, flags, "");
    std::fs::rename(&tmp, dir.join("cur").join(&name))?;
    sync_dir(&dir.join("cur"));
    Ok(name)
}

/// Store a message in the tree at `root` from outside its worker, as any
/// Maildir delivery does: the worker numbers it the next time it reads the
/// folder. The folder at `path`, and every one above it, are made if
/// missing.
pub(in crate::worker) fn deliver_into(
    root: &Path,
    path: &str,
    raw: &[u8],
    flags: &str,
) -> std::io::Result<()> {
    let mut above = String::new();
    for level in path.split('/') {
        if !above.is_empty() {
            above.push('/');
        }
        above.push_str(level);
        make_folder_dirs(root, &above)?;
    }
    write_message(&folder_dir(root, path), raw, flags).map(|_| ())
}

/// A name no other file in the tree has: Maildir's time, process and
/// count, and the host. The count is the process's, as every account's
/// worker may deliver.
fn unique_name() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static DELIVERED: AtomicU32 = AtomicU32::new(0);
    let n = DELIVERED.fetch_add(1, Ordering::Relaxed) + 1;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let host = hostname().replace(['/', ':'], "_");
    format!(
        "{}.M{}P{}Q{n}.{host}",
        now.as_secs(),
        now.subsec_micros(),
        std::process::id(),
    )
}

/// A folder path the tree can hold: no empty level, nothing hidden.
fn check_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path == INBOX || path.split('/').any(|p| p.trim().is_empty()) {
        return Err(crate::i18n::i18n("That is not a folder name"));
    }
    Ok(())
}

/// A folder name as a directory name: Maildir++ separates levels with a
/// dot, so a dot in a name is written `_2E`, and the underscore that
/// starts such a code `_5F`, as Evolution writes them.
fn encode_name(name: &str) -> String {
    name.replace('_', "_5F").replace('.', "_2E")
}

pub(super) fn decode_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(i) = rest.find('_') {
        out.push_str(&rest[..i]);
        let code = &rest[i + 1..];
        match code.get(..2).and_then(|h| u8::from_str_radix(h, 16).ok()) {
            Some(b) if b.is_ascii() => {
                out.push(b as char);
                rest = &code[2..];
            }
            _ => {
                out.push('_');
                rest = code;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The unique part of a message file's name.
fn unique_of(name: &str) -> &str {
    split_name(name).0
}

/// A file name's unique part and its flags.
pub(super) fn split_name(name: &str) -> (&str, &str) {
    match name.split_once(":2,") {
        Some((unique, flags)) => (unique, flags),
        None => (name.split(':').next().unwrap_or(name), ""),
    }
}

fn file_name_of(file: &Path) -> String {
    file.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string()
}

/// A file name for `unique` with system flags and tag letters, the
/// letters in order as Maildir asks.
fn file_name(unique: &str, flags: &str, letters: &str) -> String {
    let all: Vec<char> = flags.chars().chain(letters.chars()).collect();
    format!("{unique}:2,{}", sorted(&all))
}

fn sorted(letters: &[char]) -> String {
    let mut v = letters.to_vec();
    v.sort_unstable();
    v.dedup();
    v.into_iter().collect()
}

fn entry(uid: u32, name: &str, letters: &BTreeMap<char, String>) -> Entry {
    let flags = split_name(name).1;
    Entry {
        uid,
        flags: flags.chars().filter(|c| c.is_ascii_uppercase()).collect(),
        keywords: keywords_of(flags, letters),
    }
}

/// The tags a file name's lower case letters stand for.
pub(super) fn keywords_of(flags: &str, letters: &BTreeMap<char, String>) -> Vec<String> {
    flags
        .chars()
        .filter_map(|c| letters.get(&c).cloned())
        .collect()
}

/// A folder's uid file: number → unique name, and the next number.
fn read_uids(dir: &Path) -> (Vec<(u32, String)>, u32) {
    let Ok(text) = std::fs::read_to_string(dir.join(UIDS_FILE)) else {
        return (Vec::new(), 1);
    };
    let mut next = 1;
    let mut known = Vec::new();
    for line in text.lines() {
        let Some((a, b)) = line.split_once(' ') else {
            continue;
        };
        if a == "next" {
            next = b.trim().parse().unwrap_or(1);
        } else if let Ok(uid) = a.parse::<u32>() {
            known.push((uid, b.trim().to_string()));
            next = next.max(uid + 1);
        }
    }
    (known, next)
}

/// A folder's tag letters: `a` for the tag numbered 0, and so on.
pub(super) fn read_keywords(dir: &Path) -> BTreeMap<char, String> {
    let Ok(text) = std::fs::read_to_string(dir.join(KEYWORDS_FILE)) else {
        return BTreeMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let (n, name) = line.split_once(' ')?;
            let n: u8 = n.parse().ok()?;
            (n < 26).then(|| ((b'a' + n) as char, name.trim().to_string()))
        })
        .collect()
}

/// The letters `keywords` have in the folder at `dir`; with `add`, a tag
/// without one is given the first free letter, while there is one.
fn keyword_letters(dir: &Path, keywords: &[String], add: bool) -> String {
    let mut letters = read_keywords(dir);
    let mut changed = false;
    let mut out = String::new();
    for k in keywords {
        let found = letters.iter().find(|(_, name)| *name == k).map(|(c, _)| *c);
        let letter = match found {
            Some(c) => Some(c),
            None if add => ('a'..='z').find(|c| !letters.contains_key(c)).inspect(|c| {
                letters.insert(*c, k.clone());
                changed = true;
            }),
            None => None,
        };
        match letter {
            Some(c) => out.push(c),
            None if add => tracing::warn!("local mail: no letter left for the tag {k}"),
            None => {}
        }
    }
    if changed {
        let text: String = letters
            .iter()
            .map(|(c, name)| format!("{} {name}\n", *c as u8 - b'a'))
            .collect();
        if let Err(e) = write_atomic(&dir.join(KEYWORDS_FILE), text.as_bytes()) {
            tracing::warn!("local mail: could not write the tag letters: {e}");
        }
    }
    out
}

/// Write `path` whole or not at all.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Make a rename into `dir` survive a power cut.
fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

#[cfg(test)]
#[path = "maildir_tests.rs"]
pub(super) mod tests;
