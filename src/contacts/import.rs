//! Importing contacts into an address book (#655): `.vcf` files as they
//! are, and CSV files with their columns matched to contact fields in the
//! import dialog. The Hylki book takes them in one transaction; an EDS book
//! through its CreateContacts and ModifyContacts calls.

use std::collections::HashSet;
use std::path::PathBuf;

pub(crate) use super::csv_import::{can_import, contacts_from, detect, targets, Target};
use super::details::{parse_vcard_fields, ContactDetails, GroupFormat, Member};
use super::edit::{save_group, GroupEdit};
use super::eds_write::{
    book_call, book_get_contact, book_get_uids, factory_dest, open_book, sexpr_escape,
};
use super::local::{self, content_uid, insert_uid, ImportOutcome, MAX_VCARD_BYTES};
use super::read::{is_local_book, Book};
use crate::mail::merge::{parse_csv, Table};

/// A file chosen for import, as the dialog shows it.
#[derive(Debug)]
pub(crate) struct FileStart {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    /// The table of a CSV file, or why it cannot be read; `None` for a
    /// vCard file.
    pub(crate) table: Option<Result<Table, String>>,
}

/// What the import dialog opens with.
#[derive(Debug)]
pub(crate) struct ImportStart {
    pub(crate) files: Vec<FileStart>,
    /// The books contacts can go to, the default first.
    pub(crate) books: Vec<Book>,
    /// The book to start on, when the import was asked for in one.
    pub(crate) book: Option<String>,
}

/// One file to import, as the dialog leaves it.
#[derive(Debug, Clone)]
pub(crate) enum ImportFile {
    Vcard(PathBuf),
    Csv { path: PathBuf, fields: Vec<Target> },
}

/// What the import dialog asks for.
#[derive(Debug, Clone)]
pub(crate) struct ImportRequest {
    pub(crate) book: Book,
    pub(crate) files: Vec<ImportFile>,
}

/// The file's name, for the dialog and for errors.
fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Read what the dialog needs: each file's kind and a CSV file's table
/// (blocking: call from a background thread). `books` are the writable
/// books; the Hylki book is added when they lack it, as it is always
/// there to import into, even before it is used.
pub(crate) fn prepare(
    paths: Vec<PathBuf>,
    mut books: Vec<Book>,
    book: Option<String>,
) -> ImportStart {
    let files = paths
        .into_iter()
        .map(|path| {
            let table = match local::read_file(&path) {
                Ok(bytes) if !local::is_csv(&path, &bytes) => None,
                Ok(bytes) => Some(parse_csv(&bytes)),
                Err(e) => Some(Err(e)),
            };
            FileStart {
                name: file_name(&path),
                path,
                table,
            }
        })
        .collect();
    if !books.iter().any(|b| is_local_book(&b.uid)) {
        books.push(crate::contacts::local_book());
    }
    ImportStart { files, books, book }
}

/// Import the files into the book (blocking: call from a background thread).
pub(crate) fn run(req: &ImportRequest) -> Result<ImportOutcome, String> {
    let mut out = ImportOutcome::default();
    let mut cards: Vec<String> = Vec::new();
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for file in &req.files {
        let path = match file {
            ImportFile::Vcard(p) | ImportFile::Csv { path: p, .. } => p,
        };
        let bytes = match local::read_file(path) {
            Ok(b) => b,
            Err(e) => {
                out.failed_files.push(format!("{}: {e}", file_name(path)));
                continue;
            }
        };
        let read = match file {
            ImportFile::Vcard(_) => local::vcard_cards(&bytes).map(|(found, truncated)| {
                out.errors += truncated;
                (found, Vec::new())
            }),
            ImportFile::Csv { fields, .. } => parse_csv(&bytes).map(|table| {
                let found = contacts_from(&table, fields);
                out.skipped += found.skipped;
                (found.cards, found.groups)
            }),
        };
        match read {
            Ok((found, named)) => {
                let base = cards.len();
                cards.extend(found);
                for (name, members) in named {
                    let members = members.into_iter().map(|i| i + base);
                    match groups
                        .iter_mut()
                        .find(|(g, _)| g.eq_ignore_ascii_case(&name))
                    {
                        Some((_, all)) => all.extend(members),
                        None => groups.push((name, members.collect())),
                    }
                }
            }
            Err(e) => out.failed_files.push(format!("{}: {e}", file_name(path))),
        }
    }
    let wrote = if is_local_book(&req.book.uid) {
        let named = group_members(&cards, &[], &groups, GroupFormat::Evolution);
        let wrote = local::store_cards(&cards, &named)?;
        if wrote.added + wrote.updated > 0 {
            super::photos::local_changed();
        }
        wrote
    } else {
        store_in_eds(&req.book, &cards, &groups)?
    };
    out.merge(wrote);
    Ok(out)
}

/// The members of each group, as a book of `format` keeps them: an
/// Evolution list holds addresses, so only people with one; an Apple group
/// holds cards, by their UIDs in `uids` (one per card). Other formats keep
/// no groups here: a Nextcloud book has them in the cards' categories
/// already, and Google and Microsoft 365 take none.
pub(super) fn group_members(
    cards: &[String],
    uids: &[String],
    groups: &[(String, Vec<usize>)],
    format: GroupFormat,
) -> Vec<(String, Vec<Member>)> {
    if !matches!(format, GroupFormat::Evolution | GroupFormat::Apple) {
        return Vec::new();
    }
    groups
        .iter()
        .map(|(name, at)| {
            let mut members: Vec<Member> = Vec::new();
            for &i in at {
                let Some(c) = cards.get(i).and_then(|v| parse_vcard_fields(v)) else {
                    continue;
                };
                let m = Member {
                    name: if c.name.is_empty() {
                        c.org.clone()
                    } else {
                        c.name.clone()
                    },
                    email: c.primary_email().unwrap_or_default().to_string(),
                    uid: uids.get(i).cloned().unwrap_or_default(),
                    ..Member::default()
                };
                let fits = match format {
                    GroupFormat::Apple => !m.uid.is_empty(),
                    _ => !m.email.is_empty(),
                };
                if fits && !members.iter().any(|o| o.same(&m)) {
                    members.push(m);
                }
            }
            (name.clone(), members)
        })
        .filter(|(_, members)| !members.is_empty())
        .collect()
}

/// How many cards go to an EDS book in one call.
const BATCH: usize = 100;

/// Write the cards to an EDS book: a card whose UID the book has replaces
/// it, the others are made. A card without a UID gets one from its text, so
/// importing a file again updates what the first import made.
fn store_in_eds(
    book: &Book,
    cards: &[String],
    groups: &[(String, Vec<usize>)],
) -> Result<ImportOutcome, String> {
    let dest = factory_dest().ok_or("Evolution Data Server is not available")?;
    let conn = zbus::blocking::Connection::session().map_err(|e| e.to_string())?;
    let (bus, path) = open_book(&conn, &dest, &book.uid)?;
    let mut out = ImportOutcome::default();
    // The UID each card has in the book, once written.
    let mut uids = vec![String::new(); cards.len()];
    let mut seen = HashSet::new();
    // Each card to make or replace, with its index in `cards`.
    let mut new: Vec<(usize, String)> = Vec::new();
    let mut changed: Vec<(usize, String)> = Vec::new();
    for (i, card) in cards.iter().enumerate() {
        if card.len() > MAX_VCARD_BYTES {
            out.errors += 1;
            continue;
        }
        let Some(details) = parse_vcard_fields(card) else {
            out.skipped += 1;
            continue;
        };
        let (uid, card) = if details.eds_uid.is_empty() {
            let uid = content_uid(card);
            let with = insert_uid(card, &uid);
            (uid, with)
        } else {
            (details.eds_uid, card.clone())
        };
        if !seen.insert(uid.clone()) {
            out.skipped += 1;
            continue;
        }
        uids[i] = uid.clone();
        if book_get_contact(&conn, &bus, &path, &uid).is_ok() {
            changed.push((i, card));
        } else {
            new.push((i, card));
        }
    }
    for chunk in new.chunks(BATCH) {
        let batch: Vec<String> = chunk.iter().map(|(_, c)| c.clone()).collect();
        let reply = book_call(&conn, &bus, &path, "CreateContacts", &(batch, 0u32))?;
        let (made,): (Vec<String>,) = reply.body().deserialize().map_err(|e| e.to_string())?;
        // The book may give a card a UID of its own.
        for ((i, _), uid) in chunk.iter().zip(made) {
            uids[*i] = uid;
        }
        out.added += chunk.len();
    }
    for chunk in changed.chunks(BATCH) {
        let batch: Vec<String> = chunk.iter().map(|(_, c)| c.clone()).collect();
        book_call(&conn, &bus, &path, "ModifyContacts", &(batch, 0u32))?;
        out.updated += chunk.len();
    }
    let Some(format) = book.groups else {
        return Ok(out);
    };
    for (name, members) in group_members(cards, &uids, groups, format) {
        let found = existing_group(&conn, &bus, &path, &name, format)?;
        match found {
            Some(g) => {
                let mut all = g.members.clone();
                all.extend(g.member_refs.iter().map(|(uid, line)| Member {
                    uid: uid.clone(),
                    line: line.clone(),
                    ..Member::default()
                }));
                let before = all.len();
                for m in members {
                    if !all.iter().any(|a| a.same(&m)) {
                        all.push(m);
                    }
                }
                if all.len() > before {
                    let edit = GroupEdit {
                        name: g.name.clone(),
                        members: all,
                        hide_addresses: g.hide_addresses,
                        kept: g.hidden_members.clone(),
                    };
                    save_group(&book.uid, format, Some(&g.raw_vcard), edit)?;
                }
            }
            None => save_group(
                &book.uid,
                format,
                None,
                GroupEdit {
                    name,
                    members,
                    ..GroupEdit::default()
                },
            )?,
        }
    }
    Ok(out)
}

/// The book's group of `format` named `name`, any case, with its text.
fn existing_group(
    conn: &zbus::blocking::Connection,
    bus: &str,
    path: &str,
    name: &str,
    format: GroupFormat,
) -> Result<Option<ContactDetails>, String> {
    let query = format!("(is \"full_name\" \"{}\")", sexpr_escape(name));
    for uid in book_get_uids(conn, bus, path, &query)? {
        let card = book_get_contact(conn, bus, path, &uid)?;
        if let Some(mut g) = parse_vcard_fields(&card).filter(|g| g.group == Some(format)) {
            g.raw_vcard = card;
            return Ok(Some(g));
        }
    }
    Ok(None)
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod tests;
