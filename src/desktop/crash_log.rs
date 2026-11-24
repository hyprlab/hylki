//! What a crash leaves behind (#449). GTK ends the app with an abort when
//! its list view trips on an assertion, and a Rust panic in a GTK callback
//! ends it the same way; either left only a line or two on stderr, with
//! nothing that said which of Hylki's code paths led there.
//!
//! On a fatal signal the app writes a report: the signal, GLib's assertion
//! message, the last panic, a backtrace of the thread that crashed, and the
//! newest lines of the console log. It goes to stderr and to `crash.log`
//! beside the other data files (the owner's alone), and Settings → System →
//! Export log carries the newest one into a bug report. GLib and GTK
//! warnings and criticals are copied into the console log too, so an export
//! shows them in order with Hylki's own lines.

use std::ffi::{c_char, c_int, c_void, CString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Console lines kept with a report: the last few minutes of a busy session.
const LOG_LINES: usize = 400;

/// The signals that end the process with a report first.
const SIGNALS: [c_int; 5] = [
    libc::SIGABRT,
    libc::SIGSEGV,
    libc::SIGBUS,
    libc::SIGILL,
    libc::SIGFPE,
];

/// The main thread's stack for the handler: describing a backtrace takes
/// more than the few kilobytes the standard library sets aside, and a stack
/// that overflowed has nothing left to run it on.
const ALT_STACK: usize = 1024 * 1024;

/// `crash.log`, made ready at install: opening a path in the handler is
/// safe, building one is not.
static PATH: OnceLock<CString> = OnceLock::new();
/// The newest panic and its backtrace. A panic in a GTK callback aborts, and
/// the abort's report says what the panic was.
static LAST_PANIC: Mutex<Option<String>> = Mutex::new(None);
/// A second fault while the report is written goes straight to the default
/// action.
static REPORTING: AtomicBool = AtomicBool::new(false);

extern "C" {
    /// GLib keeps the message of the assertion that is about to abort here,
    /// for crash reporters (glib/gtestutils.c).
    static __glib_assert_msg: *const c_char;
}

fn file() -> Option<PathBuf> {
    crate::config::data_base().map(|d| d.join("hylki").join("crash.log"))
}

/// Set up the report, the panic hook and the GLib log copy. Early in
/// `main`, after the logging it copies into and before GTK starts.
pub(crate) fn install() {
    if let Some(path) = file() {
        if let Some(dir) = path.parent() {
            let _ = crate::config::private_dir(dir);
        }
        if let Ok(c) = CString::new(path.as_os_str().as_encoded_bytes()) {
            let _ = PATH.set(c);
        }
        if let Ok(meta) = std::fs::metadata(&path) {
            let when = meta
                .modified()
                .ok()
                .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            tracing::info!("crash report from {when} kept at {}", path.display());
        }
    }
    install_panic_hook();
    install_signal_handlers();
    copy_glib_log();
}

/// Panics go to the log with a backtrace, which the default hook only
/// prints with `RUST_BACKTRACE` set, and are kept for an abort that follows.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let backtrace = std::backtrace::Backtrace::force_capture();
        let text = format!("thread '{name}' {info}\n{backtrace}");
        tracing::error!("{text}");
        if let Ok(mut last) = LAST_PANIC.try_lock() {
            *last = Some(text);
        }
    }));
}

fn install_signal_handlers() {
    // SAFETY: the stack is leaked, so it outlives every signal; sigaction is
    // given a zeroed struct filled in field by field.
    unsafe {
        let stack = libc::mmap(
            std::ptr::null_mut(),
            ALT_STACK,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        if stack != libc::MAP_FAILED {
            let ss = libc::stack_t {
                ss_sp: stack,
                ss_flags: 0,
                ss_size: ALT_STACK,
            };
            libc::sigaltstack(&ss, std::ptr::null_mut());
        }
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_signal as *const () as usize;
        // Reset to the default first thing: raised again at the end, the
        // signal ends the process as it would have, core dump included.
        sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESETHAND;
        libc::sigemptyset(&mut sa.sa_mask);
        for sig in SIGNALS {
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}

extern "C" fn on_signal(sig: c_int, _info: *mut libc::siginfo_t, _ctx: *mut c_void) {
    if !REPORTING.swap(true, Ordering::SeqCst) {
        // A report stuck on a lock the crash left held still ends.
        // SAFETY: alarm has no preconditions.
        unsafe { libc::alarm(10) };
        // Best effort: a backtrace allocates, which a heap the crash broke
        // may not survive. The signal is raised again either way.
        let report = report(sig);
        write_all(2, b"\n");
        write_all(2, report.as_bytes());
        if let Some(path) = PATH.get() {
            // SAFETY: a valid C path; the descriptor is closed below.
            let fd = unsafe {
                libc::open(
                    path.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
                    0o600,
                )
            };
            if fd >= 0 {
                write_all(fd, report.as_bytes());
                // SAFETY: fd was opened above.
                unsafe { libc::close(fd) };
                let note = format!("The report is in {}\n", path.to_string_lossy());
                write_all(2, note.as_bytes());
            }
        }
    }
    // SAFETY: the handler was reset to the default on entry.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

fn write_all(fd: c_int, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: the slice is valid for its length.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n <= 0 {
            return;
        }
        bytes = &bytes[n as usize..];
    }
}

fn signal_name(sig: c_int) -> &'static str {
    match sig {
        libc::SIGABRT => "SIGABRT (abort)",
        libc::SIGSEGV => "SIGSEGV (invalid memory access)",
        libc::SIGBUS => "SIGBUS (bus error)",
        libc::SIGILL => "SIGILL (illegal instruction)",
        libc::SIGFPE => "SIGFPE (arithmetic error)",
        _ => "a fatal signal",
    }
}

fn report(sig: c_int) -> String {
    let mut out = format!(
        "Hylki {} ({}) crashed with {}\n{}, GTK {}.{}.{}\n",
        crate::VERSION,
        crate::APP_ID,
        signal_name(sig),
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z"),
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version(),
    );
    // SAFETY: GLib only ever stores a NUL-terminated string or null there.
    let assert_msg = unsafe { __glib_assert_msg };
    if !assert_msg.is_null() {
        // SAFETY: checked non-null above.
        let msg = unsafe { std::ffi::CStr::from_ptr(assert_msg) };
        out.push_str(&format!("GLib: {}\n", msg.to_string_lossy()));
    }
    if let Ok(last) = LAST_PANIC.try_lock() {
        if let Some(panic) = last.as_ref() {
            out.push_str(&format!("\nThe last panic:\n{panic}\n"));
        }
    }
    let thread = std::thread::current();
    out.push_str(&format!(
        "\nBacktrace of thread '{}':\n{}\n",
        thread.name().unwrap_or("unnamed"),
        std::backtrace::Backtrace::force_capture()
    ));
    if let Some(lines) = crate::desktop::console_log::tail(LOG_LINES) {
        out.push_str(&format!("\nThe last {} lines of the log:\n", lines.len()));
        for line in lines {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// GLib, GTK and the libraries under them log their warnings, criticals
/// and errors to stderr only; the console log gets a copy, under the
/// `glib` target, which stderr's filter leaves out so nothing prints twice.
fn copy_glib_log() {
    use gtk::glib::{log_set_writer_func, log_writer_default, LogLevel};
    log_set_writer_func(|level, fields| {
        if matches!(level, LogLevel::Error | LogLevel::Critical | LogLevel::Warning) {
            let get = |key: &str| {
                fields
                    .iter()
                    .find(|f| f.key() == key)
                    .and_then(|f| f.value_str())
                    .unwrap_or("")
            };
            let (domain, message) = (get("GLIB_DOMAIN"), get("MESSAGE"));
            match level {
                LogLevel::Warning => tracing::warn!(target: "glib", "{domain}: {message}"),
                _ => tracing::error!(target: "glib", "{domain}: {message}"),
            }
        }
        log_writer_default(level, fields)
    });
}

/// The newest report and when it was written, for an exported log.
pub(crate) fn last_report() -> Option<(String, String)> {
    let path = file()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let when = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default();
    Some((when, text))
}
