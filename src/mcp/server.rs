//! The listening side: the socket, the thread its connections are served
//! on, and the line back to the app's main loop.

use std::path::{Path, PathBuf};

use rmcp::ServiceExt;

use crate::app::AppMsg;

/// The app's main loop, as the server reaches it.
pub(crate) type AppLink = relm4::Sender<AppMsg>;

/// What the server asks of, or tells, the app: the work only the main
/// loop can do, or that the user should see. Each request carries where
/// the answer goes; a server that stopped waiting has dropped its end.
#[derive(Debug)]
pub(crate) enum AppCall {
    /// An assistant connected (`true`) or left (`false`).
    Connected(bool),
    /// Download the body of the message at `uid` in `path`, as the reader
    /// would, and hand it over (HTML), or `None` when it cannot be had.
    FetchBody {
        account_id: u32,
        path: String,
        uid: u32,
        done: tokio::sync::oneshot::Sender<Option<String>>,
    },
    /// Open a composer on `draft`, or say why not.
    Draft {
        draft: super::draft::Draft,
        done: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
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
    /// thread of the server's own; `app` hears of what needs the app.
    pub(crate) fn start(app: AppLink) -> Result<Server, String> {
        let path = super::socket_path().ok_or("there is no XDG_RUNTIME_DIR to listen in")?;
        let listener = bind(&path)?;
        if let Ok(mut l) = LISTENING.lock() {
            *l = Some(path.clone());
        }
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("hylki-mcp".into())
            .spawn(move || listen(listener, stopped, app))
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
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
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
                        tokio::spawn(serve(stream, app.clone()));
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

/// One assistant's session, until it closes.
async fn serve(stream: tokio::net::UnixStream, app: AppLink) {
    let cache = match crate::cache::Cache::open_reader() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("mcp: cannot read the mail cache: {e}");
            return;
        }
    };
    let _ = app.send(AppMsg::Assistant(AppCall::Connected(true)));
    match super::tools::Mail::new(cache, app.clone())
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
    let _ = app.send(AppMsg::Assistant(AppCall::Connected(false)));
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
