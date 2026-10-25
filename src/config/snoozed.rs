//! Snoozed mail (#527): when each message comes back, and where to, in
//! snoozed.toml. The message itself waits in the account's Snoozed folder
//! on the server; this file is the only record of its time.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::{config_file, read_toml, save_plain_toml};

/// One snoozed message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Snooze {
    /// The account's address. Account ids are positions in accounts.toml
    /// and shift when one is removed; the address does not.
    pub(crate) account: String,
    /// The message's Message-ID, normalized as the cache keeps it: how it
    /// is found again in the Snoozed folder, where its UID is new.
    pub(crate) message_id: String,
    /// Unix seconds it comes back at.
    pub(crate) until: i64,
    /// Unix seconds it was snoozed at: a reply that arrives after this
    /// brings it back early.
    pub(crate) at: i64,
    /// The folder it goes back to.
    pub(crate) folder: String,
    /// The Snoozed folder it waits in, found again there even when the
    /// sidebar no longer lists it.
    pub(crate) path: String,
    /// Its subject, for the record.
    #[serde(default)]
    pub(crate) subject: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SnoozedFile {
    #[serde(default)]
    snoozed: Vec<Snooze>,
}

fn snoozed_path() -> Option<PathBuf> {
    config_file("snoozed.toml")
}

/// Every snoozed message, in the order they were snoozed.
pub(crate) fn load_snoozed() -> Vec<Snooze> {
    read_toml::<SnoozedFile>(snoozed_path())
        .unwrap_or_default()
        .snoozed
}

pub(crate) fn save_snoozed(snoozed: &[Snooze]) {
    let file = SnoozedFile {
        snoozed: snoozed.to_vec(),
    };
    save_plain_toml(snoozed_path(), &file);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snoozes_round_trip_and_the_subject_is_optional() {
        let s = Snooze {
            account: "ann@example.com".into(),
            message_id: "abc@example.com".into(),
            until: 1_800_000_000,
            at: 1_790_000_000,
            folder: "INBOX".into(),
            path: "Snoozed".into(),
            subject: "Lunch".into(),
        };
        let file = SnoozedFile {
            snoozed: vec![s.clone()],
        };
        let text = toml::to_string(&file).unwrap();
        let back: SnoozedFile = toml::from_str(&text).unwrap();
        assert_eq!(back.snoozed, vec![s]);
        let bare: SnoozedFile = toml::from_str(
            "[[snoozed]]\naccount = \"a\"\nmessage_id = \"m\"\nuntil = 2\nat = 1\nfolder = \"INBOX\"\npath = \"Snoozed\"\n",
        )
        .unwrap();
        assert!(bare.snoozed[0].subject.is_empty());
    }
}
