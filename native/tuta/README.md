# hylki-tuta

`hylki-tuta` is the helper program Hylki uses for Tuta accounts. Tuta offers
no IMAP, SMTP or JMAP: mail is end-to-end encrypted on the server, and only
Tuta's own client library can sign in and decrypt it. The helper links that
library, so Hylki can read and send Tuta mail without running a bridge.

Hylki starts the helper once per operation, writes one request to its stdin,
reads one reply from its stdout, and waits for it to exit. The helper does not
listen on a socket, keep a local cache, or speak any mail protocol. Decrypted
mail goes back to Hylki as headers, a body and attachments, which Hylki turns
into a MIME message for its usual reader.

## Sources

The `upstream` submodule is used for source only:

- [tutabridge](https://github.com/spartanz51/tutabridge) at
  `da12fd4a479d6885f6643f05690b78205e2adbec` supplies its `tutabridge-tuta`
  mail extensions and the SDK preparation script. Its IMAP/SMTP server is not
  linked or run.
- The [Tuta SDK](https://github.com/tutao/tutanota) is Tuta release
  `362.261020.0`, commit `f5e2d8628f17be558fe63130396ff625bbe69c77`, with
  twelve patches for optional encrypted values, address normalization, account
  KDFs, batched entity loads, blobs, interactive sessions, owner keys, AEAD
  reads and bucket keys. The exact series and hashes are in
  `upstream/sdk/EXPECTED`, and both the preparation script and this crate's
  build script verify the prepared source against it.

Both are GPL-3.0-or-later, and so is this helper. It runs as a separate
program from Hylki.

Tuta's API is undocumented and third-party clients are unsupported. A pinned
SDK can fall behind the minimum client version Tuta's servers accept; the
helper then fails with `client_outdated`, and the fix is to update the SDK pin,
never to report a different client version.

## Protocol

One UTF-8 JSON object goes to the helper on stdin, followed by EOF, and one
JSON object comes back on stdout. Passwords, TOTP codes and session tokens
never go in argv or the environment. The SDK prints progress to stdout, so the
helper parks fd 1 on `/dev/null` while SDK code runs; stdout carries exactly
one line. Requests are capped at 1 MiB and each operation at 90 seconds. Hylki
allows 100 seconds including startup, then kills and reaps the process.

Every request carries `version: 1` and an `op`:

| Operation | Parameters | Result |
| --- | --- | --- |
| `login` | `email`, `password`, optional `totp` | Resumable `session` |
| `sync` | `session`, optional `folder`, `limit`, `offset` | Folder tree, headers, and the selected folder's full id snapshot when `offset` is zero |
| `message` | `session`, compound `id` | Decrypted body, original headers, recipients and attachments |
| `mark_read` | `session`, `ids`, `is_read` | Per-id outcomes |
| `move` | `session`, `ids`, compound `destination` | Per-id outcomes |
| `send` | `session`, `sender`, `sender_name`, `to`, `cc`, `bcc`, `reply_tos`, `subject`, `body_html`, `attachments`, `delivery: "ordinary"`, `ordinary_confirmed` | Server ids of the sent mail and recipient counts |
| `revoke` | `session` | Empty object; closes the server-side session |

A reply is either `{"version":1,"result":{...}}` or
`{"version":1,"error":{"code":"..."}}`. Unknown operations, unknown fields and
other versions are rejected. Move results do not guess the moved message's new
id, since Tuta can move a mail into a different underlying list.

### Error codes

An error carries a fixed code, never SDK or server text:

- Sign-in: `login_failed`, `second_factor_required`, `second_factor_failed`,
  `invalid_totp`, `resume_failed`, `session_invalid`, `session_revoked`,
  `client_outdated`.
- Transport: `transport_failed`, `request_failed`, `timeout`.
- Content: `decryption_failed`, `unsupported_legacy_body`,
  `attachment_failed`, `body_too_large`.
- Requests: `invalid_request`, `unsupported_version`, `invalid_id`,
  `invalid_page`, `invalid_batch`.
- Sending: `invalid_recipient`, `sender_unknown`,
  `confidential_send_unsupported`, `ordinary_delivery_requires_consent`,
  `send_failed`, `send_uncertain`.
- `revoke_failed` when the server would not close a session.

### Send outcomes

Sending is ordinary delivery only: the mail is encrypted in transit but not end
to end, and the caller must confirm that with `ordinary_confirmed`. The helper
never falls back to SMTP and never quietly drops encryption.

A send reports `send_uncertain` only when its `SendDraftService` request left
the process and no definite answer came back (lost in flight, or a 5xx). The
mail may have gone out and must not be resent. What happened is read from the
wire, not from the SDK's error, which reports a 412, a failed serialization and
an unreadable 200 reply alike. A request that never left, or that the server
refused with a 4xx, is `send_failed` (`session_revoked` for 401 or 440,
`client_outdated` for 474), and its draft is discarded through Trash. An
accepted send whose reply was lost succeeds with null ids.

## Building and testing

With a Rust toolchain and a C compiler, from the repository root:

```sh
git submodule update --init --recursive
python3 native/tuta/upstream/scripts/sdk-prepare.py
cd native/tuta
cargo build --locked --offline
cargo test --locked --offline
```

The first build needs the crates in Cargo's cache; drop `--offline` to fetch
them. The Flatpak build has no network, so `cargo-sources.json` lists every
crate from `Cargo.lock` for flatpak-builder. Regenerate it with
flatpak-builder-tools' `flatpak-cargo-generator.py` after any `Cargo.lock`
change, or the offline build fails. See [BUILDING.md](../../docs/BUILDING.md)
for packaging.

## Debugging

With `HYLKI_TUTA_DEBUG=1` in its environment, the helper writes diagnostics to
stderr, each line prefixed `hylki-tuta:`: every HTTP request as method, service
path and status (no host, query, body or ids), and the Debug form of every SDK
error with the stage it broke in (sign-in, recipient resolution, blob upload,
draft create, draft load, attachment keys, send draft, draft cleanup).
Addresses, ids, tokens and key bytes are redacted, and no message content is
printed. Hylki discards the helper's stderr, so run the helper by hand to see
it.

## Known limits

- Two-factor sign-in supports TOTP only; U2F and WebAuthn keys are not
  supported.
- The helper cannot encrypt for other Tuta addresses. Those recipients are
  never sent ordinary mail, and a message only to Tuta addresses fails with
  `confidential_send_unsupported`.
- Stars are not stored on the server.
- New mail is found by polling; the helper does not listen to Tuta's event
  bus.
- Sessions are not refreshed. Each operation resumes the stored session, and
  a revoked or expired one means signing in again.
