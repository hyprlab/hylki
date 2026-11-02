//! Read receipts on the wire (#520): asking for one, and the
//! `multipart/report` that answers (RFC 8098).

use lettre::message::header::{ContentTransferEncoding, ContentType};
use lettre::message::{Body, MultiPart, SinglePart};

use crate::worker::smtp::SmtpError;

/// The header that asks for a read receipt, sent to `addr`.
pub(super) fn request_header(addr: &str) -> lettre::message::header::HeaderValue {
    lettre::message::header::HeaderValue::new(
        lettre::message::header::HeaderName::new_from_ascii_str("Disposition-Notification-To"),
        format!("<{addr}>"),
    )
}

/// A receipt's body: `words` for the person, then `fields`, the report for
/// their mail app. lettre has no `multipart/report`, so its Content-Type is
/// written here, boundary and all.
pub(super) fn report_multipart(words: &str, fields: &str) -> Result<MultiPart, SmtpError> {
    let boundary = crate::rng::from_alphabet(
        40,
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
    )
    .map_err(|e| format!("could not make a MIME boundary: {e}"))?;
    let ctype = ContentType::parse(&format!(
        "multipart/report; report-type=disposition-notification; boundary=\"{boundary}\""
    ))
    .map_err(|e| format!("could not write the receipt's type: {e}"))?;
    // A message/* part may not be base64 or quoted-printable (RFC 2046
    // 5.2), so the fields go as they are: 7bit, or 8bit for an
    // internationalized address.
    let encoding = if fields.is_ascii() {
        ContentTransferEncoding::SevenBit
    } else {
        ContentTransferEncoding::EightBit
    };
    let report = SinglePart::builder()
        .header(
            ContentType::parse("message/disposition-notification")
                .map_err(|e| format!("could not write the report's type: {e}"))?,
        )
        .body(
            Body::new_with_encoding(fields.to_string(), encoding)
                .map_err(|_| "the receipt's report could not be encoded".to_string())?,
        );
    Ok(MultiPart::builder()
        .header(ctype)
        .singlepart(SinglePart::plain(words.to_string()))
        .singlepart(report))
}

#[cfg(test)]
mod tests {
    use crate::worker::compose::build::build_email;
    use crate::worker::test_support::{sample_account, sample_outgoing};
    use crate::worker::OutgoingMessage;

    #[test]
    fn a_message_can_ask_for_a_receipt() {
        let msg = OutgoingMessage {
            to: "ann@example.org".into(),
            request_receipt: true,
            ..sample_outgoing()
        };
        let raw = build_email(&sample_account(), &msg).unwrap().formatted();
        let text = String::from_utf8_lossy(&raw);
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        let request = crate::mail::receipt::detect(&parsed).expect("asks: {text}");
        assert_eq!(request.to, sample_account().email, "{text}");
        let plain = OutgoingMessage {
            to: "ann@example.org".into(),
            ..sample_outgoing()
        };
        let plain = build_email(&sample_account(), &plain).unwrap().formatted();
        assert!(!String::from_utf8_lossy(&plain).contains("Disposition-Notification-To"));
    }

    #[test]
    fn a_receipt_is_a_report_that_asks_for_nothing() {
        let msg = OutgoingMessage {
            to: "ann@example.org".into(),
            subject: "Read: Plans".into(),
            body: "This is a read receipt.".into(),
            in_reply_to: "abc@example.org".into(),
            request_receipt: true,
            receipt_report: Some(crate::mail::receipt::report_fields(
                "me@example.com",
                &Default::default(),
                "abc@example.org",
                false,
            )),
            ..sample_outgoing()
        };
        let raw = build_email(&sample_account(), &msg).unwrap().formatted();
        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.contains("multipart/report; report-type=disposition-notification;"),
            "{text}"
        );
        assert!(
            text.contains("Content-Type: message/disposition-notification"),
            "{text}"
        );
        assert!(
            text.contains("Original-Message-ID: <abc@example.org>"),
            "{text}"
        );
        assert!(text.contains("This is a read receipt."), "{text}");
        assert!(!text.contains("Disposition-Notification-To"), "{text}");
        // What the reader makes of it: the words as the body, and no
        // attachment for the report.
        let parsed = mail_parser::MessageParser::default().parse(&raw).unwrap();
        assert!(crate::worker::render::attachments::attachment_parts(&parsed, &raw).is_empty());
        assert!(crate::worker::extract_body(&raw).contains("This is a read receipt."));
    }
}
