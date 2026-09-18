//! The "Memory" section of an exported log: what the process tree is
//! resident at, and how much of the main process is the mail index versus the
//! session caches. Written for the "Vireo is using 2.6 GB" report, where the
//! first question is always "is it the index, the caches, or WebKit?" and the
//! answer used to take a back-and-forth. Nothing here is translated: it goes
//! into the log file, which is English like every other line in it.

use crate::models::Message;

/// One process in the tree, as `/proc` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub name: String,
    /// Resident set, in bytes.
    pub rss: u64,
    /// Proportional set size (shared pages divided among their sharers), in
    /// bytes. `None` when the kernel would not say.
    pub pss: Option<u64>,
    /// The anonymous (heap) part of the resident set, in bytes.
    pub heap: u64,
    pub threads: u32,
}

/// This process and every descendant, parents first. Inside the Flatpak
/// sandbox `/proc` shows only the sandbox, so the tree is exactly what a
/// system monitor lists under "Vireo": the app, WebKit's web and network
/// processes and their bwrap wrappers.
pub fn process_tree() -> Vec<Proc> {
    let me = std::process::id();
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    let mut parents: Vec<(u32, u32)> = Vec::new();
    for entry in dir.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if let Some(ppid) = read_status(pid).and_then(|s| field(&s, "PPid:")?.parse().ok()) {
            parents.push((pid, ppid));
        }
    }
    let mut tree = vec![me];
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        for (pid, ppid) in &parents {
            if *ppid == parent && !tree.contains(pid) {
                tree.push(*pid);
            }
        }
        i += 1;
    }
    tree.into_iter().filter_map(describe).collect()
}

fn read_status(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/status")).ok()
}

/// A `Key:` line's value, trimmed.
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| l.strip_prefix(key)).map(str::trim)
}

/// A `Key:   123 kB` line as bytes.
fn kb_field(text: &str, key: &str) -> Option<u64> {
    let v = field(text, key)?;
    let n: u64 = v.split_whitespace().next()?.parse().ok()?;
    Some(n * 1024)
}

fn describe(pid: u32) -> Option<Proc> {
    let status = read_status(pid)?;
    let name = field(&status, "Name:")?.to_string();
    // bwrap is only the sandbox wrapper around a WebKit process: a couple of
    // megabytes each, noise in this listing.
    if name == "bwrap" {
        return None;
    }
    let rss = kb_field(&status, "VmRSS:")?;
    let heap = kb_field(&status, "RssAnon:").unwrap_or(0);
    let threads = field(&status, "Threads:").and_then(|t| t.parse().ok()).unwrap_or(0);
    let pss = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
        .ok()
        .and_then(|t| kb_field(&t, "Pss:"));
    Some(Proc { pid, name, rss, pss, heap, threads })
}

/// The global allocator: the system one, counting what Rust code holds.
/// GTK, WebKit, SQLite, Mesa and the rest allocate through glibc directly,
/// so the difference between [`allocator`] and [`rust_heap`] is theirs — the
/// split that says whether a big heap is Vireo's own data or a library's.
pub struct CountingAllocator;

static RUST_LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static RUST_PEAK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl CountingAllocator {
    #[inline]
    fn add(n: usize) {
        use std::sync::atomic::Ordering::Relaxed;
        let live = RUST_LIVE.fetch_add(n, Relaxed) + n;
        // A racy max is fine: this is a report, not an accounting ledger.
        if live > RUST_PEAK.load(Relaxed) {
            RUST_PEAK.store(live, Relaxed);
        }
    }
    #[inline]
    fn sub(n: usize) {
        RUST_LIVE.fetch_sub(n, std::sync::atomic::Ordering::Relaxed);
    }
}

// SAFETY: every call is forwarded to `System` unchanged; only counters are
// touched around it, with atomics, so the allocator's contract is System's.
unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let p = std::alloc::System.alloc(layout);
        if !p.is_null() {
            Self::add(layout.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        let p = std::alloc::System.alloc_zeroed(layout);
        if !p.is_null() {
            Self::add(layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        std::alloc::System.dealloc(ptr, layout);
        Self::sub(layout.size());
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        let p = std::alloc::System.realloc(ptr, layout, new_size);
        if !p.is_null() {
            Self::sub(layout.size());
            Self::add(new_size);
        }
        p
    }
}

/// Bytes Rust code holds right now, and the most it has held.
pub fn rust_heap() -> (usize, usize) {
    use std::sync::atomic::Ordering::Relaxed;
    (RUST_LIVE.load(Relaxed), RUST_PEAK.load(Relaxed))
}

/// Bytes SQLite holds right now across every connection (page caches,
/// prepared statements, the lot), and the most it has held.
pub fn sqlite_heap() -> (u64, u64) {
    // SAFETY: both read a process-wide counter and take no pointers; the
    // highwater reset flag is 0, so nothing is changed.
    unsafe {
        (
            rusqlite::ffi::sqlite3_memory_used().max(0) as u64,
            rusqlite::ffi::sqlite3_memory_highwater(0).max(0) as u64,
        )
    }
}

/// glibc's `struct mallinfo2`: every field a `size_t`, in this order.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct MallInfo2 {
    arena: usize,
    ordblks: usize,
    smblks: usize,
    hblks: usize,
    hblkhd: usize,
    usmblks: usize,
    fsmblks: usize,
    uordblks: usize,
    fordblks: usize,
    keepcost: usize,
}

extern "C" {
    fn mallinfo2() -> MallInfo2;
}

/// What the C allocator is holding for this process, across every arena:
/// bytes in live allocations, bytes freed but still held from the system,
/// and bytes in large blocks it mapped directly. Rust's allocations are
/// glibc's too, so this is the whole heap. The difference between "live" and
/// the resident heap is what a `malloc_trim` could return.
pub fn allocator() -> Allocator {
    // SAFETY: mallinfo2 takes nothing and returns a plain struct by value; it
    // exists in every glibc since 2.33, which every build target has.
    let m = unsafe { mallinfo2() };
    Allocator { live: m.uordblks + m.hblkhd, held_free: m.fordblks, mapped: m.hblkhd }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allocator {
    pub live: usize,
    pub held_free: usize,
    pub mapped: usize,
}

/// The graphics driver libraries this process has loaded, by file name: what
/// GTK is actually drawing with. A software Vulkan (`libvulkan_lvp`) or
/// software GL (`libgallium` with `libLLVM`) path compiles shaders through
/// LLVM inside this process, which is hundreds of megabytes of heap that
/// have nothing to do with mail — the first thing to rule out on a VM or a
/// machine without a working GPU driver.
pub fn graphics_libraries() -> Vec<String> {
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else { return Vec::new() };
    let mut names: Vec<String> = maps
        .lines()
        .filter_map(|l| l.split_whitespace().nth(5))
        .filter_map(|path| path.rsplit('/').next())
        .filter(|name| {
            name.starts_with("libvulkan_")
                || name.starts_with("libLLVM")
                || name.starts_with("libgallium")
                || name.starts_with("libnvidia-glcore")
                || name.starts_with("libnvidia-eglcore")
                || name.starts_with("libGLX_nvidia")
                || name.contains("_dri.so")
        })
        .map(|s| s.to_string())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// How long this process has been running, from the kernel's clock.
pub fn uptime() -> Option<std::time::Duration> {
    let system: f64 =
        std::fs::read_to_string("/proc/uptime").ok()?.split_whitespace().next()?.parse().ok()?;
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Field 22 (1-based) is the start time in clock ticks; the command name
    // in parentheses may hold spaces, so count from after it.
    let after = &stat[stat.rfind(')')? + 1..];
    let ticks: f64 = after.split_whitespace().nth(19)?.parse().ok()?;
    let ticks_per_second = 100.0; // CLK_TCK on every Linux Vireo builds for
    let started = ticks / ticks_per_second;
    Some(std::time::Duration::from_secs_f64((system - started).max(0.0)))
}

/// What a message record costs in RAM: the struct plus every string and
/// keyword it owns. Capacity may exceed length a little; this is the floor.
pub fn message_bytes(m: &Message) -> usize {
    std::mem::size_of::<Message>()
        + m.from_name.len()
        + m.from_addr.len()
        + m.reply_to.len()
        + m.to.len()
        + m.cc.len()
        + m.subject.len()
        + m.preview.len()
        + m.body.len()
        + m.date.len()
        + m.message_id.len()
        + m.references.len()
        + m.keywords.iter().map(|k| std::mem::size_of::<String>() + k.len()).sum::<usize>()
}

/// The records and bytes of a batch of messages.
pub fn messages_bytes<'a>(msgs: impl IntoIterator<Item = &'a Message>) -> (usize, usize) {
    msgs.into_iter().fold((0, 0), |(n, b), m| (n + 1, b + message_bytes(m)))
}

/// A texture's pixel memory: GTK keeps decoded textures as 8-bit RGBA.
pub fn texture_bytes(t: &gtk::gdk::Texture) -> u64 {
    use gtk::prelude::*;
    (t.width() as u64) * (t.height() as u64) * 4
}

/// `1234567` → `1.2 MB`; `12345` → `12.1 KB`; `123` → `123 B`.
pub fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        format!("{:.2} GB", b / (KB * KB * KB))
    } else if b >= KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// `1234567` → `1,234,567`.
pub fn human_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `3h 12m`, `47m`, `12s`.
pub fn human_duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// The process lines of the report, with a total.
pub fn process_lines() -> Vec<String> {
    let procs = process_tree();
    let mut lines = Vec::new();
    for p in &procs {
        let pss = p.pss.map(human_bytes).unwrap_or_else(|| "?".to_string());
        lines.push(format!(
            "  {:<22} rss {:>9}   pss {:>9}   heap {:>9}   {} threads",
            p.name,
            human_bytes(p.rss),
            pss,
            human_bytes(p.heap),
            p.threads
        ));
    }
    let a = allocator();
    lines.push(format!(
        "  vireo heap as the allocator sees it: {} live, {} freed but held, {} in large mapped blocks",
        human_bytes(a.live as u64),
        human_bytes(a.held_free as u64),
        human_bytes(a.mapped as u64)
    ));
    if procs.len() > 1 {
        let rss: u64 = procs.iter().map(|p| p.rss).sum();
        let pss: Option<u64> = procs.iter().map(|p| p.pss).sum();
        lines.push(format!(
            "  {:<22} rss {:>9}   pss {:>9}   (pss is the honest total: shared pages counted once)",
            "total",
            human_bytes(rss),
            pss.map(human_bytes).unwrap_or_else(|| "?".to_string())
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_read_at_a_glance() {
        assert_eq!(human_bytes(123), "123 B");
        assert_eq!(human_bytes(12_345), "12.1 KB");
        assert_eq!(human_bytes(1_234_567), "1.2 MB");
        assert_eq!(human_bytes(2_600_000_000), "2.42 GB");
    }

    #[test]
    fn counts_get_thousands_separators() {
        assert_eq!(human_count(0), "0");
        assert_eq!(human_count(999), "999");
        assert_eq!(human_count(1_000), "1,000");
        assert_eq!(human_count(102_667), "102,667");
        assert_eq!(human_count(1_234_567), "1,234,567");
    }

    #[test]
    fn durations_read_at_a_glance() {
        let d = std::time::Duration::from_secs;
        assert_eq!(human_duration(d(12)), "12s");
        assert_eq!(human_duration(d(47 * 60)), "47m");
        assert_eq!(human_duration(d(3 * 3600 + 5 * 60)), "3h 05m");
    }

    #[test]
    fn the_tree_starts_with_this_process() {
        let tree = process_tree();
        assert_eq!(tree.first().map(|p| p.pid), Some(std::process::id()));
        assert!(tree[0].rss > 0);
    }

    #[test]
    fn uptime_is_sane() {
        let up = uptime().expect("/proc readable");
        assert!(up < std::time::Duration::from_secs(3600 * 24 * 365));
    }

    #[test]
    fn a_message_weighs_its_strings_plus_the_record() {
        let mut m = Message {
            id: 1,
            account_id: 1,
            folder_id: 1,
            uid: 1,
            from_name: String::new(),
            from_addr: String::new(),
            reply_to: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: String::new(),
            preview: String::new(),
            body: String::new(),
            date: String::new(),
            timestamp: 0,
            unread: false,
            starred: false,
            keywords: Vec::new(),
            has_attachment: false,
            message_id: String::new(),
            references: String::new(),
        };
        let empty = message_bytes(&m);
        assert_eq!(empty, std::mem::size_of::<Message>());
        m.subject = "hello".into();
        m.keywords.push("Work".into());
        assert_eq!(message_bytes(&m), empty + 5 + std::mem::size_of::<String>() + 4);
        assert_eq!(messages_bytes([&m, &m]), (2, 2 * message_bytes(&m)));
    }
}
