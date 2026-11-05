//! The listening side: the socket, the thread its connections are served
//! on, and the line back to the app's main loop.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use rmcp::ServiceExt;

use crate::app::AppMsg;
use crate::config::AssistantPolicy;

/// The app's main loop, as the server reaches it.
pub(crate) type AppLink = relm4::Sender<AppMsg>;

/// What Settings allows now: the app writes it as it changes, each tool call
/// reads it, so a switch turned off holds from the next call on.
pub(crate) type SharedPolicy = Arc<RwLock<AssistantPolicy>>;

/// An account, as the tools know it, before what is allowed in it is known.
#[derive(Clone, Debug)]
pub(super) struct KnownAccount {
    /// Its id in the cache: its place in the accounts file, from 1.
    pub(super) id: u32,
    pub(super) address: String,
    pub(super) name: String,
    /// It sends mail: every account but On This Computer.
    pub(super) sends: bool,
}

/// What every connection shares.
pub(super) struct Shared {
    pub(super) policy: SharedPolicy,
    pub(super) memo: super::content::RawMemo,
    /// The enabled accounts, read at each call: one added or removed while
    /// an assistant is connected shows at once.
    pub(super) accounts: fn() -> Vec<KnownAccount>,
}

/// The accounts as the app has them in its accounts file.
fn accounts_on_file() -> Vec<KnownAccount> {
    crate::config::load()
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .filter(|(_, a)| a.enabled)
        .map(|(i, a)| KnownAccount {
            id: i as u32 + 1,
            sends: !a.is_local(),
            name: a.label.clone().unwrap_or(a.name.clone()),
            address: a.email,
        })
        .collect()
}

/// A message on screen, where the cache has it: (account, folder path, UID).
pub(crate) type Shown = (u32, String, u32);

/// A conversation on screen: the message the user is on, and every message
/// shown with it (that one too; only it, for a lone message).
#[derive(Debug, Clone)]
pub(crate) struct OpenThread {
    pub(crate) selected: Shown,
    pub(crate) members: Vec<Shown>,
}

/// What the user has open in Hylki now, by place only: what an assistant may
/// know of each is decided from the cache, as for any other message.
#[derive(Debug, Clone, Default)]
pub(crate) struct OpenNow {
    /// The main window's: the message open in the reader, or selected in
    /// the list with the reading pane off.
    pub(crate) main: Option<OpenThread>,
    /// Each message window's.
    pub(crate) windows: Vec<OpenThread>,
    /// The message window the user was in last, of the main window and the
    /// message windows (its place in `windows`); `None` for the main one.
    pub(crate) focused: Option<usize>,
}

/// What the server asks of, or tells, the app: the work only the main
/// loop can do, or that the user should see. Each request carries where
/// the answer goes; a server that stopped waiting has dropped its end.
#[derive(Debug)]
pub(crate) enum AppCall {
    /// The message at `uid` in `path` as the server keeps it, or why not.
    /// Raw bytes rather than a body: what renders a body also decrypts, and
    /// an assistant must never set that off.
    FetchRaw {
        account_id: u32,
        path: String,
        uid: u32,
        done: tokio::sync::oneshot::Sender<Result<Vec<u8>, String>>,
    },
    /// Have an account's worker search its server, which answers on the
    /// channel `ask` carries.
    AskServer {
        account_id: u32,
        ask: crate::worker::ServerSearch,
    },
    /// Open a composer on `draft`, and say which (its id) and how; or why
    /// not.
    Draft {
        draft: super::draft::Draft,
        done: tokio::sync::oneshot::Sender<Result<(u32, String), String>>,
    },
    /// Read the composers open now (only the one `id` names, if given),
    /// each as it stands, with what is written in it when `text`. Each
    /// answers on a line of its own; none, when no composer has the id.
    ReadDrafts {
        id: Option<u32>,
        text: bool,
        done: tokio::sync::oneshot::Sender<
            Vec<tokio::sync::oneshot::Receiver<super::draft::OpenDraft>>,
        >,
    },
    /// Put `edit` before the user in composer `id`, to accept or reject.
    EditDraft {
        id: u32,
        edit: super::draft::DraftEdit,
        done: tokio::sync::oneshot::Sender<super::draft::EditAnswer>,
    },
    /// The conversations the user has open: the main window's and each
    /// message window's, and which of them the user was in last.
    ReadOpen {
        done: tokio::sync::oneshot::Sender<OpenNow>,
    },
    /// Change `targets`, the way the same action in the app does, undoable.
    Organize {
        change: super::organize::Change,
        targets: Vec<super::organize::Target>,
        done: tokio::sync::oneshot::Sender<super::organize::Organized>,
    },
    /// A tool was called, for the record Settings shows.
    Activity(super::activity::Activity),
    /// Show the prepared messages that waited while the user was busy.
    ShowWaiting,
}

/// The socket listened at, for [`remove_socket`]: the app quits without
/// dropping its model, so the [`Server`] it holds is never dropped then.
static LISTENING: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Take away the socket, as the app quits: a relay started after finds
/// nothing there, and says Hylki is not running.
pub(crate) fn remove_socket() {
    if let Some(path) = LISTENING.lock().ok().and_then(|mut l| l.take()) {
        let _ = std::fs::remove_file(path);
    }
}

/// The listening server, for as long as it is held: dropping it stops the
/// thread, closes every connection and takes the socket away.
pub(crate) struct Server {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    path: PathBuf,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").field("path", &self.path).finish()
    }
}

impl Server {
    /// Listen at [`super::socket_path`], serving each connection on a
    /// thread of the server's own; `app` hears of what needs the app, and
    /// `policy` says what is allowed.
    pub(crate) fn start(app: AppLink, policy: SharedPolicy) -> Result<Server, String> {
        let path = super::socket_path().ok_or("there is no XDG_RUNTIME_DIR to listen in")?;
        let listener = bind(&path)?;
        if let Ok(mut l) = LISTENING.lock() {
            *l = Some(path.clone());
        }
        let shared = Arc::new(Shared {
            policy,
            memo: Default::default(),
            accounts: accounts_on_file,
        });
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("hylki-mcp".into())
            .spawn(move || listen(listener, stopped, app, shared))
            .map_err(|e| e.to_string())?;
        tracing::info!("mcp: listening at {}", path.display());
        Ok(Server {
            stop: Some(stop),
            thread: Some(thread),
            path,
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // The window waits on this, so `listen` hears the stop however long
        // a tool's query takes: see the runtime it builds.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        remove_socket();
        tracing::info!("mcp: stopped listening");
    }
}

/// The socket, in a directory only the user can enter, itself only the
/// user's. A socket left by a Hylki that did not quit cleanly is taken
/// over; one another Hylki still answers on is not.
fn bind(path: &Path) -> Result<std::os::unix::net::UnixListener, String> {
    use std::os::unix::fs::PermissionsExt;
    let at = |e: std::io::Error| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(at)?;
        // The Flatpak's own directory is the sandbox's to set; ours is
        // made private.
        if std::env::var_os("FLATPAK_ID").is_none() {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(at)?;
        }
    }
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(format!(
                "{}: another Hylki is already listening",
                path.display()
            ));
        }
        std::fs::remove_file(path).map_err(at)?;
    }
    let listener = std::os::unix::net::UnixListener::bind(path).map_err(at)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(at)?;
    listener.set_nonblocking(true).map_err(at)?;
    Ok(listener)
}

/// Accept until told to stop. Each connection is a server of its own, on
/// this thread's runtime.
fn listen(
    listener: std::os::unix::net::UnixListener,
    mut stopped: tokio::sync::oneshot::Receiver<()>,
    app: AppLink,
    shared: Arc<Shared>,
) {
    // More than one worker: tools query SQLite without yielding, and on one
    // thread a slow search would hold up the stop the window is waiting for.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("hylki-mcp-tool")
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("mcp: {e}");
            return;
        }
    };
    runtime.block_on(async {
        let listener = match tokio::net::UnixListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("mcp: {e}");
                return;
            }
        };
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) if is_own(&stream) => {
                        tokio::spawn(serve(stream, app.clone(), shared.clone()));
                    }
                    Ok(_) => tracing::warn!("mcp: refused a connection from another user"),
                    Err(e) => tracing::warn!("mcp: accept: {e}"),
                },
            }
        }
    });
    // Connections still open end here, with their tasks; a contact lookup
    // still asking a directory finishes on its own.
    runtime.shutdown_background();
}

/// Whether the process at the other end runs as this user. The socket's
/// directory already keeps others out; this holds should it not.
fn is_own(stream: &tokio::net::UnixStream) -> bool {
    // SAFETY: getuid has no preconditions and cannot fail.
    let me = unsafe { libc::getuid() };
    stream.peer_cred().is_ok_and(|c| c.uid() == me)
}

/// One assistant's connection, until it closes. The relay makes one for each
/// tool call, so a connection says nothing about an assistant being around.
async fn serve(stream: tokio::net::UnixStream, app: AppLink, shared: Arc<Shared>) {
    let cache = match crate::cache::Cache::open_reader() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("mcp: cannot read the mail cache: {e}");
            return;
        }
    };
    match super::tools::Mail::new(cache, app, shared)
        .serve(stream)
        .await
    {
        Ok(service) => {
            if let Err(e) = service.waiting().await {
                tracing::warn!("mcp: {e}");
            }
        }
        Err(e) => tracing::warn!("mcp: a session could not start: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::bind;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_stale_socket_is_taken_over_and_a_live_one_is_not() {
        let dir = std::env::temp_dir().join(format!("hylki-mcp-test-{}", std::process::id()));
        let path = dir.join("sub").join("mcp.sock");
        let first = bind(&path).expect("binds where nothing is");
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        // Something answers there: it is left alone.
        let e = bind(&path).unwrap_err();
        assert!(e.contains("already listening"), "{e}");
        // Its owner went without taking the socket away.
        drop(first);
        assert!(path.exists());
        let _again = bind(&path).expect("a socket nobody answers on is taken over");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
