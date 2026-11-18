//! On This Computer's Sent folder, for mail sent from an account that has
//! nowhere on its server to keep a copy (#678): a POP3 account, or one whose
//! server lists no Sent folder.

use std::path::Path;

use crate::config::AccountConfig;
use crate::i18n::i18n_f;
use crate::worker::WorkerEvent;

use super::maildir::deliver_into;

/// The local folder copies go to.
const SENT: &str = "Sent";

/// Keep a copy of `raw`, just sent as `from`, in On This Computer's Sent,
/// unless the server files its own. The app adds On This Computer when
/// there is none yet, so the copy shows.
pub(in crate::worker) fn keep_sent_here(
    account: &AccountConfig,
    from: Option<&str>,
    raw: &[u8],
    subject: &str,
    emit: &impl Fn(WorkerEvent),
) {
    let log = |event: &str| crate::desktop::send_log::note(&account.email, subject, "", event);
    if account.is_local() {
        return;
    }
    if account.server_files_sent(from) {
        log("copy left to the server");
        return;
    }
    let filed = crate::config::local_mail_dir()
        .ok_or_else(|| std::io::Error::other("no data folder"))
        .and_then(|root| file_sent_at(&root, raw));
    match filed {
        Ok(()) => {
            log("no Sent folder on the server; copy saved to On This Computer, Sent");
            emit(WorkerEvent::SentFiledHere);
        }
        Err(e) => {
            log(&format!(
                "no Sent folder on the server; copy not saved: {e}"
            ));
            emit(WorkerEvent::error(i18n_f(
                "Message sent, but saving it to On This Computer failed: {e}",
                &[("e", &e.to_string())],
            )));
        }
    }
}

fn file_sent_at(root: &Path, raw: &[u8]) -> std::io::Result<()> {
    crate::config::private_dir(root)?;
    deliver_into(root, SENT, raw, "S")?;
    super::copy::DELIVERED.notify_one();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sent_copy_lands_read_in_the_local_sent_folder() {
        let dir = super::super::maildir::tests::Scratch::new();
        let root = dir.path().join("local");
        file_sent_at(&root, b"From: a@x\r\nSubject: s\r\n\r\nb\r\n").unwrap();
        let cur: Vec<String> = std::fs::read_dir(root.join(".Sent/cur"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        assert_eq!(cur.len(), 1);
        assert!(cur[0].ends_with(":2,S"));
    }

    #[test]
    fn a_server_that_files_its_own_copy_gets_none_here() {
        let account = AccountConfig {
            server_saves_sent: true,
            ..crate::worker::test_support::sample_account()
        };
        let emitted = std::cell::Cell::new(false);
        keep_sent_here(&account, None, b"x", "", &|_| emitted.set(true));
        assert!(!emitted.get());
    }
}
