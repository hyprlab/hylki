//! The line a reply puts above the quoted original: "On Oct 25, 2026 at
//! 14:03, Ada wrote:". Written in the app's language unless the user has
//! written their own in Settings → Composing (#528).

use crate::i18n::{fill, i18n};

/// The header used when the user has not written one.
pub(crate) fn default_template() -> String {
    i18n("On {date} at {time}, {name} wrote:")
}

/// What Settings stores for `text`: nothing when it is the default, so the
/// header follows a later change of language.
pub(crate) fn stored_template(text: &str) -> String {
    let text = text.trim();
    if text == default_template() {
        String::new()
    } else {
        text.to_string()
    }
}

/// The header for a reply to mail from `name` <`address`> sent at `ts`, from
/// `template` ("" for the default). `label` is the date to show when the
/// message's own date could not be read.
pub(crate) fn attribution(
    template: &str,
    name: &str,
    address: &str,
    ts: i64,
    label: &str,
) -> String {
    let template = if template.trim().is_empty() {
        default_template()
    } else {
        template.trim().to_string()
    };
    let name = if name.trim().is_empty() {
        address
    } else {
        name
    };
    let (date, time) = if ts > 0 {
        (crate::datefmt::day_month_year(ts), crate::datefmt::time(ts))
    } else {
        (label.to_string(), String::new())
    };
    fill(
        template,
        &[
            ("name", name),
            ("address", address),
            ("date", &date),
            ("time", &time),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_placeholders_are_filled_in() {
        let ts = 1_790_000_000;
        let (date, time) = (crate::datefmt::day_month_year(ts), crate::datefmt::time(ts));
        assert_eq!(
            attribution("", "Ada", "ada@x.org", ts, ""),
            format!("On {date} at {time}, Ada wrote:")
        );
        assert_eq!(
            attribution(
                " {name} <{address}>, {date}, {time}: ",
                "Ada",
                "ada@x.org",
                ts,
                ""
            ),
            format!("Ada <ada@x.org>, {date}, {time}:")
        );
        // No name: the address stands in for it.
        assert_eq!(
            attribution("{name} wrote:", "", "ada@x.org", ts, ""),
            "ada@x.org wrote:"
        );
        assert_eq!(attribution("{date}", "", "", 0, "30 Sep"), "30 Sep");
    }

    #[test]
    fn the_default_is_stored_as_nothing() {
        assert_eq!(stored_template(&default_template()), "");
        assert_eq!(stored_template("  "), "");
        assert_eq!(stored_template(" {name} wrote: "), "{name} wrote:");
    }
}
