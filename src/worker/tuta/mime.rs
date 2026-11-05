//! A Tuta message as MIME. The helper hands back a decrypted message in
//! parts (header, HTML body, recipients, the original header block and
//! attachments); put back together as MIME, it reads through the same
//! parser as every other account's mail.
//!
//! The original Message-ID, In-Reply-To and References are kept when the
//! header block has them. No Message-ID is ever made up: one invented here
//! would tie the message to nothing Tuta's own app knows.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::Value;

use crate::worker::helper::{HelperError, Object};

const BOUNDARY: &str = "=_hylki-tuta-part";

fn invalid() -> HelperError {
    HelperError::new("invalid_response")
}

fn text_of(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn address(value: Option<&Value>) -> (String, String) {
    match value {
        Some(Value::Object(fields)) => {
            (text_of(fields.get("name")), text_of(fields.get("address")))
        }
        _ => (String::new(), String::new()),
    }
}

/// Header text with any line break flattened: helper data never gets to
/// start a header of its own.
fn header_safe(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
        .collect()
}

/// RFC 2047 encoded words for non-ASCII text, folded so no line runs long.
fn encode_words(text: &str) -> String {
    let text = header_safe(text);
    if text.is_ascii() {
        return text;
    }
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in text.chars() {
        if chunk.len() + c.len_utf8() > 45 {
            words.push(format!("=?utf-8?b?{}?=", STANDARD.encode(&chunk)));
            chunk.clear();
        }
        chunk.push(c);
    }
    if !chunk.is_empty() {
        words.push(format!("=?utf-8?b?{}?=", STANDARD.encode(&chunk)));
    }
    words.join("\r\n ")
}

/// A bare address, or `name <address>` with the name quoted or encoded.
fn format_address((name, address): &(String, String)) -> String {
    let address = header_safe(address);
    if name.is_empty() {
        return address;
    }
    let name = if !name.is_ascii() {
        encode_words(name)
    } else if name.contains(|c: char| "()<>@,:;.\"[]\\".contains(c)) {
        let escaped = header_safe(name).replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        header_safe(name)
    };
    format!("{name} <{address}>")
}

/// A MIME type token pair, or the octet-stream fallback.
fn mime_type(value: Option<&Value>) -> String {
    let is_token = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&b))
    };
    match value
        .and_then(Value::as_str)
        .and_then(|t| t.split_once('/'))
    {
        Some((main, sub)) if is_token(main) && is_token(sub) => {
            format!("{}/{}", main.to_ascii_lowercase(), sub.to_ascii_lowercase())
        }
        _ => "application/octet-stream".into(),
    }
}

/// Base64 body lines of at most 76 characters.
fn base64_lines(data: &[u8]) -> String {
    let encoded = STANDARD.encode(data);
    let mut out = String::with_capacity(encoded.len() + encoded.len() / 38 + 2);
    for line in encoded.as_bytes().chunks(76) {
        // Base64 output is ASCII.
        out.push_str(std::str::from_utf8(line).unwrap_or_default());
        out.push_str("\r\n");
    }
    out
}

/// A filename parameter, RFC 2231-encoded when it is not plain ASCII.
fn filename_parameter(name: &str) -> String {
    if name.is_ascii() && !name.contains(|c: char| c.is_ascii_control() || c == '"' || c == '\\') {
        return format!("filename=\"{name}\"");
    }
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("filename*=utf-8''{encoded}")
}

/// The first `name` header in an unparsed header block, unfolded.
fn header_value(block: &str, name: &str) -> Option<String> {
    let mut current: Option<String> = None;
    for line in block.split('\n').map(|line| line.trim_end_matches('\r')) {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some(value) = current.as_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        if current.is_some() {
            return current;
        }
        if let Some((field, value)) = line.split_once(':') {
            if field.trim().eq_ignore_ascii_case(name) {
                current = Some(value.trim().to_string());
            }
        }
    }
    current
}

/// The date header's form of the helper's milliseconds since the epoch.
fn rfc2822_date(value: Option<&Value>) -> Result<String, HelperError> {
    let ms = value.and_then(Value::as_i64).ok_or_else(invalid)?;
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|at| at.format("%a, %d %b %Y %H:%M:%S +0000").to_string())
        .ok_or_else(invalid)
}

fn attachment_part(attachment: &Value) -> Result<String, HelperError> {
    let failed = || HelperError::new("attachment_failed");
    let Value::Object(attachment) = attachment else {
        return Err(failed());
    };
    let Some(Value::String(data)) = attachment.get("data") else {
        return Err(failed());
    };
    let data = STANDARD.decode(data).map_err(|_| failed())?;
    let filename = match attachment.get("name") {
        Some(Value::String(name)) if !name.is_empty() => name.as_str(),
        _ => "attachment",
    };
    let mut part = format!(
        "Content-Type: {}\r\nContent-Transfer-Encoding: base64\r\nContent-Disposition: attachment; {}\r\n",
        mime_type(attachment.get("mime_type")),
        filename_parameter(&header_safe(filename)),
    );
    if let Some(Value::String(cid)) = attachment.get("cid") {
        let cid = header_safe(cid);
        let cid = cid.trim().trim_start_matches('<').trim_end_matches('>');
        if !cid.is_empty() {
            part.push_str(&format!("Content-ID: <{cid}>\r\n"));
        }
    }
    part.push_str("\r\n");
    part.push_str(&base64_lines(&data));
    Ok(part)
}

/// Put a `message` reply back together as MIME.
pub(super) fn mime_message(item: &Object) -> Result<Vec<u8>, HelperError> {
    let Some(Value::Object(header)) = item.get("header") else {
        return Err(invalid());
    };
    let Some(Value::String(html)) = item.get("html") else {
        return Err(invalid());
    };
    let mut head = String::new();
    let mut put = |name: &str, value: &str| {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    };
    let subject = text_of(header.get("subject"));
    if !subject.is_empty() {
        put("Subject", &encode_words(&subject));
    }
    put("From", &format_address(&address(header.get("sender"))));
    put("Date", &rfc2822_date(header.get("date_ms"))?);
    for (name, key) in [("To", "to"), ("Cc", "cc"), ("Reply-To", "reply_to")] {
        let addresses = match item.get(key) {
            Some(Value::Array(addresses)) => addresses.as_slice(),
            None | Some(Value::Null) => &[],
            Some(_) => return Err(invalid()),
        };
        if !addresses.is_empty() {
            let formatted: Vec<String> = addresses
                .iter()
                .map(|a| format_address(&address(Some(a))))
                .collect();
            put(name, &formatted.join(", "));
        }
    }
    let original = item
        .get("headers")
        .and_then(Value::as_str)
        .unwrap_or_default();
    for name in [
        "Message-ID",
        "In-Reply-To",
        "References",
        "List-Unsubscribe",
        "List-Unsubscribe-Post",
    ] {
        if let Some(value) = header_value(original, name).filter(|v| !v.is_empty()) {
            put(name, &header_safe(&value));
        }
    }
    let attachments = match item.get("attachments") {
        Some(Value::Array(attachments)) => attachments.as_slice(),
        None | Some(Value::Null) => &[],
        Some(_) => return Err(invalid()),
    };
    let html_part = format!(
        "Content-Type: text/html; charset=\"utf-8\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}",
        base64_lines(html.as_bytes())
    );
    let mut raw = head;
    raw.push_str("MIME-Version: 1.0\r\n");
    if attachments.is_empty() {
        raw.push_str(&html_part);
        return Ok(raw.into_bytes());
    }
    raw.push_str(&format!(
        "Content-Type: multipart/mixed; boundary=\"{BOUNDARY}\"\r\n\r\n"
    ));
    raw.push_str(&format!("--{BOUNDARY}\r\n{html_part}"));
    for attachment in attachments {
        raw.push_str(&format!("--{BOUNDARY}\r\n"));
        raw.push_str(&attachment_part(attachment)?);
    }
    raw.push_str(&format!("--{BOUNDARY}--\r\n"));
    Ok(raw.into_bytes())
}
