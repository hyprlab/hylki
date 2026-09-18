//! The initials circle a sender gets when no picture is known: drawn here
//! rather than by `adw::Avatar`'s label, so the letters sit centred by their
//! ink. A label centres its *logical* box, and a glyph's ink rarely fills
//! that box evenly — a lone "J" or "T" drifts, and pairs lean — which is
//! what this paintable corrects, at every size it is drawn at.
//!
//! Colours are libadwaita's own avatar palette (the same fourteen gradients
//! `adw::Avatar` picks from, chosen by the same hash of the name), so a
//! sender keeps the colour they have always had.
//!
//! The same drawing serves the sidebar's account circles (a glyph alone
//! over the circle's own colour — see [`glyph_picture`]) and the reader's
//! cards, where it is rendered to a PNG the document embeds
//! ([`png_data_uri`]).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use gtk::{gdk, glib, graphene, gsk, pango, prelude::*, subclass::prelude::*};

/// libadwaita's `avatar.color1`…`color14`: gradient top, gradient bottom,
/// text (from its stylesheet; one palette for both themes).
const PALETTE: [(&str, &str, &str); 14] = [
    ("#83b6ec", "#337fdc", "#cfe1f5"),
    ("#7ad9f1", "#0f9ac8", "#caeaf2"),
    ("#8de6b1", "#29ae74", "#cef8d8"),
    ("#b5e98a", "#6ab85b", "#e6f9d7"),
    ("#f8e359", "#d29d09", "#f9f4e1"),
    ("#ffcb62", "#d68400", "#ffead1"),
    ("#ffa95a", "#ed5b00", "#ffe5c5"),
    ("#f78773", "#e62d42", "#f8d2ce"),
    ("#e973ab", "#e33b6a", "#fac7de"),
    ("#cb78d4", "#9945b5", "#e7c2e8"),
    ("#9e91e8", "#7a59ca", "#d5d2f5"),
    ("#e3cf9c", "#b08952", "#f2eade"),
    ("#be916d", "#785336", "#e5d6ca"),
    ("#c0bfbc", "#6e6d71", "#d8d7d3"),
];

/// The palette entry `adw::Avatar` gives `text`: GLib's `g_str_hash`
/// (djb2) modulo the palette, as libadwaita does it.
fn palette_index(text: &str) -> usize {
    let hash = text
        .bytes()
        .fold(5381u32, |h, b| h.wrapping_mul(33).wrapping_add(u32::from(b)));
    (hash % PALETTE.len() as u32) as usize
}

/// The initials `adw::Avatar` would show for `text`: the first letter of
/// the first word and of the last, uppercased; nothing for an empty name.
pub fn initials_of(text: &str) -> String {
    let upper = text.trim().to_uppercase();
    let mut words = upper.split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric));
    let Some(first) = words.next() else { return String::new() };
    let mut out: String = first.chars().take(1).collect();
    if let Some(last) = words.last() {
        out.extend(last.chars().take(1));
    }
    out
}

thread_local! {
    /// A label to borrow the theme font from (its Pango context carries
    /// the display's font settings), never shown.
    static FONT_SOURCE: gtk::Label = gtk::Label::new(None);
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct InitialsPaintable {
        pub initials: RefCell<String>,
        /// The ground: a gradient top and bottom, or nothing (the widget
        /// beneath paints its own).
        pub ground: RefCell<Option<(gdk::RGBA, gdk::RGBA)>>,
        pub fg: RefCell<Option<gdk::RGBA>>,
        /// Text height as a share of the shorter side; 0 = by the count
        /// of letters, as the avatar sizes its label.
        pub scale: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InitialsPaintable {
        const NAME: &'static str = "HylkiInitialsPaintable";
        type Type = super::InitialsPaintable;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for InitialsPaintable {}

    impl PaintableImpl for InitialsPaintable {
        fn flags(&self) -> gdk::PaintableFlags {
            gdk::PaintableFlags::SIZE | gdk::PaintableFlags::CONTENTS
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let Some(snapshot) = snapshot.downcast_ref::<gtk::Snapshot>() else { return };
            // The circle's ground: the avatar clips this to its disc.
            if let Some((top, bottom)) = *self.ground.borrow() {
                snapshot.append_linear_gradient(
                    &graphene::Rect::new(0.0, 0.0, width as f32, height as f32),
                    &graphene::Point::new(0.0, 0.0),
                    &graphene::Point::new(0.0, height as f32),
                    &[gsk::ColorStop::new(0.0, top), gsk::ColorStop::new(1.0, bottom)],
                );
            }

            let initials = self.initials.borrow();
            if initials.is_empty() {
                return;
            }
            let fg = self.fg.borrow().unwrap_or(gdk::RGBA::WHITE);
            // Bold, at a size that lets a pair sit comfortably inside the
            // disc: the same weight of presence the avatar's own label has.
            let size = width.min(height);
            let share = match self.scale.get() {
                s if s > 0.0 => s,
                _ if initials.chars().count() > 1 => 0.42,
                _ => 0.5,
            };
            let px = size * share;
            let layout = FONT_SOURCE.with(|l| l.create_pango_layout(Some(initials.as_str())));
            let mut desc = layout.context().font_description().unwrap_or_default();
            desc.set_weight(pango::Weight::Bold);
            desc.set_absolute_size(px * f64::from(pango::SCALE));
            layout.set_font_description(Some(&desc));

            // Centre the ink, not the logical box.
            let scale = f64::from(pango::SCALE);
            let (ink, _) = layout.extents();
            let ink_x = f64::from(ink.x()) / scale;
            let ink_y = f64::from(ink.y()) / scale;
            let ink_w = f64::from(ink.width()) / scale;
            let ink_h = f64::from(ink.height()) / scale;
            let dx = (width - ink_w) / 2.0 - ink_x;
            let dy = (height - ink_h) / 2.0 - ink_y;
            snapshot.save();
            snapshot.translate(&graphene::Point::new(dx as f32, dy as f32));
            snapshot.append_layout(&layout, &fg);
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    pub struct InitialsPaintable(ObjectSubclass<imp::InitialsPaintable>)
        @implements gdk::Paintable;
}

impl InitialsPaintable {
    /// The circle for `name` (a sender's display name): its initials in the
    /// colour libadwaita would give that name. `None` when there is no
    /// letter to show, so the avatar falls back to its silhouette.
    pub fn for_name(name: &str) -> Option<Self> {
        let initials = initials_of(name);
        if initials.is_empty() {
            return None;
        }
        let (top, bottom, fg) = PALETTE[palette_index(name)];
        let rgba = |s: &str| gdk::RGBA::parse(s).unwrap_or(gdk::RGBA::BLACK);
        let this: Self = glib::Object::new();
        this.imp().initials.replace(initials);
        this.imp().ground.replace(Some((rgba(top), rgba(bottom))));
        this.imp().fg.replace(Some(rgba(fg)));
        Some(this)
    }

    /// `text` alone (initials, or an emoji) in `fg`, over nothing: for a
    /// circle that paints its own colour. `scale` is the text height as a
    /// share of the circle.
    pub fn glyph(text: &str, fg: gdk::RGBA, scale: f64) -> Self {
        let this: Self = glib::Object::new();
        this.imp().initials.replace(text.to_string());
        this.imp().fg.replace(Some(fg));
        this.imp().scale.set(scale);
        this
    }

    /// `text` in `fg` on a flat `bg`.
    pub fn solid(text: &str, bg: gdk::RGBA, fg: gdk::RGBA, scale: f64) -> Self {
        let this = Self::glyph(text, fg, scale);
        this.imp().ground.replace(Some((bg, bg)));
        this
    }
}

/// A sidebar circle's glyph: `text` ink-centred in the colour that reads
/// on `bg_hex` (the circle's own ground, painted by its CSS), exactly
/// `size` square — sized outright rather than expanding, since an expand
/// flag would climb into the row and stretch it.
pub fn glyph_picture(text: &str, bg_hex: &str, scale: f64, size: i32) -> gtk::Picture {
    let fg = gdk::RGBA::parse(crate::color::readable_text(bg_hex)).unwrap_or(gdk::RGBA::WHITE);
    let picture = gtk::Picture::for_paintable(&InitialsPaintable::glyph(text, fg, scale));
    picture.set_content_fit(gtk::ContentFit::Fill);
    picture.set_can_shrink(true);
    picture.set_size_request(size, size);
    picture.set_halign(gtk::Align::Center);
    picture.set_valign(gtk::Align::Center);
    picture
}

thread_local! {
    /// Account avatar pictures already loaded, by path and modification
    /// time: the sidebar rebuilds often and must not read the file each
    /// time.
    static AVATAR_TEXTURES: RefCell<HashMap<(std::path::PathBuf, Option<std::time::SystemTime>), gdk::Texture>> =
        RefCell::new(HashMap::new());
}

/// The texture behind an account's avatar picture, by path and modification
/// time: the sidebar rebuilds often and must not read the file each time.
pub fn avatar_texture(path: &std::path::Path) -> Option<gdk::Texture> {
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let key = (path.to_path_buf(), mtime);
    AVATAR_TEXTURES.with(|c| {
        if let Some(t) = c.borrow().get(&key) {
            return Some(t.clone());
        }
        let t = gdk::Texture::from_filename(path).ok()?;
        c.borrow_mut().insert(key, t.clone());
        Some(t)
    })
}

/// An account's avatar picture (#162) for a disc of `size` px: the stored
/// copy is square already, so it fills the disc and the disc clips it. An
/// image at a fixed pixel size, not a picture: a picture's natural size is
/// the texture's, and the disc (a box, whose size request is only a floor)
/// would grow to it.
pub fn avatar_picture(path: &std::path::Path, size: i32) -> gtk::Image {
    match avatar_texture(path) {
        Some(texture) => picture_from_texture(&texture, size),
        None => {
            let image = gtk::Image::new();
            image.set_pixel_size(size);
            image
        }
    }
}

/// The same, for a picture already in hand rather than on disk — an
/// account's Gravatar (#189), fetched and decoded elsewhere.
pub fn picture_from_texture(texture: &gdk::Texture, size: i32) -> gtk::Image {
    let image = gtk::Image::from_paintable(Some(texture));
    image.set_pixel_size(size);
    image.set_halign(gtk::Align::Center);
    image.set_valign(gtk::Align::Center);
    image
}

/// One scaled copy of a picture: which file, as it was when read, at how
/// many pixels.
type PictureKey = (std::path::PathBuf, Option<std::time::SystemTime>, i32);

thread_local! {
    /// Account pictures already scaled for a document, by that key.
    static PICTURE_URIS: RefCell<HashMap<PictureKey, String>> = RefCell::new(HashMap::new());
}

/// An account's avatar picture as a `data:` PNG for a reader card's circle
/// (#189), `size` CSS pixels across. Scaled on the way in: the stored copy is
/// 256px square, a conversation embeds one copy per card of yours, and none
/// of that detail survives a 26px circle. Read through gdk-pixbuf rather than
/// the window's renderer, so the document builder still needs no display.
pub fn picture_data_uri(path: &std::path::Path, size: i32) -> Option<String> {
    // Twice the CSS size is what a HiDPI screen wants; a 300% screen would
    // want three, which is not worth the bytes on every other screen.
    let px = (size * 2).max(1);
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let key = (path.to_path_buf(), mtime, px);
    if let Some(hit) = PICTURE_URIS.with(|c| c.borrow().get(&key).cloned()) {
        return Some(hit);
    }
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(path, px, px, false).ok()?;
    let png = pixbuf.save_to_bufferv("png", &[]).ok()?;
    let uri = format!("data:image/png;base64,{}", glib::base64_encode(&png));
    PICTURE_URIS.with(|c| {
        c.borrow_mut().insert(key, uri.clone());
    });
    Some(uri)
}

thread_local! {
    /// Reader-card circles already rendered this session, by what they
    /// show: a conversation repeats its senders, and every render of the
    /// document would otherwise draw them again.
    static PNG_CACHE: RefCell<HashMap<(String, String, i32), String>> = RefCell::new(HashMap::new());
}

/// The session's circle caches, for the memory section of an export: account
/// pictures as textures and their pixel bytes, then the rendered `data:` URIs
/// (account pictures and initials circles together) and their bytes.
pub fn cache_stats() -> (usize, u64, usize, usize) {
    let (pictures, picture_bytes) = AVATAR_TEXTURES.with(|c| {
        let c = c.borrow();
        (c.len(), c.values().map(crate::memory_report::texture_bytes).sum())
    });
    let (uris, uri_bytes) = PICTURE_URIS.with(|c| {
        let c = c.borrow();
        (c.len(), c.values().map(String::len).sum::<usize>())
    });
    let (pngs, png_bytes) = PNG_CACHE.with(|c| {
        let c = c.borrow();
        (c.len(), c.values().map(String::len).sum::<usize>())
    });
    (pictures, picture_bytes, uris + pngs, uri_bytes + png_bytes)
}

/// `text` on a flat `bg`, as a `data:` PNG for an `<img>` of `size` CSS
/// pixels — rendered at the screen's scale, so it is as crisp as the
/// document's own text. Drawn through the window's renderer; `None` before
/// a window exists.
pub fn png_data_uri(text: &str, bg: gdk::RGBA, size: i32) -> Option<String> {
    let key = (text.to_string(), bg.to_string(), size);
    if let Some(hit) = PNG_CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return Some(hit);
    }
    // No display (the document builder under test, say): there is nothing to
    // render with, and asking GTK for its toplevels would panic rather than
    // say so. The caller falls back to markup.
    if !gtk::is_initialized_main_thread() {
        return None;
    }
    let window = gtk::Window::list_toplevels().into_iter().find_map(|w| w.downcast::<gtk::Window>().ok())?;
    let renderer = window.renderer()?;
    let scale = window.scale_factor().max(1);
    let px = size * scale;
    let paintable = InitialsPaintable::solid(text, bg, gdk::RGBA::WHITE, 0.0);
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(px), f64::from(px));
    let node = snapshot.to_node()?;
    let texture = renderer.render_texture(&node, Some(&graphene::Rect::new(0.0, 0.0, px as f32, px as f32)));
    let uri = format!("data:image/png;base64,{}", glib::base64_encode(&texture.save_to_png_bytes()));
    PNG_CACHE.with(|c| {
        c.borrow_mut().insert(key, uri.clone());
    });
    Some(uri)
}

/// `hsl()` as CSS means it (hue in degrees, the rest as fractions), for the
/// reader's per-sender tints.
pub fn hsl(h: f64, s: f64, l: f64) -> gdk::RGBA {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp {
        v if v < 1.0 => (c, x, 0.0),
        v if v < 2.0 => (x, c, 0.0),
        v if v < 3.0 => (0.0, c, x),
        v if v < 4.0 => (0.0, x, c),
        v if v < 5.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    gdk::RGBA::new((r + m) as f32, (g + m) as f32, (b + m) as f32, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_follow_the_avatar_rules() {
        assert_eq!(initials_of("Marcus Chen"), "MC");
        assert_eq!(initials_of("  sophie   turner "), "ST");
        assert_eq!(initials_of("Apple"), "A");
        assert_eq!(initials_of("GNOME Foundation Board"), "GB");
        assert_eq!(initials_of("émile zola"), "ÉZ");
        assert_eq!(initials_of("  "), "");
        // GLib's djb2, as libadwaita hashes the name for its colour.
        assert_eq!(palette_index(""), (5381u32 % 14) as usize);
        assert!(palette_index("Marcus Chen") < PALETTE.len());
    }

    #[test]
    fn hsl_matches_css() {
        let c = hsl(0.0, 1.0, 0.5);
        assert!((c.red() - 1.0).abs() < 0.01 && c.green().abs() < 0.01 && c.blue().abs() < 0.01);
        let c = hsl(120.0, 0.52, 0.45);
        assert!((c.green() - 0.684).abs() < 0.01 && (c.red() - 0.216).abs() < 0.01);
        let c = hsl(240.0, 0.52, 0.38);
        assert!((c.blue() - 0.5776).abs() < 0.01);
    }
}
