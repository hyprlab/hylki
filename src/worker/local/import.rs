//! Importing mail into On This Computer (#552): a Maildir tree, such as
//! Evolution's own local folders or Dovecot's, or mbox files, such as
//! Thunderbird's. The whole import is planned at once, then a batch is
//! stored each step between requests. A message whose Message-ID its folder
//! already holds is left out, so importing twice adds nothing.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{BufRead, Read, Seek};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::maildir::{decode_name, keywords_of, read_keywords, split_name, INBOX};
use super::LocalWorker;
use crate::i18n::i18n_f;
use crate::worker::WorkerEvent;

/// Messages stored a step.
const BATCH: usize = 50;

/// Where one message comes from.
enum Source {
    /// A Maildir file, with the flags and tags its name gives.
    File {
        path: PathBuf,
        flags: String,
        keywords: Vec<String>,
    },
    /// A stretch of an mbox file, after its `From ` line.
    Mbox {
        file: Rc<PathBuf>,
        start: u64,
        end: u64,
    },
}

/// One message to import, and the local folder it goes to.
struct Item {
    source: Source,
    dest: String,
}

/// An import under way.
#[derive(Default)]
pub(super) struct Import {
    queue: VecDeque<Item>,
    total: usize,
    stored: usize,
    /// Already in the folder, or in the import twice.
    skipped: usize,
    /// The Message-IDs each folder holds, read when the first message for
    /// it comes up.
    known: HashMap<String, HashSet<String>>,
    dests: BTreeSet<String>,
}

impl Import {
    pub(super) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl<E: Fn(WorkerEvent)> LocalWorker<E> {
    /// Plan an import of `sources`, Maildir trees or, with `mbox`, mbox
    /// files, behind any import under way.
    pub(super) fn on_import(&mut self, sources: Vec<PathBuf>, mbox: bool) {
        let own = self.store.root().canonicalize().ok();
        for source in sources {
            let planned = if source
                .canonicalize()
                .ok()
                .is_some_and(|s| Some(&s) == own.as_ref())
            {
                Ok(Vec::new())
            } else if mbox {
                plan_mbox(&source)
            } else {
                plan_maildir(&source)
            };
            match planned {
                Ok(items) => {
                    tracing::info!(
                        "local mail: importing {} message(s) from {}",
                        items.len(),
                        source.display()
                    );
                    self.import.total += items.len();
                    self.import.queue.extend(items);
                }
                Err(e) => (self.emit)(WorkerEvent::error(i18n_f(
                    "Could not import {path}: {e}",
                    &[
                        ("path", &source.display().to_string()),
                        ("e", &e.to_string()),
                    ],
                ))),
            }
        }
    }

    /// Store the next batch; once the queue is empty, show what came in.
    pub(super) fn import_step(&mut self) {
        let mut import = std::mem::take(&mut self.import);
        self.store.hold_uid_files(true);
        for _ in 0..BATCH {
            let Some(item) = import.queue.pop_front() else {
                break;
            };
            match self.import_one(&mut import, item) {
                Ok(true) => import.stored += 1,
                Ok(false) => import.skipped += 1,
                Err(e) => {
                    import.skipped += 1;
                    tracing::warn!("local mail: a message could not be imported: {e}");
                }
            }
        }
        self.store.hold_uid_files(false);
        let done = import.stored + import.skipped;
        if import.queue.is_empty() {
            tracing::info!(
                "local mail: imported {} message(s), {} already here or unreadable",
                import.stored,
                import.skipped
            );
            (self.emit)(WorkerEvent::Status(String::new()));
            self.refresh_folders();
            for dest in &import.dests {
                if !self.unread_folders.contains(dest) {
                    self.unread_folders.push(dest.clone());
                }
            }
            self.import = Import::default();
            return;
        }
        (self.emit)(WorkerEvent::Status(i18n_f(
            "Importing {done} of {total} messages…",
            &[
                ("done", &done.to_string()),
                ("total", &import.total.to_string()),
            ],
        )));
        self.import = import;
    }

    /// Store one message unless its folder has it. True when stored.
    fn import_one(&mut self, import: &mut Import, item: Item) -> std::io::Result<bool> {
        let Some((raw, flags, keywords)) = read_item(&item.source)? else {
            return Ok(false);
        };
        if !import.known.contains_key(&item.dest) {
            let ids: HashSet<String> = if self.store.has_folder(&item.dest) {
                let folder_id = self.folder_id(&item.dest);
                self.read_folder(folder_id, &item.dest)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|m| m.message_id)
                    .filter(|id| !id.is_empty())
                    .collect()
            } else {
                HashSet::new()
            };
            import.known.insert(item.dest.clone(), ids);
        }
        if let Some(id) = message_id_of(&raw) {
            let known = import.known.entry(item.dest.clone()).or_default();
            if !known.insert(id) {
                return Ok(false);
            }
        }
        self.store.deliver(&item.dest, &raw, &flags, &keywords)?;
        import.dests.insert(item.dest);
        Ok(true)
    }
}

/// Every message of the Maildir tree at `root`: the root itself is the
/// Inbox when it is a Maildir, `.Name` directories are Maildir++ folders
/// (Evolution's local folders, Dovecot), and other directories with a
/// `cur` are folders nested on disk, level by level.
fn plan_maildir(root: &Path) -> std::io::Result<Vec<Item>> {
    if !root.is_dir() {
        return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    }
    let mut items = Vec::new();
    walk(root, "", &mut items)?;
    Ok(items)
}

fn walk(dir: &Path, prefix: &str, items: &mut Vec<Item>) -> std::io::Result<()> {
    if dir.join("cur").is_dir() {
        let dest = if prefix.is_empty() { INBOX } else { prefix };
        add_folder(dir, dest, items);
    }
    let mut children: Vec<(String, PathBuf)> = std::fs::read_dir(dir)?
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
        .filter(|(name, _)| !matches!(name.as_str(), "cur" | "new" | "tmp"))
        .collect();
    children.sort();
    for (name, path) in children {
        let join = |rest: &str| {
            if prefix.is_empty() {
                rest.to_string()
            } else {
                format!("{prefix}/{rest}")
            }
        };
        match name.strip_prefix('.') {
            // Maildir++ keeps every level flat beside the Inbox.
            Some(rest) if !rest.is_empty() && !rest.starts_with(['.', '#']) => {
                if path.join("cur").is_dir() {
                    let levels: Vec<String> = rest.split('.').map(folder_name).collect();
                    add_folder(&path, &join(&levels.join("/")), items);
                }
            }
            Some(_) => {}
            None => walk(&path, &join(&folder_name(&name)), items)?,
        }
    }
    Ok(())
}

/// A folder name as a Maildir tree spells it: Evolution's `_2E` codes,
/// and Dovecot's modified UTF-7.
fn folder_name(name: &str) -> String {
    let name = decode_name(name);
    let name = if name.contains('&') {
        crate::mail::mutf7::decode(&name)
    } else {
        name
    };
    name.replace('/', "∕")
}

/// The messages of one Maildir folder, into `dest`. A message marked
/// deleted (`T`, waiting to be expunged) is left out.
fn add_folder(dir: &Path, dest: &str, items: &mut Vec<Item>) {
    let letters = read_keywords(dir);
    for sub in ["cur", "new"] {
        let Ok(entries) = std::fs::read_dir(dir.join(sub)) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.'))
            .collect();
        names.sort();
        for name in names {
            let flags = if sub == "cur" {
                split_name(&name).1
            } else {
                ""
            };
            if flags.contains('T') {
                continue;
            }
            items.push(Item {
                source: Source::File {
                    path: dir.join(sub).join(&name),
                    flags: flags.chars().filter(|c| c.is_ascii_uppercase()).collect(),
                    keywords: keywords_of(flags, &letters),
                },
                dest: dest.to_string(),
            });
        }
    }
}

/// Every message of an mbox file, into a folder named after it: Inbox
/// for one called Inbox, and Thunderbird's `Name.sbd` directories as the
/// folders above.
fn plan_mbox(file: &Path) -> std::io::Result<Vec<Item>> {
    let dest = mbox_folder(file);
    let mut reader = std::io::BufReader::new(std::fs::File::open(file)?);
    let mut line = Vec::new();
    let mut offset = 0u64;
    let mut blank_before = true;
    // (where the message starts, where its `From ` line starts)
    let mut starts: Vec<(u64, u64)> = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)? as u64;
        if n == 0 {
            break;
        }
        if blank_before && line.starts_with(b"From ") {
            starts.push((offset + n, offset));
        }
        blank_before = line == b"\n" || line == b"\r\n";
        offset += n;
    }
    let file = Rc::new(file.to_path_buf());
    Ok(starts
        .iter()
        .enumerate()
        .map(|(i, (start, _))| Item {
            source: Source::Mbox {
                file: file.clone(),
                start: *start,
                end: starts.get(i + 1).map_or(offset, |next| next.1),
            },
            dest: dest.clone(),
        })
        .collect())
}

fn mbox_folder(file: &Path) -> String {
    let stem = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Imported");
    let stem = [".mbox", ".mbx", ".mbs"]
        .iter()
        .find_map(|ext| stem.strip_suffix(ext))
        .unwrap_or(stem);
    let mut levels = vec![stem.replace('/', "∕")];
    let mut parent = file.parent();
    while let Some(dir) = parent {
        let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
            break;
        };
        let Some(above) = name.strip_suffix(".sbd") else {
            break;
        };
        levels.insert(0, above.to_string());
        parent = dir.parent();
    }
    if levels.len() == 1 && levels[0].eq_ignore_ascii_case("inbox") {
        return INBOX.to_string();
    }
    levels.join("/")
}

/// A message's bytes, flags and tags; `None` for one its mbox marks as
/// deleted.
type ReadItem = Option<(Vec<u8>, String, Vec<String>)>;

fn read_item(source: &Source) -> std::io::Result<ReadItem> {
    match source {
        Source::File {
            path,
            flags,
            keywords,
        } => Ok(Some((
            std::fs::read(path)?,
            flags.clone(),
            keywords.clone(),
        ))),
        Source::Mbox { file, start, end } => {
            let mut f = std::fs::File::open(file.as_path())?;
            f.seek(std::io::SeekFrom::Start(*start))?;
            let mut raw = vec![0; end.saturating_sub(*start) as usize];
            f.read_exact(&mut raw)?;
            // The blank line before the next `From ` is the separator's.
            if raw.ends_with(b"\r\n") {
                raw.truncate(raw.len() - 2);
            } else if raw.ends_with(b"\n") {
                raw.truncate(raw.len() - 1);
            }
            let raw = unescape_from(&raw);
            Ok(mbox_flags(&raw).map(|flags| (raw, flags, Vec::new())))
        }
    }
}

/// An mbox body line `>From ` (or `>>From `…) loses one `>`: the writer
/// added it so the line would not read as the next message.
fn unescape_from(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    for line in raw.split_inclusive(|b| *b == b'\n') {
        let quoted = line.iter().take_while(|b| **b == b'>').count();
        if quoted > 0 && line[quoted..].starts_with(b"From ") {
            out.extend_from_slice(&line[1..]);
        } else {
            out.extend_from_slice(line);
        }
    }
    out
}

/// The Maildir flags an mbox message's headers give: `Status` and
/// `X-Status` as mutt and Evolution write them, Thunderbird's
/// `X-Mozilla-Status`. `None` when they mark it deleted.
fn mbox_flags(raw: &[u8]) -> Option<String> {
    let mut flags = BTreeSet::new();
    let header_end = raw
        .windows(2)
        .position(|w| w == b"\n\n")
        .or_else(|| raw.windows(4).position(|w| w == b"\r\n\r\n"))
        .unwrap_or(raw.len());
    let head = String::from_utf8_lossy(&raw[..header_end]);
    for line in head.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "status" if value.contains('R') => {
                flags.insert('S');
            }
            "x-status" => {
                if value.contains('D') {
                    return None;
                }
                if value.contains('F') {
                    flags.insert('F');
                }
                if value.contains('A') {
                    flags.insert('R');
                }
            }
            "x-mozilla-status" => {
                let bits = u16::from_str_radix(value, 16).unwrap_or(0);
                if bits & 0x0008 != 0 {
                    return None;
                }
                for (bit, flag) in [(0x0001, 'S'), (0x0002, 'R'), (0x0004, 'F'), (0x1000, 'P')] {
                    if bits & bit != 0 {
                        flags.insert(flag);
                    }
                }
            }
            _ => {}
        }
    }
    Some(flags.into_iter().collect())
}

/// A message's Message-ID as the cache keys it: no brackets, lower case.
fn message_id_of(raw: &[u8]) -> Option<String> {
    let parsed = mail_parser::MessageParser::default().parse_headers(raw)?;
    let id = parsed
        .message_id()?
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_ascii_lowercase();
    (!id.is_empty()).then_some(id)
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod tests;
