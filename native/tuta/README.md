# hylki-tuta

`hylki-tuta` is the helper program Hylki uses for Tuta accounts. Tuta offers
no IMAP, SMTP or JMAP: mail is end-to-end encrypted on the server, and only
Tuta's own client library can sign in and decrypt it. The helper links that
library, so Hylki can read and send Tuta mail without running a bridge.

Hylki starts the helper once per operation, writes one request to its stdin,
reads one reply from its stdout, and waits for it to exit; only `watch`, which
holds Tuta's push channel open, streams one line per change instead. The
helper does not listen on a socket, keep a local cache, or speak any mail
protocol. Decrypted mail goes back to Hylki as headers, a body and
attachments, which Hylki turns into a MIME message for its usual reader.

## Sources

The `upstream` submodule is used for source only:

- [tutabridge](https://github.com/spartanz51/tutabridge) at
  `da12fd4a479d6885f6643f05690b78205e2adbec` supplies its `tutabridge-tuta`
  mail extensions and the SDK preparation script. Its IMAP/SMTP server is not
  linked or run.
- The [Tuta SDK](https://github.com/tutao/tutanota) is Tuta release
  `362.261020.0`, commit `f5e2d8628f17be558fe63130396ff625bbe69c77`, with
  tutabridge's twelve patches for optional encrypted values, address
  normalization, account KDFs, batched entity loads, blobs, interactive
  sessions, owner keys, AEAD reads and bucket keys (series and hashes in
  `upstream/sdk/EXPECTED`), plus Hylki's own patches in `sdk-patches/`:
  - `01-recipient-key-data.patch` exposes
    `LoggedInSdk::encrypt_bucket_key_for_internal_recipients`, which loads
    each Tuta recipient's current public key and encrypts a mail's bucket key
    for it through the SDK's own `AsymmetricCryptoFacade` (RSA, or TutaCrypt
    with the sender identity key), as the official client's
    `encryptBucketKeyForInternalRecipient` does. It adds no cryptography.
  - `02-service-response-session-key.patch` decrypts a service response
    whose type has no owner key of its own (a created folder, an updated
    draft) with the request's session key, as the official client's
    `ServiceExecutor.decryptResponse` does, instead of failing after the
    server already applied the request.
  - `03-entity-patch.patch` adds `CryptoEntityClient::patch_instance`, which
    PATCHes a `sys/PatchList` naming only the changed attributes, as the
    official client's `EntityRestClient.update` writes every update. The
    SDK's own whole-instance PUT would encrypt every value again, final ones
    such as a mail's subject included. It adds `PATCH` to the SDK's
    `HttpMethod` for it.

  `prepare.sh` runs tutabridge's `sdk-prepare.py`, which prepares and
  verifies the pinned SDK in `upstream/sdk/generated`, then writes a copy with
  Hylki's patches applied to `prepared/`, beside a copy of `tutabridge-tuta`.
  That crate's build script verifies the SDK it builds against, so
  `prepared/sdk/EXPECTED` lists both patch series (Hylki's as
  `hylki-<name>`) and the content digest of the patched tree; the build
  verifies exactly what it compiles. The crate builds only from `prepared/`.

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
one line. Requests are capped at 36 MiB (room for a 25 MiB `import` or a
send's attachments in base64) and each operation at 90 seconds. Hylki
allows 100 seconds including startup, then kills and reaps the process. The
one exception is `watch`, which streams (see [Watch](#watch)).

Every request carries `version: 1` and an `op`:

| Operation | Parameters | Result |
| --- | --- | --- |
| `login` | `email`, `password`, optional `totp` | Resumable `session` |
| `sync` | `session`, optional `folder`, `limit`, `offset` | Folder tree, labels, headers (each with its label ids and `message_id`), and the selected folder's full id snapshot when `offset` is zero |
| `message` | `session`, compound `id` | Decrypted body, original headers, recipients and attachments; its header carries `message_id` too |
| `mark_read` | `session`, `ids`, `is_read` | Per-id outcomes |
| `move` | `session`, `ids`, compound `destination` | Per-id outcomes with `new_id` and `source` |
| `send` | `session`, `sender`, `sender_name`, `to`, `cc`, `bcc`, `reply_tos`, `subject`, `body_html`, `attachments`, `delivery` (`"ordinary"` or `"secure"`), `ordinary_confirmed`, optional `in_reply_to`, `references`, `action` (see [Replies and forwards](#replies-and-forwards)) | Server ids of the sent mail, recipient counts, the delivery mode and each Tuta recipient's key; for a reply or forward, `parent_marked` |
| `revoke` | `session` | Empty object; closes the server-side session |
| `list_labels` | `session` | `labels`: `{id, name, color}` each |
| `create_folder` | `session`, `name`, optional compound `parent` | The new folder's `id` |
| `rename_folder` | `session`, compound `id`, optional `name`, optional `parent` (`""` for the top level); at least one of the two | Empty object |
| `delete_folder` | `session`, compound `id` | `outcome`: `deleted` or `trashed` |
| `create_label` | `session`, `name`, `color` (`#rrggbb`) | The new label's `id` |
| `update_label` | `session`, compound `id`, optional `name`, optional `color`; at least one | Empty object |
| `delete_label` | `session`, compound `id` | Empty object |
| `apply_labels` | `session`, `ids`, `add` and `remove` (label ids) | Per-id outcomes |
| `delete` | `session`, `ids`, compound `folder` (Trash, Spam or a folder inside them) | Per-id outcomes |
| `empty_folder` | `session`, compound `id` of Trash or Spam | `deleted`: how many mails it held |
| `save_draft` | `session`, optional compound `draft_id`, `sender`, `sender_name`, `to`, `cc`, `bcc`, `reply_tos`, `subject`, `body_html`, `confidential`, `attachments`, optional `in_reply_to`, `references`, `action` | The draft's `id` |
| `delete_draft` | `session`, compound `id` | Empty object |
| `import` | `session`, compound `folder`, `mime_base64` (the whole RFC 5322 message, standard base64), optional `unread` (default `false`) | The new mail's `id`, or null when the server had not listed it yet |
| `watch` | `session` | A stream of event lines until stdin closes |

A reply is either `{"version":1,"result":{...}}` or
`{"version":1,"error":{"code":"..."}}`. Unknown operations, unknown fields and
other versions are rejected.

Per-id outcomes are `{"outcomes":[{"id","code"}]}` in request order, at most
50 ids: `code` is null when applied, the failure's code for the batch that
failed, and `not_attempted` after it. A move row adds `new_id` and `source`
from the server's own report of what it moved: a Tuta mail keeps its id when
it moves, so `new_id` equals `id`, and `source` is the folder it left -- what
the official client moves back to undo. Both are null for a mail the server
did not move (it was already in the destination).

### Error codes

An error carries a fixed code, never SDK or server text:

- Sign-in: `login_failed`, `second_factor_required`, `second_factor_failed`,
  `invalid_totp`, `resume_failed`, `session_invalid`, `session_revoked`,
  `client_outdated`.
- Transport: `transport_failed`, `request_failed`, `timeout`.
- Content: `decryption_failed`, `unsupported_legacy_body`,
  `attachment_failed`, `body_too_large`.
- Requests: `invalid_request`, `unsupported_version`, `invalid_id`,
  `invalid_page`, `invalid_batch`, `invalid_thread` (a malformed Message-ID
  in `in_reply_to` or `references`, more than 100 references, or an `action`
  with no Message-ID).
- Sending: `invalid_recipient`, `sender_unknown`, `recipient_not_found`,
  `secure_external_unsupported`, `ordinary_delivery_requires_consent`,
  `send_failed`, `send_uncertain`.
- `revoke_failed` when the server would not close a session.
- Folders and labels: `folder_not_found`, `folder_exists` (a sibling has the
  name, ignoring case), `not_custom_folder` (system folders are never
  renamed or deleted), `invalid_parent`, `invalid_name`, `operation_still_active`
  (the server is still working on the folder), `folder_failed`,
  `label_not_found`, `label_exists`, `invalid_color`, `label_failed`,
  `limit_reached` (the plan allows no more).
- Deleting: `not_trash_or_spam`, `delete_failed`.
- Drafts: `draft_not_found`, `not_a_draft` (the id names a received or sent
  mail), `draft_failed`, plus the sending codes for addresses and attachments.
- Importing: `import_invalid` (not a MIME message: no header at all),
  `import_too_large` (over 25 MiB decoded, over 256 MIME parts, or headers
  and body past the 8 MiB the official importer sends per mail),
  `import_refused` (the account's plan is not Legend or Unlimited, which the
  official client requires before it imports, or the server answered
  `import.disabled`), `import_failed`, plus `folder_not_found`.
- `watch_failed` when the event bus cannot be held.

### Folders, labels and drafts

Each operation is the official client's own: the same services, the same
keys, the same rules.

- A folder or label name is encrypted with a fresh session key, which is
  wrapped with the mail group's current key (`MailFolderService`,
  `ManageLabelService`). A rename updates the folder itself; a re-parent goes
  through `MailFolderService`.
- `delete_folder` behaves like the sidebar's delete: a folder inside Trash or
  Spam is deleted for good (with its mail); elsewhere, empty descendants are
  deleted, then the folder too if it and all of them are empty (`deleted`),
  and otherwise it moves under Trash with its mail (`trashed`).
- Labels are mail sets beside the folders. `sync` lists them under `labels`
  rather than `folders`, names each header's labels, and lists a label's mail
  when its id is the selected `folder`. A mail carries at most five labels.
- `delete` deletes mail for good, and only from Trash, Spam or a folder
  inside them, as the server allows; move it to Trash first. `empty_folder`
  asks the server to empty Trash or Spam in one request; it reports how many
  mails the folder listed just before.
- `save_draft` creates a draft in Drafts, or updates the given one wherever
  it is filed (a template folder works). An update replaces the draft's
  recipients, subject, body and its whole attachment set. `delete_draft`
  discards a draft through Trash, never from Sent. Both refuse any id that is
  not a draft, and both treat a draft that is already gone as done.

### Import

`import` files one message into a folder the way the official desktop
client imports a mail file (tutanota `src/app-kit/mimimi`), so the mail is
stored end-to-end encrypted like any other and reads the same in Tuta's own
app:

1. The message is converted by a port of the official importer's own code
   (`src/import/mime.rs`, on the same `mail-parser` 0.10.2): one HTML body
   (plain text converted, of a `multipart/alternative` the last part), every
   other part an attachment, the raw header block kept, the sender, the
   recipients, Message-ID, In-Reply-To and References taken over. Its tests
   are upstream's, including the MIME-tools messages it reads from the
   submodule. Where upstream would panic on hostile input, the port does not.
2. The account's plan is read from its customer info, as the official
   client's import button does: only Legend and Unlimited (TS
   `HighestTierPlans`) may import. Any other plan is `import_refused` before
   anything is written; the server answers such an import with a bare 400.
3. `ImportMailService` GET creates an `ImportFileMailState` for the target
   folder.
4. Each attachment goes to the blob store under a fresh file key; the post
   carries the mail's `ImportMailData`, encrypted with a fresh session key
   wrapped by the mail group key, to the blob server the write token names.
5. The state's imported-mail list is read until it names the mail (eight
   tries, 750 ms apart); its mail set entry gives the mail's id. A post that
   failed without a definite refusal is checked the same way, so a mail the
   server did take is never reported as failed.
6. The state is closed as the official client closes one: `Finished` with
   one successful mail, or `Canceled` with one failed mail when the import
   stopped. The API offers no way to delete a state, so each import leaves
   one behind, as every import in the official client does.

The mail is filed as received and read unless `unread` is true. The target
must be a folder: not a label, the IMPORTED set or the scheduled-mail set.
Unlike the official import dialog, the Inbox is allowed; it leaves the
Inbox out only so a bulk import cannot hold up incoming mail. `sync` leaves
the IMPORTED set out of the folder tree, as the official client does: its
mail is listed in the folders it was imported into.

### Watch

`watch` keeps Tuta's event bus websocket open, as the official client does,
and is the one operation that does not answer with exactly one line. Its
request must be a single line ending in a newline, and stdin must stay open:
the helper stops when stdin reaches EOF and then exits 0 without writing
anything more. Without the newline the request is `invalid_request`. No
operation timeout applies. Every line carries `version: 1`:

- `{"event":"ready"}` once the bus first connects.
- `{"event":"mail_changed","folder":"<id>"}` when mail in that folder arrived,
  left or changed (read state, labels). `folder` is `""` when the folder
  could not be told; resync everything then.
- `{"event":"folders_changed"}` when a folder or label was created, renamed,
  moved or deleted. It comes before the `mail_changed` lines of the same
  batch.
- `{"event":"reconnect"}` after the bus dropped and came back. The server
  replays what was missed, but a caller should resync all the same.
- `{"error":{"code":...}}` when the watch cannot go on (`session_revoked`,
  `client_outdated`, `transport_failed`, `watch_failed`); the helper exits
  after it. While the bus reconnects, the session is checked over REST, so
  an expired session ends the watch instead of retrying forever.

Lines name folders only, never a mail's content.

### Delivery

A send goes out in one of two modes, decided from its recipients before
anything is written on the server (no blob, no draft):

- **Confidential** (end-to-end encrypted) when every recipient is on Tuta: a
  Tuta domain, or an address the public-key service knows. This is the
  official client's own rule (`SendMailModel.isConfidential()`), so it needs
  no consent and applies to `delivery: "ordinary"` too. A fresh bucket key is
  encrypted for each recipient's current public key inside the SDK; the
  mail's and attachments' session keys are wrapped with the bucket key, the
  draft is marked confidential, and an all-TutaCrypt send also carries its
  `TutacryptSender` authentication status. A Tuta recipient without a key
  fails the whole send with `recipient_not_found`; it is never downgraded.
- **Ordinary** (encrypted in transit, not end to end) for everyone when any
  recipient is outside Tuta, only with `delivery: "ordinary"` and
  `ordinary_confirmed: true` (`ordinary_delivery_requires_consent`
  otherwise). `delivery: "secure"` with a recipient outside Tuta needs a
  password-protected secure-external delivery, which the helper does not
  offer: `secure_external_unsupported`.

If no recipient is outside Tuta but a key lookup failed, the mode cannot be
decided and the send fails with the lookup's code (`transport_failed` or
`send_failed`). The helper never falls back to SMTP and never quietly drops
encryption.

The result carries `confidential` (bool) and `recipients`: for a
confidential send one `{address, key_version, protocol, fingerprint}` per
distinct recipient, where `protocol` is `tuta_crypt` or `rsa` and
`fingerprint` is the hex SHA-256 over the public key it was encrypted with
(each of RSA, X25519 and Kyber component as the public-key service serves
it, length-prefixed as a big-endian u32); an empty list for an ordinary one.
Key bytes are never reported.

Keys are not verified. The official client only refuses a key that differs
from one the user verified by hand (`KeyVerificationMismatchError`); Hylki
has no verified contacts, so the current key from the public-key service is
used, as the official client does for every unverified contact. The
reported version and fingerprint are the hook for pinning keys later (trust
on first use).

Mail sent confidentially to the account's own address is read back by
`message` like any other: the recipient's copy decrypts through the SDK's
bucket-key path.

### Send outcomes

A send reports `send_uncertain` only when its `SendDraftService` request left
the process and no definite answer came back (lost in flight, or a 5xx). The
mail may have gone out and must not be resent. What happened is read from the
wire, not from the SDK's error, which reports a 412, a failed serialization and
an unreadable 200 reply alike. A request that never left, or that the server
refused with a 4xx, is `send_failed` (`session_revoked` for 401 or 440,
`client_outdated` for 474), and its draft is discarded through Trash. An
accepted send whose reply was lost succeeds with null ids; its delivery mode
is still reported.

### Replies and forwards

A `send` or a new `save_draft` takes its place in a conversation the way the
official client's does (`SendMailModel.initAsResponse`,
`MailFacade.createDraft`), from three optional fields:

- `in_reply_to`: the parent's Message-ID, with or without angle brackets.
- `references`: the References chain, whitespace-separated, oldest first.
  Tuta keeps no References of its own, so this only names the parent (its
  newest id) when `in_reply_to` is absent.
- `action`: `"reply"`, `"reply_all"` or `"forward"`. A parent without an
  action is answered as a reply; an action without a parent is
  `invalid_thread`.

Every header (`sync`'s and `message`'s) carries `message_id`: the mail's
own Message-ID as its conversation entry keeps it, without angle brackets,
or null for a draft or when the entry could not be read. Mail between Tuta
accounts has no header block, so this is the only Message-ID it has, and
the one a reply to it names. A `sync` page reads the entries one request
per conversation.

Every id must be printable ASCII without spaces or brackets, at most 998
bytes, and `references` holds at most 100; anything else refuses the whole
request with `invalid_thread` rather than being dropped. The draft is created
with `previousMessageId` set to the parent's bare id and `conversationType`
REPLY (a reply-all too: Tuta has no separate mark for it) or FORWARD, so the
server files it in the parent's conversation. Without the fields it is a new
conversation, as before. A draft's conversation is fixed when it is created;
an update checks the fields and has nowhere to put them.

After a send went out, the mail the server linked it to -- the previous
entry of the new mail's conversation entry, when it carries the given
Message-ID -- has its `replyType` moved on as the official client's
`updatePreviousMail` does: none becomes replied or forwarded, and one
becomes replied-and-forwarded after the other. Only that attribute is
written, encrypted with the parent's session key, as a PATCH. The result's
`parent_marked` says whether the parent ended up marked; it is false when
the account holds no such mail (outside mail never received here, or
deleted) or the write failed, and the send has gone out either way. For mail
the account sent to itself both copies share a Message-ID, and the server
links the sent copy.

## Building and testing

With a Rust toolchain and a C compiler, from the repository root:

```sh
git submodule update --init --recursive
native/tuta/prepare.sh
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
error with the stage it broke in (sign-in, recipient resolution, recipient
keys, blob upload, draft create, draft load, attachment keys, send draft,
draft cleanup, the parent's reply mark, folder and label services, deletes,
the event bus).
Addresses, ids, tokens and key bytes are redacted, and no message content is
printed. Hylki discards the helper's stderr, so run the helper by hand to see
it.

## Known limits

- Two-factor sign-in supports TOTP only; U2F and WebAuthn keys are not
  supported.
- Confidential mail goes to Tuta recipients only: password-protected
  delivery to other providers (secure external recipients) is not
  supported, and recipient keys are not verified (see Delivery).
- Stars are not stored on the server.
- Importing (`import`) needs a Legend or Unlimited plan, as in Tuta's own
  app; on any other plan it is refused before anything is written.
- `watch` reports which folders changed, not what changed in them; the caller
  syncs those folders. Missed batches are replayed only while one `watch`
  runs: a new `watch` starts from the present.
- Sessions are not refreshed. Each operation resumes the stored session, and
  a revoked or expired one means signing in again.
