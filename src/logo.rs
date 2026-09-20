//! Sender logos (opt-in): the brand's logo in place of coloured initials, so
//! mail from Apple, Amazon or PayPal is recognisable at a glance (#30).
//!
//! Three sources, best first:
//!
//! 1. **BIMI** — the logo the sender publishes for mail clients: a DNS TXT
//!    record at `default._bimi.<domain>` naming an SVG on the sender's own
//!    site (RFC draft "Brand Indicators for Message Identification"). Vector,
//!    authoritative, and what Apple Mail and Gmail show. The sending host is
//!    asked first, then the registrable domain.
//! 2. **Bundled** — `data/logos/`: a curated map of sender domains to marks
//!    from gilbarbara/logos (full colour) and Simple Icons (a glyph on the
//!    brand colour), plus the app's own service marks (`data/brands/`).
//!    Shipped in the binary, so these show with no request at all.
//! 3. **The site's own icon**, the largest it declares first: the icons its
//!    home page links (`<link rel="icon" sizes="192x192">`,
//!    `apple-touch-icon`, an SVG icon) and the ones in its web manifest —
//!    where the 512px icons usually live — then the well-known paths,
//!    `apple-touch-icon.png` (180px) and `favicon.ico` (16–48px, the last
//!    resort).
//!
//! Addresses at a **mailbox host** (Gmail, Outlook, iCloud, Yahoo, Proton and
//! the rest of `MAILBOX_HOSTS`) are skipped entirely, whichever source might
//! have answered: such an address belongs to a person, not to the provider, so
//! it keeps its coloured initials rather than wearing the provider's mark.
//!
//! No third-party service is involved and no per-user identifier is sent,
//! but the BIMI and site requests do tell that domain your IP address —
//! which is exactly what blocking remote content avoids. So this is off by
//! default and gated behind a Preferences switch, as Gravatar is.
//!
//! One fetch per domain per session, remembered either way: a miss is cached too,
//! or every row from the same sender would ask again.
//!
//! Icons persist on disk (`~/.local/share/hylki/logos/<domain>.img`) so a
//! restart shows them without touching the network; a `.miss` marker remembers
//! a domain with nothing to give. Both go stale after a week: the next message
//! from that sender then re-asks the domain — a changed brand icon appears, a
//! domain that gained one is picked up — while the stale icon keeps showing in
//! the meantime.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;

/// The edge every logo is decoded to: drawn at avatar size, kept per
/// domain for the session (issue #106), and what vector sources are
/// rasterised at.
const LOGO_PX: u32 = 160;

/// A BIMI logo is a small SVG (the spec asks for 32KB); anything bigger
/// is not one.
const BIMI_MAX_BYTES: u64 = 64 * 1024;

thread_local! {
    static CACHE: RefCell<HashMap<String, gtk::gdk::Texture>> = RefCell::new(HashMap::new());
    /// Domains with no usable icon, so they are asked once and not again.
    static MISSES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// Domains whose weekly refresh has already been kicked off this session.
    static REFRESHED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// The session's logo cache: domains with a texture, their pixel bytes, and
/// domains remembered as having none. For the memory section of an export.
pub fn cache_stats() -> (usize, u64, usize) {
    let (n, bytes) = CACHE.with(|c| {
        let c = c.borrow();
        (c.len(), c.values().map(crate::memory_report::texture_bytes).sum())
    });
    (n, bytes, MISSES.with(|m| m.borrow().len()))
}

/// How long a stored icon (or miss) is trusted before the domain is re-asked.
const REFRESH_AFTER: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

fn store_dir() -> Option<PathBuf> {
    let dir = crate::config::data_base()?.join("hylki").join("logos");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

fn img_path(domain: &str) -> Option<PathBuf> {
    Some(store_dir()?.join(format!("{domain}.img")))
}

fn miss_path(domain: &str) -> Option<PathBuf> {
    Some(store_dir()?.join(format!("{domain}.miss")))
}

/// The sender's BIMI logo when it has one (the SVG bytes), or an empty
/// marker saying it was asked within the week and has none. Kept apart
/// from the site icon (`.img`) so a bundled mark can outrank a stored
/// favicon without outranking a stored BIMI logo.
fn bimi_path(domain: &str) -> Option<PathBuf> {
    Some(store_dir()?.join(format!("{domain}.bimi")))
}

fn stored_bimi(domain: &str) -> Option<Vec<u8>> {
    let p = bimi_path(domain)?;
    let bytes = std::fs::read(p).ok()?;
    (!bytes.is_empty()).then_some(bytes)
}

/// Mailbox hosts: domains that host other people's mail rather than send
/// their own. An address at one of these belongs to a person, not to the
/// provider, so it gets initials like any individual — otherwise every
/// friend on Gmail wears the Gmail mark (and, through the site-icon step,
/// so would every other freemail sender).
///
/// The provider's mark still identifies an *account* in Settings — that is
/// `brand.rs`, a different question from who sent a message.
const MAILBOX_HOSTS: &[&str] = &[
    // Google
    "gmail.com", "googlemail.com",
    // Microsoft
    "outlook.com", "outlook.co.uk", "hotmail.com", "hotmail.co.uk", "hotmail.fr",
    "hotmail.it", "hotmail.es", "hotmail.de", "live.com", "live.co.uk", "live.fr",
    "live.nl", "live.ca", "msn.com",
    // Apple
    "icloud.com", "me.com", "mac.com", "privaterelay.appleid.com",
    // Yahoo / AOL
    "yahoo.com", "yahoo.co.uk", "yahoo.co.jp", "yahoo.fr", "yahoo.de", "yahoo.es",
    "yahoo.it", "yahoo.ca", "yahoo.com.au", "yahoo.com.br", "yahoo.co.in",
    "ymail.com", "rocketmail.com", "aol.com", "aol.co.uk",
    // Privacy-minded providers
    "proton.me", "protonmail.com", "protonmail.ch", "pm.me", "tutanota.com",
    "tutanota.de", "tuta.com", "tuta.io", "mailbox.org", "posteo.de", "posteo.net",
    "hushmail.com", "runbox.com", "startmail.com", "disroot.org", "riseup.net",
    // Other mailbox hosts
    "fastmail.com", "fastmail.fm", "zoho.com", "zoho.eu", "hey.com", "duck.com",
    "mail.com", "email.com", "usa.com", "gmx.com", "gmx.net", "gmx.de", "gmx.at",
    "gmx.ch", "web.de", "t-online.de", "freenet.de", "arcor.de",
    // Russia / Ukraine
    "yandex.com", "yandex.ru", "yandex.by", "yandex.kz", "mail.ru", "bk.ru",
    "list.ru", "inbox.ru", "internet.ru", "rambler.ru", "ukr.net", "i.ua",
    // Asia
    "qq.com", "foxmail.com", "163.com", "126.com", "yeah.net", "sina.com",
    "sina.cn", "sohu.com", "naver.com", "daum.net", "hanmail.net",
    "rediffmail.com",
    // Europe / Americas ISPs and portals
    "orange.fr", "wanadoo.fr", "free.fr", "sfr.fr", "laposte.net", "gmx.fr",
    "libero.it", "virgilio.it", "alice.it", "tiscali.it", "seznam.cz",
    "centrum.cz", "wp.pl", "o2.pl", "onet.pl", "interia.pl",
    "btinternet.com", "sky.com", "talktalk.net", "ntlworld.com", "bigpond.com",
    "comcast.net", "verizon.net", "att.net", "sbcglobal.net", "bellsouth.net",
    "cox.net", "charter.net", "earthlink.net", "juno.com", "optonline.net",
    "shaw.ca", "rogers.com", "sympatico.ca", "telus.net",
    "uol.com.br", "bol.com.br", "terra.com.br", "ig.com.br",
];

/// Whether this address is at a [`MAILBOX_HOSTS`] domain, so it names a
/// person rather than a brand.
fn is_mailbox_host(email: &str) -> bool {
    let Some(domain) = domain_of(email) else {
        return false;
    };
    MAILBOX_HOSTS.contains(&domain.as_str())
}

/// The host part of an address, lowercased — the sending host itself
/// (`notifications.usbank.com`), as BIMI is looked up on it first.
fn host_of(email: &str) -> Option<String> {
    let host = email.rsplit('@').next()?.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || !host.contains('.') || host.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    Some(host)
}

/// Whether the file at `path` exists and was written within [`REFRESH_AFTER`].
fn fresh(path: &PathBuf) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < REFRESH_AFTER)
}

/// The registrable domain of an email address: the part that owns the brand.
///
/// Mail comes from `em6917.cloudways.com` or `mail.notifications.apple.com`, and
/// the icon lives at the domain those hang off. Two labels, or three when the
/// last two are a country-code pair like `co.uk`, which is a heuristic rather
/// than the public suffix list — close enough to point a favicon request at, and
/// wrong only for a handful of unusual suffixes.
pub fn domain_of(email: &str) -> Option<String> {
    let host = email.rsplit('@').next()?.trim().trim_end_matches('.');
    let host = host.to_ascii_lowercase();
    if host.is_empty() || !host.contains('.') || host.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() < 2 {
        return None;
    }
    let tail = &labels[labels.len().saturating_sub(2)..];
    let take = if labels.len() > 2 && tail[0].len() <= 3 && tail[1].len() == 2 {
        3
    } else {
        2
    };
    Some(labels[labels.len() - take.min(labels.len())..].join("."))
}

/// A previously decoded logo for this sender's domain (main thread only).
/// Falls back to the on-disk copy — however old — so a restart shows icons
/// without a single network request; [`wants_refresh`] handles staleness.
pub fn cached(email: &str) -> Option<gtk::gdk::Texture> {
    if is_mailbox_host(email) {
        return None;
    }
    let domain = domain_of(email)?;
    if let Some(tex) = CACHE.with(|c| c.borrow().get(&domain).cloned()) {
        return Some(tex);
    }
    // The stored BIMI logo, else a bundled mark, else the stored site icon.
    let bytes = stored_bimi(&domain)
        .or_else(|| bundled_entry(email).and_then(bundled_bytes))
        .or_else(|| img_path(&domain).and_then(|p| std::fs::read(p).ok()))?;
    let tex = decode(&bytes)?;
    CACHE.with(|c| {
        c.borrow_mut().insert(domain, tex.clone());
    });
    Some(tex)
}

/// Whether the stored icon for this sender's domain is a week old — time to
/// look again in the background while the old one keeps showing. Says yes at
/// most once a session per domain, so a screenful of rows from one sender
/// doesn't fan out into a fetch per row.
pub fn wants_refresh(email: &str) -> bool {
    if is_mailbox_host(email) {
        return false;
    }
    let Some(domain) = domain_of(email) else {
        return false;
    };
    // Due when nothing on disk is within the week: a stale site icon, a
    // stale BIMI answer, or a bundled mark whose sender has never been
    // asked for a BIMI logo at all.
    let bimi_fresh = bimi_path(&domain).is_some_and(|p| fresh(&p));
    let img_fresh = img_path(&domain).is_some_and(|p| fresh(&p));
    let due = !bimi_fresh && !img_fresh;
    due && REFRESHED.with(|r| r.borrow_mut().insert(domain))
}

/// Whether this domain has already been asked about and had nothing to give.
/// A miss remembered on disk expires after a week, so a domain that gains an
/// icon is eventually found.
pub fn known_missing(email: &str) -> bool {
    // Nothing will ever be looked up for a person's own mailbox.
    if is_mailbox_host(email) {
        return true;
    }
    if bundled_entry(email).is_some() {
        return false;
    }
    match domain_of(email) {
        Some(domain) => {
            MISSES.with(|m| m.borrow().contains(&domain))
                || miss_path(&domain).is_some_and(|p| fresh(&p))
        }
        // Nothing to look up counts as answered.
        None => true,
    }
}

/// Blocking fetch of a domain's icon. Call off the main thread.
///
/// A fresh on-disk copy answers without touching the network; otherwise the
/// domain is asked and the answer stored — icon or miss. When the network
/// fails with a stale copy in hand, the stale copy stands (and stays due for
/// refresh, so it is retried later).
pub fn fetch(email: &str) -> Option<Vec<u8>> {
    fetch_from(email).map(|(bytes, _)| bytes)
}

/// What [`fetch`] would answer for an address and where from, in words —
/// for the `HYLKI_LOGO_PROBE` hook.
pub fn probe(email: &str) -> String {
    match fetch_from(email) {
        Some((bytes, source)) => format!("{source} ({} bytes)", bytes.len()),
        None => "nothing".to_string(),
    }
}

/// The fetch, with the source it answered from: the stored BIMI logo, a
/// fresh BIMI lookup, a bundled mark, a stored or fetched site icon.
fn fetch_from(email: &str) -> Option<(Vec<u8>, String)> {
    if is_mailbox_host(email) {
        return None;
    }
    let domain = domain_of(email)?;
    let img = img_path(&domain);
    let bimi = bimi_path(&domain);
    let bimi_fresh = bimi.as_ref().is_some_and(|p| fresh(p));
    if bimi_fresh {
        if let Some(bytes) = stored_bimi(&domain) {
            return Some((bytes, "stored BIMI logo".into()));
        }
    } else {
        // Asked at most once a week: a hit is kept as the logo, a
        // confirmed miss as an empty marker. A lookup that could not be
        // made (no network) leaves no marker, so it is tried again.
        match bimi_logo(email) {
            Bimi::Logo(bytes) => {
                if let Some(p) = bimi.as_ref() {
                    let _ = std::fs::write(p, &bytes);
                }
                if let Some(p) = miss_path(&domain) {
                    let _ = std::fs::remove_file(p);
                }
                return Some((bytes, "BIMI logo".into()));
            }
            Bimi::None => {
                if let Some(p) = bimi.as_ref() {
                    let _ = std::fs::write(p, b"");
                }
            }
            Bimi::Unreachable => {}
        }
    }
    if let Some(entry) = bundled_entry(email) {
        if let Some(bytes) = bundled_bytes(entry) {
            if let Some(p) = miss_path(&domain) {
                let _ = std::fs::remove_file(p);
            }
            return Some((bytes, format!("bundled {}:{}", entry.source, entry.file)));
        }
    }
    if let Some(p) = img.as_ref().filter(|p| fresh(p)) {
        if let Ok(bytes) = std::fs::read(p) {
            return Some((bytes, "stored site icon".into()));
        }
    }
    if miss_path(&domain).is_some_and(|p| fresh(&p)) {
        return None;
    }
    for url in candidate_urls(&domain) {
        if let Some(bytes) = get(&url) {
            if let Some(p) = img.as_ref() {
                let _ = std::fs::write(p, &bytes);
            }
            if let Some(p) = miss_path(&domain) {
                let _ = std::fs::remove_file(p);
            }
            return Some((bytes, format!("site icon {url}")));
        }
    }
    if let Some(p) = img.as_ref().filter(|p| p.exists()) {
        // Nothing new, but yesterday's icon beats initials.
        return std::fs::read(p).ok().map(|b| (b, "stale site icon".into()));
    }
    if let Some(p) = miss_path(&domain) {
        let _ = std::fs::write(p, b"");
    }
    None
}

// ---------------------------------------------------------------------------
// BIMI: the logo the sender publishes for mail clients.
// ---------------------------------------------------------------------------

/// What asking for a sender's BIMI logo came to.
enum Bimi {
    /// The logo, as a `LOGO_PX`-square SVG document.
    Logo(Vec<u8>),
    /// The sender publishes none (or nothing usable): not asked again for a week.
    None,
    /// The question could not be asked (no network, resolver down): asked again next time.
    Unreachable,
}

/// The sender's BIMI logo, from the sending host's record or the
/// registrable domain's. Blocking (DNS, then one HTTPS fetch); call off
/// the main thread.
fn bimi_logo(email: &str) -> Bimi {
    let mut names: Vec<String> = Vec::new();
    if let Some(host) = host_of(email) {
        names.push(host);
    }
    if let Some(domain) = domain_of(email) {
        if !names.contains(&domain) {
            names.push(domain);
        }
    }
    let mut unreachable = false;
    for name in names {
        let url = match bimi_record_url(&name) {
            Ok(Some(url)) => url,
            Ok(None) => continue,
            Err(()) => {
                unreachable = true;
                continue;
            }
        };
        if let Some(svg) = bimi_fetch(&url) {
            if let Some(framed) = square_svg(&svg, LOGO_PX, None, 0.0, None) {
                return Bimi::Logo(framed.into_bytes());
            }
        }
    }
    if unreachable { Bimi::Unreachable } else { Bimi::None }
}

/// The logo URL in `default._bimi.<name>`'s TXT record: `Ok(None)` when
/// the name has no such record, `Err` when the resolver could not say.
fn bimi_record_url(name: &str) -> Result<Option<String>, ()> {
    use gtk::gio::prelude::*;
    let resolver = gtk::gio::Resolver::default();
    let records = match resolver.lookup_records(
        &format!("default._bimi.{name}"),
        gtk::gio::ResolverRecordType::Txt,
        gtk::gio::Cancellable::NONE,
    ) {
        Ok(r) => r,
        Err(e) if e.kind::<gtk::gio::ResolverError>() == Some(gtk::gio::ResolverError::NotFound) => {
            tracing::debug!("bimi: no record for {name}");
            return Ok(None);
        }
        Err(e) => {
            tracing::debug!("bimi: could not look up {name}: {e}");
            return Err(());
        }
    };
    for record in records {
        // A TXT record is `(as)`: the strings of one record, to be joined.
        let text: String = record
            .child_value(0)
            .iter()
            .filter_map(|v| v.str().map(str::to_string))
            .collect();
        tracing::debug!("bimi: {name} record {text:?}");
        if let Some(url) = parse_bimi_record(&text) {
            return Ok(Some(url));
        }
    }
    Ok(None)
}

/// The `l=` location of a `v=BIMI1` record, when it is an https URL.
/// An empty `l=` (a declared opt-out) or any other version is nothing.
pub(crate) fn parse_bimi_record(txt: &str) -> Option<String> {
    let mut is_bimi = false;
    let mut location: Option<String> = None;
    for tag in txt.split(';') {
        let Some((k, v)) = tag.split_once('=') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        match k.as_str() {
            "v" => is_bimi = v.eq_ignore_ascii_case("BIMI1"),
            "l" => location = Some(v.to_string()),
            _ => {}
        }
    }
    let url = location.filter(|_| is_bimi)?;
    url.to_ascii_lowercase().starts_with("https://").then_some(url)
}

/// The BIMI SVG at `url`: small, served as an image or XML, and an SVG.
fn bimi_fetch(url: &str) -> Option<String> {
    use std::io::Read;
    let resp = match ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "image/svg+xml,*/*;q=0.5")
        .timeout(std::time::Duration::from_secs(5))
        .call()
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("bimi: {url}: {e}");
            return None;
        }
    };
    let ct = resp.content_type().to_ascii_lowercase();
    if !(ct.is_empty() || ct.contains("svg") || ct.contains("xml") || ct.contains("octet-stream")) {
        tracing::debug!("bimi: {url}: not an SVG ({ct})");
        return None;
    }
    let mut buf = Vec::new();
    resp.into_reader().take(BIMI_MAX_BYTES + 1).read_to_end(&mut buf).ok()?;
    if buf.is_empty() || buf.len() as u64 > BIMI_MAX_BYTES || !looks_like_svg(&buf) {
        tracing::debug!("bimi: {url}: {} bytes, not usable", buf.len());
        return None;
    }
    String::from_utf8(buf).ok()
}

// ---------------------------------------------------------------------------
// Bundled marks: data/logos/, embedded by build.rs; see tools/fetch-logos.py.
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct LogoFile {
    #[serde(default)]
    logo: Vec<LogoEntry>,
}

/// One line of `data/logos/logos.toml`: a sender domain and the mark it
/// gets — `source` is `gilbarbara`, `simple` or `brand` (one of
/// `data/brands/`), `file` the SVG or mark id, `color` Simple Icons' brand
/// hex.
#[derive(serde::Deserialize, Clone)]
struct LogoEntry {
    domain: String,
    source: String,
    file: String,
    #[serde(default)]
    color: Option<String>,
}

fn bundled_map() -> &'static HashMap<String, LogoEntry> {
    static MAP: OnceLock<HashMap<String, LogoEntry>> = OnceLock::new();
    MAP.get_or_init(|| {
        let file: LogoFile = match toml::from_str(include_str!("../data/logos/logos.toml")) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("bundled logo map unreadable: {e}");
                LogoFile { logo: Vec::new() }
            }
        };
        file.logo.into_iter().map(|e| (e.domain.clone(), e)).collect()
    })
}

/// The bundled mark for a sender: the sending host's entry first
/// (`aws.amazon.com`), then the registrable domain's.
fn bundled_entry(email: &str) -> Option<&'static LogoEntry> {
    let map = bundled_map();
    host_of(email)
        .and_then(|h| map.get(&h))
        .or_else(|| domain_of(email).and_then(|d| map.get(&d)))
}

/// Whether a sender has a bundled mark (shown with no request made).
pub fn has_bundled(email: &str) -> bool {
    !is_mailbox_host(email) && bundled_entry(email).is_some()
}

/// A bundled mark's bytes: the app's service marks as their PNG, the SVG
/// sets framed to a `LOGO_PX` square — a Simple Icons glyph in the colour
/// that reads on its brand colour, over that colour; a gilbarbara mark
/// over white, inset a little, since many are dark on transparent.
fn bundled_bytes(entry: &LogoEntry) -> Option<Vec<u8>> {
    match entry.source.as_str() {
        "brand" => crate::brand::png(&entry.file).map(<[u8]>::to_vec),
        "simple" | "gilbarbara" => {
            let path = format!("/co/hyprlab/Hylki/logos/{}/{}", entry.source, entry.file);
            let data = gtk::gio::resources_lookup_data(&path, gtk::gio::ResourceLookupFlags::NONE).ok()?;
            let svg = std::str::from_utf8(&data).ok()?;
            let framed = if entry.source == "simple" {
                let bg = entry.color.as_deref().unwrap_or("#000000");
                let fg = crate::color::readable_text(bg).to_string();
                square_svg(svg, LOGO_PX, Some(bg), 0.22, Some(&fg))?
            } else {
                square_svg(svg, LOGO_PX, Some("#ffffff"), 0.12, None)?
            };
            Some(framed.into_bytes())
        }
        _ => None,
    }
}

/// `svg` re-framed as a `px`-square document: its drawing centred and
/// scaled to fit, inset by `pad` (a fraction of the edge) on each side,
/// over `bg` when given, with `fill` as the colour of content that sets
/// none (Simple Icons' glyphs). The original root's attributes ride on
/// the nested element, so namespaces, styles and gradients keep working;
/// a root with no `viewBox` gets one from its width and height. `None`
/// for anything that is not an SVG document.
pub(crate) fn square_svg(svg: &str, px: u32, bg: Option<&str>, pad: f32, fill: Option<&str>) -> Option<String> {
    let lower = svg.to_ascii_lowercase();
    let start = svg_root_start(&lower)?;
    let open_end = start + svg[start..].find('>')?;
    let close = lower.rfind("</svg>")?;
    if close <= open_end {
        return None;
    }
    let attrs = parse_attrs(svg[start + 4..open_end].trim_end_matches('/'));
    let content = &svg[open_end + 1..close];
    let view_box = attrs
        .iter()
        .find(|(k, _)| k == "viewbox")
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| v.split([' ', ',']).filter(|s| !s.is_empty()).count() == 4)
        .or_else(|| {
            let dim = |name: &str| {
                attrs
                    .iter()
                    .find(|(k, _)| k == name)
                    .and_then(|(_, v)| v.trim().trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '%').parse::<f32>().ok())
                    .filter(|n| *n > 0.0)
            };
            Some(format!("0 0 {} {}", dim("width")?, dim("height")?))
        })?;
    let inset = (px as f32 * pad).round();
    let inner = px as f32 - 2.0 * inset;
    let mut kept = String::new();
    for (k, v) in &attrs {
        if matches!(k.as_str(), "width" | "height" | "x" | "y" | "viewbox" | "preserveaspectratio")
            || (fill.is_some() && k == "fill")
        {
            continue;
        }
        kept.push(' ');
        kept.push_str(k);
        kept.push_str("=\"");
        kept.push_str(&v.replace('"', "&quot;"));
        kept.push('"');
    }
    let bg_rect = bg
        .map(|c| format!("<rect width=\"{px}\" height=\"{px}\" fill=\"{c}\"/>"))
        .unwrap_or_default();
    let fill_attr = fill.map(|f| format!(" fill=\"{f}\"")).unwrap_or_default();
    Some(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{px}\" height=\"{px}\" viewBox=\"0 0 {px} {px}\">\
         {bg_rect}<svg x=\"{inset}\" y=\"{inset}\" width=\"{inner}\" height=\"{inner}\" viewBox=\"{view_box}\" \
         preserveAspectRatio=\"xMidYMid meet\"{kept}{fill_attr}>{content}</svg></svg>"
    ))
}

/// Where the root `<svg` tag starts in a lowercased document, skipping the
/// XML declaration, comments and a doctype.
fn svg_root_start(lower: &str) -> Option<usize> {
    let mut at = 0;
    while let Some(i) = lower[at..].find("<svg") {
        let pos = at + i;
        let next = lower[pos + 4..].chars().next();
        if next.is_none_or(|c| c.is_whitespace() || c == '>' || c == '/') {
            return Some(pos);
        }
        at = pos + 4;
    }
    None
}

/// `name="value"` pairs of a tag (names lowercased and kept whole, so
/// `xmlns:xlink` survives).
fn parse_attrs(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = tag.trim();
    while !rest.is_empty() {
        let name_len = rest.find(|c: char| c == '=' || c.is_whitespace()).unwrap_or(rest.len());
        let name = rest[..name_len].trim().to_ascii_lowercase();
        rest = rest[name_len..].trim_start();
        let mut value = String::new();
        if let Some(r) = rest.strip_prefix('=') {
            let r = r.trim_start();
            if let Some(q) = r.chars().next().filter(|c| *c == '"' || *c == '\'') {
                let inner = &r[1..];
                let end = inner.find(q).unwrap_or(inner.len());
                value = inner[..end].to_string();
                rest = inner[end..].strip_prefix(q).unwrap_or("").trim_start();
            } else {
                let end = r.find(|c: char| c.is_whitespace()).unwrap_or(r.len());
                value = r[..end].to_string();
                rest = r[end..].trim_start();
            }
        }
        if !name.is_empty() {
            out.push((name, value));
        }
    }
    out
}

/// Whether these bytes are an SVG document (an `<svg` tag near the top).
fn looks_like_svg(bytes: &[u8]) -> bool {
    let head: String = bytes.iter().take(1024).map(|b| *b as char).collect::<String>().to_ascii_lowercase();
    svg_root_start(&head).is_some()
}

/// Where a site publishes its icon, largest first: what its home page and
/// web manifest declare, merged with the well-known paths (the root
/// `apple-touch-icon.png` counts as 180px, `favicon.ico` as 32px) — so a
/// declared 32px icon still loses to a root apple-touch-icon, and a declared
/// 512px one is tried before anything else.
fn candidate_urls(domain: &str) -> Vec<String> {
    let mut found = discover(domain);
    found.extend([
        (180, format!("https://{domain}/apple-touch-icon.png")),
        (180, format!("https://www.{domain}/apple-touch-icon.png")),
        (32, format!("https://{domain}/favicon.ico")),
        (32, format!("https://www.{domain}/favicon.ico")),
    ]);
    found.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out: Vec<String> = Vec::new();
    for (_, url) in found {
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

/// The icons a site's home page declares — its `<link rel="icon">`s and
/// `apple-touch-icon`s — and those in its web manifest, each with the size
/// the site claims for it. Empty when the page cannot be read.
fn discover(domain: &str) -> Vec<(u32, String)> {
    let Some((base, html)) = get_text(&format!("https://{domain}/"))
        .or_else(|| get_text(&format!("https://www.{domain}/")))
    else {
        return Vec::new();
    };
    let mut found = link_icons(&html, &base);
    if let Some(manifest) = link_manifest(&html, &base) {
        if let Some((mbase, json)) = get_text(&manifest) {
            found.extend(manifest_icons(&json, &mbase));
        }
    }
    found
}

/// The browser-ish identity sites see: plenty answer a bare library
/// identity with a challenge page instead of their icon.
pub(crate) const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux) Hylki";

/// A page or manifest as text, with the URL it was finally served from (so
/// relative links resolve against where redirects landed). Capped: the head
/// of a page is what matters, and a manifest is small.
fn get_text(url: &str) -> Option<(String, String)> {
    use std::io::Read;
    let resp = ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "text/html,application/manifest+json,application/json;q=0.9,*/*;q=0.5")
        .timeout(std::time::Duration::from_secs(5))
        .call()
        .ok()?;
    let final_url = resp.get_url().to_string();
    let mut buf = Vec::new();
    resp.into_reader().take(512 * 1024).read_to_end(&mut buf).ok()?;
    Some((final_url, String::from_utf8_lossy(&buf).into_owned()))
}

/// The `<link>` tags of a page, each as its attributes (names lowercased,
/// entity `&amp;` unescaped in values). A tolerant scan, not a parser: enough
/// for the `rel`/`href`/`sizes`/`type` of icon links.
fn link_tags(html: &str) -> Vec<Vec<(String, String)>> {
    let lower = html.to_ascii_lowercase();
    let mut tags = Vec::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find("<link") {
        let start = at + i + 5;
        let Some(len) = html[start..].find('>') else { break };
        let tag = &html[start..start + len];
        at = start + len;
        if !tag.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        let mut attrs = Vec::new();
        let mut rest = tag.trim();
        while !rest.is_empty() {
            let name_len = rest
                .find(|c: char| c == '=' || c.is_whitespace() || c == '/')
                .unwrap_or(rest.len());
            let name = rest[..name_len].to_ascii_lowercase();
            rest = rest[name_len..].trim_start();
            let mut value = String::new();
            if let Some(r) = rest.strip_prefix('=') {
                let r = r.trim_start();
                if let Some(q) = r.chars().next().filter(|c| *c == '"' || *c == '\'') {
                    let inner = &r[1..];
                    let end = inner.find(q).unwrap_or(inner.len());
                    value = inner[..end].to_string();
                    rest = inner[end..].strip_prefix(q).unwrap_or("").trim_start();
                } else {
                    let end = r.find(|c: char| c.is_whitespace()).unwrap_or(r.len());
                    value = r[..end].to_string();
                    rest = r[end..].trim_start();
                }
            } else {
                rest = rest.trim_start_matches('/').trim_start();
            }
            if !name.is_empty() {
                attrs.push((name, value.replace("&amp;", "&")));
            }
        }
        tags.push(attrs);
    }
    tags
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

/// The largest edge a `sizes` attribute claims ("32x32 192x192" → 192;
/// "any" or nothing → `None`).
fn largest_size(sizes: Option<&str>) -> Option<u32> {
    sizes?
        .split_whitespace()
        .filter_map(|s| s.split(['x', 'X']).next()?.parse::<u32>().ok())
        .max()
}

fn is_svg(href: &str, mime: Option<&str>) -> bool {
    mime.is_some_and(|t| t.to_ascii_lowercase().contains("svg"))
        || href.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase().ends_with(".svg")
}

/// The icon links a page declares, with their claimed (or assumed) sizes.
/// A vector icon counts as 256px: it is rasterised at logo size (see
/// `square_svg`), which beats any bitmap short of the manifest's big ones.
/// `mask-icon`s are monochrome silhouettes, not the brand.
fn link_icons(html: &str, base: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for attrs in link_tags(html) {
        let Some(href) = attr(&attrs, "href").map(str::trim).filter(|h| !h.is_empty()) else { continue };
        let rel = attr(&attrs, "rel").unwrap_or("").to_ascii_lowercase();
        let rels: Vec<&str> = rel.split_whitespace().collect();
        if rels.contains(&"mask-icon") {
            continue;
        }
        let claimed = largest_size(attr(&attrs, "sizes"));
        let size = if is_svg(href, attr(&attrs, "type")) && rels.iter().any(|r| *r == "icon" || r.starts_with("apple-touch-icon")) {
            256
        } else if rels.iter().any(|r| r.starts_with("apple-touch-icon")) {
            claimed.unwrap_or(180)
        } else if rels.contains(&"fluid-icon") {
            claimed.unwrap_or(128)
        } else if rels.contains(&"icon") {
            claimed.unwrap_or(48)
        } else {
            continue;
        };
        if let Some(url) = resolve_url(base, href) {
            out.push((size, url));
        }
    }
    out
}

/// The page's web manifest, if it links one.
fn link_manifest(html: &str, base: &str) -> Option<String> {
    link_tags(html).into_iter().find_map(|attrs| {
        let rel = attr(&attrs, "rel")?.to_ascii_lowercase();
        if !rel.split_whitespace().any(|r| r == "manifest") {
            return None;
        }
        resolve_url(base, attr(&attrs, "href")?.trim())
    })
}

/// The icons a web manifest lists, with their claimed sizes.
fn manifest_icons(json: &str, base: &str) -> Vec<(u32, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return Vec::new() };
    v["icons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|icon| {
            let src = icon["src"].as_str()?.trim();
            if src.is_empty() {
                return None;
            }
            let size = if is_svg(src, icon["type"].as_str()) {
                256
            } else {
                largest_size(icon["sizes"].as_str()).unwrap_or(48)
            };
            Some((size, resolve_url(base, src)?))
        })
        .collect()
}

/// `href` as seen from `base` (an absolute URL): absolute, scheme-relative,
/// root-relative or relative to the base's directory. Only `http(s)`.
fn resolve_url(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Some(href.to_string());
    }
    if lower.starts_with("//") {
        return Some(format!("https:{href}"));
    }
    if href.contains(':') && !href.starts_with('/') && !href.starts_with('.') {
        // data:, mailto: and the like.
        return None;
    }
    let scheme_end = base.find("://")? + 3;
    let host_end = base[scheme_end..].find('/').map(|i| scheme_end + i).unwrap_or(base.len());
    let origin = &base[..host_end];
    if let Some(rest) = href.strip_prefix('/') {
        return Some(format!("{origin}/{rest}"));
    }
    let path = &base[host_end..];
    let dir = match path.rfind('/') {
        Some(i) => &path[..=i],
        None => "/",
    };
    let mut segments: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let mut tail = href;
    loop {
        if let Some(r) = tail.strip_prefix("../") {
            segments.pop();
            tail = r;
        } else if let Some(r) = tail.strip_prefix("./") {
            tail = r;
        } else {
            break;
        }
    }
    let mut url = format!("{origin}/");
    for seg in segments {
        url.push_str(seg);
        url.push('/');
    }
    url.push_str(tail);
    Some(url)
}

fn get(url: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let resp = ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .timeout(std::time::Duration::from_secs(5))
        .call()
        .ok()?;
    // Some sites answer every icon path with their home page and a 200 — pm.me
    // sends 347KB of HTML for `/favicon.ico`. Ask what it is before reading it.
    if !is_image_type(resp.content_type()) {
        return None;
    }
    let mut buf = Vec::new();
    resp.into_reader()
        .take(1_000_000)
        .read_to_end(&mut buf)
        .ok()?;
    // And a backstop for the ones that mislabel it.
    (!buf.is_empty() && !looks_like_markup(&buf)).then_some(buf)
}

/// Whether a response claims to be an image. An empty or unknown type is
/// allowed through — plenty of servers send `application/octet-stream` for an
/// `.ico`, and the bytes are checked either way.
fn is_image_type(content_type: &str) -> bool {
    let ct = content_type.trim().to_ascii_lowercase();
    let ct = ct.split(';').next().unwrap_or("").trim();
    ct.is_empty() || ct.starts_with("image/") || ct == "application/octet-stream"
}

/// Whether these bytes are a web page rather than an image — some sites answer a
/// missing icon with their home page and a 200.
fn looks_like_markup(bytes: &[u8]) -> bool {
    let head: String = bytes
        .iter()
        .take(64)
        .map(|b| *b as char)
        .collect::<String>()
        .trim_start()
        .to_ascii_lowercase();
    if looks_like_svg(bytes) {
        return false;
    }
    head.starts_with("<!doctype") || head.starts_with("<html") || head.starts_with("<?xml")
}

/// Decode icon bytes into a texture and cache them under the sender's domain.
///
/// `GdkTexture` reads PNG and JPEG; an `.ico` needs GdkPixbuf, which the platform
/// supplies loaders for. Failing to decode is remembered as a miss, so a domain
/// serving something unreadable is not asked on every row.
pub fn decode_and_cache(email: &str, bytes: &[u8]) -> Option<gtk::gdk::Texture> {
    let domain = domain_of(email)?;
    let tex = decode(bytes);
    match tex {
        Some(tex) => {
            CACHE.with(|c| {
                c.borrow_mut().insert(domain, tex.clone());
            });
            Some(tex)
        }
        None => {
            // Undecodable bytes: drop the stored copies, and — unless a
            // bundled mark stands in — persist the miss so the next
            // session doesn't fetch and fail to decode them again.
            if let Some(p) = img_path(&domain) {
                let _ = std::fs::remove_file(p);
            }
            if let Some(p) = bimi_path(&domain) {
                let _ = std::fs::write(p, b"");
            }
            if bundled_entry(email).is_none() {
                if let Some(p) = miss_path(&domain) {
                    let _ = std::fs::write(p, b"");
                }
                remember_miss(&domain);
            }
            None
        }
    }
}

/// Remember that a sender's domain has no usable icon.
pub fn remember_missing(email: &str) {
    if let Some(domain) = domain_of(email) {
        remember_miss(&domain);
    }
}

fn remember_miss(domain: &str) {
    MISSES.with(|m| {
        m.borrow_mut().insert(domain.to_string());
    });
}

fn decode(bytes: &[u8]) -> Option<gtk::gdk::Texture> {
    use gtk::gdk_pixbuf::prelude::*;
    use std::cell::Cell;
    use std::rc::Rc;

    if looks_like_svg(bytes) {
        return decode_svg(bytes);
    }

    // These textures live in the session-long cache above and are drawn at
    // avatar size, but sites publish `apple-touch-icon`s at up to 1024² — a
    // few MB of decoded pixels each, held forever per domain (issue #106).
    // Downscale during decode, exactly as avatars do; the pixel limit also
    // rejects decompression bombs. Going through a size-prepared PixbufLoader
    // covers PNG, JPEG and ICO alike.
    const MAX_PIXELS: i64 = 4_194_304;
    const THUMBNAIL_EDGE: i32 = 160;
    let loader = gtk::gdk_pixbuf::PixbufLoader::new();
    let valid = Rc::new(Cell::new(false));
    loader.connect_size_prepared({
        let valid = valid.clone();
        move |loader, width, height| {
            if width <= 0 || height <= 0 || i64::from(width) * i64::from(height) > MAX_PIXELS {
                loader.set_size(1, 1);
                return;
            }
            valid.set(true);
            let longest = width.max(height);
            if longest > THUMBNAIL_EDGE {
                loader.set_size(
                    (width * THUMBNAIL_EDGE / longest).max(1),
                    (height * THUMBNAIL_EDGE / longest).max(1),
                );
            }
        }
    });
    if loader.write(bytes).is_err() {
        let _ = loader.close();
        return None;
    }
    loader.close().ok()?;
    if !valid.get() {
        return None;
    }
    let pixbuf = loader.pixbuf()?;
    Some(gtk::gdk::Texture::for_pixbuf(&pixbuf))
}

/// An SVG rasterised at logo size: framed to a `LOGO_PX` square first, so
/// the renderer has a definite size whatever the document declares (a
/// bare `viewBox`, `100%`, or a nominal 16px), then loaded as a texture.
fn decode_svg(bytes: &[u8]) -> Option<gtk::gdk::Texture> {
    if bytes.len() > 512 * 1024 {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let framed = square_svg(text, LOGO_PX, None, 0.0, None)?;
    let data = gtk::glib::Bytes::from_owned(framed.into_bytes());
    match gtk::gdk::Texture::from_bytes(&data) {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::debug!("logo svg not rendered: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brand_domain_is_found_under_its_sending_subdomains() {
        assert_eq!(domain_of("no-reply@apple.com").as_deref(), Some("apple.com"));
        assert_eq!(
            domain_of("news@mail.notifications.apple.com").as_deref(),
            Some("apple.com")
        );
        assert_eq!(
            domain_of("bounces+42933322-387e@em6917.cloudways.com").as_deref(),
            Some("cloudways.com")
        );
        // Country-code pairs keep three labels.
        assert_eq!(domain_of("a@mail.bbc.co.uk").as_deref(), Some("bbc.co.uk"));
        assert_eq!(domain_of("a@shop.example.com.au").as_deref(), Some("example.com.au"));
    }

    #[test]
    fn declared_icons_are_found_and_ranked_by_size() {
        let html = r##"<html><head>
            <link rel='stylesheet' href='/style.css'>
            <link rel="icon" href="/img/favicon-32x32.png" sizes="32x32" />
            <LINK REL="icon" HREF="//cdn.example.com/icon-192.png?v=2&amp;x=1" SIZES="192x192">
            <link rel="apple-touch-icon" href="touch.png">
            <link rel="icon" type="image/svg+xml" href="/icon.svg">
            <link rel="mask-icon" href="/pin.png" color="#000">
            <link rel="manifest" href="../site.webmanifest">
            </head></html>"##;
        let base = "https://www.example.com/a/b/index.html";
        let icons = link_icons(html, base);
        assert_eq!(
            icons,
            vec![
                (32, "https://www.example.com/img/favicon-32x32.png".to_string()),
                (192, "https://cdn.example.com/icon-192.png?v=2&x=1".to_string()),
                (180, "https://www.example.com/a/b/touch.png".to_string()),
                // A vector icon is rasterised at logo size, so it ranks high.
                (256, "https://www.example.com/icon.svg".to_string()),
            ]
        );
        assert_eq!(link_manifest(html, base).as_deref(), Some("https://www.example.com/a/site.webmanifest"));
        let manifest = r#"{"icons":[{"src":"/i/512.png","sizes":"512x512","type":"image/png"},
                                     {"src":"v.svg","sizes":"any","type":"image/svg+xml"},
                                     {"src":"i/any.png","sizes":"any"}]}"#;
        assert_eq!(
            manifest_icons(manifest, "https://www.example.com/a/site.webmanifest"),
            vec![
                (512, "https://www.example.com/i/512.png".to_string()),
                (256, "https://www.example.com/a/v.svg".to_string()),
                (48, "https://www.example.com/a/i/any.png".to_string()),
            ]
        );
        assert_eq!(largest_size(Some("16x16 48x48 32x32")), Some(48));
        assert_eq!(resolve_url("https://example.com", "favicon.ico").as_deref(), Some("https://example.com/favicon.ico"));
        assert_eq!(resolve_url("https://example.com/", "data:image/png;base64,AAAA"), None);
    }

    #[test]
    fn addresses_with_no_domain_to_ask_are_skipped() {
        assert_eq!(domain_of(""), None);
        assert_eq!(domain_of("someone"), None);
        assert_eq!(domain_of("someone@localhost"), None);
        // An IP literal is nobody's brand.
        assert_eq!(domain_of("a@192.168.1.1"), None);
        // Nothing to look up is treated as already answered, so no fetch is made.
        assert!(known_missing("someone"));
    }

    #[test]
    fn only_images_are_read() {
        assert!(is_image_type("image/png"));
        assert!(is_image_type("image/vnd.microsoft.icon; charset=utf-8"));
        assert!(is_image_type("image/x-icon"));
        // Servers that don't know what an .ico is get the benefit of the doubt;
        // the bytes are sniffed anyway.
        assert!(is_image_type(""));
        assert!(is_image_type("application/octet-stream"));
        // A home page is not an icon, and is not worth downloading to find out.
        assert!(!is_image_type("text/html; charset=utf-8"));
        assert!(!is_image_type("application/json"));
    }

    #[test]
    fn a_bimi_record_names_its_logo() {
        assert_eq!(
            parse_bimi_record("v=BIMI1; l=https://www.apple.com/bimi/v2/apple.svg;a=https://www.apple.com/bimi/v2/apple.pem;").as_deref(),
            Some("https://www.apple.com/bimi/v2/apple.svg")
        );
        // Tags in any order, spacing free.
        assert_eq!(parse_bimi_record("l=https://x.example/l.svg;v=bimi1").as_deref(), Some("https://x.example/l.svg"));
        // A declared opt-out, a plain-http location, an SPF record.
        assert_eq!(parse_bimi_record("v=BIMI1; l=;"), None);
        assert_eq!(parse_bimi_record("v=BIMI1; l=http://x.example/l.svg"), None);
        assert_eq!(parse_bimi_record("v=spf1 include:spf.example ~all"), None);
    }

    #[test]
    fn an_svg_is_reframed_to_a_square_and_keeps_its_root_attributes() {
        let glyph = r##"<svg role="img" viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><title>PayPal</title><path d="M1 1h22v22H1z"/></svg>"##;
        let out = square_svg(glyph, 160, Some("#002991"), 0.22, Some("#ffffff")).unwrap();
        assert!(out.starts_with(r##"<svg xmlns="http://www.w3.org/2000/svg" width="160" height="160" viewBox="0 0 160 160">"##), "{out}");
        assert!(out.contains(r##"<rect width="160" height="160" fill="#002991"/>"##), "{out}");
        assert!(out.contains(r##"<svg x="35" y="35" width="90" height="90" viewBox="0 0 24 24" preserveAspectRatio="xMidYMid meet" role="img" xmlns="http://www.w3.org/2000/svg" fill="#ffffff">"##), "{out}");
        assert!(out.ends_with(r##"<path d="M1 1h22v22H1z"/></svg></svg>"##), "{out}");
        // A prologue, a doctype and px-suffixed dimensions in place of a viewBox.
        let mark = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><!DOCTYPE svg><svg width=\"256px\" height=\"128px\" xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\"><rect width=\"256\" height=\"128\"/></svg>";
        let out = square_svg(mark, 160, Some("#ffffff"), 0.0, None).unwrap();
        assert!(out.contains(r##"viewBox="0 0 256 128""##), "{out}");
        assert!(out.contains(r##"xmlns:xlink="http://www.w3.org/1999/xlink""##), "{out}");
        // Not an SVG at all.
        assert_eq!(square_svg("<html><body>no</body></html>", 160, None, 0.0, None), None);
        assert!(looks_like_svg(b"<?xml version=\"1.0\"?>\n<!-- x -->\n<svg viewBox=\"0 0 1 1\"/>"));
        assert!(!looks_like_svg(b"<svgfoo>"));
    }

    #[test]
    fn the_bundled_map_knows_common_senders_by_host_and_domain() {
        let paypal = bundled_entry("service@paypal.com").expect("paypal is bundled");
        assert_eq!(paypal.source, "simple");
        assert_eq!(paypal.file, "paypal.svg");
        assert!(paypal.color.is_some());
        // The sending host wins over the registrable domain when both are listed.
        assert_eq!(bundled_entry("no-reply@aws.amazon.com").map(|e| e.file.as_str()), Some("aws.svg"));
        // A subdomain falls back to the registrable domain's mark.
        assert_eq!(bundled_entry("noreply@mail.spotify.com").map(|e| e.source.as_str()), Some("gilbarbara"));
        // The app's own service marks cover the mail providers.
        assert!(bundled_entry("someone@example.org").is_none());
        assert!(has_bundled("x@paypal.com") && !known_missing("x@paypal.com"));
    }

    /// A person on a freemail provider is a person, not the provider: no
    /// mark from any source, and nothing to fetch or refresh.
    #[test]
    fn mailbox_hosts_get_no_logo() {
        for addr in [
            "jane@gmail.com",
            "jane.doe@googlemail.com",
            "bob@hotmail.co.uk",
            "bob@outlook.com",
            "sam@yahoo.co.uk",
            "sam@icloud.com",
            "kim@proton.me",
            "lee@qq.com",
            "ann@mail.ru",
            "joe@comcast.net",
        ] {
            assert!(is_mailbox_host(addr), "{addr} should be a mailbox host");
            assert!(!has_bundled(addr), "{addr} should get no bundled mark");
            assert!(known_missing(addr), "{addr} should count as answered");
            assert!(!wants_refresh(addr), "{addr} should never be refreshed");
        }
    }

    /// The brands themselves keep their marks — it is the mailbox hosts,
    /// not the companies behind them, that are excluded.
    #[test]
    fn brands_behind_mailbox_hosts_keep_their_marks() {
        for addr in ["no-reply@google.com", "news@apple.com", "billing@microsoft.com"] {
            assert!(!is_mailbox_host(addr), "{addr} is the brand, not a mailbox");
            assert!(has_bundled(addr), "{addr} should keep its bundled mark");
        }
        // A subdomain still resolves to the brand.
        assert!(!is_mailbox_host("alerts@mail.google.com"));
    }

    #[test]
    fn a_home_page_served_in_place_of_an_icon_is_rejected() {
        assert!(looks_like_markup(b"<!DOCTYPE html><html>"));
        assert!(looks_like_markup(b"  <html lang=\"en\">"));
        // An XML prologue is a page unless an SVG follows it.
        assert!(looks_like_markup(b"<?xml version=\"1.0\"?><html>"));
        assert!(!looks_like_markup(b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>"));
        assert!(!looks_like_markup(b"\x89PNG\r\n\x1a\n"));
        assert!(!looks_like_markup(b"\x00\x00\x01\x00"));
    }
}
