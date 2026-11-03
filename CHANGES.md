# Changes

## Startup diagnostics

- Added info-level logs with timings along the startup path (boot, GOA reconcile, cache prime, worker spawn, password/keyring resolve, cache open, account and folders reaching the UI, sidebar rebuild) to investigate a blank window with no accounts or folders.
- `config::load()` now logs unreadable or empty `accounts.toml` instead of failing silently.

## Blank window when the keyring does not answer

- Log showed the IMAP worker stuck on the keyring read, before it emitted the account, so the sidebar stayed empty. `ImapWorker::start` now emits the account and its cached folders first, and resolves the passwords after.
- Keyring reads log their duration and outcome.

## Dialog when the keyring does not give the password

- New `WorkerEvent::KeyringFailed` / `AppMsg::KeyringReadFailed`. The IMAP worker reads the account (and separate SMTP) password through `read_keyring_password`, which reports a keyring error at once, and a keyring still silent after 10 s (it keeps waiting).
- The UI shows an `AdwAlertDialog` with the account, the error, a hint about unlocking the keyring and a Retry button (reconnects all accounts). One dialog per account at a time.
- POP3 and JMAP workers still use the silent path.
