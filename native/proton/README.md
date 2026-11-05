# Proton native helper

`postcard-proton` speaks the same stdio JSON envelope as the
[Tuta helper](../tuta/README.md): one request object on stdin, one reply line
on stdout, protocol version pinned at `1`. Every operation is implemented
over [`go-proton-api`](https://github.com/ProtonMail/go-proton-api) and
gopenpgp: SRP login with TOTP, folder/label sync with event-based
increments, decrypted RFC 5322 messages, read state, moves, end-to-end
encrypted sending and server-side logout. It does not listen on a socket,
speak IMAP/SMTP, or write anything to disk.

## Framing

- Request: one UTF-8 JSON object, `{"version": 1, "op": "<name>", ...}`,
  at most 32 MiB (attachments travel base64 inside `send`). Unknown fields
  are refused (`invalid_request`), as the Tuta helper's
  `deny_unknown_fields` does.
- Reply: exactly one line, always exit 0 —
  `{"version": 1, "result": {...}}` or
  `{"version": 1, "error": {"code": "<fixed>"}}`.
- A missing `version` is `invalid_request`; a present but different one is
  `unsupported_version` and nothing is contacted.
- Every operation runs under a 90-second budget (the parent allows 100).
- Passwords, codes and tokens travel on stdin only. Nothing is logged:
  go-proton-api's logrus output and resty's logger are both silenced, a
  library panic becomes `helper_failed`, and replies never carry server
  text — only the codes below.
- The client identifies as `x-pm-appversion: Other`, the API's third-party
  slot, never as an official client.

## Sessions and token rotation

`login` returns `result.session`, the resumable blob the caller stores in
its keyring and sends back as `session` on every other operation:

| field           | meaning                                                         |
|-----------------|-----------------------------------------------------------------|
| `login`         | the `email` login was called with (for the identity check)      |
| `user_id`, `uid`| Proton user id and auth session UID                             |
| `access_token`  | current access token                                            |
| `refresh_token` | current refresh token                                           |
| `password_mode` | `1` one password, `2` separate mailbox password                 |
| `key_pass`      | base64 mailbox passphrase, already bcrypt-salted for the primary user key — what unlocks the keys. Never the password itself. |

Treat the whole object as a secret (it is as powerful as the session).

**Rotation.** Access tokens expire; the library refreshes on a 401, and the
API rotates the refresh token on every refresh — the old one stops working.
So every session-bearing reply carries the session as it stands at the end
of the run:

- success: `result.session`;
- failure after the session was resumed: `error.session`
  (`{"code": "...", "session": {...}}`).

The caller MUST persist whichever is present before interpreting the
result, or a refresh that happened mid-run signs the account out. `revoke`
success is the only session-taking reply without one.

## Operations

### `login`

Request: `email`, `password`, optional `totp` (six digits, or `null`),
optional `mailbox_password` (two-password accounts only), optional
`hv_token` + `hv_type` (a solved human-verification challenge — both or
neither; printable ASCII without spaces, `hv_type` a method name).

Proves SRP, completes a TOTP challenge when the account has one, then
derives the key passphrase and unlocks the user and address keys once to
prove it. Result: `{"session": {...}}`.

A login that cannot finish (second factor or mailbox password missing or
wrong, verification demanded after auth) logs the half-made session out
again, best-effort.

#### Human verification (CAPTCHA)

Proton may refuse a sign-in from a new client or network with API code
9001. The helper then replies:

```json
{"version": 1, "error": {"code": "human_verification_required",
  "hv": {"token": "<challenge token>", "methods": ["captcha", "email"]}}}
```

`hv.token` and `hv.methods` are copied verbatim from the error's `Details`
(`HumanVerificationToken`, `HumanVerificationMethods`) and are opaque;
method names that are not `[A-Za-z0-9_-]` are dropped, and the error's text
and `WebUrl` are never passed on. If the details are missing or malformed
the code arrives without `hv` and cannot be solved from that reply.

The client flow:

1. Open
   `https://verify.proton.me/?methods=<methods joined with ",">&token=<hv.token>&embed=true`
   (URL-encode both values) in an embedded WebKit view.
2. Before the page loads, inject a user script that forwards the page's
   own messages to the native side:

   ```js
   window.addEventListener('message', (event) => {
       if (event.origin === 'https://verify.proton.me') {
           window.webkit.messageHandlers.protonVerify.postMessage(event.data);
       }
   });
   ```

   (register `protonVerify` with `WebKitUserContentManager`). In a
   top-level view `window.parent` is the page itself, so the page's
   `window.parent.postMessage(…, '*')` lands on this listener.
3. The verify app (`WebClients/applications/verify`, `broadcast.ts` +
   `packages/shared/lib/broadcast/broadcast.ts`) posts every message as a
   **JSON string**, not an object:

   ```json
   "{\"type\":\"HUMAN_VERIFICATION_SUCCESS\",\"payload\":{\"token\":\"<solved token>\",\"type\":\"captcha\"}}"
   ```

   Parse it; on `type == "HUMAN_VERIFICATION_SUCCESS"` take
   `payload.token` and `payload.type`. Other messages the page sends:
   `{"type":"RESIZE","payload":{"height":N}}`, `{"type":"LOADED"}`,
   `{"type":"CLOSE"}` (user dismissed it), `{"type":"ERROR","payload":{…}}`
   and `{"type":"NOTIFICATION","payload":{"type","text"}}`. Ignore
   anything else, and anything not from `https://verify.proton.me`.
4. Retry `login` with the same credentials plus
   `hv_token: payload.token, hv_type: payload.type` (and the `totp` if
   the account has one — a solved token should not be spent on a sign-in
   that then stops at `second_factor_required`). The helper sends them as
   `x-pm-human-verification-token` / `x-pm-human-verification-token-type`
   on `POST /auth/v4` and on the following `GET /core/v4/users`, as
   go-proton-api and Bridge do and as WebClients' `getVerificationHeaders`
   builds them.
5. `human_verification_failed` (API code 12087) means the token was
   rejected or already used: show the challenge again from a fresh
   `human_verification_required` reply. A second
   `human_verification_required` means the same.

`embed=true` matters: without it the page treats a top-level view as an
external browser and itself redeems the CAPTCHA against the challenge
token (`POST core/v4/verification/captcha/<token>`) before posting the
message. In that mode the retry must send the *original* `hv.token` with
`hv_type: "captcha"` instead (the flow Bridge's CLI uses after the user
solves the challenge in a browser), and the posted `payload.token` is
already spent. The embedded flow keeps the posted token as the one to
present.

### `sync`

Request: `session`, `limit` (1–1000, required), optional `folder` (label
id, default `"0"` = Inbox), optional `offset` (0–1 000 000, default 0),
optional `cursor` (a message id; the page after it — not combinable with a
non-zero `offset`), optional `since` (an `event_id` from an earlier sync).

Result:

| key           | value                                                                 |
|---------------|-----------------------------------------------------------------------|
| `folders`     | every folder row (below)                                              |
| `folder`      | the label id listed                                                   |
| `messages`    | headers (below), newest first                                         |
| `exists`      | total messages in the folder                                          |
| `offset`      | the request's offset                                                  |
| `all_ids`     | every message id in the folder when `offset` is 0 and neither `cursor` nor `since` is given and the folder holds ≤ 10 000 messages; otherwise `null` |
| `next_cursor` | last id of a full page (pass as `cursor`), or `null` at the end       |
| `event_id`    | event to pass as `since` next time — taken *before* listing, so anything that changes during the run is reported again, never skipped |
| `changes`     | only with `since`: `{"refresh", "more", "deleted", "updated"}`        |
| `session`     | see rotation                                                          |

Folder row: `{"id", "role", "label", "parent", "kind", "total", "unread"}`.
`kind` is `system`, `folder` (user folder, exclusive) or `label` (user
label, a tag). System rows, in order: Inbox `0` (`inbox`), Drafts `8`
(`drafts`), Sent `7` (`sent`), Starred `10` (`starred`), Archive `6`
(`archive`), Spam `4` (`junk`), Trash `3` (`trash`), All Mail `5`
(`other`). User folders and labels have role `other` and their parent's id
in `parent` (`""` at the top). Proton's other system labels (all-drafts,
all-sent, outbox, scheduled, snoozed) are views or transient states and are
not listed.

Header row (the Tuta shape plus what Proton metadata gives for free):
`id`, `subject`, `date_ms`, `sender` `{name, address}`, `recipient` (first
To, else Cc, else Bcc, or `null`), `is_unread`, `confidential` (arrived
end-to-end encrypted), `message_id` (original Message-ID without `<>`),
`label_ids`, `is_starred`, `is_draft`, `has_attachments`, `size`.

Listing pages by keyset (`EndID`), so mail arriving mid-walk cannot shift
a page and hide a message from the `all_ids` snapshot. `offset` pages are
offered for Tuta parity; `cursor` is the stable way to page.

`changes` replays the event log from `since` (up to 500 events per call;
`more: true` means call again from the returned `event_id`). `deleted` is
the ids gone since; `updated` is the current header row of every message
created or changed since, re-read rather than taken from a partial event.
`refresh: true` — the server demanded a resync, or no longer knows `since`
— means discard the deltas and sync from scratch; `event_id` is then the
current one.

### `message`

Request: `session`, `id`.

Fetches the message, unlocks the receiving address's key with `key_pass`,
downloads and decrypts every attachment (Proton-native messages store them
separately; PGP/MIME messages carry them inside the body), and builds the
RFC 5322 message the way Bridge serves it over IMAP.

Result: `{"header": <header row>, "mime": "<base64 RFC 5322 bytes>",
"attachments": [...], "session": {...}}`. Attachment rows are
`{"id", "name", "mime_type", "cid", "size", "disposition"}` — metadata only;
the bytes are already inside `mime`.

### `mark_read`

Request: `session`, `ids` (≤ 50), `is_read`.

Result: `{"outcomes": [{"id", "code"}...], "session"}` in input order, a
`null` code meaning applied. The batch goes out in one call; if the server
refuses it, each id is replayed alone (the mutation is idempotent) so every
id gets its own outcome — `request_failed` for an id the server refused. A
failure that says nothing about the ids (revoked session, outage, timeout)
fails the whole request; an outage during the replay marks that id and
leaves the rest `not_attempted`.

### `move`

Request: `session`, `ids` (≤ 50), `destination`.

Proton's move is labelling with an exclusive label, which drops the
previous folder. Allowed destinations: Inbox `0`, Archive `6`, Spam `4`,
Trash `3` and the user's own folders. Other system views (All Mail, Sent,
Drafts, Starred) and user labels are `invalid_destination`; an unknown id
is `folder_not_found`. Outcomes as for `mark_read`. Like Tuta, the reply
asserts no destination id: Proton keeps the message id across moves.

### `send`

Request (the Tuta shape): `session`, `sender`, `sender_name`, `to`, `cc`,
`bcc`, `reply_tos` (each a list of `{name, address}`), `subject`,
`body_html`, `delivery` (`"secure"` or `"ordinary"`), `ordinary_confirmed`,
`attachments` (`[{name, mime_type, cid, data}]`, `data` standard base64,
`cid` `null` or a Content-ID that makes the attachment inline), and
optionally `message_id` (with or without `<>`; one is generated on the
sender's domain when absent).

Everything that can refuse does so before anything is written server
side: request shape, consent (`ordinary` needs `ordinary_confirmed: true`),
the sender (must be one of the account's enabled sending addresses), and
every recipient's preferences. Those follow Proton Bridge's table
(`sendprefs.go` is a port): a Proton recipient is always end-to-end
encrypted to its key (internal package); an outside recipient with a key
the API knows (WKD / key transparency) or a contact card pins gets PGP/MIME
(or PGP/Inline when the card says so), signed; any other outside recipient
gets cleartext — signed MIME if the account signs external mail, plain
otherwise — over TLS. With `delivery: "secure"`, a recipient that would get
cleartext refuses the whole send with `confidential_send_unsupported`
(password-protected external mail is not implemented).

Then a draft is created (body encrypted to the sender's key, Reply-To and
the Message-ID set), attachments are uploaded encrypted, and the packages
go out in one submit. Bcc recipients get their packages like everyone else
but are never written into a header; the server delivers them and files the
Sent copy itself.

Result: `{"message_id", "sent_mail", "sent_date_ms", "internal_recipients",
"external_recipients", "session"}` — `message_id` is the sent mail's
Message-ID without `<>`, `sent_mail` its Proton id (the Sent copy).

Failure semantics:

- Anything before the submit is definite: nothing went out, a retry is
  safe, and any draft already created is deleted.
- A submit the server refused before executing (4xx on the first attempt)
  is definite too: `send_failed`, `invalid_recipient`, `message_too_large`,
  `transport_failed` (429), `session_revoked`, ... — and the draft is
  deleted.
- Anything else after the submit went out — lost connection, timeout, 5xx,
  or a refusal of a *retried* submit (the first attempt may have gone
  through) — may have delivered. The helper then reads the draft back: if
  it already reads as sent, that is a success; otherwise the reply is
  `send_uncertain`, the draft is kept, and the caller must not retry
  automatically.

### `revoke`

Request: `session`. Logs the session out server-side (`DELETE /auth/v4`).
Idempotent: a session the server already dropped is a success. Result:
`{}`.

## Error codes

| code | meaning |
|------|---------|
| `invalid_request` | malformed JSON, unknown op or field, missing field, oversize |
| `unsupported_version` | `version` present but not `1` — caller must stop |
| `helper_failed` | the helper itself failed (a library panic) |
| `login_failed` | wrong password or unknown user |
| `second_factor_required` | TOTP challenge, no code given |
| `invalid_totp` | TOTP code is not six digits |
| `unsupported_second_factor` | the account only offers non-TOTP factors |
| `second_factor_failed` | TOTP rejected by the server |
| `mailbox_password_required` | two-password account, no `mailbox_password` given |
| `mailbox_password_failed` | the mailbox password does not unlock the keys |
| `human_verification_required` | the API demands a CAPTCHA; `error.hv` carries the challenge (see `login`) |
| `human_verification_failed` | the presented `hv_token` was rejected or already used (API 12087) |
| `client_outdated` | the API refuses this app version |
| `session_invalid` | the stored session is missing fields or its `key_pass` no longer opens the keys (password changed elsewhere) — sign in again |
| `session_revoked` | the server ended the session (logout, refresh token spent) |
| `revoke_failed` | logout could not be confirmed |
| `transport_failed` | network failure, throttling (429) or server error (5xx) |
| `timeout` | the 90-second budget ran out |
| `request_failed` | the server refused a request for a reason with no code of its own |
| `invalid_id` | an id is empty or not URL-safe base64 (ids go into request paths) |
| `invalid_page` | `limit`/`offset` out of range, or `cursor` with an `offset` |
| `invalid_batch` | more than 50 ids |
| `folder_not_found` | the folder/destination id does not exist |
| `invalid_destination` | a view or label, not a place mail can be moved to |
| `decryption_failed` | the message body cannot be decrypted |
| `attachment_failed` | an attachment cannot be fetched, decrypted, decoded or uploaded |
| `invalid_recipient` | no recipients, a malformed address, or one the server refuses |
| `sender_unknown` | the sender is not one of the account's sending addresses |
| `ordinary_delivery_requires_consent` | `delivery: "ordinary"` without `ordinary_confirmed` |
| `confidential_send_unsupported` | `delivery: "secure"` but a recipient would get cleartext |
| `message_too_large` | the server refuses the message size |
| `send_failed` | definite send failure: nothing was sent, safe to retry |
| `send_uncertain` | the submit may have delivered: never retry automatically |

Per-id outcome codes (`mark_read`, `move`): `null`, `request_failed`,
`not_attempted`, or one of `transport_failed` / `timeout` /
`session_revoked` for the id an outage hit.

## Deviations from the Tuta contract

- Every session-bearing reply carries `session` (in `result` or `error`):
  Proton rotates refresh tokens, Tuta sessions do not change.
- `login` takes optional `mailbox_password`, `hv_token` and `hv_type`,
  and its `human_verification_required` error carries `hv`; the session
  carries `key_pass` instead of Tuta's `encrypted_passphrase_key`.
- `sync` adds `cursor`, `since`, `next_cursor`, `event_id` and `changes`;
  folder rows add `kind`, `total` and `unread`; header rows add
  `message_id`, `label_ids`, `is_starred`, `is_draft`, `has_attachments`
  and `size`. `offset` is optional (default 0); `all_ids` is `null` past
  10 000 messages.
- `message` returns the finished RFC 5322 message (`mime`, base64) instead
  of Tuta's `html` / `headers` / `to` / `cc` / `reply_to` pieces, and its
  attachment rows carry no `data` — the bytes are inside `mime`.
- `mark_read` / `move`: when the server refuses a batch, each id is
  replayed and reports its own outcome, rather than Tuta's "first id of the
  failed chunk carries the code, the rest `not_attempted`".
- `send` really encrypts: `delivery: "secure"` is honoured for Proton and
  PGP recipients instead of always refusing; `message_id` is an optional
  extra; failures after the submit are probed before `send_uncertain`.
- `revoke` closes the session server-side (Tuta's pinned SDK cannot).
- New codes: `mailbox_password_required`, `mailbox_password_failed`,
  `human_verification_required`, `human_verification_failed`,
  `invalid_destination`,
  `message_too_large`. `unsupported_operation`, `unsupported_u2f`,
  `unsupported_webauthn`, `resume_failed`, `incomplete_response`,
  `unsupported_legacy_body` and `body_too_large` are never produced.

## Verifying

```sh
cd native/proton
gofmt -l . | grep -v '^vendor/'   # must print nothing
go vet -mod=vendor ./...
go test -mod=vendor ./...
go build -mod=vendor -o postcard-proton .
echo '{"version":2,"op":"login"}' | ./postcard-proton
```

The tests are offline. They run go-proton-api's own in-process API server
(`github.com/ProtonMail/go-proton-api/server`) behind a reverse proxy the
tests control, and drive the helper through its real framing: login →
sync (pages, cursor, snapshot, events) → message decrypt → mark_read →
move → send to Proton recipients (whose copies are then decrypted from
their own accounts), to a cleartext outsider and to a PGP outsider (whose
package is opened with the recipient's key) → revoke, plus token rotation
and every failure mode the proxy can inject (refusals, lost submits, lost
replies, retried submits, timeouts, human verification, outdated client).
No test talks to the real Proton API.

## Dependencies

- `go-proton-api` is pinned at
  `v0.4.1-0.20260901104215-8a62023d3e38`, the revision Proton Mail Bridge
  ships. The last tagged release, `v0.4.0`, is from 2022 and pulls
  gopenpgp 2.4 and a 2022 go-crypto; the pinned revision is what the
  official client currently runs against the live API, with gopenpgp
  `v2.10.0-proton` and go-crypto `v1.4.1-proton`. Its `go.mod` replaces
  resty with Proton's fork, and a
  dependency's replace does not apply to us, so `go.mod` repeats it:
  `github.com/go-resty/resty/v2 => github.com/ProtonMail/resty/v2`.
- The pin needs Go ≥ 1.26.1 (`go.mod`'s `go` directive). The Flatpak's
  golang SDK extension ships 1.27; any CI pin must be at least 1.26.1.
- `vendor/` (~51 MiB) is committed for the offline Flatpak build. About
  two thirds of it is the test server's own dependencies (gin and its JSON
  and HTTP/3 stacks): `go mod vendor` includes test imports, though the
  binary links none of them. After any `go.mod` change run
  `go mod tidy && go mod vendor` and commit both.

## Flatpak packaging

The manifest builds the helper and installs it to `/app/bin`, so the
adapter finds it on `PATH` inside the sandbox:

- The `postcard-proton` module runs `go build` with `GOFLAGS=-mod=vendor`
  and `GOPROXY=off` from a `native/proton` dir source; the build sandbox has
  no network.
- `GOPATH` and `GOCACHE` point at the module's build directory, which is
  writable in the sandbox.
- The Go toolchain comes from the `org.freedesktop.Sdk.Extension.golang`
  SDK extension. The GNOME SDK itself ships no `go`.

## Limits

- Only TOTP is supported as a second factor; FIDO2/U2F is refused.
- The CAPTCHA itself is solved by the user in the client's WebKit view;
  the helper only relays the challenge and presents the solved token.
- Replies and forwards are sent as new conversations: no `ParentID`, so
  Proton does not mark the original as answered.
- Password-protected mail to outsiders ("encrypted outside") is not
  implemented; `secure` delivery refuses such recipients instead.
- `all_ids` is skipped past 10 000 messages to stay inside the budget; use
  `since` for incremental sync.
- Not yet validated against a live account.
