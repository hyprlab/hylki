//! The attachments of a raw message: which parts count as files (inline
//! images only above a size, calendar parts always), and their names.

use crate::worker::strip;

/// Decoded size at or above which a part carrying a Content-ID counts as an
/// attachment as well as being rendered in the body.
///
/// A `cid:` part is referenced from the HTML and drawn in place, so listing it
/// would give newsletters a paperclip for their logo, spacer and social icons —
/// the false-attachment noise fixed in 1.4.1. But when someone emails you a
/// photo, Gmail and Apple Mail send exactly the same shape, and that picture has
/// to be saveable. Size is the only honest discriminator: decoration is small,
/// content is not. 64 KiB sits well above logos and icons and well below any
/// photo worth keeping.
pub(in crate::worker) const INLINE_ATTACHMENT_MIN: usize = 64 * 1024;

/// The attachments of a raw message. Public so the Outbox can list a queued
/// message's files without a fetch.
pub(crate) fn extract_attachments_of(raw: &[u8]) -> Vec<crate::models::Attachment> {
    extract_attachments(raw)
}

/// The attachments of a raw message, through any OpenPGP wrapping, and
/// whether they may be cached (never for OpenPGP mail, like its body).
pub(in crate::worker) fn attachments_of(raw: &[u8]) -> (Vec<crate::models::Attachment>, bool) {
    match crate::mail::pgp::detect(raw) {
        None => (extract_attachments(raw), true),
        Some(_) => {
            let items = crate::mail::pgp::unwrap_message(raw, &crate::mail::pgp::Gpg::system())
                .and_then(|u| u.inner)
                .map(|inner| extract_attachments(&inner))
                .unwrap_or_default();
            (items, false)
        }
    }
}

/// Parse attachment parts (name, mime, decoded bytes) out of a raw message.
pub(in crate::worker) fn extract_attachments(raw: &[u8]) -> Vec<crate::models::Attachment> {
    let Some(parsed) = mail_parser::MessageParser::default().parse(raw) else {
        return Vec::new();
    };
    attachment_parts(&parsed, raw)
        .into_iter()
        .map(|(_, a)| a)
        .collect()
}

/// The attachments of a parsed message, each with the index of the MIME
/// part it came from, so the part can be found again to remove it (#289).
pub(in crate::worker) fn attachment_parts(
    parsed: &mail_parser::Message<'_>,
    raw: &[u8],
) -> Vec<(usize, crate::models::Attachment)> {
    use mail_parser::MimeHeaders;
    let mut out = Vec::new();
    let legacy = crate::mail::pgp::legacy_display_part(parsed);
    for (i, &id) in parsed.attachments.iter().enumerate() {
        if Some(id) == legacy {
            continue;
        }
        let Some(part) = parsed.parts.get(id) else {
            continue;
        };
        // What is left where an attachment was removed, here or by
        // Thunderbird (#289): a note, not a file.
        if strip::is_placeholder(part) {
            continue;
        }
        // A read receipt's report (#520) is for the mail app, not a file.
        if crate::mail::receipt::is_report_part(part) {
            continue;
        }
        // Decoration referenced from the body (a `cid:` logo) is rendered in
        // place, not listed — unless it's big enough to be real content.
        //
        // A part the sender marked `Content-Disposition: attachment` is never
        // decoration, however small. Gmail's composer stamps a Content-ID
        // (`<f_…>`) on every file it uploads, so the size test alone silently
        // dropped real attachments under 64 KiB: a message with two small PDFs
        // and one large font file listed only the font. `structure_has_attachment`
        // already tests disposition first, which is why such mail showed a
        // paperclip and then an incomplete list — the two disagreed.
        let declared_attachment = part
            .content_disposition()
            .is_some_and(|d| d.is_attachment());
        if !declared_attachment
            && part.content_id().is_some()
            && part.contents().len() < INLINE_ATTACHMENT_MIN
        {
            continue;
        }
        let name = attachment_name_of(part, raw).unwrap_or_else(|| {
            // Unnamed, as a meeting request's calendar part often is (#223):
            // give it the suffix its type implies, or nothing opens it.
            let ext = part
                .content_type()
                .map(|c| mime_extension(c.ctype(), c.subtype().unwrap_or_default()))
                .unwrap_or_default();
            format!("attachment-{}{ext}", i + 1)
        });
        out.push((
            id,
            crate::models::Attachment {
                name,
                data: part.contents().to_vec(),
            },
        ));
    }
    out
}

/// An attachment part's filename, repaired if the sender split it across adjacent
/// RFC 2047 encoded-words.
///
/// RFC 2047 §6.2 joins adjacent encoded-words separated only by linear whitespace
/// and discards that whitespace — it is there so a long word can be folded, not as
/// part of the text. `mail_parser` keeps the separator inside parameter values, so
/// a file announced as
///
/// ```text
/// Content-Disposition: attachment; filename="=?utf-8?q?docu?= =?utf-8?q?ment.pdf?="
/// ```
///
/// arrives named `docu ment.pdf`. That is not a cosmetic problem: with no `.pdf`
/// suffix [`guess_mime`] can't type the file, so it gets no PDF thumbnail and
/// opens in nothing. A header folded between the encoded-words leaves a tab
/// instead of a space, with the same result.
///
/// Splice such pairs in the raw header and let `mail_parser` decode the repaired
/// version, so the charset handling stays in one place.
fn attachment_name_of(part: &mail_parser::MessagePart<'_>, raw: &[u8]) -> Option<String> {
    use mail_parser::{HeaderName, MessageParser, MimeHeaders};

    let names = |h: &mail_parser::Header<'_>| {
        matches!(
            h.name,
            HeaderName::ContentDisposition | HeaderName::ContentType
        )
    };
    let plain = || part.attachment_name().map(|s| s.to_string());

    let mut repaired = Vec::new();
    let mut spliced = false;
    for h in part.headers.iter().filter(|h| names(h)) {
        let Some(value) = raw.get(h.offset_start..h.offset_end) else {
            return plain();
        };
        let joined = splice_encoded_words(value);
        spliced |= joined != value;
        repaired.extend_from_slice(h.name().as_bytes());
        repaired.push(b':');
        repaired.extend_from_slice(&joined);
        if !repaired.ends_with(b"\n") {
            repaired.extend_from_slice(b"\r\n");
        }
    }
    // Nothing to repair: the common case, and no second parse.
    if !spliced {
        return plain();
    }
    repaired.extend_from_slice(b"\r\n");
    MessageParser::default()
        .parse(&repaired)
        .and_then(|m| m.attachment_name().map(|s| s.to_string()))
        .or_else(plain)
}

/// Join adjacent RFC 2047 encoded-words, discarding the linear whitespace between
/// them. Bytes are safe to scan directly: `?=`, `=?` and the whitespace are all
/// ASCII, which never occurs inside a multi-byte UTF-8 sequence.
fn splice_encoded_words(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        if value[i..].starts_with(b"?=") {
            let mut j = i + 2;
            while matches!(value.get(j), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                j += 1;
            }
            if j > i + 2 && value[j..].starts_with(b"=?") {
                out.extend_from_slice(b"?=");
                i = j;
                continue;
            }
        }
        out.push(value[i]);
        i += 1;
    }
    out
}

/// Guess a MIME type from a filename, from shared-mime-info's table of
/// suffixes: the one the desktop opens files by, current Office formats
/// included. Only the name is looked at, and GIO calls a name it has no
/// suffix for `application/x-zerosize` (it reads the missing data as empty),
/// so that becomes the generic type.
pub(in crate::worker) fn guess_mime(name: &str) -> String {
    let (content_type, _uncertain) = gtk::gio::content_type_guess(Some(name), &[]);
    gtk::gio::content_type_get_mime_type(&content_type)
        .map(|m| m.to_string())
        .filter(|m| m != "application/x-zerosize")
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

/// A filename extension for a MIME type, for parts that arrive unnamed. Only
/// the types that actually turn up without a filename are worth listing; the
/// rest get none, and show as a generic file.
pub(in crate::worker) fn mime_extension(ty: &str, subtype: &str) -> &'static str {
    // A meeting request's calendar part often arrives with no name at all
    // (Outlook sends one); without the suffix nothing would open it (#223).
    if ty.eq_ignore_ascii_case("text") && subtype.eq_ignore_ascii_case("calendar") {
        return ".ics";
    }
    if !ty.eq_ignore_ascii_case("image") {
        return "";
    }
    match subtype.to_ascii_lowercase().as_str() {
        "jpeg" | "jpg" => ".jpg",
        "png" => ".png",
        "gif" => ".gif",
        "heic" => ".heic",
        "heif" => ".heif",
        "webp" => ".webp",
        "tiff" => ".tif",
        _ => "",
    }
}

#[cfg(test)]
mod guess_mime_tests {
    use super::guess_mime;

    #[test]
    fn types_attachments_by_suffix() {
        assert_eq!(guess_mime("scan.PDF"), "application/pdf");
        assert_eq!(guess_mime("photo.jpeg"), "image/jpeg");
        assert_eq!(
            guess_mime("report.docx"),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
        assert_eq!(
            guess_mime("budget.xlsx"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(guess_mime("blob.zzqx"), "application/octet-stream");
    }
}
