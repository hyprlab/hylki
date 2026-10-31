//! LanguageTool on this computer (#452): the server and a Java runtime to
//! run it, downloaded when the user asks, and started by the app when there
//! is something to check. It takes a few seconds to start and the better
//! part of a gigabyte of memory while it runs, so it is stopped again once
//! nothing has been checked for a while.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::install::{self, Archive, Kind, Progress};
use crate::i18n::{i18n, i18n_f};

pub(crate) const VERSION: &str = "6.6";

const LANGUAGETOOL: Archive = Archive {
    url: "https://languagetool.org/download/LanguageTool-6.6.zip",
    size: 251_998_221,
    sha256: "53600506b399bb5ffe1e4c8dec794fd378212f14aaf38ccef9b6f89314d11631",
    kind: Kind::Zip,
    folder: "languagetool",
};

/// Eclipse Temurin's Java 21 runtime, the build for this computer.
fn java() -> Option<&'static Archive> {
    const X86_64: Archive = Archive {
        url: "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.13%2B9/OpenJDK21U-jre_x64_linux_hotspot_21.0.13_9.tar.gz",
        size: 52_181_297,
        sha256: "a535a1df9c6284d9618872984e026ae0352d1f7e9e0605425c0e6114f47351dd",
        kind: Kind::TarGz,
        folder: "java",
    };
    const AARCH64: Archive = Archive {
        url: "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.13%2B9/OpenJDK21U-jre_aarch64_linux_hotspot_21.0.13_9.tar.gz",
        size: 51_266_660,
        sha256: "b1734db7ade49c3f6acc6ae06233b8c2dd90e3e2f00f67000ae1c972aaa64d8c",
        kind: Kind::TarGz,
        folder: "java",
    };
    match std::env::consts::ARCH {
        "x86_64" => Some(&X86_64),
        "aarch64" => Some(&AARCH64),
        _ => None,
    }
}

/// Unpacked, LanguageTool is about 400 MB and Java 160 MB; the archives
/// are on disk too until each is unpacked.
const SPACE: u64 = 900_000_000;

/// How long the server is kept after the last check.
const IDLE: Duration = Duration::from_secs(15 * 60);

/// How long a start may take: the first one after a download reads every
/// language's rules from a cold disk.
const START: Duration = Duration::from_secs(90);

fn dir() -> Option<PathBuf> {
    install::folder("languagetool")
}

/// Whether it can be downloaded for this computer at all.
pub(crate) fn available() -> bool {
    java().is_some()
}

/// The version downloaded, if it is.
pub(crate) fn installed() -> Option<String> {
    let dir = dir()?;
    let v = install::installed(&dir)?;
    (dir.join("java/bin/java").is_file() && server_jar(&dir).is_file()).then_some(v)
}

fn server_jar(dir: &std::path::Path) -> PathBuf {
    dir.join("languagetool").join("languagetool-server.jar")
}

pub(crate) fn download_size() -> u64 {
    LANGUAGETOOL.size + java().map_or(0, |a| a.size)
}

/// Download and unpack LanguageTool and Java (blocking).
pub(crate) fn download(progress: &Progress) -> Result<(), String> {
    let java = java().ok_or_else(not_available)?;
    let dir = dir().ok_or_else(not_available)?;
    stop();
    install::install(&dir, VERSION, &[java, &LANGUAGETOOL], SPACE, progress)
}

pub(crate) fn remove() -> Result<(), String> {
    stop();
    install::remove(&dir().ok_or_else(not_available)?)
}

fn not_available() -> String {
    i18n("LanguageTool is not available for this computer.")
}

// ─── The running server ──────────────────────────────────────────────────

enum Ask {
    /// Start it if it is not running, and say where it answers.
    Address(mpsc::Sender<Result<String, String>>),
    Stop,
}

/// The one thread that starts and stops the server. The server is its
/// child, and is told to end when this thread does, which is when the app
/// does: a crash leaves no Java process behind.
fn keeper() -> &'static Mutex<mpsc::Sender<Ask>> {
    static KEEPER: OnceLock<Mutex<mpsc::Sender<Ask>>> = OnceLock::new();
    KEEPER.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("languagetool".into())
            .spawn(move || keep(rx))
            .expect("thread");
        Mutex::new(tx)
    })
}

fn send(ask: Ask) {
    if let Ok(tx) = keeper().lock() {
        let _ = tx.send(ask);
    }
}

/// Where the server answers, `http://127.0.0.1:<port>/v2`, once it does
/// (blocking: it may have to start first).
pub(super) fn address() -> Result<String, String> {
    let (tx, rx) = mpsc::channel();
    send(Ask::Address(tx));
    rx.recv()
        .unwrap_or_else(|_| Err(i18n("LanguageTool stopped unexpectedly.")))
}

/// Start the server now, so the first check does not wait for it.
pub(crate) fn warm() {
    if installed().is_none() {
        return;
    }
    std::thread::spawn(|| {
        if let Err(e) = address() {
            tracing::warn!("LanguageTool did not start: {e}");
        }
    });
}

/// Stop the server, if it is running.
pub(crate) fn stop() {
    if KEEPER_STARTED.load(std::sync::atomic::Ordering::Relaxed) {
        send(Ask::Stop);
    }
}

static KEEPER_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn keep(rx: mpsc::Receiver<Ask>) {
    KEEPER_STARTED.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut server: Option<(Child, String)> = None;
    let mut last = Instant::now();
    loop {
        let ask = if server.is_some() {
            match rx.recv_timeout(IDLE.saturating_sub(last.elapsed())) {
                Ok(ask) => Some(ask),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(ask) => Some(ask),
                Err(_) => break,
            }
        };
        match ask {
            None => {
                if last.elapsed() >= IDLE {
                    tracing::info!("LanguageTool idle, stopping it");
                    end(&mut server);
                }
            }
            Some(Ask::Stop) => end(&mut server),
            Some(Ask::Address(reply)) => {
                last = Instant::now();
                let running = server
                    .as_mut()
                    .is_some_and(|(child, _)| matches!(child.try_wait(), Ok(None)));
                if !running {
                    end(&mut server);
                    match start() {
                        Ok(s) => server = Some(s),
                        Err(e) => {
                            let _ = reply.send(Err(e));
                            continue;
                        }
                    }
                }
                let address = server.as_ref().map(|(_, a)| a.clone()).unwrap_or_default();
                let _ = reply.send(Ok(address));
            }
        }
    }
    end(&mut server);
}

fn end(server: &mut Option<(Child, String)>) {
    if let Some((mut child, _)) = server.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn start() -> Result<(Child, String), String> {
    let dir = dir().filter(|_| installed().is_some()).ok_or_else(|| {
        i18n("LanguageTool is not downloaded. Download it in Settings → Composing.")
    })?;
    // A port nobody is using, found by asking for one. The server takes
    // only connections from this computer (its default).
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| e.to_string())?;
    let log = std::fs::File::create(dir.join("server.log")).ok();
    let mut cmd = Command::new(dir.join("java/bin/java"));
    cmd.current_dir(&dir)
        .args([
            "-Xms64m",
            "-Xmx2g",
            "-XX:+UseSerialGC",
            "-XX:+ExitOnOutOfMemoryError",
            "-Djava.awt.headless=true",
            "-cp",
        ])
        .arg(server_jar(&dir))
        .args(["org.languagetool.server.HTTPServer", "--port"])
        .arg(port.to_string())
        .stdin(Stdio::null());
    match log
        .as_ref()
        .and_then(|f| Some((f.try_clone().ok()?, f.try_clone().ok()?)))
    {
        Some((out, err)) => {
            cmd.stdout(out).stderr(err);
        }
        None => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    // SAFETY: prctl is async-signal-safe and touches nothing of the
    // parent's; it asks the kernel to end the server with this thread.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| {
        i18n_f(
            "LanguageTool could not be started: {e}",
            &[("e", &e.to_string())],
        )
    })?;
    let address = format!("http://127.0.0.1:{port}/v2");
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(2))
        .build();
    let began = Instant::now();
    while began.elapsed() < START {
        if let Ok(Some(status)) = child.try_wait() {
            tracing::warn!(
                "LanguageTool exited at start ({status}); see {}",
                dir.join("server.log").display()
            );
            return Err(i18n("LanguageTool could not be started."));
        }
        if agent.get(&format!("{address}/languages")).call().is_ok() {
            tracing::info!(
                "LanguageTool {VERSION} answering on port {port} after {:?}",
                began.elapsed()
            );
            return Ok((child, address));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(i18n("LanguageTool took too long to start."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_computer_has_a_java_build() {
        if matches!(std::env::consts::ARCH, "x86_64" | "aarch64") {
            assert!(available());
            assert!(download_size() > 300_000_000);
        }
    }
}
