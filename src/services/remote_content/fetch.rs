//! The threads that fetch remote content: a few of the app's own, so a slow
//! server never holds up GIO's thread pool, which the file dialogs and the
//! disk reads share.
//!
//! The reader's requests go first. What is fetched ahead takes at most two
//! threads, so the message being opened always has one free, and is dropped
//! when another message is selected. A URL is fetched once however many
//! wait for it.

use std::collections::VecDeque;
use std::io::Read;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use futures::channel::oneshot::Sender;

use super::store::{self, Entry};

pub(super) enum Priority {
    /// The reader is waiting for it.
    Now,
    /// For a message not opened yet.
    Ahead,
}

const THREADS: usize = 6;
const AHEAD_AT_ONCE: usize = 2;
/// Larger than any picture worth showing in mail; a response past it is
/// treated as a failed fetch rather than held in memory.
const MAX_BYTES: u64 = 50 * 1024 * 1024;

type Reply = Sender<Option<Arc<Entry>>>;

struct Job {
    url: String,
    ahead: bool,
    waiting: Vec<Reply>,
}

#[derive(Default)]
struct Queue {
    now: VecDeque<Job>,
    ahead: VecDeque<Job>,
    /// Fetches under way: their URL and who waits for them.
    running: Vec<(String, Vec<Reply>)>,
    ahead_running: usize,
    threads: usize,
}

fn pool() -> &'static (Mutex<Queue>, Condvar) {
    static POOL: OnceLock<(Mutex<Queue>, Condvar)> = OnceLock::new();
    POOL.get_or_init(Default::default)
}

static USER_AGENT: OnceLock<String> = OnceLock::new();

/// The reader's own user agent, so a server answers these fetches as it
/// answered WebKit's.
pub(super) fn set_user_agent(ua: String) {
    let _ = USER_AGENT.set(ua);
}

/// Queue `url`, or join the fetch already queued or under way. Asked for
/// `Now`, a fetch queued ahead moves to the front.
pub(super) fn submit(url: String, priority: Priority, reply: Option<Reply>) {
    let (lock, cv) = pool();
    let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
    let now = matches!(priority, Priority::Now);
    if let Some((_, waiting)) = q.running.iter_mut().find(|(u, _)| *u == url) {
        waiting.extend(reply);
        return;
    }
    if let Some(job) = q.now.iter_mut().find(|j| j.url == url) {
        job.waiting.extend(reply);
        return;
    }
    if let Some(at) = q.ahead.iter().position(|j| j.url == url) {
        if now {
            let mut job = q.ahead.remove(at).expect("position is in range");
            job.ahead = false;
            job.waiting.extend(reply);
            q.now.push_back(job);
            cv.notify_one();
        } else if let Some(r) = reply {
            q.ahead[at].waiting.push(r);
        }
        return;
    }
    let job = Job {
        url,
        ahead: !now,
        waiting: reply.into_iter().collect(),
    };
    if now {
        q.now.push_back(job);
    } else {
        q.ahead.push_back(job);
    }
    if q.threads < THREADS {
        q.threads += 1;
        let spawned = std::thread::Builder::new()
            .name("remote-content".into())
            .spawn(work);
        if spawned.is_err() {
            q.threads -= 1;
        }
    }
    cv.notify_one();
}

/// What is fetched ahead becomes `urls`: the queued fetches no longer wanted
/// are dropped, the new ones queued. Those under way finish.
pub(super) fn replace_ahead(urls: Vec<String>) {
    {
        let (lock, _) = pool();
        let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
        q.ahead
            .retain(|j| !j.waiting.is_empty() || urls.contains(&j.url));
    }
    for url in urls {
        submit(url, Priority::Ahead, None);
    }
}

fn work() {
    let (lock, cv) = pool();
    loop {
        let job = {
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            let job = loop {
                if let Some(j) = q.now.pop_front() {
                    break j;
                }
                if q.ahead_running < AHEAD_AT_ONCE {
                    if let Some(j) = q.ahead.pop_front() {
                        q.ahead_running += 1;
                        break j;
                    }
                }
                q = cv.wait(q).unwrap_or_else(|e| e.into_inner());
            };
            q.running.push((job.url.clone(), job.waiting));
            Job {
                waiting: Vec::new(),
                ..job
            }
        };

        // Ahead of the reader there is nothing to do for what is already
        // on disk, or when the network is not one to spend on it.
        let skip = job.ahead && (store::has(&job.url) || !crate::desktop::power::fetches_ahead());
        let mut entry = if skip {
            None
        } else {
            store::read(&job.url).or_else(|| {
                let fetched = get(&job.url)?;
                store::write(&job.url, &fetched);
                Some(fetched)
            })
        };

        let waiting = {
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            if job.ahead {
                q.ahead_running -= 1;
                // A thread is free for the next fetch ahead.
                cv.notify_one();
            }
            q.running
                .iter()
                .position(|(u, _)| *u == job.url)
                .map(|at| q.running.swap_remove(at).1)
                .unwrap_or_default()
        };
        // The reader came asking for a fetch that was skipped as already done.
        if skip && !waiting.is_empty() {
            entry = store::read(&job.url);
        }
        let entry = entry.map(Arc::new);
        for reply in waiting {
            let _ = reply.send(entry.clone());
        }
    }
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
    })
}

/// One GET, with no cookies and no Referer. Blocking.
fn get(url: &str) -> Option<Entry> {
    let mut request = agent().get(url).set("Accept", "*/*");
    if let Some(ua) = USER_AGENT.get().filter(|ua| !ua.is_empty()) {
        request = request.set("User-Agent", ua);
    }
    let response = match request.call() {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("remote content fetch failed: {e}");
            return None;
        }
    };
    let content_type = response.header("Content-Type").unwrap_or("").to_string();
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_BYTES).then_some(Entry {
        content_type,
        bytes,
    })
}
