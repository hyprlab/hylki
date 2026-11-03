# Changes

## Startup diagnostics

- Added info-level logs with timings along the startup path (boot, GOA reconcile, cache prime, worker spawn, password/keyring resolve, cache open, account and folders reaching the UI, sidebar rebuild) to investigate a blank window with no accounts or folders.
- `config::load()` now logs unreadable or empty `accounts.toml` instead of failing silently.
