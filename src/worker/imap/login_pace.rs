//! How often each IMAP account has been signed in to lately, so the folder
//! watchers keep their logins within what the provider tolerates.
//!
//! Every folder a user opens gets an IDLE watcher, and each watcher is a
//! login of its own. Browsing five folders on Yahoo made ten logins in eight
//! minutes, and Yahoo answered the next with "[LIMIT] LOGIN Rate limit hit."
//! (#676). A refused login can land on the main session too, and then a move
//! fails. The watchers are a freshness bonus over the unread sweep, so they
//! are the ones that wait: they dial only while the account has had fewer
//! than [`WATCHER_BUDGET`] logins in the last [`WINDOW`], and not at all for
//! [`COOL_OFF`] after the server says it is limiting. The main session is
//! never held back: it carries what the user asked for.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::AccountConfig;

/// How far back the logins are counted.
const WINDOW: Duration = Duration::from_secs(600);

/// A watcher dials only while fewer logins than this went to the account in
/// the last [`WINDOW`]. The main session, the Inbox watcher and a reconnect
/// fit under it with room to spare.
const WATCHER_BUDGET: usize = 5;

/// How long watchers stay off an account once its server refused a login
/// for being too frequent or too many.
const COOL_OFF: Duration = Duration::from_secs(900);

/// One account's recent logins and any refusal still being waited out.
#[derive(Default)]
struct Ledger {
    logins: VecDeque<Instant>,
    refused_until: Option<Instant>,
}

impl Ledger {
    fn forget_before(&mut self, now: Instant) {
        while self
            .logins
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= WINDOW)
        {
            self.logins.pop_front();
        }
    }

    /// How long a watcher has to wait before it may dial; `None` when it may
    /// dial now.
    fn watcher_wait(&mut self, now: Instant) -> Option<Duration> {
        if let Some(until) = self.refused_until {
            if until > now {
                return Some(until - now);
            }
            self.refused_until = None;
        }
        self.forget_before(now);
        if self.logins.len() < WATCHER_BUDGET {
            return None;
        }
        // The login that has to age out before the count is under budget.
        let oldest_to_go = self.logins[self.logins.len() - WATCHER_BUDGET];
        Some(
            (oldest_to_go + WINDOW)
                .saturating_duration_since(now)
                .max(Duration::from_secs(1)),
        )
    }
}

static LEDGERS: Mutex<Option<HashMap<String, Ledger>>> = Mutex::new(None);

fn key(account: &AccountConfig) -> String {
    format!(
        "{}\n{}",
        account.imap_host.to_ascii_lowercase(),
        account.username.to_ascii_lowercase()
    )
}

fn with_ledger<T>(account: &AccountConfig, f: impl FnOnce(&mut Ledger) -> T) -> T {
    let mut guard = LEDGERS.lock().unwrap_or_else(|e| e.into_inner());
    f(guard
        .get_or_insert_with(HashMap::new)
        .entry(key(account))
        .or_default())
}

/// Count a login about to be made to `account`, by any connection.
pub(super) fn note_login(account: &AccountConfig) {
    let now = Instant::now();
    with_ledger(account, |l| {
        l.forget_before(now);
        l.logins.push_back(now);
    });
}

/// Whether a refused login says the server is limiting how often, or how
/// many: RFC 5530's LIMIT and UNAVAILABLE codes, and the wording providers
/// use without them.
fn is_limit_refusal(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    ["[limit]", "[unavailable]", "rate limit", "too many"]
        .iter()
        .any(|w| e.contains(w))
}

/// A login to `account` failed with `error`: if the server is limiting
/// logins, keep the watchers off it for a while.
pub(super) fn note_refusal(account: &AccountConfig, error: &str) {
    if !is_limit_refusal(error) {
        return;
    }
    let now = Instant::now();
    tracing::info!(
        "{}: the server is limiting logins, folder watchers wait {} minutes",
        account.imap_host,
        COOL_OFF.as_secs() / 60
    );
    with_ledger(account, |l| l.refused_until = Some(now + COOL_OFF));
}

/// How long a folder watcher has to wait before it may log in to `account`;
/// `None` when it may go now.
pub(super) fn watcher_wait(account: &AccountConfig) -> Option<Duration> {
    with_ledger(account, |l| l.watcher_wait(Instant::now()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger_with(now: Instant, ages: &[u64]) -> Ledger {
        let mut l = Ledger::default();
        for age in ages {
            l.logins.push_back(now - Duration::from_secs(*age));
        }
        l
    }

    #[test]
    fn a_watcher_dials_while_the_account_is_under_budget() {
        let now = Instant::now() + WINDOW * 2;
        let mut l = ledger_with(now, &[300, 200, 100, 10]);
        assert_eq!(l.watcher_wait(now), None);
    }

    #[test]
    fn a_watcher_waits_for_the_oldest_login_to_age_out() {
        let now = Instant::now() + WINDOW * 2;
        // Yahoo's pattern: five logins in the last eight minutes.
        let mut l = ledger_with(now, &[480, 400, 300, 120, 30]);
        assert_eq!(l.watcher_wait(now), Some(Duration::from_secs(120)));
    }

    #[test]
    fn logins_older_than_the_window_no_longer_count() {
        let now = Instant::now() + WINDOW * 2;
        let mut l = ledger_with(now, &[900, 700, 610, 100, 50]);
        assert_eq!(l.watcher_wait(now), None);
        assert_eq!(l.logins.len(), 2);
    }

    #[test]
    fn a_refusal_keeps_watchers_off_for_the_cool_off() {
        let now = Instant::now() + WINDOW * 2;
        let mut l = ledger_with(now, &[30]);
        l.refused_until = Some(now + COOL_OFF);
        assert_eq!(l.watcher_wait(now), Some(COOL_OFF));
        assert_eq!(l.watcher_wait(now + COOL_OFF), None);
        assert_eq!(l.refused_until, None);
    }

    #[test]
    fn only_a_limit_refusal_starts_the_cool_off() {
        // What Yahoo sent on 2026-11-26.
        assert!(is_limit_refusal(
            "no response: code: None, info: Some(\"[LIMIT] LOGIN Rate limit hit.\")"
        ));
        assert!(is_limit_refusal("[UNAVAILABLE] Try again later"));
        assert!(is_limit_refusal("Too many simultaneous connections"));
        assert!(!is_limit_refusal(
            "no response: code: None, info: Some(\"[AUTHENTICATIONFAILED] Invalid credentials\")"
        ));
    }
}
