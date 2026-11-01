//! Mail merge (#447): one message per row of a table, each with the row's
//! values written in where the message names a field as `{{Field}}`.
//!
//! The table is a CSV file whose first row names the fields, or the members
//! of a contact group. Nothing here touches GTK, so the parsing and the
//! filling in are tested on their own.

use crate::config::ComposeFormat;

/// The rows a merge sends to, with the names of their fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Table {
    pub(crate) columns: Vec<String>,
    /// Every row as long as `columns`.
    pub(crate) rows: Vec<Vec<String>>,
}

/// The parts of a message a merge fills in, as the composer read them.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Letter {
    pub(crate) to: String,
    pub(crate) cc: String,
    pub(crate) bcc: String,
    pub(crate) reply_to: String,
    pub(crate) subject: String,
    /// The editor's HTML, or the source of a Markdown or HTML message.
    pub(crate) body: String,
    pub(crate) text: String,
}

/// Where in a message a field is written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Place {
    To,
    Cc,
    Bcc,
    ReplyTo,
    Subject,
    Text,
}

/// A field the message names that the rows have no column for.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Missing {
    /// As written, without its braces.
    pub(crate) name: String,
    pub(crate) places: Vec<Place>,
}

/// A field as written between the braces, compared the way people type it:
/// letters and digits only, in any case, so `{{E-mail}}`, `{{email}}` and
/// `{{first_name}}` find "Email" and "First Name". In a rich body the markup
/// and entities the editor may have put in it are left out too.
fn field_key(raw: &str) -> String {
    field_text(raw)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A field's name as it reads: its text without markup, entities or extra
/// spaces.
fn field_text(raw: &str) -> String {
    let mut plain = String::with_capacity(raw.len());
    let mut in_tag = false;
    for c in raw.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => plain.push(c),
            _ => {}
        }
    }
    crate::encoding::unescape_html(&plain)
        .split(|c: char| c.is_whitespace() || c == '\u{a0}')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Each `{{…}}` in `text`: its byte range and the field it names. A pair
/// that names nothing, or runs on past a line's worth of text, is not a
/// field.
fn placeholders(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(mut open) = text[from..].find("{{").map(|i| from + i) {
        // "{{{a}}" names "a": the field starts at the last pair of a run.
        while text[open + 2..].starts_with('{') {
            open += 1;
        }
        let inner = open + 2;
        let Some(close) = text[inner..].find("}}").map(|i| inner + i) else {
            break;
        };
        let raw = &text[inner..close];
        // "{{ x {{a}}": the first pair never closed, the second does.
        if let Some(again) = raw.rfind("{{") {
            from = inner + again;
            continue;
        }
        let key = field_key(raw);
        if !key.is_empty() && raw.len() <= 200 && !raw.contains('\n') {
            found.push((open..close + 2, key));
        }
        from = close + 2;
    }
    found
}

impl Table {
    /// The column a field names, by [`field_key`].
    fn column(&self, key: &str) -> Option<usize> {
        self.columns.iter().position(|c| field_key(c) == key)
    }

    /// The column most of whose values are email addresses, the first of
    /// the best: it is where To looks when the message names no field there.
    pub(crate) fn address_column(&self) -> Option<usize> {
        let counts = (0..self.columns.len())
            .map(|c| self.rows.iter().filter(|r| is_address(r[c].trim())).count());
        let (best, n) =
            counts.enumerate().fold(
                (None, 0),
                |(bi, bn), (i, n)| {
                    if n > bn {
                        (Some(i), n)
                    } else {
                        (bi, bn)
                    }
                },
            );
        best.filter(|_| n * 2 >= self.rows.len().max(1))
    }

    /// Every column at least half of whose values are email addresses: the
    /// fields To can take.
    pub(crate) fn address_columns(&self) -> Vec<usize> {
        (0..self.columns.len())
            .filter(|&c| {
                let n = self.rows.iter().filter(|r| is_address(r[c].trim())).count();
                n > 0 && n * 2 >= self.rows.len()
            })
            .collect()
    }

    /// The fields `letter` names that the table has no column for, each
    /// once, as written, with where it is written.
    pub(crate) fn missing_fields(&self, letter: &Letter) -> Vec<Missing> {
        let mut missing: Vec<Missing> = Vec::new();
        for (place, part) in letter.parts() {
            for (range, key) in placeholders(part) {
                if self.column(&key).is_some() {
                    continue;
                }
                match missing.iter_mut().find(|m| field_key(&m.name) == key) {
                    Some(m) if !m.places.contains(&place) => m.places.push(place),
                    Some(_) => {}
                    None => missing.push(Missing {
                        name: field_text(&part[range.start + 2..range.end - 2]),
                        places: vec![place],
                    }),
                }
            }
        }
        missing
    }

    /// `text` with each field it names replaced by row `row`'s value, HTML
    /// escaped when `html`. A field with no column is left as written, or
    /// with `blank` taken out.
    fn fill(&self, text: &str, row: usize, html: bool, blank: bool) -> String {
        let Some(values) = self.rows.get(row) else {
            return text.to_string();
        };
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for (range, key) in placeholders(text) {
            let value = match self.column(&key) {
                Some(col) => values[col].trim(),
                None if blank => "",
                None => continue,
            };
            out.push_str(&text[last..range.start]);
            if html {
                out.push_str(&crate::encoding::escape_html(value));
            } else {
                out.push_str(value);
            }
            last = range.end;
        }
        out.push_str(&text[last..]);
        out
    }

    /// The message for row `row`. A body written as HTML gets its values
    /// escaped; Markdown and plain text take them as they are. With `blank`,
    /// a field the table has no column for is left empty.
    pub(crate) fn letter_for(
        &self,
        letter: &Letter,
        row: usize,
        format: ComposeFormat,
        blank: bool,
    ) -> Letter {
        let html = matches!(format, ComposeFormat::Rich | ComposeFormat::Html);
        let fill = |text: &str, html: bool| self.fill(text, row, html, blank);
        Letter {
            to: fill(&letter.to, false),
            cc: fill(&letter.cc, false),
            bcc: fill(&letter.bcc, false),
            reply_to: fill(&letter.reply_to, false),
            subject: fill(&letter.subject, false),
            body: fill(&letter.body, html),
            text: fill(&letter.text, false),
        }
    }
}

impl Letter {
    fn parts(&self) -> [(Place, &str); 7] {
        [
            (Place::To, &self.to),
            (Place::Cc, &self.cc),
            (Place::Bcc, &self.bcc),
            (Place::ReplyTo, &self.reply_to),
            (Place::Subject, &self.subject),
            (Place::Text, &self.body),
            (Place::Text, &self.text),
        ]
    }

    /// Whether To names a field, so that each message goes to its own row.
    pub(crate) fn to_has_field(&self) -> bool {
        !placeholders(&self.to).is_empty()
    }

    /// Whether the message can go where its recipient fields say: an
    /// address in To, and nothing in any field that is not one.
    pub(crate) fn sendable(&self) -> bool {
        let to = crate::worker::parse_recipients(&self.to);
        to.iter().any(|(_, a)| !a.is_empty())
            && [&self.to, &self.cc, &self.bcc, &self.reply_to]
                .iter()
                .all(|f| crate::worker::invalid_recipient(f).is_none())
    }

    /// The body as text to read in a preview, in `format`'s terms.
    pub(crate) fn preview_text(&self, format: ComposeFormat) -> String {
        match format {
            ComposeFormat::Rich | ComposeFormat::Plain => self.text.clone(),
            ComposeFormat::Markdown => self.body.clone(),
            ComposeFormat::Html => crate::mail::markdown::html_to_text(&self.body),
        }
    }
}

fn is_address(s: &str) -> bool {
    s.contains('@') && crate::worker::invalid_recipient(s).is_none()
}

/// A contact group's members as a table: the name whole and split in two,
/// and the address.
pub(crate) fn group_table(members: &[crate::contacts::Contact]) -> Table {
    use crate::i18n::i18n;
    let columns = vec![
        i18n("Name"),
        i18n("First Name"),
        i18n("Last Name"),
        i18n("Email"),
    ];
    let rows = members
        .iter()
        .filter(|m| !m.email.trim().is_empty())
        .map(|m| {
            let name = if m.name.trim() == m.email.trim() {
                ""
            } else {
                m.name.trim()
            };
            let (first, last) = name.split_once(' ').unwrap_or((name, ""));
            vec![
                name.to_string(),
                first.to_string(),
                last.trim().to_string(),
                m.email.trim().to_string(),
            ]
        })
        .collect();
    Table { columns, rows }
}

/// Bytes read from a file as text: UTF-8 (with or without its byte order
/// mark), UTF-16 with one, as spreadsheets save "Unicode text", and
/// otherwise Windows-1252, which is what Excel writes a CSV in on Windows.
fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    let utf16 = |rest: &[u8], le: bool| {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&p| {
                if le {
                    u16::from_le_bytes(p)
                } else {
                    u16::from_be_bytes(p)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, false);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => mail_parser::decoders::charsets::map::charset_decoder(b"windows-1252")
            .map(|decode| decode(bytes))
            .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned()),
    }
}

/// The delimiter of the first line: the most frequent of comma, semicolon
/// (a spreadsheet in a language whose decimal mark is a comma) and tab,
/// outside quotes. A single column has none of them, and reads as commas.
fn delimiter(text: &str) -> char {
    let mut counts = [(',', 0), (';', 0), ('\t', 0)];
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            '\n' | '\r' if !quoted => break,
            _ if !quoted => {
                if let Some(entry) = counts.iter_mut().find(|(d, _)| *d == c) {
                    entry.1 += 1;
                }
            }
            _ => {}
        }
    }
    counts
        .iter()
        .fold(
            (',', 0),
            |best, &(d, n)| if n > best.1 { (d, n) } else { best },
        )
        .0
}

/// The records of a CSV (RFC 4180): fields in double quotes may hold the
/// delimiter, line breaks and doubled quotes.
fn records(text: &str, delim: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.trim().is_empty() => {
                field.clear();
                quoted = true;
            }
            c if c == delim => row.push(std::mem::take(&mut field)),
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    // Blank lines, a spreadsheet's trailing empty rows among them.
    rows.retain(|r| r.iter().any(|f| !f.trim().is_empty()));
    rows
}

/// The header's names, a blank one called by its number.
fn column_names(header: &[String]) -> Vec<String> {
    header
        .iter()
        .enumerate()
        .map(|(i, name)| match name.trim() {
            "" => crate::i18n::i18n_f("Column {n}", &[("n", &(i + 1).to_string())]),
            name => name.to_string(),
        })
        .collect()
}

/// The fields a CSV file names in its first row, read from its start alone:
/// the composer's field palette asks for them each time it opens. With
/// `addresses`, only those whose values are mostly email addresses, judged
/// by the rows in that start.
pub(crate) fn csv_columns(path: &std::path::Path, addresses: bool) -> Vec<String> {
    use std::io::Read;
    let mut start = Vec::new();
    let read = std::fs::File::open(path).and_then(|f| f.take(64 * 1024).read_to_end(&mut start));
    if read.is_err() {
        return Vec::new();
    }
    let text = decode(&start);
    let mut records = records(&text, delimiter(&text)).into_iter();
    let Some(header) = records.next() else {
        return Vec::new();
    };
    let columns = column_names(&header);
    if !addresses {
        return columns;
    }
    let rows = records
        .map(|mut r| {
            r.resize(columns.len(), String::new());
            r
        })
        .collect();
    let table = Table { columns, rows };
    table
        .address_columns()
        .into_iter()
        .map(|c| table.columns[c].clone())
        .collect()
}

/// The fields of a contact group, as [`group_table`] names them; with
/// `addresses`, only the one that holds the address.
pub(crate) fn group_columns(addresses: bool) -> Vec<String> {
    let table = group_table(&[]);
    if addresses {
        // The last column: an empty table has no values to judge by.
        table.columns.last().cloned().into_iter().collect()
    } else {
        table.columns
    }
}

/// Whether a recipient is a mail merge field rather than an address, as
/// To holds one until the merge fills it in.
pub(crate) fn is_field(recipient: &str) -> bool {
    !placeholders(recipient).is_empty()
}

/// A CSV file's table: the first row names the fields. A column with no
/// name is called by its number; a row shorter than the first is padded,
/// and what a longer one has past it is dropped.
pub(crate) fn parse_csv(bytes: &[u8]) -> Result<Table, String> {
    use crate::i18n::i18n;
    let text = decode(bytes);
    let mut rows = records(&text, delimiter(&text)).into_iter();
    let Some(header) = rows.next() else {
        return Err(i18n("The file is empty."));
    };
    let columns = column_names(&header);
    let rows: Vec<Vec<String>> = rows
        .map(|mut r| {
            r.resize(columns.len(), String::new());
            r
        })
        .collect();
    if rows.is_empty() {
        return Err(i18n("The file has no rows below its first."));
    }
    Ok(Table { columns, rows })
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
