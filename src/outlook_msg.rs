//! Outlook `.msg` files (#398): a compound file (CFB) of MAPI properties
//! rather than a mail message. They are turned into an RFC 5322 message, so
//! the reader renders them the way it renders everything else, attachments
//! and inline pictures included. The header block Outlook kept from the
//! server is used when there is one; otherwise the headers are built from
//! the properties. A body only in compressed RTF, with no plain or HTML
//! copy, is not read.

use std::io::{Cursor, Read};

use base64::Engine;

/// What a `.msg` file starts with: the compound file signature.
const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

// The MAPI properties read, by id.
const SUBJECT: u16 = 0x0037;
const CLIENT_SUBMIT_TIME: u16 = 0x0039;
const SENT_REPRESENTING_NAME: u16 = 0x0042;
const SENT_REPRESENTING_EMAIL: u16 = 0x0065;
const TRANSPORT_HEADERS: u16 = 0x007D;
const SENDER_NAME: u16 = 0x0C1A;
const SENDER_EMAIL: u16 = 0x0C1F;
const DISPLAY_CC: u16 = 0x0E03;
const DISPLAY_TO: u16 = 0x0E04;
const DELIVERY_TIME: u16 = 0x0E06;
const BODY: u16 = 0x1000;
const CREATION_TIME: u16 = 0x3007;
const LAST_MODIFICATION_TIME: u16 = 0x3008;
const HTML: u16 = 0x1013;
const INTERNET_MESSAGE_ID: u16 = 0x1035;
const REFERENCES: u16 = 0x1039;
const IN_REPLY_TO: u16 = 0x1042;
const RECIPIENT_TYPE: u16 = 0x0C15;
const RECIPIENT_NAME: u16 = 0x3001;
const RECIPIENT_EMAIL: u16 = 0x3003;
const RECIPIENT_SMTP: u16 = 0x39FE;
const SENDER_SMTP: u16 = 0x5D01;
const ATTACH_DATA: u16 = 0x3701;
const ATTACH_FILENAME: u16 = 0x3704;
const ATTACH_LONG_FILENAME: u16 = 0x3707;
const ATTACH_MIME: u16 = 0x370E;
const ATTACH_CONTENT_ID: u16 = 0x3712;

/// Whether `bytes` are a compound file, which a `.msg` is.
pub fn is_msg(bytes: &[u8]) -> bool {
    bytes.starts_with(&CFB_MAGIC)
}

type Cfb = cfb::CompoundFile<Cursor<Vec<u8>>>;

fn stream(file: &mut Cfb, path: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    file.open_stream(path).ok()?.read_to_end(&mut out).ok()?;
    Some(out)
}

/// A string property of the storage at `dir`: Unicode (`001F`) or the
/// 8-bit kind (`001E`, read as UTF-8, else Latin-1).
fn string(file: &mut Cfb, dir: &str, id: u16) -> Option<String> {
    if let Some(raw) = stream(file, &format!("{dir}/__substg1.0_{id:04X}001F")) {
        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let s = String::from_utf16_lossy(&units);
        return Some(s.trim_end_matches('\0').to_string()).filter(|s| !s.is_empty());
    }
    let raw = stream(file, &format!("{dir}/__substg1.0_{id:04X}001E"))?;
    let s = match String::from_utf8(raw.clone()) {
        Ok(s) => s,
        Err(_) => raw.iter().map(|&b| b as char).collect(),
    };
    Some(s.trim_end_matches('\0').to_string()).filter(|s| !s.is_empty())
}

fn binary(file: &mut Cfb, dir: &str, id: u16) -> Option<Vec<u8>> {
    stream(file, &format!("{dir}/__substg1.0_{id:04X}0102"))
}

/// The fixed-size properties of a storage, by id: its `__properties`
/// stream, a header (32 bytes for the message, 8 for a recipient or an
/// attachment) and then 16 bytes a property, the value in the last 8.
fn fixed(file: &mut Cfb, dir: &str, header: usize) -> std::collections::HashMap<u16, u64> {
    let mut out = std::collections::HashMap::new();
    let Some(raw) = stream(file, &format!("{dir}/__properties_version1.0")) else { return out };
    for entry in raw.get(header..).unwrap_or_default().chunks_exact(16) {
        let tag = u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]);
        let value = u64::from_le_bytes(entry[8..16].try_into().unwrap_or_default());
        out.insert((tag >> 16) as u16, value);
    }
    out
}

/// A FILETIME (100 ns since 1601) as a Unix time.
fn unix_time(filetime: u64) -> i64 {
    (filetime / 10_000_000) as i64 - 11_644_473_600
}

/// The storages under the root whose names start with `prefix`.
fn children(file: &Cfb, prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = file
        .read_storage("/")
        .map(|entries| entries.filter(|e| e.is_storage() && e.name().starts_with(prefix)).map(|e| e.name().to_string()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// An address for a header: the SMTP one when there is one, since an
/// Exchange one ("/O=EXCHANGELABS/…") means nothing outside it.
fn smtp(candidates: &[Option<String>]) -> Option<String> {
    candidates.iter().flatten().find(|a| a.contains('@') && !a.starts_with('/')).cloned()
}

/// A header value, RFC 2047-encoded when it is not plain ASCII.
fn encode_word(text: &str) -> String {
    if text.is_ascii() {
        text.to_string()
    } else {
        format!("=?UTF-8?B?{}?=", base64::engine::general_purpose::STANDARD.encode(text))
    }
}

fn mailbox(name: Option<&str>, addr: Option<&str>) -> Option<String> {
    match (name.filter(|n| !n.is_empty()), addr) {
        (Some(n), Some(a)) if n != a => Some(format!("{} <{a}>", encode_word(&n.replace('"', "")))),
        (_, Some(a)) => Some(format!("<{a}>")),
        (Some(n), None) => Some(encode_word(n)),
        (None, None) => None,
    }
}

/// `bytes` in base64, in lines of 76.
fn base64_lines(bytes: &[u8]) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut out = String::with_capacity(encoded.len() + encoded.len() / 76 * 2);
    for chunk in encoded.as_bytes().chunks(76) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push_str("\r\n");
    }
    out
}

struct Attachment {
    name: String,
    mime: String,
    content_id: Option<String>,
    data: Vec<u8>,
}

/// The headers Outlook kept from the server, without the ones describing
/// the body, which is built anew.
fn kept_headers(headers: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in headers.lines() {
        if line.is_empty() {
            break;
        }
        let continued = line.starts_with(' ') || line.starts_with('\t');
        if !continued {
            let name = line.split(':').next().unwrap_or_default().trim().to_ascii_lowercase();
            skipping = matches!(name.as_str(), "content-type" | "content-transfer-encoding" | "mime-version");
        }
        if !skipping {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    out
}

/// The message in a `.msg` file, as RFC 5322 bytes.
pub fn to_eml(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut file = cfb::CompoundFile::open(Cursor::new(bytes.to_vec())).map_err(|e| e.to_string())?;
    let root = "";
    let props = fixed(&mut file, root, 32);

    // Sent, received, or for a message never sent (a draft saved from
    // Outlook), last changed or made.
    let date = [CLIENT_SUBMIT_TIME, DELIVERY_TIME, LAST_MODIFICATION_TIME, CREATION_TIME]
        .iter()
        .find_map(|id| props.get(id).copied().filter(|t| *t > 0))
        .and_then(|t| chrono::DateTime::from_timestamp(unix_time(t), 0))
        .map(|d| format!("Date: {}\r\n", d.to_rfc2822()));
    let mut headers = String::new();
    let transport = string(&mut file, root, TRANSPORT_HEADERS).filter(|h| h.to_ascii_lowercase().contains("from:"));
    if let Some(kept) = transport {
        headers.push_str(&kept_headers(&kept));
        // Not every server's header block has one.
        let dated = headers.lines().any(|l| l.to_ascii_lowercase().starts_with("date:"));
        if !dated {
            headers.extend(date);
        }
    } else {
        headers.extend(date);
        let sender_name = string(&mut file, root, SENT_REPRESENTING_NAME).or_else(|| string(&mut file, root, SENDER_NAME));
        let sender = smtp(&[
            string(&mut file, root, SENDER_SMTP),
            string(&mut file, root, SENT_REPRESENTING_EMAIL),
            string(&mut file, root, SENDER_EMAIL),
        ]);
        if let Some(from) = mailbox(sender_name.as_deref(), sender.as_deref()) {
            headers.push_str(&format!("From: {from}\r\n"));
        }
        // The recipients, by kind: 1 is To, 2 Cc; Bcc is left out.
        let mut to = Vec::new();
        let mut cc = Vec::new();
        for dir in children(&file, "__recip_version1.0_") {
            let dir = format!("/{dir}");
            let kind = fixed(&mut file, &dir, 8).get(&RECIPIENT_TYPE).copied().unwrap_or(1) as u32;
            let name = string(&mut file, &dir, RECIPIENT_NAME);
            let addr = smtp(&[string(&mut file, &dir, RECIPIENT_SMTP), string(&mut file, &dir, RECIPIENT_EMAIL)]);
            let Some(m) = mailbox(name.as_deref(), addr.as_deref()) else { continue };
            match kind {
                1 => to.push(m),
                2 => cc.push(m),
                _ => {}
            }
        }
        if to.is_empty() {
            to.extend(string(&mut file, root, DISPLAY_TO).map(|t| encode_word(&t)));
        }
        if cc.is_empty() {
            cc.extend(string(&mut file, root, DISPLAY_CC).map(|t| encode_word(&t)));
        }
        if !to.is_empty() {
            headers.push_str(&format!("To: {}\r\n", to.join(", ")));
        }
        if !cc.is_empty() {
            headers.push_str(&format!("Cc: {}\r\n", cc.join(", ")));
        }
        if let Some(subject) = string(&mut file, root, SUBJECT) {
            headers.push_str(&format!("Subject: {}\r\n", encode_word(&subject)));
        }

        for (id, name) in [(INTERNET_MESSAGE_ID, "Message-ID"), (IN_REPLY_TO, "In-Reply-To"), (REFERENCES, "References")] {
            if let Some(v) = string(&mut file, root, id) {
                headers.push_str(&format!("{name}: {v}\r\n"));
            }
        }
    }

    // The body: HTML when the message has it, else the plain text.
    let html = binary(&mut file, root, HTML).or_else(|| string(&mut file, root, HTML).map(String::into_bytes));
    let (body, subtype) = match html {
        Some(h) if !h.is_empty() => (h, "html"),
        _ => match string(&mut file, root, BODY) {
            Some(text) => (text.into_bytes(), "plain"),
            None => (Vec::new(), "plain"),
        },
    };
    // Outlook writes HTML in the message's code page; most is UTF-8, the
    // rest Windows-1252.
    let charset = if std::str::from_utf8(&body).is_ok() { "utf-8" } else { "windows-1252" };

    let mut attachments = Vec::new();
    for dir in children(&file, "__attach_version1.0_") {
        let dir = format!("/{dir}");
        // An attached message is a storage, not data: left out.
        let Some(data) = binary(&mut file, &dir, ATTACH_DATA) else { continue };
        let name = string(&mut file, &dir, ATTACH_LONG_FILENAME)
            .or_else(|| string(&mut file, &dir, ATTACH_FILENAME))
            .unwrap_or_else(|| "attachment".to_string());
        let mime = string(&mut file, &dir, ATTACH_MIME).unwrap_or_else(|| "application/octet-stream".to_string());
        let content_id = string(&mut file, &dir, ATTACH_CONTENT_ID);
        attachments.push(Attachment { name, mime, content_id, data });
    }

    let mut out = headers;
    out.push_str("MIME-Version: 1.0\r\n");
    let boundary = "hylki-msg-mixed";
    let related = "hylki-msg-related";
    out.push_str(&format!("Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n"));
    let body_part = format!(
        "Content-Type: text/{subtype}; charset={charset}\r\nContent-Transfer-Encoding: base64\r\n\r\n{}",
        base64_lines(&body)
    );
    let attachment_part = |a: &Attachment, inline: bool| -> String {
        let name = a.name.replace('"', "");
        let disposition = if inline { "inline" } else { "attachment" };
        let mut part = format!(
            "Content-Type: {}; name=\"{}\"\r\nContent-Disposition: {disposition}; filename=\"{}\"\r\nContent-Transfer-Encoding: base64\r\n",
            a.mime,
            encode_word(&name),
            encode_word(&name)
        );
        if let Some(cid) = &a.content_id {
            part.push_str(&format!("Content-ID: <{}>\r\n", cid.trim_matches(|c| c == '<' || c == '>')));
        }
        part.push_str("\r\n");
        part.push_str(&base64_lines(&a.data));
        part
    };
    // Pictures the HTML shows by Content-ID go beside it, in a related
    // part, where the reader looks for them.
    let (inline, files): (Vec<&Attachment>, Vec<&Attachment>) = attachments.iter().partition(|a| {
        subtype == "html"
            && a.content_id.as_deref().is_some_and(|cid| {
                let cid = cid.trim_matches(|c| c == '<' || c == '>');
                body.windows(cid.len() + 4).any(|w| w == format!("cid:{cid}").as_bytes())
            })
    });
    out.push_str(&format!("--{boundary}\r\n"));
    if inline.is_empty() {
        out.push_str(&body_part);
    } else {
        out.push_str(&format!("Content-Type: multipart/related; boundary=\"{related}\"\r\n\r\n"));
        out.push_str(&format!("--{related}\r\n{body_part}"));
        for a in &inline {
            out.push_str(&format!("--{related}\r\n{}", attachment_part(a, true)));
        }
        out.push_str(&format!("--{related}--\r\n"));
    }
    for a in &files {
        out.push_str(&format!("--{boundary}\r\n{}", attachment_part(a, false)));
    }
    out.push_str(&format!("--{boundary}--\r\n"));
    Ok(out.into_bytes())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use mail_parser::MimeHeaders;

    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    /// A `.msg` as Outlook lays one out, with a recipient, an attachment and
    /// an inline picture.
    fn sample(transport: Option<&str>) -> Vec<u8> {
        let mut file = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        let mut put = |file: &mut cfb::CompoundFile<Cursor<Vec<u8>>>, path: &str, data: &[u8]| {
            file.create_stream(path).unwrap().write_all(data).unwrap();
        };
        put(&mut file, "/__substg1.0_0037001F", &utf16("Quarterly numbers – draft"));
        put(&mut file, "/__substg1.0_0C1A001F", &utf16("Ada Lovelace"));
        put(&mut file, "/__substg1.0_0C1F001F", &utf16("/O=EXCHANGELABS/OU=ADMIN/CN=ADA"));
        put(&mut file, "/__substg1.0_5D01001F", &utf16("ada@example.com"));
        put(&mut file, "/__substg1.0_1035001F", &utf16("<q3@example.com>"));
        put(&mut file, "/__substg1.0_10130102", b"<html><body><p>Numbers attached.</p><img src=\"cid:chart1\"></body></html>");
        put(&mut file, "/__substg1.0_1000001F", &utf16("Numbers attached."));
        if let Some(t) = transport {
            put(&mut file, "/__substg1.0_007D001F", &utf16(t));
        }
        // 2026-10-09 10:00:00 UTC as a FILETIME, the client submit time.
        let ft: u64 = (1_791_540_000 + 11_644_473_600) * 10_000_000;
        let mut props = vec![0u8; 32];
        props.extend_from_slice(&((0x0039u32 << 16) | 0x0040).to_le_bytes());
        props.extend_from_slice(&0u32.to_le_bytes());
        props.extend_from_slice(&ft.to_le_bytes());
        put(&mut file, "/__properties_version1.0", &props);

        file.create_storage("/__recip_version1.0_#00000000").unwrap();
        put(&mut file, "/__recip_version1.0_#00000000/__substg1.0_3001001F", &utf16("Grace Hopper"));
        put(&mut file, "/__recip_version1.0_#00000000/__substg1.0_39FE001F", &utf16("grace@example.com"));
        let mut rprops = vec![0u8; 8];
        rprops.extend_from_slice(&((0x0C15u32 << 16) | 0x0003).to_le_bytes());
        rprops.extend_from_slice(&0u32.to_le_bytes());
        rprops.extend_from_slice(&1u64.to_le_bytes());
        put(&mut file, "/__recip_version1.0_#00000000/__properties_version1.0", &rprops);

        file.create_storage("/__attach_version1.0_#00000000").unwrap();
        put(&mut file, "/__attach_version1.0_#00000000/__substg1.0_37010102", b"a,b\n1,2\n");
        put(&mut file, "/__attach_version1.0_#00000000/__substg1.0_3707001F", &utf16("numbers.csv"));
        put(&mut file, "/__attach_version1.0_#00000000/__substg1.0_370E001F", &utf16("text/csv"));
        file.create_storage("/__attach_version1.0_#00000001").unwrap();
        put(&mut file, "/__attach_version1.0_#00000001/__substg1.0_37010102", &[0x89, b'P', b'N', b'G']);
        put(&mut file, "/__attach_version1.0_#00000001/__substg1.0_3707001F", &utf16("chart.png"));
        put(&mut file, "/__attach_version1.0_#00000001/__substg1.0_370E001F", &utf16("image/png"));
        put(&mut file, "/__attach_version1.0_#00000001/__substg1.0_3712001F", &utf16("chart1"));
        file.flush().unwrap();
        file.into_inner().into_inner()
    }

    #[test]
    fn a_msg_becomes_a_message() {
        let bytes = sample(None);
        assert!(is_msg(&bytes));
        let eml = to_eml(&bytes).unwrap();
        let parsed = mail_parser::MessageParser::default().parse(&eml).unwrap();
        assert_eq!(parsed.subject(), Some("Quarterly numbers – draft"));
        let from = parsed.from().and_then(|a| a.first()).unwrap();
        assert_eq!(from.address(), Some("ada@example.com"));
        assert_eq!(from.name(), Some("Ada Lovelace"));
        assert_eq!(parsed.to().and_then(|a| a.first()).and_then(|a| a.address()), Some("grace@example.com"));
        assert_eq!(parsed.message_id(), Some("q3@example.com"));
        assert_eq!(parsed.date().map(|d| d.to_timestamp()), Some(1_791_540_000));
        assert!(parsed.body_html(0).unwrap().contains("Numbers attached."));
        let names: Vec<String> = parsed.attachments().filter_map(|a| a.attachment_name().map(str::to_string)).collect();
        assert!(names.contains(&"numbers.csv".to_string()), "{names:?}");
        assert!(names.contains(&"chart.png".to_string()), "{names:?}");
    }

    #[test]
    fn the_server_headers_are_kept_when_outlook_has_them() {
        let headers = "Received: from mx\r\nFrom: \"Ada L.\" <ada@lab.example>\r\nTo: x@example.com\r\nSubject: From the server\r\nContent-Type: multipart/alternative; boundary=zzz\r\n\r\n";
        let eml = to_eml(&sample(Some(headers))).unwrap();
        let parsed = mail_parser::MessageParser::default().parse(&eml).unwrap();
        assert_eq!(parsed.subject(), Some("From the server"));
        assert_eq!(parsed.from().and_then(|a| a.first()).and_then(|a| a.address()), Some("ada@lab.example"));
        // Outlook's body structure is not carried over: the body is ours.
        assert!(parsed.body_html(0).unwrap().contains("Numbers attached."));
    }

    /// Real files: `MSG_DIR=<dir of .msg files> cargo test --bin hylki
    /// outlook_msg::tests::real_files -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_files() {
        let Ok(dir) = std::env::var("MSG_DIR") else { return };
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let bytes = std::fs::read(entry.path()).unwrap();
            let eml = to_eml(&bytes).unwrap();
            let p = mail_parser::MessageParser::default().parse(&eml).unwrap();
            if std::env::var("MSG_HEADERS").is_ok() {
                let text = String::from_utf8_lossy(&eml);
                println!("{}", text.split("\r\n\r\n").next().unwrap_or_default());
            }
            let names: Vec<String> = p.attachments().filter_map(|a| a.attachment_name().map(str::to_string)).collect();
            println!(
                "{}: subject={:?} from={:?} to={} date={:?} html={} text={} attachments={names:?}",
                entry.file_name().to_string_lossy(),
                p.subject(),
                p.from().and_then(|a| a.first()).map(|a| (a.name().map(str::to_string), a.address().map(str::to_string))),
                p.to().map(|a| a.iter().count()).unwrap_or(0),
                p.date().map(|d| d.to_rfc3339()),
                p.body_html(0).map(|b| b.len()).unwrap_or(0),
                p.body_text(0).map(|b| b.len()).unwrap_or(0),
            );
        }
    }

    #[test]
    fn not_a_msg() {
        assert!(!is_msg(b"From: a@b.c\r\n\r\nhi"));
        assert!(to_eml(b"not a compound file").is_err());
    }
}
