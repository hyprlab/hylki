//! Every secret Hylki keeps, in the system keyring (Secret Service), and
//! the one thread all keyring calls run on.

use super::accounts::AccountConfig;

/// Service name used for keyring entries; password items are keyed by email.
pub(super) const KEYRING_SERVICE: &str = "co.hyprlab.Hylki";

// Every keyring call runs on one thread, in the order it was made. A Secret
// Service provider may wait on its user before it answers (a security key
// that wants a tap, #466), and nothing bounds that wait, so the main thread
// must never make the call itself: it queues it here with [`keyring_later`]
// or [`keyring_then`]. Other threads still call the functions below and wait
// for their turn. One thread keeps the order: a password saved and then read
// back, or read and then replaced, is never overtaken.

type KeyringJob = Box<dyn FnOnce() + Send>;

/// Jobs queued with [`keyring_later`] and not yet done.
static KEYRING_QUEUED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

thread_local! {
    static ON_KEYRING_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn keyring_thread() -> Option<&'static std::sync::mpsc::Sender<KeyringJob>> {
    static THREAD: std::sync::OnceLock<Option<std::sync::mpsc::Sender<KeyringJob>>> =
        std::sync::OnceLock::new();
    THREAD
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<KeyringJob>();
            std::thread::Builder::new()
                .name("hylki-keyring".into())
                .spawn(move || {
                    ON_KEYRING_THREAD.with(|on| on.set(true));
                    for job in rx {
                        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                            tracing::error!("a keyring call panicked");
                        }
                    }
                })
                .map_err(|e| tracing::error!("could not start the keyring thread: {e}"))
                .ok()
                .map(|_| tx)
        })
        .as_ref()
}

/// Queue `job` for the keyring thread and return at once.
pub(crate) fn keyring_later(job: impl FnOnce() + Send + 'static) {
    use std::sync::atomic::Ordering;
    // Counted down even when the job panics.
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            KEYRING_QUEUED.fetch_sub(1, Ordering::SeqCst);
        }
    }
    KEYRING_QUEUED.fetch_add(1, Ordering::SeqCst);
    let job: KeyringJob = Box::new(move || {
        let _done = Done;
        job();
    });
    match keyring_thread() {
        Some(thread) => {
            if let Err(std::sync::mpsc::SendError(job)) = thread.send(job) {
                std::thread::spawn(job);
            }
        }
        None => {
            std::thread::spawn(job);
        }
    }
}

/// Wait, up to `limit`, for the jobs queued with [`keyring_later`] to be
/// done; whether they were. Called on the way out: a password saved just
/// before quitting would otherwise go with the process.
pub(crate) fn keyring_drain(limit: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    while KEYRING_QUEUED.load(std::sync::atomic::Ordering::SeqCst) > 0 {
        if started.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    true
}

/// Run `job` on the keyring thread and hand its answer to `done` on the main
/// loop. Called from the main thread.
pub(crate) fn keyring_then<T: Send + 'static>(
    job: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    let (tx, rx) = futures::channel::oneshot::channel();
    keyring_later(move || {
        let _ = tx.send(job());
    });
    gtk::glib::spawn_future_local(async move {
        if let Ok(answer) = rx.await {
            done(answer);
        }
    });
}

/// Run `f` on the keyring thread and wait for it.
pub(super) fn on_keyring_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    if ON_KEYRING_THREAD.with(|on| on.get()) {
        return f();
    }
    if gtk::glib::MainContext::default().is_owner() {
        tracing::warn!(
            "keyring call on the main thread: the window waits until the keyring answers\n{}",
            std::backtrace::Backtrace::force_capture()
        );
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let job: KeyringJob = Box::new(move || {
        let _ = tx.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)));
    });
    match keyring_thread() {
        Some(thread) => {
            if let Err(std::sync::mpsc::SendError(job)) = thread.send(job) {
                job();
            }
        }
        None => job(),
    }
    match rx.recv() {
        Ok(Ok(answer)) => answer,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => unreachable!("the keyring job always answers"),
    }
}

fn keyring_entry(key: &str) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, key)
}

fn set_secret(key: &str, secret: &str) -> keyring::Result<()> {
    let (key, secret) = (key.to_owned(), secret.to_owned());
    on_keyring_thread(move || keyring_entry(&key)?.set_password(&secret))
}

fn delete_secret(key: &str) -> keyring::Result<()> {
    let key = key.to_owned();
    on_keyring_thread(move || keyring_entry(&key)?.delete_credential())
}

/// Keyring key for an account's separate SMTP password.
pub(super) fn smtp_key(email: &str) -> String {
    format!("smtp:{email}")
}

/// Keyring key for an alias's own SMTP password (#34). The alias address is
/// lowercased so lookups can't miss on letter case.
pub(super) fn alias_smtp_key(email: &str, alias_addr: &str) -> String {
    format!("smtp-alias:{email}:{}", alias_addr.to_lowercase())
}

/// Keyring key for a natively-added OAuth account's refresh token.
pub(super) fn oauth_key(email: &str) -> String {
    format!("oauth:{email}")
}

/// Keyring key for the mail cache's encryption key (`cache::at_rest`). No
/// address can be this, so it never meets an account's password.
const CACHE_KEY: &str = "cache-key";

/// Run `f` on the keyring thread and wait up to `limit` for it. The cache
/// key is read before the window exists, so a keyring that never answers
/// must not leave the app invisible: past `limit` the read counts as
/// failed, and the user is asked what to do.
fn on_keyring_thread_within<T: Send + 'static>(
    limit: std::time::Duration,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    keyring_later(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(limit) {
        Ok(answer) => answer,
        Err(_) => Err(format!(
            "the keyring did not answer within {} seconds",
            limit.as_secs()
        )),
    }
}

/// The mail cache's key, `Ok(None)` when there is none, `Err` when the
/// keyring is locked or did not answer within `limit`. Not looked for under
/// Vireo's or Veem's service: neither encrypted its cache.
pub(crate) fn read_cache_key(limit: std::time::Duration) -> Result<Option<String>, String> {
    on_keyring_thread_within(limit, || {
        match keyring_entry(CACHE_KEY).and_then(|e| e.get_password()) {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    })
}

pub(crate) fn store_cache_key(key: &str, limit: std::time::Duration) -> Result<(), String> {
    let key = key.to_owned();
    on_keyring_thread_within(limit, move || {
        set_secret(CACHE_KEY, &key).map_err(|e| e.to_string())
    })
}

pub(crate) fn store_oauth_refresh(email: &str, token: &str) -> keyring::Result<()> {
    set_secret(&oauth_key(email), token)
}

pub(crate) fn load_oauth_refresh(email: &str) -> Option<String> {
    load_key(&oauth_key(email))
}

pub(crate) fn store_password(email: &str, password: &str) -> keyring::Result<()> {
    set_secret(email, password)
}

/// Cloud attachments (#144): an account's app password, by `CloudAccount::key`.
/// The LDAP directories keep theirs through the same three functions, under
/// `ldap:<source uid>` (see `crate::contacts::directory`).
pub(crate) fn store_cloud_password(key: &str, password: &str) -> keyring::Result<()> {
    set_secret(key, password)
}

pub(crate) fn load_cloud_password(key: &str) -> Option<String> {
    load_key(key)
}

pub(crate) fn delete_cloud_password(key: &str) {
    let _ = delete_secret(key);
}

/// A keyring item in the shape Evolution Data Server keeps its own
/// passwords in, found by the source's UID, so EDS signs in to a source
/// Hylki set up without asking anyone, also while Hylki is not running
/// (#408).
fn eds_entry(uid: &str, label: &str) -> keyring::Result<keyring::Entry> {
    // Hylki's own attributes stay, as the keyring crate finds its items by
    // them; EDS finds the item by the source UID alone.
    let mut cred = keyring::secret_service::SsCredential::new_with_target(
        None,
        KEYRING_SERVICE,
        &format!("eds:{uid}"),
    )?;
    cred.attributes.extend([
        (
            "xdg:schema".to_string(),
            "org.gnome.Evolution.Data.Source".to_string(),
        ),
        ("e-source-uid".to_string(), uid.to_string()),
        (
            "eds-origin".to_string(),
            "evolution-data-server".to_string(),
        ),
    ]);
    cred.label = format!("Evolution Data Source \"{label}\"");
    Ok(keyring::Entry::new_with_credential(Box::new(cred)))
}

/// Store the password EDS signs in to the source `uid` with; `label` names
/// the source in the keyring's own app.
pub(crate) fn store_eds_password(uid: &str, label: &str, password: &str) -> keyring::Result<()> {
    let (uid, label, password) = (uid.to_owned(), label.to_owned(), password.to_owned());
    on_keyring_thread(move || eds_entry(&uid, &label)?.set_password(&password))
}

/// Whether the keyring holds EDS's password for the source `uid`.
pub(crate) fn has_eds_password(uid: &str) -> bool {
    let uid = uid.to_owned();
    on_keyring_thread(move || eds_entry(&uid, "").and_then(|e| e.get_password()).is_ok())
}

pub(crate) fn delete_eds_password(uid: &str) {
    let uid = uid.to_owned();
    let _ = on_keyring_thread(move || eds_entry(&uid, "")?.delete_credential());
}

pub(crate) fn load_password(email: &str) -> Option<String> {
    load_key(email)
}

pub(crate) fn store_smtp_password(email: &str, password: &str) -> keyring::Result<()> {
    set_secret(&smtp_key(email), password)
}

pub(crate) fn load_smtp_password(email: &str) -> Option<String> {
    load_key(&smtp_key(email))
}

pub(super) fn store_alias_smtp_password(
    email: &str,
    alias_addr: &str,
    password: &str,
) -> keyring::Result<()> {
    set_secret(&alias_smtp_key(email, alias_addr), password)
}

pub(crate) fn load_alias_smtp_password(email: &str, alias_addr: &str) -> Option<String> {
    load_key(&alias_smtp_key(email, alias_addr))
}

pub(crate) fn delete_alias_smtp_password(email: &str, alias_addr: &str) {
    delete_key(&alias_smtp_key(email, alias_addr));
}

/// Drop every keyring entry an account owns: its password(s), OAuth token, and
/// each alias's own SMTP password. Prefer this over bare [`delete_password`]
/// whenever the account's config is still at hand — the alias entries are keyed
/// by address, which only the config knows.
pub(crate) fn delete_account_secrets(account: &AccountConfig) {
    delete_password(&account.email);
    for alias in &account.aliases {
        delete_alias_smtp_password(&account.email, &alias.address());
    }
}

fn load_key(key: &str) -> Option<String> {
    read_key(key).unwrap_or_else(|e| {
        tracing::warn!("could not read keyring entry for {key}: {e}");
        None
    })
}

/// Read a keyring entry, telling one that is not there (`Ok(None)`) apart
/// from a keyring that did not answer (`Err`). At login the keyring can be
/// locked or still starting, and taking that for "no password" signed the
/// account in with an empty one until Hylki restarted (#375).
fn read_key(key: &str) -> Result<Option<String>, String> {
    let key = key.to_owned();
    on_keyring_thread(
        move || match keyring_entry(&key).and_then(|e| e.get_password()) {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(load_legacy_key(&key)),
            Err(e) => Err(e.to_string()),
        },
    )
}

/// [`load_password`], with a keyring that could not be read as an error.
pub(crate) fn read_password(email: &str) -> Result<Option<String>, String> {
    read_key(email)
}

/// [`load_smtp_password`], with a keyring that could not be read as an error.
pub(crate) fn read_smtp_password(email: &str) -> Result<Option<String>, String> {
    read_key(&smtp_key(email))
}

/// Fall back to an entry stored under an earlier name's service (Vireo,
/// then Veem — see `legacy::PREDECESSORS`), copying it to the current
/// service so accounts added there keep working. The old entry is left as
/// it is: the old app may still be installed and using it.
fn load_legacy_key(key: &str) -> Option<String> {
    for pred in crate::desktop::legacy::PREDECESSORS {
        let Ok(old) = keyring::Entry::new(pred.keyring_service, key) else {
            continue;
        };
        let Ok(password) = old.get_password() else {
            continue;
        };
        if password.is_empty() {
            continue;
        }
        match keyring_entry(key).and_then(|new| new.set_password(&password)) {
            Ok(()) => tracing::info!("keyring entry for {key} carried over from {}", pred.name),
            Err(e) => tracing::warn!("could not carry the keyring entry for {key} over: {e}"),
        }
        return Some(password);
    }
    None
}

pub(crate) fn delete_password(email: &str) {
    delete_key(email);
    // Also drop the account's separate SMTP password and OAuth token, if any.
    delete_key(&smtp_key(email));
    delete_key(&oauth_key(email));
}

fn delete_key(key: &str) {
    match delete_secret(key) {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(e) => tracing::warn!("could not delete keyring entry for {key}: {e}"),
    }
}
