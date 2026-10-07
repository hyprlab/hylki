#!/usr/bin/env python3
"""Time real Hylki body loading against a synthetic TLS IMAP server.

Requires a graphical session, Python 3, openssl, bwrap and dbus-run-session.
Uses a private D-Bus session and synthetic XDG directories; no mail is sent.
Build both revisions with the same features, then run each with fresh output:

    python3 tools/reader-latency-probe.py --binary target/debug/hylki \
        --output /tmp/hylki-reader-after

Default workload: 121 folders, 70 ms per STATUS, an uncached message, an
inbox refresh at 1 s and a reader selection at 2 s. Output includes command
timings, application logs and time until the body is cached. Automatic body
prefetch can satisfy the reader, so this measures body availability, not paint.
Use --fetch-delay 8 to stall list loading, or --body-delay 20 to stall the
first prefetch. --repeat-select-delay 10 isolates repeated mailbox selection
(no inbox refresh; the reader selection is still at 2 s).

--usr-overlay is optional, for builds using an extracted dependency prefix.
Otherwise the binary uses the host libraries, including LD_LIBRARY_PATH.
"""

import argparse
import asyncio
from contextlib import closing
import json
import os
from pathlib import Path
import re
import signal
import sqlite3
import ssl
import subprocess
import time


RAW = (b"From: sender@example.test\r\nTo: test@example.test\r\n"
       b"Subject: Reader latency fixture\r\nMessage-ID: <latency@example.test>\r\n"
       b"Content-Type: text/plain; charset=UTF-8\r\n\r\n"
       b"A new message should open before folder counts finish.\r\n")
ENVELOPE = ('("Wed, 07 Oct 2026 08:00:00 +0000" "Reader latency fixture" '
            '(("Sender" NIL "sender" "example.test")) NIL NIL '
            '((NIL NIL "test" "example.test")) NIL NIL NIL "<latency@example.test>")')


def seed_cache(path):
    # Read the checked-in schema, never an installed account's database.
    source = (Path(__file__).resolve().parents[1] / "src/cache.rs").read_text()
    schema = re.search(r'const SCHEMA: &str = "(.*?)";', source, re.S).group(1)
    version = re.search(r"const SCHEMA_VERSION: i64 = (\d+);", source).group(1)
    with closing(sqlite3.connect(path)) as cache:
        cache.executescript(schema)
        cache.execute(f"PRAGMA user_version = {version}")
        cache.execute('INSERT INTO folders VALUES (1,"INBOX","Inbox",0,0,0)')
        cache.execute('''INSERT INTO messages
            (account_id,folder_path,uid,from_name,from_addr,subject,date,ts,
             unread,starred,has_attachment,message_id)
            VALUES (1,"INBOX",42,"Sender","sender@example.test",
                    "Reader latency fixture","Today",1791350000,0,0,0,
                    "latency@example.test")''')
        cache.execute('INSERT INTO refs_repair VALUES (1,"INBOX",43,1)')
        cache.commit()


async def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--usr-overlay", type=Path)
    parser.add_argument("--fetch-delay", type=float, default=0)
    parser.add_argument("--body-delay", type=float, default=0)
    parser.add_argument("--repeat-select-delay", type=float, default=0)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    root = args.output.resolve()
    root.mkdir(mode=0o700)  # Refuse to reuse a warmed cache.
    for directory in ("config/hylki", "data/hylki", "cache", "state", "empty-services"):
        (root / directory).mkdir(parents=True)
    cert, key = root / "cert.pem", root / "key.pem"
    subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                    "-days", "1", "-subj", "/CN=localhost", "-keyout", str(key),
                    "-out", str(cert)], check=True, stdout=subprocess.DEVNULL,
                   stderr=subprocess.DEVNULL)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(cert, key)
    events = []
    started = time.monotonic()
    connections = 0
    body_fetches = 0

    def event(kind, **fields):
        events.append(dict(event=kind, seconds=round(time.monotonic() - started, 3), **fields))

    async def peer(reader, writer):
        nonlocal connections, body_fetches
        connections += 1
        connection = connections
        selected = None

        async def send(data):
            writer.write(data.encode() if isinstance(data, str) else data)
            await writer.drain()

        try:
            await send("* OK fixture ready\r\n")
            while line := await reader.readline():
                tag, command = line.decode().strip().split(" ", 1)
                upper = command.upper()
                event("command", connection=connection, command=command)
                if upper.startswith("LOGIN"):
                    await send(f"{tag} OK login\r\n")
                elif upper.startswith("CAPABILITY"):
                    await send(f"* CAPABILITY IMAP4rev1\r\n{tag} OK capability\r\n")
                elif upper.startswith("LIST"):
                    names = ['* LIST (\\HasNoChildren) "/" "INBOX"\r\n']
                    names += [f'* LIST (\\HasNoChildren) "/" "Label {i:03d}"\r\n'
                              for i in range(120)]
                    await send("".join(names) + f"{tag} OK list\r\n")
                elif upper.startswith("STATUS"):
                    await asyncio.sleep(0.07)
                    mailbox = command[7:command.rfind(" (")]
                    count = 1 if mailbox.strip('"') == "INBOX" else 0
                    await send(f"* STATUS {mailbox} (UNSEEN 0 MESSAGES {count})\r\n"
                               f"{tag} OK status\r\n")
                elif upper.startswith(("SELECT", "EXAMINE")):
                    folder = command.split(" ", 1)[1].strip('"')
                    if upper.startswith("SELECT") and selected == folder:
                        await asyncio.sleep(args.repeat_select_delay)
                    selected = folder
                    count = 1 if folder == "INBOX" else 0
                    mode = "READ-WRITE" if upper.startswith("SELECT") else "READ-ONLY"
                    await send(f"* {count} EXISTS\r\n* FLAGS (\\Seen)\r\n"
                               f"* OK [UIDVALIDITY 1] valid\r\n* OK [UIDNEXT 43] next\r\n"
                               f"{tag} OK [{mode}] selected\r\n")
                elif upper.startswith(("UID SEARCH", "SEARCH")):
                    found = "42" if selected == "INBOX" and "UNSEEN" not in upper else ""
                    await send(f"* SEARCH {found}\r\n{tag} OK search\r\n")
                elif upper.startswith(("UID FETCH", "FETCH")):
                    if selected != "INBOX":
                        await send(f"{tag} NO message is not in this mailbox\r\n")
                    elif "BODY.PEEK[]" in upper:
                        body_fetches += 1
                        event("body_fetch", connection=connection)
                        if body_fetches == 1:
                            await asyncio.sleep(args.body_delay)
                        await send(f"* 1 FETCH (UID 42 BODY[] {{{len(RAW)}}}\r\n".encode()
                                   + RAW + f")\r\n{tag} OK body\r\n".encode())
                    elif "ENVELOPE" in upper:
                        await asyncio.sleep(args.fetch_delay)
                        await send('* 1 FETCH (UID 42 FLAGS (\\Seen) '
                                   'INTERNALDATE "07-Oct-2026 08:00:00 +0000" '
                                   f'ENVELOPE {ENVELOPE} BODYSTRUCTURE '
                                   '("TEXT" "PLAIN" ("CHARSET" "UTF-8") NIL NIL '
                                   f'"7BIT" 51 1))\r\n{tag} OK fetched\r\n')
                    else:
                        await send(f"* 1 FETCH (UID 42 FLAGS (\\Seen))\r\n{tag} OK fetched\r\n")
                elif upper.startswith("LOGOUT"):
                    await send(f"* BYE goodbye\r\n{tag} OK logout\r\n")
                    break
                else:
                    await send(f"{tag} OK done\r\n")
        except (ConnectionError, asyncio.CancelledError):
            pass
        finally:
            writer.close()

    server = await asyncio.start_server(peer, "127.0.0.1", 0, ssl=tls)
    port = server.sockets[0].getsockname()[1]
    (root / "config/hylki/accounts.toml").write_text(f'''[[accounts]]
name = "Reader latency fixture"
email = "test@example.test"
username = "test"
password = "test"
imap_host = "127.0.0.1"
imap_port = {port}
smtp_host = "127.0.0.1"
smtp_port = 1
enabled = true
push = false
[accounts.security]
imap_starttls = false
imap_accept_invalid_certs = true
''')
    (root / "config/hylki/privacy.toml").write_text(
        'fetch_interval_secs = 3\npush = false\npreview_lines = 0\nnotifications = false\n'
        'autostart = false\ntray = false\nrun_in_background = false\nthreading = false\n'
        'read_mark = "manual"\n')
    (root / "config/hylki/state.toml").write_text("wizard_completed = true\n")
    database = root / "data/hylki/cache.db"
    seed_cache(database)
    env = dict(os.environ, XDG_CONFIG_HOME=str(root / "config"),
               XDG_DATA_HOME=str(root / "data"), XDG_CACHE_HOME=str(root / "cache"),
               XDG_STATE_HOME=str(root / "state"), GSETTINGS_BACKEND="memory",
               GTK_A11Y="none", RUST_LOG="hylki=debug", GSK_RENDERER="cairo",
               HYLKI_SHOWCASE_SELECT="1:42@2", HYLKI_SHOWCASE_INBOX="1@1")
    env.pop("FLATPAK_ID", None)
    if args.repeat_select_delay:
        env.pop("HYLKI_SHOWCASE_INBOX")
    sandbox = ["bwrap", "--ro-bind", "/", "/"]
    if args.usr_overlay:
        overlay = args.usr_overlay.resolve(strict=True)
        sandbox += ["--overlay-src", "/usr", "--overlay-src", str(overlay),
                    "--ro-overlay", "/usr"]
    # Prevent portal activation in the test bus from disturbing the desktop.
    sandbox += ["--tmpfs", "/tmp", "--bind", str(root), str(root),
                "--ro-bind", str(root / "empty-services"),
                "/usr/share/dbus-1/services", "--dev", "/dev",
                "--proc", "/proc", "--", "dbus-run-session", str(binary), "--hidden"]
    loaded = None
    try:
        with (root / "app.log").open("w") as log:
            child = subprocess.Popen(sandbox, env=env, stdout=log, stderr=subprocess.STDOUT,
                                     start_new_session=True)
            try:
                for _ in range(450):
                    await asyncio.sleep(0.1)
                    with closing(sqlite3.connect(f"file:{database}?mode=ro", uri=True)) as cache:
                        row = cache.execute('SELECT body FROM bodies WHERE '
                                            'account_id=1 AND folder_path="INBOX" AND uid=42').fetchone()
                    if row:
                        assert "A new message should open before folder counts finish." in row[0]
                        loaded = round(time.monotonic() - started, 3)
                        event("body_cached", length=len(row[0]))
                        break
                    if child.poll() is not None:
                        break
            finally:
                if child.poll() is None:
                    # Terminate the fixture even on success; -15 is expected.
                    os.killpg(child.pid, signal.SIGTERM)
                    child.wait(timeout=10)
    finally:
        server.close()
        await server.wait_closed()
    result = dict(body_cached_seconds=loaded,
                  body_fetch_seconds=[e["seconds"] for e in events if e["event"] == "body_fetch"],
                  status_commands=sum(e.get("command", "").upper().startswith("STATUS") for e in events),
                  returncode=child.returncode)
    (root / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    (root / "events.json").write_text(json.dumps(events, indent=2) + "\n")
    print(json.dumps(result))
    if loaded is None:
        raise SystemExit("body was not cached; inspect app.log")


if __name__ == "__main__":
    asyncio.run(main())
