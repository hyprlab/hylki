//! Text out of the PDF and DOCX attachments already stored locally, so the
//! search field finds what they say (see `cache/search_index.rs`).
//!
//! Parsing a document is the riskiest thing the app does with mail from a
//! stranger, so it is done by a child process: the same binary started with
//! [`EXTRACT_FLAG`], given the file on stdin and limited in memory, CPU time
//! and wall-clock time. A file that crashes poppler or never finishes costs
//! that child and nothing else. Nothing is downloaded for this, and nothing
//! is read optically: a scanned PDF without a text layer is recorded as
//! having no text.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use crate::cache::ExtractStatus;

/// The hidden command-line flag that makes the binary an extractor.
pub const EXTRACT_FLAG: &str = "--hylki-extract-text";

/// Largest attachment handed to an extractor.
pub const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// Most text taken back from one attachment.
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// Longest one extraction may take, start to finish.
const WALL_LIMIT: Duration = Duration::from_secs(30);

/// CPU seconds and address space the child may use.
const CPU_LIMIT_SECS: u64 = 20;
const MEMORY_LIMIT_BYTES: u64 = 1536 * 1024 * 1024;

/// Most pages read from one PDF.
const MAX_PDF_PAGES: i32 = 2000;

/// Most bytes a DOCX part may inflate to: a zip bomb stops here.
const MAX_INFLATED_BYTES: u64 = 64 * 1024 * 1024;

/// The child's exit code for a document with no text in it.
const EXIT_NO_TEXT: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Pdf,
    Docx,
}

impl Kind {
    pub fn of_name(name: &str) -> Option<Kind> {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".pdf") {
            Some(Kind::Pdf)
        } else if lower.ends_with(".docx") {
            Some(Kind::Docx)
        } else {
            None
        }
    }

    fn arg(self) -> &'static str {
        match self {
            Kind::Pdf => "pdf",
            Kind::Docx => "docx",
        }
    }
}

/// Extract the text of one attachment in a child process.
pub fn extract(name: &str, data: &[u8]) -> (ExtractStatus, String) {
    let Some(kind) = Kind::of_name(name) else { return (ExtractStatus::Failed, String::new()) };
    if data.is_empty() || data.len() > MAX_INPUT_BYTES {
        return (ExtractStatus::Failed, String::new());
    }
    match run_child(kind, data) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("text extraction of an attachment failed: {e}");
            (ExtractStatus::Failed, String::new())
        }
    }
}

fn run_child(kind: Kind, data: &[u8]) -> std::io::Result<(ExtractStatus, String)> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg(EXTRACT_FLAG)
        .arg(kind.arg())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // The child needs no display and must not register on the bus.
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                let cpu = libc::rlimit { rlim_cur: CPU_LIMIT_SECS, rlim_max: CPU_LIMIT_SECS };
                let mem = libc::rlimit { rlim_cur: MEMORY_LIMIT_BYTES, rlim_max: MEMORY_LIMIT_BYTES };
                libc::setrlimit(libc::RLIMIT_CPU, &cpu);
                libc::setrlimit(libc::RLIMIT_AS, &mem);
                // Below the app's own work, so the UI never waits on it.
                libc::setpriority(libc::PRIO_PROCESS, 0, 15);
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn()?;
    let mut stdin = child.stdin.take().expect("piped");
    let input = data.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let mut stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(MAX_OUTPUT_BYTES as u64).read_to_end(&mut out);
        out
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if started.elapsed() > WALL_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = writer.join();
    let out = reader.join().unwrap_or_default();
    let Some(status) = status else {
        tracing::debug!("text extraction took longer than {WALL_LIMIT:?} and was stopped");
        return Ok((ExtractStatus::Failed, String::new()));
    };
    Ok(match status.code() {
        Some(0) => {
            let text = String::from_utf8_lossy(&out).into_owned();
            if text.trim().is_empty() {
                (ExtractStatus::NoText, String::new())
            } else {
                (ExtractStatus::Text, text)
            }
        }
        Some(EXIT_NO_TEXT) => (ExtractStatus::NoText, String::new()),
        _ => (ExtractStatus::Failed, String::new()),
    })
}

/// The child's side: read the file from stdin, write its text to stdout,
/// and exit. Called from `main` before anything else starts.
pub fn child_main(kind: &str) -> ! {
    let mut data = Vec::new();
    if std::io::stdin().take(MAX_INPUT_BYTES as u64 + 1).read_to_end(&mut data).is_err() || data.len() > MAX_INPUT_BYTES
    {
        std::process::exit(1);
    }
    let text = match kind {
        "pdf" => pdf_text(&data),
        "docx" => docx_text(&data),
        _ => None,
    };
    match text {
        None => std::process::exit(1),
        Some(t) if t.trim().is_empty() => std::process::exit(EXIT_NO_TEXT),
        Some(t) => {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(t.as_bytes());
            let _ = out.flush();
            std::process::exit(0);
        }
    }
}

/// Every page's text layer, page by page. `None` when poppler cannot open
/// the file (damaged, or encrypted with a password).
fn pdf_text(data: &[u8]) -> Option<String> {
    let bytes = gtk::glib::Bytes::from(data);
    let doc = poppler::Document::from_bytes(&bytes, None).ok()?;
    let mut out = String::new();
    for i in 0..doc.n_pages().min(MAX_PDF_PAGES) {
        if let Some(text) = doc.page(i).and_then(|p| p.text()) {
            out.push_str(text.as_str());
            out.push('\n');
        }
        if out.len() >= MAX_OUTPUT_BYTES {
            break;
        }
    }
    Some(out)
}

/// The text of a Word document: the body, then its footnotes and endnotes.
fn docx_text(data: &[u8]) -> Option<String> {
    let entries = zip_entries(data)?;
    let mut out = String::new();
    let mut found = false;
    for part in ["word/document.xml", "word/footnotes.xml", "word/endnotes.xml"] {
        let Some(entry) = entries.iter().find(|e| e.name == part) else { continue };
        let xml = zip_read(data, entry)?;
        found = true;
        out.push_str(&wordml_text(&String::from_utf8_lossy(&xml)));
        out.push('\n');
    }
    found.then_some(out)
}

/// One file in a zip archive, from its central directory.
struct ZipEntry {
    name: String,
    method: u16,
    compressed: u64,
    local_offset: u64,
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The archive's table of contents. A DOCX is an ordinary zip; ZIP64 (over
/// 4 GiB, or 65,535 files) is not something a Word document needs and is
/// refused.
fn zip_entries(data: &[u8]) -> Option<Vec<ZipEntry>> {
    // The end-of-central-directory record sits within the last 64 KiB (its
    // comment is at most that long).
    let start = data.len().saturating_sub(22 + 65_535);
    let eocd = (start..data.len().saturating_sub(21)).rev().find(|&i| le32(data, i) == Some(0x0605_4b50))?;
    let count = le16(data, eocd + 10)? as usize;
    let mut at = le32(data, eocd + 16)? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if le32(data, at)? != 0x0201_4b50 {
            return None;
        }
        let method = le16(data, at + 10)?;
        let compressed = le32(data, at + 20)? as u64;
        let name_len = le16(data, at + 28)? as usize;
        let extra_len = le16(data, at + 30)? as usize;
        let comment_len = le16(data, at + 32)? as usize;
        let local_offset = le32(data, at + 42)? as u64;
        let name = String::from_utf8_lossy(data.get(at + 46..at + 46 + name_len)?).into_owned();
        out.push(ZipEntry { name, method, compressed, local_offset });
        at += 46 + name_len + extra_len + comment_len;
    }
    Some(out)
}

/// One entry's contents, inflated, at most [`MAX_INFLATED_BYTES`].
fn zip_read(data: &[u8], e: &ZipEntry) -> Option<Vec<u8>> {
    let at = usize::try_from(e.local_offset).ok()?;
    if le32(data, at)? != 0x0403_4b50 {
        return None;
    }
    let name_len = le16(data, at + 26)? as usize;
    let extra_len = le16(data, at + 28)? as usize;
    let begin = at + 30 + name_len + extra_len;
    let raw = data.get(begin..begin.checked_add(usize::try_from(e.compressed).ok()?)?)?;
    let mut out = Vec::new();
    match e.method {
        0 => out.extend_from_slice(raw.get(..raw.len().min(MAX_INFLATED_BYTES as usize))?),
        8 => {
            flate2::read::DeflateDecoder::new(raw).take(MAX_INFLATED_BYTES).read_to_end(&mut out).ok()?;
        }
        _ => return None,
    }
    Some(out)
}

/// The words of a WordprocessingML part: the text runs (`w:t`), a space
/// for a tab, a line break for a paragraph or a break.
fn wordml_text(xml: &str) -> String {
    let mut out = String::new();
    let mut in_text = false;
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        if in_text {
            out.push_str(&unescape_xml(&rest[..lt]));
        }
        let Some(gt) = rest[lt..].find('>') else { break };
        let tag = &rest[lt + 1..lt + gt];
        let closing = tag.starts_with('/');
        let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        let self_closing = tag.ends_with('/');
        match name {
            "w:t" => in_text = !closing && !self_closing,
            "w:tab" => out.push('\t'),
            "w:br" | "w:cr" => out.push('\n'),
            "w:p" if closing => out.push('\n'),
            _ => {}
        }
        rest = &rest[lt + gt + 1..];
    }
    out
}

fn unescape_xml(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let Some(semi) = after.find(';').filter(|&i| i <= 10) else {
            out.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..semi];
        let c = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if entity.starts_with("#x") => u32::from_str_radix(&entity[2..], 16).ok().and_then(char::from_u32),
            _ if entity.starts_with('#') => entity[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zip with the given entries, each stored (method 0) or deflated.
    pub(crate) fn zip(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
        use std::io::Write as _;
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, content, deflate) in entries {
            let body = if *deflate {
                let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(content).unwrap();
                e.finish().unwrap()
            } else {
                content.to_vec()
            };
            let method: u16 = if *deflate { 8 } else { 0 };
            let offset = out.len() as u32;
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&[20, 0, 0, 0]);
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0; 8]); // time, date, crc
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
            out.extend_from_slice(&(content.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&body);

            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
            central.extend_from_slice(&method.to_le_bytes());
            central.extend_from_slice(&[0; 8]);
            central.extend_from_slice(&(body.len() as u32).to_le_bytes());
            central.extend_from_slice(&(content.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0; 12]); // extra, comment, disk, attrs
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    const DOC: &str = r#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body>
        <w:p><w:r><w:t>Quarterly</w:t></w:r><w:r><w:t xml:space="preserve"> report for </w:t></w:r><w:r><w:t>Lisbon &amp; Porto</w:t></w:r></w:p>
        <w:p><w:r><w:t>Größe</w:t><w:tab/><w:t>&#x41;&#66;</w:t></w:r></w:p>
        <w:tbl><w:tr><w:tc><w:p><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
        </w:body></w:document>"#;

    #[test]
    fn word_text_keeps_runs_paragraphs_and_entities() {
        let text = wordml_text(DOC);
        assert!(text.contains("Quarterly report for Lisbon & Porto\n"), "{text}");
        assert!(text.contains("Größe\tAB\n"), "{text}");
        assert!(text.contains("cell"), "{text}");
        assert!(!text.contains("w:"), "no markup: {text}");
    }

    #[test]
    fn a_docx_is_read_stored_or_deflated() {
        for deflate in [false, true] {
            let z =
                zip(&[("[Content_Types].xml", b"<Types/>", deflate), ("word/document.xml", DOC.as_bytes(), deflate)]);
            let text = docx_text(&z).expect("readable");
            assert!(text.contains("Lisbon & Porto"), "{text}");
        }
    }

    #[test]
    fn something_that_is_not_a_docx_has_no_text() {
        assert!(docx_text(b"not a zip at all").is_none());
        let z = zip(&[("other.xml", b"<x/>", true)]);
        assert!(docx_text(&z).is_none(), "a zip without a Word body");
        // A truncated archive is refused, not read past its end.
        let full = zip(&[("word/document.xml", DOC.as_bytes(), true)]);
        assert!(docx_text(&full[..full.len() / 2]).is_none());
    }

    #[test]
    fn a_zip_bomb_inflates_no_further_than_the_limit() {
        let big = vec![b'a'; (MAX_INFLATED_BYTES + 1024) as usize];
        let z = zip(&[("word/document.xml", &big, true)]);
        let entries = zip_entries(&z).unwrap();
        assert_eq!(zip_read(&z, &entries[0]).unwrap().len() as u64, MAX_INFLATED_BYTES);
    }

    #[test]
    fn only_pdf_and_docx_are_read() {
        assert_eq!(Kind::of_name("Report.PDF"), Some(Kind::Pdf));
        assert_eq!(Kind::of_name("a.docx"), Some(Kind::Docx));
        assert_eq!(Kind::of_name("a.doc"), None);
        assert_eq!(extract("photo.jpg", b"x").0, ExtractStatus::Failed);
    }

    /// A one-page PDF with `text` on it, or with no text at all (the shape a
    /// scan has: a page and nothing to read on it).
    pub(crate) fn pdf(text: Option<&str>) -> Vec<u8> {
        let content = match text {
            Some(t) => format!("BT /F1 12 Tf 72 720 Td ({t}) Tj ET"),
            None => "0 0 1 rg 10 10 50 50 re f".to_string(),
        };
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes(),
        );
        out
    }

    #[test]
    fn a_pdf_text_layer_is_read_and_a_scan_has_none() {
        let text = pdf_text(&pdf(Some("Invoice from Lisbon"))).expect("opens");
        assert!(text.contains("Invoice from Lisbon"), "{text}");
        let scan = pdf_text(&pdf(None)).expect("opens");
        assert!(scan.trim().is_empty(), "no text layer: {scan:?}");
        assert!(pdf_text(b"%PDF-1.4 broken").is_none());
    }
}
