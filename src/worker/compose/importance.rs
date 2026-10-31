//! The importance a sender gives their mail (#563), on the wire.

use lettre::message::header::{HeaderName, HeaderValue};

use crate::models::Importance;

/// The headers that say `importance`: `X-Priority` for the mail apps that
/// read only that, `Importance` for Outlook and Exchange. Normal mail
/// carries neither, as most mail apps send it.
pub(super) fn headers(importance: Importance) -> Vec<HeaderValue> {
    let (priority, word) = match importance {
        Importance::Normal => return Vec::new(),
        Importance::High => ("1 (Highest)", "high"),
        Importance::Low => ("5 (Lowest)", "low"),
    };
    vec![
        HeaderValue::new(
            HeaderName::new_from_ascii_str("X-Priority"),
            priority.to_string(),
        ),
        HeaderValue::new(
            HeaderName::new_from_ascii_str("Importance"),
            word.to_string(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use crate::models::Importance;
    use crate::worker::compose::build::build_email;
    use crate::worker::test_support::{sample_account, sample_outgoing};
    use crate::worker::OutgoingMessage;

    fn sent(importance: Importance) -> String {
        let msg = OutgoingMessage {
            to: "ann@example.org".into(),
            importance,
            ..sample_outgoing()
        };
        let raw = build_email(&sample_account(), &msg).unwrap().formatted();
        String::from_utf8_lossy(&raw).into_owned()
    }

    #[test]
    fn importance_goes_out_and_comes_back() {
        for importance in [Importance::High, Importance::Low] {
            let text = sent(importance);
            let parsed = mail_parser::MessageParser::default()
                .parse(text.as_bytes())
                .unwrap();
            let read = Importance::from_headers(|name| parsed.header_raw(name));
            assert_eq!(read, importance, "{text}");
            let editable =
                crate::worker::compose::editable::editable_from_raw(text.as_bytes(), &[]);
            assert_eq!(editable.importance, importance, "{text}");
        }
    }

    #[test]
    fn normal_mail_says_nothing() {
        let text = sent(Importance::Normal);
        assert!(!text.contains("X-Priority"), "{text}");
        assert!(!text.contains("Importance"), "{text}");
    }
}
