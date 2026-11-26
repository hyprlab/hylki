//! Sensitivity labels (#411): the classification an organization puts on a
//! message, shown read-only as a pill in the reader.
//!
//! Microsoft Purview writes the label into an `msip_labels` header, one
//! `MSIP_Label_<id>_<key>=<value>` pair per field, so any client can read
//! it; Outlook's older `Sensitivity:` header (RFC 2156) says Personal,
//! Private or Company-Confidential. Hylki shows what the label says and
//! enforces nothing: protection that encrypts a message (Rights Management)
//! is a separate matter the label only names.

use crate::i18n::{i18n, i18n_f};
use crate::mail::mail_header::unfold;

/// The label a message carries.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SensitivityLabel {
    /// The name the organization gave it, as written in the header, or for
    /// the older header its value (`personal`, `private`,
    /// `company-confidential`). Empty when Purview left the name out.
    pub(crate) name: String,
    /// The Purview label's id; empty for the older header.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) id: String,
    /// The label's color as `#rrggbb`, and what the organization says it
    /// means: both from its list of labels (`graph_labels`), empty when
    /// that was not read.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) color: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) tooltip: String,
}

impl SensitivityLabel {
    fn purview(&self) -> bool {
        !self.id.is_empty()
    }

    /// The pill's text.
    pub(crate) fn short_label(&self) -> String {
        if self.purview() {
            return if self.name.is_empty() {
                i18n("Labeled")
            } else {
                self.name.clone()
            };
        }
        match self.name.as_str() {
            "personal" => i18n("Personal"),
            "private" => i18n("Private"),
            _ => i18n("Confidential"),
        }
    }

    /// The pill's tooltip: where the label comes from, and that nothing is
    /// enforced here.
    pub(crate) fn summary(&self) -> String {
        if !self.purview() {
            return i18n_f(
                "The sender marked this message “{label}”.",
                &[("label", &self.short_label())],
            );
        }
        if self.name.is_empty() {
            return i18n("The sender’s organization labeled this message without naming the label. For a Microsoft 365 account, turn on “Show sensitivity label names” in its settings to see the name. The label’s rules are not applied here.");
        }
        let said = i18n_f(
            "The sender’s organization labeled this message “{label}”. The label’s rules are not applied here.",
            &[("label", &self.name)],
        );
        // The organization's own words, as Outlook shows them.
        if self.tooltip.is_empty() {
            said
        } else {
            format!("{said}\n\n{}", self.tooltip)
        }
    }
}

/// The label in a parsed message's top-level headers, if it has one.
pub(crate) fn detect(parsed: &mail_parser::Message) -> Option<SensitivityLabel> {
    let raw = parsed.raw_message();
    let value = |name: &str| -> Option<String> {
        parsed
            .headers()
            .iter()
            .find(|h| h.name().eq_ignore_ascii_case(name))
            .and_then(|h| raw.get(h.offset_start()..h.offset_end()))
            .map(|b| unfold(&String::from_utf8_lossy(b)))
            .filter(|v| !v.is_empty())
    };
    value("msip_labels")
        .and_then(|v| from_msip(&v))
        .or_else(|| value("Sensitivity").and_then(|v| from_sensitivity(&v)))
}

/// The enabled label of an `msip_labels` value. A message relabeled keeps
/// the old label's pairs with `Enabled=false`; the first enabled one is the
/// label it has.
fn from_msip(value: &str) -> Option<SensitivityLabel> {
    // Label id → (enabled, name), in the order the ids first appear.
    let mut labels: Vec<(String, bool, String)> = Vec::new();
    for pair in value.split(';') {
        let Some((key, val)) = pair.trim().split_once('=') else {
            continue;
        };
        let Some(rest) = strip_prefix_ci(key.trim(), "MSIP_Label_") else {
            continue;
        };
        let Some((id, field)) = rest.rsplit_once('_') else {
            continue;
        };
        let id = id.to_ascii_lowercase();
        let at = match labels.iter().position(|(i, _, _)| *i == id) {
            Some(at) => at,
            None => {
                labels.push((id, false, String::new()));
                labels.len() - 1
            }
        };
        let val = val.trim();
        if field.eq_ignore_ascii_case("Enabled") {
            labels[at].1 = val.eq_ignore_ascii_case("true");
        } else if field.eq_ignore_ascii_case("Name") {
            labels[at].2 = val.to_string();
        }
    }
    let (id, _, name) = labels.into_iter().find(|(_, on, _)| *on)?;
    // A tenant that keeps names out of its metadata writes the id there,
    // which says nothing to a reader; a sublabel is "Parent\Child".
    let name = if looks_like_guid(&name) {
        String::new()
    } else {
        name.replace('\\', " / ")
    };
    Some(SensitivityLabel {
        name,
        id,
        ..Default::default()
    })
}

fn from_sensitivity(value: &str) -> Option<SensitivityLabel> {
    let v = value.trim().to_ascii_lowercase();
    matches!(v.as_str(), "personal" | "private" | "company-confidential").then(|| {
        SensitivityLabel {
            name: v,
            ..Default::default()
        }
    })
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &s[prefix.len()..])
}

fn looks_like_guid(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Header unfolding (`crate::mail::mail_header::unfold`), on the corpus both
    /// header readers were pinned with before their copies were merged.
    #[test]
    fn folded_header_values_become_one_line() {
        let corpus = [
            ("a\r\n b", "a b"),
            ("a\r\n\tb\r\n  c", "a b c"),
            ("  padded  ", "padded"),
            ("a\n\n\nb", "a b"),
            ("a\rb", "a b"),
            ("<x>,\r\n <y>", "<x>, <y>"),
            ("", ""),
            ("\r\n", ""),
        ];
        for (raw, one) in corpus {
            assert_eq!(unfold(raw), one, "{raw:?}");
        }
    }

    fn parse(headers: &str) -> Option<SensitivityLabel> {
        let raw = format!("From: a@b.c\r\nSubject: x\r\n{headers}\r\nbody\r\n");
        detect(
            &mail_parser::MessageParser::default()
                .parse(raw.as_bytes())
                .unwrap(),
        )
    }

    #[test]
    fn a_purview_label_is_read_from_msip_labels() {
        let l = parse(
            "msip_labels: MSIP_Label_f42aa342-8706-4288-bd11-ebb85995028c_Enabled=true;\r\n \
             MSIP_Label_f42aa342-8706-4288-bd11-ebb85995028c_SetDate=2026-10-01T09:00:00Z;\r\n \
             MSIP_Label_f42aa342-8706-4288-bd11-ebb85995028c_Method=Standard;\r\n \
             MSIP_Label_f42aa342-8706-4288-bd11-ebb85995028c_Name=Internal Use;\r\n \
             MSIP_Label_f42aa342-8706-4288-bd11-ebb85995028c_SiteId=72f988bf-86f1-41af-91ab-2d7cd011db47;\r\n",
        )
        .unwrap();
        assert_eq!(l.name, "Internal Use");
        assert_eq!(l.id, "f42aa342-8706-4288-bd11-ebb85995028c");
        assert_eq!(l.short_label(), "Internal Use");
    }

    #[test]
    fn the_enabled_label_wins_and_a_hidden_name_stays_hidden() {
        let l = parse(
            "MSIP_Labels: MSIP_Label_aaaaaaaa-0000-0000-0000-000000000001_Enabled=false; \
             MSIP_Label_aaaaaaaa-0000-0000-0000-000000000001_Name=Public; \
             MSIP_Label_bbbbbbbb-0000-0000-0000-000000000002_Enabled=True; \
             MSIP_Label_bbbbbbbb-0000-0000-0000-000000000002_Name=Confidential\\Finance",
        )
        .unwrap();
        assert_eq!(l.name, "Confidential / Finance");
        let hidden = parse(
            "msip_labels: MSIP_Label_cccccccc-0000-0000-0000-000000000003_Enabled=true; \
             MSIP_Label_cccccccc-0000-0000-0000-000000000003_Name=defa4170-0d19-0005-0004-bc88714345d2",
        )
        .unwrap();
        assert!(hidden.name.is_empty() && !hidden.id.is_empty());
        assert!(parse(
            "msip_labels: MSIP_Label_dddddddd-0000-0000-0000-000000000004_Enabled=false"
        )
        .is_none());
    }

    #[test]
    fn the_older_sensitivity_header_counts_too() {
        assert_eq!(
            parse("Sensitivity: Company-Confidential").unwrap().name,
            "company-confidential"
        );
        assert_eq!(parse("Sensitivity: Personal").unwrap().id, "");
        assert!(parse("Sensitivity: normal").is_none());
        assert!(parse("X-Other: 1").is_none());
    }
}
