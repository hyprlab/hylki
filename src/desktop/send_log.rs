//! What happened to each message written here, kept across restarts (#678):
//! sent, failed, held in the Outbox, discarded, and where its copy went.
//! The console only holds what this run printed, so "did that message go
//! yesterday?" had no answer once Hylki was closed.
//!
//! One line per event in `send-log.tsv` beside the other data files, the
//! owner's alone. It holds subjects and addresses for the owner to read;
//! an exported log takes neither subjects nor full addresses from it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The file is cut back to its newest half once it grows past this, so it
/// never needs pruning by hand: a few thousand events.
const MAX_BYTES: u64 = 512 * 1024;

/// How far back an exported log looks.
const EXPORT_DAYS: i64 = 30;

/// Workers and the app write from several threads.
static WRITE: Mutex<()> = Mutex::new(());

fn file() -> Option<PathBuf> {
    // The send paths' own tests would write to the real log.
    if cfg!(test) {
        return None;
    }
    crate::config::data_base().map(|d| d.join("hylki").join("send-log.tsv"))
}

/// Note an event for a message from `account`. `subject` and `recipients`
/// say which message; either may be empty when the event does not know.
pub(crate) fn note(account: &str, subject: &str, recipients: &str, event: &str) {
    let Some(path) = file() else { return };
    if let Err(e) = note_at(
        &path,
        crate::datefmt::now(),
        account,
        subject,
        recipients,
        event,
    ) {
        tracing::warn!("could not write the send log: {e}");
    }
}

/// [`note`] for a message being sent.
pub(crate) fn note_message(account: &str, m: &crate::worker::OutgoingMessage, event: &str) {
    let recipients = [&m.to, &m.cc, &m.bcc]
        .iter()
        .map(|f| f.trim())
        .filter(|f| !f.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    note(account, &m.subject, &recipients, event);
}

fn note_at(
    path: &Path,
    at: i64,
    account: &str,
    subject: &str,
    recipients: &str,
    event: &str,
) -> std::io::Result<()> {
    let _held = WRITE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(dir) = path.parent() {
        crate::config::private_dir(dir)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        trim(path)?;
    }
    let line = [account, event, subject, recipients].map(field).join("\t");
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?
        .write_all(format!("{at}\t{line}\n").as_bytes())
}

/// A field on one line: tabs and line breaks would split the record.
fn field(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '\t' || c == '\n' || c == '\r' {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn trim(path: &Path) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut bytes = 0;
    let mut kept: Vec<&str> = text
        .lines()
        .rev()
        .take_while(|line| {
            bytes += line.len() as u64 + 1;
            bytes <= MAX_BYTES / 2
        })
        .collect();
    kept.reverse();
    crate::config::write_private_file(path, &format!("{}\n", kept.join("\n")))
}

/// The log's last [`EXPORT_DAYS`] for an exported log, oldest first:
/// time, account, event and recipients. Subjects stay out, and the caller
/// shortens the addresses.
pub(crate) fn export_lines() -> Vec<String> {
    file()
        .map(|path| export_lines_at(&path, crate::datefmt::now()))
        .unwrap_or_default()
}

fn export_lines_at(path: &Path, now: i64) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let since = now - EXPORT_DAYS * 86_400;
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let at: i64 = f.next()?.parse().ok()?;
            let account = f.next()?;
            let event = f.next()?;
            let _subject = f.next();
            let recipients = f.next().unwrap_or("");
            (at >= since).then(|| {
                let when = chrono::DateTime::from_timestamp(at, 0)
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M:%S")
                            .to_string()
                    })
                    .unwrap_or_default();
                if recipients.is_empty() {
                    format!("{when}  {account}  {event}")
                } else {
                    format!("{when}  {account}  {event}  (to {recipients})")
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hylki-send-log-{}-{}",
            std::process::id(),
            crate::rng::token(8).unwrap()
        ));
        dir.join("send-log.tsv")
    }

    #[test]
    fn an_export_leaves_out_subjects_and_old_events() {
        let path = scratch();
        let now = 2_000_000_000;
        note_at(&path, now - 40 * 86_400, "a@x", "Old", "b@y", "sent").unwrap();
        note_at(
            &path,
            now - 60,
            "a@x",
            "Plans\tfor\nMay",
            "b@y, c@z",
            "sent",
        )
        .unwrap();
        note_at(&path, now, "a@x", "Plans", "", "copy saved to Sent").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\tPlans for May\t"));
        let lines = export_lines_at(&path, now);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("  a@x  sent  (to b@y, c@z)"));
        assert!(lines[1].ends_with("  a@x  copy saved to Sent"));
        assert!(!lines.join("\n").contains("Plans"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_full_log_keeps_its_newest_lines() {
        let path = scratch();
        let pad = "x".repeat(400);
        for i in 0..1400 {
            note_at(&path, i, "a@x", &pad, "", &format!("event {i}")).unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.len() as u64 <= MAX_BYTES + 500);
        assert!(text.contains("\tevent 1399\t"));
        assert!(!text.contains("\tevent 0\t"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
