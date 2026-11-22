//! The sender's face on a new-mail notification (#693): the contact photo,
//! Gravatar or domain icon the list and the reader show, by the same Privacy
//! switches, cut to the same circle.
//!
//! How the picture travels depends on which way GLib sends the notification.
//! The portal, the only way out of the Flatpak sandbox, takes the PNG itself.
//! Outside it, GNOME Shell takes either, but `org.freedesktop.Notifications`
//! (Plasma and every other desktop) only gets a picture GLib can name by
//! path, so there it is a file in the cache.

use crate::ui::message_view::faces;
use gtk::gdk_pixbuf::{Colorspace, InterpType, Pixbuf};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// How long a notification waits for a face not looked up yet: time for a
/// contact photo and most Gravatar answers. One that comes later is still
/// kept for the list.
const WAIT: Duration = Duration::from_millis(1500);
/// Pixels across: twice the largest a desktop draws a notification's image,
/// for a HiDPI screen.
const PX: i32 = 128;
/// Picture files kept for notifications still on screen or in the
/// notification list. Each sender's has its own name: GNOME Shell keeps an
/// icon it has loaded by its path, and would show the old face for a new one.
const KEEP: usize = 32;

type Post = Box<dyn FnOnce(Option<gio::Icon>)>;

/// Call `post` with `email`'s face as a notification icon: at once when the
/// face is known, else after looking for it for [`WAIT`] at most. `None`
/// when there is no face, and the notification wears the app's icon.
pub(crate) fn with_face(email: &str, post: impl FnOnce(Option<gio::Icon>) + 'static) {
    let email = email.trim().to_lowercase();
    if email.is_empty() {
        return post(None);
    }
    if let Some(texture) = faces::known_texture(&email) {
        return post(icon(&texture));
    }
    let Some(what) = faces::still_to_ask(&email) else {
        return post(None);
    };
    let post: Rc<RefCell<Option<Post>>> = Rc::new(RefCell::new(Some(Box::new(post))));
    let late = post.clone();
    glib::timeout_add_local_once(WAIT, move || {
        let post = late.borrow_mut().take();
        if let Some(post) = post {
            tracing::debug!("notification face: none after {} ms", WAIT.as_millis());
            post(None);
        }
    });
    glib::spawn_future_local(async move {
        let found = relm4::spawn_blocking(move || faces::ask(email, what)).await;
        let face = found
            .ok()
            .map(faces::keep)
            .and_then(|email| faces::known_texture(&email));
        let post = post.borrow_mut().take();
        if let Some(post) = post {
            post(face.as_ref().and_then(icon));
        }
    });
}

/// The face as an icon the notification can carry, see the module notes.
fn icon(texture: &gdk::Texture) -> Option<gio::Icon> {
    let png = round_png(texture)?;
    if crate::desktop::platform::is_flatpak() {
        return Some(gio::BytesIcon::new(&glib::Bytes::from_owned(png)).upcast());
    }
    match write_file(&png) {
        Ok(path) => Some(gio::FileIcon::new(&gio::File::for_path(path)).upcast()),
        Err(e) => {
            tracing::warn!("notification face: could not keep the picture: {e}");
            None
        }
    }
}

/// The middle square of the picture, as the list's round avatar crops it,
/// scaled to [`PX`] and cut to a circle with a soft edge: the desktop draws
/// the image as it comes, and Hylki's faces are round everywhere else.
fn round_png(texture: &gdk::Texture) -> Option<Vec<u8>> {
    let stream = gio::MemoryInputStream::from_bytes(&texture.save_to_png_bytes());
    let pixbuf = Pixbuf::from_stream(&stream, None::<&gio::Cancellable>).ok()?;
    let (w, h) = (pixbuf.width(), pixbuf.height());
    let side = w.min(h);
    if side < 1 {
        return None;
    }
    let square = pixbuf.new_subpixbuf((w - side) / 2, (h - side) / 2, side, side);
    let scaled = square
        .scale_simple(PX, PX, InterpType::Hyper)?
        .add_alpha(false, 0, 0, 0)
        .ok()?;
    let stride = scaled.rowstride();
    let mut pixels = scaled.read_pixel_bytes().to_vec();
    round_alpha(&mut pixels, PX as usize, stride as usize);
    Pixbuf::from_bytes(
        &glib::Bytes::from_owned(pixels),
        Colorspace::Rgb,
        true,
        8,
        PX,
        PX,
        stride,
    )
    .save_to_bufferv("png", &[])
    .ok()
}

/// Clear what lies outside the circle in a square of RGBA pixels, with a
/// pixel's width of fade at the edge.
fn round_alpha(pixels: &mut [u8], side: usize, stride: usize) {
    let r = side as f64 / 2.0;
    for y in 0..side {
        for x in 0..side {
            let (dx, dy) = (x as f64 + 0.5 - r, y as f64 + 0.5 - r);
            let cover = (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            let a = y * stride + x * 4 + 3;
            if let Some(alpha) = pixels.get_mut(a) {
                *alpha = (f64::from(*alpha) * cover).round() as u8;
            }
        }
    }
}

/// Keep the picture in the cache, named for what it shows, and let go of
/// the oldest beyond [`KEEP`]. Owner-only: they are people's photos.
fn write_file(png: &[u8]) -> std::io::Result<std::path::PathBuf> {
    use sha2::Digest;
    let dir = glib::user_cache_dir().join("hylki").join("notifications");
    crate::config::private_dir(&dir)?;
    let hash = sha2::Sha256::digest(png);
    let name: String = hash[..12].iter().map(|b| format!("{b:02x}")).collect();
    let path = dir.join(format!("{name}.png"));
    if path.exists() {
        // Touched, so the trim counts it as new.
        let _ = std::fs::File::options()
            .append(true)
            .open(&path)
            .and_then(|f| f.set_modified(std::time::SystemTime::now()));
    } else {
        crate::config::write_private_bytes(&path, png)?;
    }
    trim(&dir);
    Ok(path)
}

fn trim(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= KEEP {
        return;
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, path) in files.into_iter().skip(KEEP) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::round_alpha;

    #[test]
    fn circle_clears_the_corners_and_keeps_the_middle() {
        let side = 16;
        let mut px = vec![255u8; side * side * 4];
        round_alpha(&mut px, side, side * 4);
        let alpha = |x: usize, y: usize| px[y * side * 4 + x * 4 + 3];
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(side - 1, side - 1), 0);
        assert_eq!(alpha(side / 2, side / 2), 255);
        assert!(alpha(side / 2, 0) > 200, "the top edge stays");
    }
}
