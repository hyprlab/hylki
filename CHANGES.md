# Changes

## Startup diagnostics

- Added info-level logs with timings along the startup path (boot, GOA reconcile, cache prime, worker spawn, password/keyring resolve, cache open, account and folders reaching the UI, sidebar rebuild) to investigate a blank window with no accounts or folders.
- `config::load()` now logs unreadable or empty `accounts.toml` instead of failing silently.

## Blank window when the keyring does not answer

- Log showed the IMAP worker stuck on the keyring read, before it emitted the account, so the sidebar stayed empty. `ImapWorker::start` now emits the account and its cached folders first, and resolves the passwords after.
- Keyring reads log their duration and outcome.
