//! Answers given from the reader that are remembered: the lists
//! unsubscribed from (unsubscribed.toml), the invitations answered
//! (invites.toml) and the read receipts sent or declined (receipts.toml).
//! Also the receipts that came back for mail sent (read_by.toml).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::{config_file, read_toml, save_plain_toml};

/// A mailing list the user has unsubscribed from through the reader's
/// button, so a later message from the same list can say so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UnsubscribedList {
    /// [`crate::models::Unsubscribe::key`]: the List-Id, or the From address.
    pub(crate) key: String,
    /// Who the mail came from, for the record.
    #[serde(default)]
    pub(crate) name: String,
    /// Unix seconds of the request.
    #[serde(default)]
    pub(crate) at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct UnsubscribedFile {
    #[serde(default)]
    lists: Vec<UnsubscribedList>,
}

fn unsubscribed_path() -> Option<PathBuf> {
    config_file("unsubscribed.toml")
}

/// The lists left through the reader's Unsubscribe button, newest last.
pub(crate) fn load_unsubscribed() -> Vec<UnsubscribedList> {
    read_toml::<UnsubscribedFile>(unsubscribed_path())
        .unwrap_or_default()
        .lists
}

pub(crate) fn save_unsubscribed(lists: &[UnsubscribedList]) {
    let file = UnsubscribedFile {
        lists: lists.to_vec(),
    };
    save_plain_toml(unsubscribed_path(), &file);
}

/// How many answered invitations are remembered. An invitation is
/// answered far more often than a mailing list is left, and the file is
/// read at every start — the oldest fall off rather than growing it
/// without end. A meeting whose answer has been forgotten simply offers
/// the buttons again.
const INVITE_ANSWER_LIMIT: usize = 500;

/// A meeting invitation answered through the reader's buttons (#223), so
/// the card says so when the message is opened again. The organizer's
/// calendar is the record that counts; this is only what the reader shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InviteAnswer {
    /// [`crate::models::Invite::key`]: the event's UID, plus the occurrence
    /// when the answer was to one of a series.
    pub(crate) key: String,
    /// The `PARTSTAT` sent: ACCEPTED, TENTATIVE or DECLINED.
    pub(crate) status: String,
    /// Unix seconds of the answer.
    #[serde(default)]
    pub(crate) at: i64,
    /// What the meeting was called, for the record.
    #[serde(default)]
    pub(crate) summary: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct InviteAnswerFile {
    #[serde(default)]
    answers: Vec<InviteAnswer>,
}

fn invites_path() -> Option<PathBuf> {
    config_file("invites.toml")
}

/// The invitations answered through the reader, newest last.
pub(crate) fn load_invite_answers() -> Vec<InviteAnswer> {
    read_toml::<InviteAnswerFile>(invites_path())
        .unwrap_or_default()
        .answers
}

pub(crate) fn save_invite_answers(answers: &[InviteAnswer]) {
    let kept = answers.len().saturating_sub(INVITE_ANSWER_LIMIT);
    let file = InviteAnswerFile {
        answers: answers[kept..].to_vec(),
    };
    save_plain_toml(invites_path(), &file);
}

/// How many answered receipt requests are remembered, oldest dropped
/// first. The server keeps its own mark where it can (`$MDNSent`); this
/// covers the accounts that keep none, and the moment before a sync.
const RECEIPT_ANSWER_LIMIT: usize = 1000;

/// A request for a read receipt (#520) that was answered, so it is not
/// asked about again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReceiptAnswer {
    /// The Message-ID of the message that asked, bare.
    pub(crate) key: String,
    /// Whether a receipt was sent; false when the request was ignored.
    #[serde(default)]
    pub(crate) sent: bool,
    /// Unix seconds of the answer.
    #[serde(default)]
    pub(crate) at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct ReceiptAnswerFile {
    #[serde(default)]
    answers: Vec<ReceiptAnswer>,
}

fn receipts_path() -> Option<PathBuf> {
    config_file("receipts.toml")
}

/// The receipt requests answered, newest last.
pub(crate) fn load_receipts() -> Vec<ReceiptAnswer> {
    read_toml::<ReceiptAnswerFile>(receipts_path())
        .unwrap_or_default()
        .answers
}

pub(crate) fn save_receipts(answers: &[ReceiptAnswer]) {
    let kept = answers.len().saturating_sub(RECEIPT_ANSWER_LIMIT);
    let file = ReceiptAnswerFile {
        answers: answers[kept..].to_vec(),
    };
    save_plain_toml(receipts_path(), &file);
}

/// How many sent messages waiting for a receipt, and how many receipts
/// that came back, are remembered, oldest dropped first.
const READ_BY_LIMIT: usize = 2000;

/// A read receipt that came back (#520): the message `key` was shown to
/// `by` at `at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReadBy {
    /// The Message-ID of the message read, bare.
    pub(crate) key: String,
    /// The address it was shown to.
    #[serde(default)]
    pub(crate) by: String,
    /// Unix seconds.
    #[serde(default)]
    pub(crate) at: i64,
}

/// The mail sent that asked for a read receipt, and the receipts that
/// came back. The receipt mail itself is filed away once it is read, so
/// this is the only record of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReadByFile {
    /// Message-IDs, bare, newest last.
    #[serde(default)]
    pub(crate) asked: Vec<String>,
    /// Newest last.
    #[serde(default)]
    pub(crate) read: Vec<ReadBy>,
}

fn read_by_path() -> Option<PathBuf> {
    config_file("read_by.toml")
}

pub(crate) fn load_read_by() -> ReadByFile {
    read_toml::<ReadByFile>(read_by_path()).unwrap_or_default()
}

pub(crate) fn save_read_by(file: &ReadByFile) {
    let asked = file.asked.len().saturating_sub(READ_BY_LIMIT);
    let read = file.read.len().saturating_sub(READ_BY_LIMIT);
    let file = ReadByFile {
        asked: file.asked[asked..].to_vec(),
        read: file.read[read..].to_vec(),
    };
    save_plain_toml(read_by_path(), &file);
}
