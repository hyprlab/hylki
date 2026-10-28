//! The cache's schema and its upgrades, pinned: the tables and indexes a
//! database ends up with, and what each version gate drops or rewrites.

use std::path::PathBuf;

use rusqlite::Connection;

use super::{Cache, SCHEMA};
use crate::config::golden::assert_golden;

/// A database file of its own in the temp dir, removed (with its WAL
/// sidecars) when dropped.
struct TempDb(PathBuf);

impl TempDb {
    fn new(name: &str) -> TempDb {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        TempDb(std::env::temp_dir().join(format!(
            "hylki-schema-{name}-{}-{nanos}.db",
            std::process::id()
        )))
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut path = self.0.as_os_str().to_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The user version, then every table and index with its SQL, by name.
fn dump(conn: &Connection) -> String {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    let mut out = format!("user_version {version}\n");
    let mut stmt = conn
        .prepare("SELECT type, name, sql FROM sqlite_master ORDER BY name")
        .unwrap();
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .unwrap();
    for row in rows {
        let (kind, name, sql) = row.unwrap();
        out += &format!("\n{kind} {name}\n{}\n", sql.unwrap_or_default());
    }
    out
}

#[test]
fn a_new_database_gets_the_whole_schema() {
    let db = TempDb::new("fresh");
    let cache = Cache::open_at(&db.0).unwrap();
    let fresh = dump(&cache.conn);
    assert_golden("cache/testdata/schema-new.txt", &fresh);
    // The demo's in-memory cache runs the same schema.
    assert_eq!(dump(&Cache::in_memory().unwrap().conn), fresh);
}

/// The three tables as layout v6 created them, before the columns that
/// `migrate` adds in place.
const V6_TABLES: &str = "
CREATE TABLE messages (
    account_id     INTEGER NOT NULL,
    folder_path    TEXT    NOT NULL,
    uid            INTEGER NOT NULL,
    from_name      TEXT    NOT NULL,
    from_addr      TEXT    NOT NULL,
    recipients     TEXT    NOT NULL DEFAULT '',
    cc             TEXT    NOT NULL DEFAULT '',
    subject        TEXT    NOT NULL,
    date           TEXT    NOT NULL,
    ts             INTEGER NOT NULL,
    unread         INTEGER NOT NULL,
    starred        INTEGER NOT NULL,
    has_attachment INTEGER NOT NULL,
    message_id     TEXT    NOT NULL DEFAULT '',
    references_    TEXT    NOT NULL DEFAULT '',
    PRIMARY KEY (account_id, folder_path, uid)
);
CREATE TABLE sender_checks (
    account_id  INTEGER NOT NULL,
    folder_path TEXT NOT NULL,
    uid         INTEGER NOT NULL,
    trust       TEXT NOT NULL,
    summary     TEXT NOT NULL,
    findings    TEXT NOT NULL,
    unsubscribe TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (account_id, folder_path, uid)
);
CREATE TABLE outbox (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id  INTEGER NOT NULL,
    from_addr   TEXT    NOT NULL,
    rcpts       TEXT    NOT NULL,
    recipients  TEXT    NOT NULL,
    subject     TEXT    NOT NULL,
    preview     TEXT    NOT NULL DEFAULT '',
    raw         BLOB    NOT NULL,
    sent_path   TEXT,
    queued_at   INTEGER NOT NULL,
    attempts    INTEGER NOT NULL DEFAULT 0,
    last_error  TEXT    NOT NULL DEFAULT ''
);
PRAGMA user_version = 21;
";

/// An index written by an older build gains its columns at the end of each
/// table, in place, and keeps its rows.
#[test]
fn an_older_layout_gains_the_new_columns() {
    let db = TempDb::new("v6");
    {
        let conn = Connection::open(&db.0).unwrap();
        conn.execute_batch(V6_TABLES).unwrap();
        conn.execute(
            "INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment) \
             VALUES (1, 'INBOX', 7, 'Ann', 'ann@x', 'Hello', '', 100, 1, 0, 0)",
            [],
        )
        .unwrap();
    }
    let cache = Cache::open_at(&db.0).unwrap();
    assert_golden("cache/testdata/schema-upgraded.txt", &dump(&cache.conn));
    assert_eq!(cache.count_messages(1, "INBOX"), 1);
}

/// One row or more in every table, chosen so each version gate has
/// something to drop or rewrite.
const FIXTURE: &str = r#"
INSERT INTO folders VALUES (1, 'INBOX', 'Inbox', 0, 0, 0);
INSERT INTO msgid_case VALUES ('a@x', 'A@X');
INSERT INTO refs_repair VALUES (1, 'INBOX', 5, 0);
INSERT INTO label_fill VALUES (1, 'INBOX', 5, 1);
INSERT INTO messages (account_id, folder_path, uid, from_name, from_addr, subject, date, ts, unread, starred, has_attachment, preview) VALUES
    (1, 'INBOX', 1, '"Ann \"A\""', 'ann@x', '=?utf-8?Q?Caf=C3=A9?=', '', 100, 1, 0, 0, '--b2=_boundary'),
    (1, 'INBOX', 2, 'Bob', 'bob@x', 'say \"hi\"', '', 200, 1, 0, 0, 'Version: 1'),
    (1, 'INBOX', 3, 'Cy', 'cy@x', 'Plain', '', 300, 1, 0, 0, '( https://track.example'),
    (1, 'INBOX', 4, 'Di', 'di@x', 'Plain', '', 400, 1, 0, 0, 'Encrypted message'),
    (1, 'INBOX', 5, 'Ed', 'ed@x', 'Plain', '', 500, 1, 0, 0, 'Hello there');
INSERT INTO local_tags VALUES (1, 'a@x', 'later');
INSERT INTO bodies VALUES (1, 'INBOX', 1, 'b1'), (1, 'INBOX', 2, 'b2'), (1, 'INBOX', 3, 'b3'), (1, 'INBOX', 5, 'b5');
INSERT INTO sender_checks (account_id, folder_path, uid, trust, summary, findings, pgp) VALUES
    (1, 'INBOX', 1, 'pass', '', '', ''),
    (1, 'INBOX', 2, 'suspicious', '', '', ''),
    (1, 'INBOX', 3, 'pass', '', '', '{"signed":true}'),
    (1, 'INBOX', 5, 'fail', '', '', '');
INSERT INTO attachments VALUES (1, 'INBOX', 1, 0, 'a.pdf', x'00'), (1, 'INBOX', 2, 0, 'b.pdf', x'00'), (1, 'INBOX', 3, 0, 'c.pdf', x'00');
INSERT INTO addresses VALUES ('ann@x', '"Ann \"A\""', 2), ('bob@x', 'Bob', 1);
INSERT INTO outbox (account_id, from_addr, rcpts, recipients, subject, raw, queued_at) VALUES (1, 'a@x', 'b@x', 'b@x', 'Hi', x'00', 1);
INSERT INTO attachments_checked VALUES (1, 'INBOX', 1), (1, 'INBOX', 2), (1, 'INBOX', 3), (1, 'INBOX', 4);
INSERT INTO attachment_meta (account_id, folder_path, uid, idx, name) VALUES (1, 'INBOX', 5, 0, 'e.png');
INSERT INTO attachment_scan VALUES (1, 'INBOX', 4), (1, 'INBOX', 5);
INSERT INTO account_owner VALUES (1, 'a@x');
"#;

/// What is left of the fixture after `migrate`, table by table.
fn contents(conn: &Connection) -> String {
    let mut out = String::new();
    for table in [
        "folders",
        "msgid_case",
        "refs_repair",
        "label_fill",
        "messages",
        "local_tags",
        "bodies",
        "sender_checks",
        "attachments",
        "addresses",
        "outbox",
        "attachments_checked",
        "attachment_meta",
        "attachment_scan",
        "account_owner",
    ] {
        let n: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        out += &format!("{table}: {n}\n");
    }
    let mut lines = |sql: &str| {
        let mut stmt = conn.prepare(sql).unwrap();
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                (0..n)
                    .map(|i| r.get::<_, rusqlite::types::Value>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap();
        for row in rows {
            out += &format!("  {:?}\n", row.unwrap());
        }
    };
    lines("SELECT uid, from_name, subject, preview FROM messages ORDER BY uid");
    lines("SELECT uid FROM bodies ORDER BY uid");
    lines("SELECT uid, trust FROM sender_checks ORDER BY uid");
    lines("SELECT uid FROM attachments_checked ORDER BY uid");
    lines("SELECT uid, idx, name, size, bucket FROM attachment_meta ORDER BY uid, idx");
    lines("SELECT uid FROM attachment_scan ORDER BY uid");
    lines("SELECT email, name FROM addresses ORDER BY email");
    out
}

/// Each version gate of `migrate`, run over the same rows written at every
/// version that one of them distinguishes.
#[test]
fn every_version_gate_drops_and_rewrites_what_it_should() {
    let mut report = String::new();
    for version in [0, 5, 6, 12, 13, 14, 15, 18, 19, 20, 21] {
        let db = TempDb::new(&format!("v{version}"));
        {
            let conn = Connection::open(&db.0).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute_batch(FIXTURE).unwrap();
            conn.execute_batch(&format!("PRAGMA user_version = {version};"))
                .unwrap();
        }
        let cache = Cache::open_at(&db.0).unwrap();
        report += &format!("== from version {version}\n{}", contents(&cache.conn));
        let after: i64 = cache
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, super::SCHEMA_VERSION);
    }
    assert_golden("cache/testdata/migrations.txt", &report);
}

/// A sender check as `sender_checks` stores it: the trust tag, the findings
/// one per line, and the JSON of the OpenPGP verdict, the unsubscribe
/// handles, the invitation, the sensitivity label and the read receipt
/// asked for.
#[test]
fn a_sender_check_row_as_stored() {
    use crate::models::{
        Invite, InvitePerson, PgpSignature, PgpStatus, PgpTrust, ReplyRoute, SenderCheck,
        SenderTrust, Unsubscribe,
    };
    let cache = Cache::in_memory().unwrap();
    let check = SenderCheck {
        trust: SenderTrust::Suspicious,
        summary: "Looks off".into(),
        findings: vec!["one".into(), "two".into()],
        pgp: Some(PgpStatus {
            encrypted: true,
            decrypted: true,
            signature: PgpSignature::Good {
                signer: "Ada <ada@x>".into(),
                key_id: "ABCD".into(),
                trust: PgpTrust::Marginal,
            },
            ..Default::default()
        }),
        unsubscribe: Some(Unsubscribe {
            one_click: Some("https://list.example/u".into()),
            mailto: Some("mailto:leave@list.example".into()),
            reply: Some(ReplyRoute {
                to: "owner@list.example".into(),
                subject: "UNSUBSCRIBE".into(),
            }),
            web: Some("https://list.example/page".into()),
            list_id: "news.list.example".into(),
            in_headers: true,
        }),
        invite: Some(Box::new(Invite {
            method: "REQUEST".into(),
            uid: "uid-1".into(),
            sequence: 2,
            summary: "Standup".into(),
            location: "Room 1".into(),
            start: 1_700_000_000,
            end: 1_700_003_600,
            all_day: false,
            recurrence_id: "20240101T090000Z".into(),
            repeats: "FREQ=DAILY".into(),
            organizer: Some(InvitePerson {
                name: "Ada".into(),
                email: "ada@x".into(),
                status: "ACCEPTED".into(),
                rsvp: false,
                resource: false,
            }),
            attendees: vec![InvitePerson {
                name: String::new(),
                email: "room@x".into(),
                status: "NEEDS-ACTION".into(),
                rsvp: true,
                resource: true,
            }],
            cancelled: false,
            ics: "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".into(),
        })),
        label: Some(crate::mail::sensitivity::SensitivityLabel {
            name: "Confidential".into(),
            id: "id-1".into(),
            color: String::new(),
            tooltip: String::new(),
        }),
        receipt: Some(crate::mail::receipt::ReceiptRequest {
            to: "ada@x".into(),
            matches_return_path: true,
            original_recipient: String::new(),
            message_id: String::new(),
        }),
        read: None,
    };
    cache.save_sender_check(1, "INBOX", 9, &check);
    let row: Vec<String> = cache
        .conn
        .query_row(
            "SELECT trust, summary, findings, pgp, unsubscribe, invite, label, receipt FROM sender_checks",
            [],
            |r| (0..8).map(|i| r.get::<_, String>(i)).collect(),
        )
        .unwrap();
    assert_golden(
        "cache/testdata/sender-check-row.txt",
        &(row.join("\n---\n") + "\n"),
    );
    // Served back with the OpenPGP verdict left out: it is a marker only.
    let back = cache.load_sender_check(1, "INBOX", 9).unwrap();
    assert_eq!(back.pgp, None);
    assert_eq!(back.unsubscribe, check.unsubscribe);
    assert_eq!(back.invite, check.invite);
    assert_eq!(back.label, check.label);
    assert_eq!(back.receipt, check.receipt);
    assert_eq!(back.findings, check.findings);
}
