//! The console-mode log store: a capped ring buffer fed by a dedicated
//! tracing layer (see `main.rs`), read by the status bar's console view.
//!
//! The layer runs at `hylki=debug` regardless of `RUST_LOG`, so the console
//! is verbose even when stderr is quiet. Lines are sequence-numbered so the
//! UI can poll cheaply ("everything after seq N") without copying the whole
//! buffer each tick.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// How many lines the console keeps; older ones fall off the top. Large
/// enough that an export (#132) carries a whole session at debug level — a
/// few megabytes at most.
const CAP: usize = 20_000;

static BUF: Mutex<VecDeque<(u64, String)>> = Mutex::new(VecDeque::new());
static SEQ: AtomicU64 = AtomicU64::new(0);

fn push_line(line: &str) {
    let line = line.trim_end();
    if line.is_empty() {
        return;
    }
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let mut buf = BUF.lock().unwrap();
    if buf.len() >= CAP {
        buf.pop_front();
    }
    buf.push_back((seq, line.to_string()));
}

/// Every buffered line newer than `after`, plus the newest sequence number
/// (pass it back next call). Cheap when nothing is new.
pub fn lines_since(after: u64) -> (u64, Vec<String>) {
    let newest = SEQ.load(Ordering::Relaxed);
    if newest == after {
        return (newest, Vec::new());
    }
    let buf = BUF.lock().unwrap();
    let lines = buf.iter().filter(|(s, _)| *s > after).map(|(_, l)| l.clone()).collect();
    (newest, lines)
}

/// `MakeWriter` for the console's fmt layer: collects the layer's output and
/// buffers it line by line.
#[derive(Clone, Default)]
pub struct ConsoleWriter;

impl std::io::Write for ConsoleWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for line in String::from_utf8_lossy(bytes).split('\n') {
            push_line(line);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ConsoleWriter {
    type Writer = ConsoleWriter;
    fn make_writer(&'a self) -> Self::Writer {
        ConsoleWriter
    }
}

/// How many lines the console holds and what they weigh, for the memory
/// section of an export.
pub fn stats() -> (usize, usize) {
    let buf = BUF.lock().unwrap();
    (buf.len(), buf.iter().map(|(_, l)| l.len()).sum())
}

/// Everything the console holds, oldest first.
pub fn all_lines() -> Vec<String> {
    BUF.lock().unwrap().iter().map(|(_, l)| l.clone()).collect()
}

/// The log as a file for a bug report (#132): a header naming the build and
/// the desktop it runs on, then every line the console has kept since the
/// app started, with email addresses shortened to their domain so the file
/// can be attached to an issue as it is. `memory` is the app's memory
/// section (see `memory_report`), placed between the header and the lines.
pub fn export_text(memory: &str) -> String {
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string()))
        })
        .unwrap_or_else(|| "unknown OS".to_string());
    let host = if crate::platform::is_flatpak() {
        "Flatpak"
    } else if crate::platform::is_appimage() {
        "AppImage"
    } else {
        "host build"
    };
    let mut out = String::new();
    out.push_str(&format!(
        "Hylki {} ({}), {host}\n{os}, GTK {}.{}.{}, libadwaita {}.{}.{}\nExported {}\nEmail addresses are shortened to their domain.\n\n",
        crate::VERSION,
        crate::APP_ID,
        gtk::major_version(),
        gtk::minor_version(),
        gtk::micro_version(),
        adw::major_version(),
        adw::minor_version(),
        adw::micro_version(),
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z"),
    ));
    if !memory.is_empty() {
        out.push_str(memory);
        if !memory.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    for line in all_lines() {
        out.push_str(&redact_addresses(&line));
        out.push('\n');
    }
    out
}

/// `name@example.com` → `…@example.com`: the local part is the personal
/// bit; the domain still says which provider was involved.
fn redact_addresses(line: &str) -> String {
    let is_local = |c: char| c.is_ascii_alphanumeric() || "._%+-".contains(c);
    let is_domain = |c: char| c.is_ascii_alphanumeric() || ".-".contains(c);
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' && i > 0 && is_local(chars[i - 1]) && i + 1 < chars.len() && is_domain(chars[i + 1]) {
            // Walk the local part back out of `out`, then keep the domain.
            let mut start = i;
            while start > 0 && is_local(chars[start - 1]) {
                start -= 1;
            }
            let local_len: usize = chars[start..i].iter().map(|c| c.len_utf8()).sum();
            out.truncate(out.len() - local_len);
            out.push('\u{2026}');
            out.push('@');
            i += 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::redact_addresses;

    #[test]
    fn addresses_keep_their_domain_only() {
        assert_eq!(redact_addresses("filter: jane.doe@example.com tagged Work"), "filter: \u{2026}@example.com tagged Work");
        assert_eq!(redact_addresses("two a@b.org and c+d@e.f.net"), "two \u{2026}@b.org and \u{2026}@e.f.net");
        assert_eq!(redact_addresses("no address here @ all"), "no address here @ all");
    }
}
