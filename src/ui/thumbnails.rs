//! Attachment thumbnails: decoding images and rendering a PDF's first page
//! off the main thread, the budgeted render caches, and the type icons
//! shown for files without a thumbnail. Shared with the composer's
//! attachment drawer, the lightbox and printing.

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::models::is_image_name;

/// A `gdk::Texture` from raw image bytes, or `None` if the format isn't loadable.
pub(super) fn texture_from(data: &[u8]) -> Option<gdk::Texture> {
    gdk::Texture::from_bytes(&glib::Bytes::from(data)).ok()
}

/// What a grid cell can show for an attachment right now.
pub(super) enum Thumbnail {
    /// A texture is available immediately: a decoded image, or a PDF page
    /// already in the render cache.
    Ready(gdk::Texture),
    /// An image or PDF not decoded/rendered yet. Show a spinner and call
    /// [`spawn_thumbnail_render`] — decoding on the main thread while cells
    /// were being built froze the whole window (the "Force Quit" dialog).
    Pending,
    /// No thumbnail for this type (or it wouldn't decode): type icon.
    Fallback,
}

/// Rendered textures by content hash, under a pixel-byte budget: the oldest
/// go when a new one would exceed it. Failures (`None`) cost nothing and
/// stay, so an attachment that would not decode is not tried again. A
/// user's log showed 62 thumbnails holding 116 MB of pixels twelve minutes
/// into a session; the deep archive would have kept adding to that for as
/// long as they scrolled.
pub(super) struct TextureCache {
    map: std::collections::HashMap<u64, Option<gdk::Texture>>,
    /// Insertion order of the rendered entries, oldest first.
    order: std::collections::VecDeque<u64>,
    bytes: u64,
    budget: u64,
}

impl TextureCache {
    fn new(budget: u64) -> Self {
        TextureCache {
            map: Default::default(),
            order: Default::default(),
            bytes: 0,
            budget,
        }
    }

    pub(super) fn get(&self, key: &u64) -> Option<&Option<gdk::Texture>> {
        self.map.get(key)
    }

    fn insert(&mut self, key: u64, tex: Option<gdk::Texture>) {
        if let Some(Some(old)) = self.map.insert(key, tex.clone()) {
            self.bytes -= crate::desktop::memory_report::texture_bytes(&old);
            self.order.retain(|k| *k != key);
        }
        if let Some(tex) = tex {
            self.bytes += crate::desktop::memory_report::texture_bytes(&tex);
            self.order.push_back(key);
        }
        // Keep at least the newest entry even when it alone busts the budget.
        while self.bytes > self.budget && self.order.len() > 1 {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(Some(old)) = self.map.remove(&oldest) {
                self.bytes -= crate::desktop::memory_report::texture_bytes(&old);
            }
        }
    }

    /// (entries, of which rendered, pixel bytes), for the memory section.
    fn stats(&self) -> (usize, usize, u64) {
        (self.map.len(), self.order.len(), self.bytes)
    }
}

/// Pixel bytes the thumbnail cache may hold: 360px-wide renders, so a few
/// hundred of them.
const THUMB_CACHE_BUDGET: u64 = 96 * 1024 * 1024;
/// Pixel bytes the lightbox PDF cache may hold: 1600px-wide pages, so a
/// dozen or so.
const PDF_PREVIEW_BUDGET: u64 = 64 * 1024 * 1024;

thread_local! {
    /// Finished thumbnail renders — decoded images and PDF pages alike,
    /// successes and failures, keyed by content hash — so a gallery rebuild or
    /// a revisit this session never decodes the same attachment twice.
    /// Main-thread only; results land here from `spawn_thumbnail_render`.
    static THUMB_CACHE: std::cell::RefCell<TextureCache> =
        std::cell::RefCell::new(TextureCache::new(THUMB_CACHE_BUDGET));
    /// Lightbox-size PDF page renders, cached separately from the thumbnails
    /// (same key, much bigger pixels). Failures cache too.
    pub(super) static PDF_PREVIEWS: std::cell::RefCell<TextureCache> =
        std::cell::RefCell::new(TextureCache::new(PDF_PREVIEW_BUDGET));
}

/// The session's render caches, for the memory section of an export:
/// thumbnails (entries, of which rendered, pixel bytes) then the lightbox
/// PDF pages the same way.
pub(crate) fn cache_stats() -> ((usize, usize, u64), (usize, usize, u64)) {
    (
        THUMB_CACHE.with(|c| c.borrow().stats()),
        PDF_PREVIEWS.with(|c| c.borrow().stats()),
    )
}

#[cfg(test)]
mod texture_cache_tests {
    use super::*;

    fn texture(px: i32) -> gdk::Texture {
        let bytes = glib::Bytes::from_owned(vec![0u8; (px * px * 4) as usize]);
        gdk::MemoryTexture::new(
            px,
            px,
            gdk::MemoryFormat::R8g8b8a8,
            &bytes,
            (px * 4) as usize,
        )
        .upcast()
    }

    #[test]
    fn evicts_the_oldest_render_under_the_budget() {
        // 100×100 RGBA = 40,000 bytes each; room for two.
        let mut c = TextureCache::new(90_000);
        c.insert(1, Some(texture(100)));
        c.insert(2, None); // a failure costs nothing
        c.insert(3, Some(texture(100)));
        assert_eq!(c.stats(), (3, 2, 80_000));
        c.insert(4, Some(texture(100)));
        assert!(c.get(&1).is_none(), "the oldest render went");
        assert!(c.get(&2).is_some(), "failures stay");
        assert!(c.get(&3).is_some() && c.get(&4).is_some());
        assert_eq!(c.stats(), (3, 2, 80_000));
    }

    #[test]
    fn a_cached_failure_is_not_a_miss() {
        let (failed, rendered, unseen) = (&b"locked"[..], &b"fine"[..], &b"unseen"[..]);
        PDF_PREVIEWS.with(|c| {
            let mut c = c.borrow_mut();
            c.insert(thumb_cache_key(failed), None);
            c.insert(thumb_cache_key(rendered), Some(texture(1)));
        });
        assert!(matches!(cached_pdf_preview(failed), Some(None)));
        assert!(matches!(cached_pdf_preview(rendered), Some(Some(_))));
        assert!(cached_pdf_preview(unseen).is_none());
    }

    #[test]
    fn a_single_render_over_budget_is_still_kept() {
        let mut c = TextureCache::new(1_000);
        c.insert(1, Some(texture(100)));
        assert!(c.get(&1).is_some());
        c.insert(2, Some(texture(100)));
        assert!(c.get(&1).is_none() && c.get(&2).is_some());
    }
}

/// Whether a filename names a PDF (by extension).
pub(super) fn is_pdf_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".pdf")
}

/// The content hash the thumbnail/preview caches key on — for callers that
/// need to correlate an async render with the item it belongs to.
pub(super) fn content_key(data: &[u8]) -> u64 {
    thumb_cache_key(data)
}

/// What the preview cache holds for a PDF: `None` while it has not been
/// rendered, `Some(None)` once poppler could not open it, such as a PDF locked
/// with a password. Callers must tell those apart: asking for a render of a
/// failure answers at once, so treating it as "not rendered yet" never ends
/// (#582).
pub(super) fn cached_pdf_preview(data: &[u8]) -> Option<Option<gdk::Texture>> {
    PDF_PREVIEWS.with(|c| c.borrow().get(&thumb_cache_key(data)).cloned())
}

/// A lightbox-size render of a PDF's first page: handed to `on_done` on the
/// main thread — at once from the cache, or after a worker renders it.
/// Failures cache too, so a broken PDF is rendered at most once.
pub(super) fn lightbox_pdf_texture(
    data: &[u8],
    on_done: impl FnOnce(Option<gdk::Texture>) + 'static,
) {
    let key = thumb_cache_key(data);
    if let Some(cached) = PDF_PREVIEWS.with(|c| c.borrow().get(&key).cloned()) {
        on_done(cached);
        return;
    }
    let data = data.to_vec();
    glib::spawn_future_local(async move {
        let tex = gtk::gio::spawn_blocking(move || pdf_page_texture(&data, PREVIEW_RENDER_WIDTH))
            .await
            .ok()
            .flatten();
        PDF_PREVIEWS.with(|c| {
            c.borrow_mut().insert(key, tex.clone());
        });
        on_done(tex);
    });
}

pub(super) fn thumb_cache_key(data: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    data.len().hash(&mut h);
    data.hash(&mut h);
    h.finish()
}

/// A grid-cell thumbnail for any attachment type that has one — a decoded
/// image or a PDF's first page — answered from the render cache, or `Pending`
/// until a worker produces it.
pub(super) fn thumbnail_texture(name: &str, data: &[u8]) -> Thumbnail {
    if !is_image_name(name) && !is_pdf_name(name) {
        return Thumbnail::Fallback;
    }
    match THUMB_CACHE.with(|c| c.borrow().get(&thumb_cache_key(data)).cloned()) {
        Some(Some(tex)) => Thumbnail::Ready(tex),
        Some(None) => Thumbnail::Fallback,
        None => Thumbnail::Pending,
    }
}

/// Decode an image, or render a PDF's first page, off the main thread; hand
/// the result to `on_done` back on it and record it in the cache either way.
/// The GTK loop keeps running — cells show their spinner instead of the
/// window freezing.
pub(super) fn spawn_thumbnail_render(
    name: &str,
    data: Vec<u8>,
    on_done: impl FnOnce(Option<gdk::Texture>) + 'static,
) {
    let key = thumb_cache_key(&data);
    let image = is_image_name(name);
    glib::spawn_future_local(async move {
        let tex = gtk::gio::spawn_blocking(move || {
            if image {
                texture_from(&data)
            } else {
                pdf_page_texture(&data, THUMB_RENDER_WIDTH)
            }
        })
        .await
        .ok()
        .flatten();
        THUMB_CACHE.with(|c| {
            c.borrow_mut().insert(key, tex.clone());
        });
        on_done(tex);
    });
}

/// The centred spinner a cell shows while its PDF page renders.
pub(super) fn thumbnail_spinner() -> gtk::Spinner {
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    spinner.set_width_request(28);
    spinner.set_height_request(28);
    spinner.set_halign(gtk::Align::Center);
    spinner.set_valign(gtk::Align::Center);
    spinner.set_hexpand(true);
    spinner.set_vexpand(true);
    spinner
}

/// Width thumbnails render at — soft would do, but sharp is cheap at 360px.
const THUMB_RENDER_WIDTH: f64 = 360.0;
/// Width the lightbox renders a PDF page at: sharp on a large window without
/// tripping the decoder's pixel ceiling (A4 portrait at 1600 ≈ 3.6M pixels).
const PREVIEW_RENDER_WIDTH: f64 = 1600.0;

/// One PDF render at a time, process-wide: every poppler call holds this.
/// Poppler's color management (lcms2) shares state across documents: two
/// thumbnail threads rendering concurrently crashed with heap corruption, one
/// thread tearing down its Gfx (cmsCloseProfile) while the other still
/// rendered. Printing (#359) renders with poppler too, on the main thread.
pub(crate) fn pdf_render_lock() -> std::sync::MutexGuard<'static, ()> {
    static PDF_RENDER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    PDF_RENDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Render a PDF's first page to a texture at `target_width`, by way of an
/// in-memory PNG — the same route every other thumbnail here already goes
/// through, so cropping, caching, and format all stay uniform.
pub(super) fn pdf_page_texture(data: &[u8], target_width: f64) -> Option<gdk::Texture> {
    let _guard = pdf_render_lock();
    let doc = poppler::Document::from_bytes(&glib::Bytes::from(data), None).ok()?;
    let page = doc.page(0)?;
    let (w, h) = page.size();
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    // Poppler's default is one pixel per point (72 dpi) — fine for text but
    // soft on screen, so render at the caller's width instead.
    let scale = target_width / w;
    let surface = gtk::cairo::ImageSurface::create(
        gtk::cairo::Format::ARgb32,
        target_width.round() as i32,
        (h * scale).round() as i32,
    )
    .ok()?;
    let cr = gtk::cairo::Context::new(&surface).ok()?;
    // A PDF page's own background is transparent; without painting white first,
    // the thumbnail would show through to whatever is behind it (a hole in
    // dark mode rather than a page).
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.paint().ok()?;
    cr.scale(scale, scale);
    page.render(&cr);
    drop(cr);
    let mut png = Vec::new();
    surface.write_to_png(&mut png).ok()?;
    texture_from(&png)
}

/// A symbolic icon name for a filename by extension.
pub(super) fn icon_for(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "pdf" => "x-office-document-symbolic",
        "doc" | "docx" | "odt" | "rtf" | "txt" | "md" => "x-office-document-symbolic",
        "xls" | "xlsx" | "ods" | "csv" => "x-office-spreadsheet-symbolic",
        "ppt" | "pptx" | "odp" => "x-office-presentation-symbolic",
        "zip" | "gz" | "tar" | "7z" | "rar" | "xz" | "bz2" => "package-x-generic-symbolic",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => "audio-x-generic-symbolic",
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => "video-x-generic-symbolic",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "heif" | "avif" | "ico" => {
            "image-x-generic-symbolic"
        }
        "ics" => "x-office-calendar-symbolic",
        _ => "text-x-generic-symbolic",
    }
}

/// The GNOME palette color each tinted file kind wears (#651): shade 1 on a
/// dark ground, shade 5 on a light one, the only steps at which every hue
/// keeps 3:1 against its card. Yellow holds at no step on white, so archives
/// are brown; the palette has fewer hues than kinds, so audio and video share
/// purple and a calendar shares blue. A PDF wears the error color and an image
/// the grey of everything else (styles.css). The GTK side names the palette's
/// variables ([`ftype_css`]); the reader, which has no libadwaita stylesheet,
/// takes the hex values.
pub(crate) const FTYPE_PALETTE: [FtypeColor; 7] = [
    FtypeColor::new("doc", "blue", "#99c1f1", "#1a5fb4"),
    FtypeColor::new("sheet", "green", "#8ff0a4", "#26a269"),
    FtypeColor::new("slides", "orange", "#ffbe6f", "#c64600"),
    FtypeColor::new("archive", "brown", "#cdab8f", "#63452c"),
    FtypeColor::new("audio", "purple", "#dc8add", "#613583"),
    FtypeColor::new("video", "purple", "#dc8add", "#613583"),
    FtypeColor::new("calendar", "blue", "#99c1f1", "#1a5fb4"),
];

/// One row of [`FTYPE_PALETTE`]: the `ftype-` class suffix, the palette hue,
/// and that hue's shade 1 and shade 5.
pub(crate) struct FtypeColor {
    pub kind: &'static str,
    pub hue: &'static str,
    pub dark: &'static str,
    pub light: &'static str,
}

impl FtypeColor {
    const fn new(
        kind: &'static str,
        hue: &'static str,
        dark: &'static str,
        light: &'static str,
    ) -> Self {
        Self {
            kind,
            hue,
            dark,
            light,
        }
    }
}

/// The `.ftype-*` tints for the app's scheme provider, which reloads on a
/// scheme flip: static CSS cannot pick a palette step by scheme. The
/// lightbox sits on a dark ground in either scheme, so it always takes
/// shade 1.
pub(crate) fn ftype_css(dark: bool) -> String {
    let shade = if dark { 1 } else { 5 };
    let mut css = String::new();
    for FtypeColor { kind, hue, .. } in FTYPE_PALETTE {
        css.push_str(&format!(
            ".ftype-{kind} {{ color: var(--{hue}-{shade}); }}\
             .gallery-lightbox .ftype-{kind} {{ color: var(--{hue}-1); }}"
        ));
    }
    css
}

/// CSS class that tints a type icon by file kind: PDFs red, Word docs blue,
/// spreadsheets green, and so on (see `styles.css`). Symbolic icons pick up the
/// class's `color`, so an unthumbnailed attachment reads at a glance.
pub(super) fn icon_color_class(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "pdf" => "ftype-pdf",
        "doc" | "docx" | "odt" | "rtf" => "ftype-doc",
        "xls" | "xlsx" | "ods" | "csv" => "ftype-sheet",
        "ppt" | "pptx" | "odp" => "ftype-slides",
        "zip" | "gz" | "tar" | "7z" | "rar" | "xz" | "bz2" => "ftype-archive",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => "ftype-audio",
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => "ftype-video",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "heif" | "avif" | "ico" => {
            "ftype-image"
        }
        "ics" => "ftype-calendar",
        _ => "ftype-generic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal one-page PDF, built by hand — just enough structure for
    /// poppler to load and render, with no dependency on an external file.
    fn minimal_pdf() -> Vec<u8> {
        let content = b"1 0 0 rg 100 100 300 300 re f";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] /Contents 4 0 R >>".to_string(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                std::str::from_utf8(content).unwrap()
            ),
        ];
        let mut out = Vec::new();
        out.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = vec![0usize];
        for (i, obj) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(obj.as_bytes());
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets[1..] {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
                objects.len() + 1,
                xref_offset
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn pdf_thumbnail_renders_the_first_page() {
        let tex = pdf_page_texture(&minimal_pdf(), 360.0).expect("should render a page");
        assert!(tex.width() > 0 && tex.height() > 0);
        assert!(pdf_page_texture(b"not a real pdf", 360.0).is_none());
    }

    #[test]
    fn thumbnail_texture_classifies_attachment_types() {
        // No thumbnail for a type without one; an uncached PDF is rendered
        // asynchronously, so the immediate answer is Pending.
        assert!(matches!(
            thumbnail_texture("archive.zip", b"not a real zip"),
            Thumbnail::Fallback
        ));
        assert!(matches!(
            thumbnail_texture("notes.pdf", &minimal_pdf()),
            Thumbnail::Pending
        ));
    }

    #[test]
    fn type_icons_follow_the_extension() {
        assert_eq!(icon_for("report.PDF"), "x-office-document-symbolic");
        assert_eq!(icon_for("notes.md"), "x-office-document-symbolic");
        assert_eq!(icon_for("budget.csv"), "x-office-spreadsheet-symbolic");
        assert_eq!(icon_for("deck.odp"), "x-office-presentation-symbolic");
        assert_eq!(icon_for("a.tar.gz"), "package-x-generic-symbolic");
        assert_eq!(icon_for("song.flac"), "audio-x-generic-symbolic");
        assert_eq!(icon_for("clip.webm"), "video-x-generic-symbolic");
        assert_eq!(icon_for("photo.HEIC"), "image-x-generic-symbolic");
        assert_eq!(icon_for("invite.ics"), "x-office-calendar-symbolic");
        assert_eq!(icon_for("data.bin"), "text-x-generic-symbolic");
        // No dot: the whole name is taken as the extension.
        assert_eq!(icon_for("pdf"), "x-office-document-symbolic");
        assert_eq!(icon_for("README"), "text-x-generic-symbolic");
    }

    #[test]
    fn icon_color_class_maps_types() {
        assert_eq!(icon_color_class("report.pdf"), "ftype-pdf");
        assert_eq!(icon_color_class("Notes.DOCX"), "ftype-doc"); // case-insensitive
        assert_eq!(icon_color_class("budget.xlsx"), "ftype-sheet");
        assert_eq!(icon_color_class("deck.pptx"), "ftype-slides");
        assert_eq!(icon_color_class("bundle.zip"), "ftype-archive");
        assert_eq!(icon_color_class("song.mp3"), "ftype-audio");
        assert_eq!(icon_color_class("clip.mov"), "ftype-video");
        assert_eq!(icon_color_class("invite.ics"), "ftype-calendar");
        assert_eq!(icon_color_class("photo.png"), "ftype-image");
        assert_eq!(icon_color_class("data.bin"), "ftype-generic");
        assert_eq!(icon_color_class("noext"), "ftype-generic");
    }
}
