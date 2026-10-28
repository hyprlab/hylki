//! The cache's tables and the version numbers that decide what an
//! upgrade drops. Both are the on-disk format: see `open.rs` for how an
//! older database is brought up to date.

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS folders (
    account_id INTEGER NOT NULL,
    path       TEXT    NOT NULL,
    name       TEXT    NOT NULL,
    kind       INTEGER NOT NULL,
    unread     INTEGER NOT NULL,
    ord        INTEGER NOT NULL,
    PRIMARY KEY (account_id, path)
);
CREATE TABLE IF NOT EXISTS msgid_case (
    lower TEXT NOT NULL PRIMARY KEY,
    exact TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS refs_repair (
    account_id  INTEGER NOT NULL,
    folder_path TEXT    NOT NULL,
    next_uid    INTEGER NOT NULL,
    done        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, folder_path)
);
CREATE TABLE IF NOT EXISTS label_fill (
    account_id  INTEGER NOT NULL,
    folder_path TEXT    NOT NULL,
    next_uid    INTEGER NOT NULL,
    done        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (account_id, folder_path)
);
CREATE TABLE IF NOT EXISTS messages (
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
    preview        TEXT    NOT NULL DEFAULT '',
    reply_to       TEXT    NOT NULL DEFAULT '',
    keywords       TEXT    NOT NULL DEFAULT '',
    importance     INTEGER NOT NULL DEFAULT 0,
    due            INTEGER NOT NULL DEFAULT 0,
    gmail_labels   TEXT    NOT NULL DEFAULT '',
    answered       INTEGER NOT NULL DEFAULT 0,
    forwarded      INTEGER NOT NULL DEFAULT 0,
    bcc            TEXT    NOT NULL DEFAULT '',
    PRIMARY KEY (account_id, folder_path, uid)
);
CREATE INDEX IF NOT EXISTS messages_by_message_id ON messages (message_id);
-- Starred mail across an account's folders (the unified Starred view, #482,
-- and smart folders asking for stars): a few rows out of a whole mailbox.
CREATE INDEX IF NOT EXISTS messages_starred ON messages (account_id, ts) WHERE starred = 1;
-- A folder's newest messages, read first so a list shows before the rest of
-- a large folder is in.
CREATE INDEX IF NOT EXISTS messages_by_folder_ts ON messages (account_id, folder_path, ts);
CREATE TABLE IF NOT EXISTS local_tags (
    account_id  INTEGER NOT NULL,
    message_id  TEXT    NOT NULL,
    keyword     TEXT    NOT NULL,
    PRIMARY KEY (account_id, message_id, keyword)
);
CREATE TABLE IF NOT EXISTS bodies (
    account_id  INTEGER NOT NULL,
    folder_path TEXT NOT NULL,
    uid         INTEGER NOT NULL,
    body        TEXT NOT NULL,
    PRIMARY KEY (account_id, folder_path, uid)
);
CREATE TABLE IF NOT EXISTS sender_checks (
    account_id  INTEGER NOT NULL,
    folder_path TEXT NOT NULL,
    uid         INTEGER NOT NULL,
    trust       TEXT NOT NULL,
    summary     TEXT NOT NULL,
    findings    TEXT NOT NULL,
    pgp         TEXT NOT NULL DEFAULT '',
    unsubscribe TEXT NOT NULL DEFAULT '',
    invite      TEXT NOT NULL DEFAULT '',
    label       TEXT NOT NULL DEFAULT '',
    receipt     TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (account_id, folder_path, uid)
);
CREATE TABLE IF NOT EXISTS attachments (
    account_id  INTEGER NOT NULL,
    folder_path TEXT NOT NULL,
    uid         INTEGER NOT NULL,
    idx         INTEGER NOT NULL,
    name        TEXT NOT NULL,
    data        BLOB NOT NULL,
    PRIMARY KEY (account_id, folder_path, uid, idx)
);
CREATE TABLE IF NOT EXISTS addresses (
    email TEXT PRIMARY KEY,
    name  TEXT NOT NULL DEFAULT '',
    count INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS outbox (
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
    last_error  TEXT    NOT NULL DEFAULT '',
    send_at     INTEGER,
    forwards    TEXT    NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS attachments_checked (
    account_id  INTEGER NOT NULL,
    folder_path TEXT NOT NULL,
    uid         INTEGER NOT NULL,
    PRIMARY KEY (account_id, folder_path, uid)
);
CREATE TABLE IF NOT EXISTS attachment_meta (
    account_id  INTEGER NOT NULL,
    folder_path TEXT NOT NULL,
    uid         INTEGER NOT NULL,
    idx         INTEGER NOT NULL,
    name        TEXT    NOT NULL,
    mime        TEXT    NOT NULL DEFAULT '',
    size        INTEGER NOT NULL DEFAULT 0,
    section     TEXT    NOT NULL DEFAULT '',
    ext         TEXT    NOT NULL DEFAULT '',
    bucket      INTEGER NOT NULL DEFAULT 6,
    keywords    TEXT    NOT NULL DEFAULT '',
    PRIMARY KEY (account_id, folder_path, uid, idx)
);
CREATE TABLE IF NOT EXISTS attachment_scan (
    account_id  INTEGER NOT NULL,
    folder_path TEXT NOT NULL,
    uid         INTEGER NOT NULL,
    PRIMARY KEY (account_id, folder_path, uid)
);
CREATE INDEX IF NOT EXISTS attachment_meta_by_folder
    ON attachment_meta (account_id, folder_path);
CREATE TABLE IF NOT EXISTS account_owner (
    account_id INTEGER PRIMARY KEY,
    email      TEXT    NOT NULL
);
-- The words of each body on disk, for search (#509), by the body's rowid.
-- No copy of the text is kept; a body that goes takes its row with it.
CREATE VIRTUAL TABLE IF NOT EXISTS body_text USING fts5(
    text, content='', contentless_delete=1, tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER IF NOT EXISTS body_text_gone AFTER DELETE ON bodies BEGIN
    DELETE FROM body_text WHERE rowid = old.rowid;
END;
";

/// Every table whose rows belong to one account, by `account_id`. An account's
/// id is its place in the account list, so `Cache::claim_accounts` moves
/// these rows when the places change.
pub(super) const ACCOUNT_TABLES: [&str; 11] = [
    "folders",
    "refs_repair",
    "messages",
    "local_tags",
    "bodies",
    "sender_checks",
    "attachments",
    "outbox",
    "attachments_checked",
    "attachment_meta",
    "attachment_scan",
];

/// The tables holding what was downloaded for one message, which are
/// dropped together.
pub(super) const DOWNLOADED_TABLES: [&str; 3] = ["bodies", "attachments", "attachments_checked"];

/// Bump when the table layout changes; older rows are dropped on open.
/// v8: bodies are re-rendered with clickable links, so cached bodies (which
/// `LoadBody` serves without re-fetching) must be dropped and rebuilt on open.
/// v9: re-decode subjects cached as raw RFC 2047 encoded-words by builds that
/// aborted on over-long encoded-words (e.g. Mailchimp newsletters).
/// v10: bodies are re-rendered with `cid:` image references resolved to `data:`
/// URIs, so cached bodies showing a broken image must be dropped and rebuilt.
/// v11: sender authentication is computed from the raw message at the same
/// moment the body is rendered, so dropping `bodies` re-fetches both together
/// and every cached message gains a verdict.
/// v12: plain-text bodies no longer bake `padding:16px` into the cached
/// document — the reader injects the default inset at render time — so cached
/// bodies carrying the old baked padding must be dropped and re-rendered.
/// v13: builds before the declared-attachment exemption dropped small files a
/// web-Gmail sender attached, and stored one blob copy per Gmail label. The
/// checked table remembers those messages as done, so the wrong lists would
/// survive forever — drop both tables and let attachments re-fetch on demand.
/// v14: `attachment_meta`/`attachment_scan` for the gallery — purely additive,
/// so nothing cached is dropped for it (see [`RENDER_VERSION`]).
/// v15: the first attachment scan wrote off a whole batch when one message's
/// BODYSTRUCTURE would not parse, so up to 200 messages were recorded as
/// holding nothing when several held files. Messages marked as scanned with
/// nothing to show are re-queued once, to be asked about again by the scan that
/// now isolates the one message at fault.
/// v19: the message is also read for a meeting invitation (its
/// `text/calendar` part, #223), which rides with the sender check — and a
/// calendar part now counts as an attachment, which the stored attachment
/// lists and "already scanned" marks predate.
/// v18: the unsubscribe scan now reads the body as well as the headers
/// (a footer link, a mailto link, "reply with UNSUBSCRIBE"), so a verdict
/// stored by an earlier build knows only what its headers said.
/// v17: the sender check now carries the message's unsubscribe handles
/// (List-Unsubscribe, RFC 8058), read from the raw headers at the fetch. A
/// check stored by an earlier build knows nothing of them, and a cached
/// body is served without ever re-fetching — so the derived tables are
/// dropped once and every message read again gains its Unsubscribe banner.
/// v20: sender names and subjects were stored with the backslash escapes of
/// the server's quoted string, and a name with the header's own quotes
/// around it (#312). The rows are cleaned in place, see
/// [`Cache::clean_quoted_names`].
/// v21: a Reply-To on another domain no longer marks a verified sender
/// suspicious unless replies go to a mailbox host (A1 mail from a1.at
/// answered on email.a1.net was flagged). Suspicious verdicts stored earlier,
/// and the bodies served with them, are dropped once so those messages are
/// checked again when next opened; every other cached body stays.
pub(super) const SCHEMA_VERSION: i64 = 21;

/// The newest version whose change altered how bodies are *rendered* or how
/// senders are checked. Opening a database older than this drops `bodies` and
/// `sender_checks` so they rebuild; a later purely-additive bump must not,
/// or every such release would cost users a full re-fetch of everything they
/// had read. Raise this only when the rendering itself changes.
pub(super) const RENDER_VERSION: i64 = 19;

/// (Adding a *new* table needs no bump: `SCHEMA` runs `CREATE TABLE IF NOT
/// EXISTS` on every open, so an existing cache gains it in place. A bump is for
/// changing or invalidating what is already stored — it costs users a re-render
/// or a re-sync.)
///
/// The first version whose table *layout* matches the current `SCHEMA`. At or
/// above this, an upgrade only needs to drop the derived caches, not the
/// expensive message index (which would force a whole-mailbox re-sync).
pub(super) const LAYOUT_VERSION: i64 = 6;
