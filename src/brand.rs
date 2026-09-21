//! The cloud services' own marks (`data/brands/`), shown wherever the app
//! names one of them: the Service picker, the Cloud Storage list, the
//! account editor. They are the services' trademarks, used only to say
//! "this works with…", and are not under the project's licence; see
//! data/brands/README.md for where each came from. Shipped as PNGs
//! rendered from the official files and decoded at the size shown (like
//! the app-icon gallery), so no SVG loader is needed on the host.

use gtk::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;

macro_rules! brands {
    ($($id:literal),* $(,)?) => {
        /// The PNG for a brand id, or none for an id we have no mark for.
        pub(crate) fn png(id: &str) -> Option<&'static [u8]> {
            match id {
                $($id => Some(include_bytes!(concat!("../data/brands/", $id, ".png"))),)*
                _ => None,
            }
        }
    };
}
brands!(
    "nextcloud", "owncloud", "opencloud", "onedrive", "dropbox", "seafile",
    "gmail", "outlook", "icloud", "yahoo", "proton", "fastmail", "aol", "zoho", "gmx", "yandex", "mailcom",
    "stalwart",
    "mail", "mail-oauth",
);

thread_local! {
    static CACHE: RefCell<HashMap<(String, i32), gtk::gdk::Texture>> = RefCell::new(HashMap::new());
}

/// The mark decoded at `px` pixels, cached per size.
pub fn texture(id: &str, px: i32) -> Option<gtk::gdk::Texture> {
    use gtk::gdk_pixbuf::PixbufLoader;
    use gtk::prelude::PixbufLoaderExt;
    let key = (id.to_string(), px);
    if let Some(t) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return Some(t);
    }
    let bytes = png(id)?;
    let loader = PixbufLoader::with_type("png").ok()?;
    loader.set_size(px, px);
    loader.write(bytes).ok()?;
    loader.close().ok()?;
    let texture = gtk::gdk::Texture::for_pixbuf(&loader.pixbuf()?);
    CACHE.with(|c| c.borrow_mut().insert(key, texture.clone()));
    Some(texture)
}

/// The generic icons for a mark we do not have: a cloud for storage, an
/// envelope for mail.
pub const GENERIC_CLOUD: &str = "co.hyprlab.Hylki-cloud-symbolic";
pub const GENERIC_MAIL: &str = "co.hyprlab.Hylki-mail-unread-symbolic";

/// An image of the mark, `px` logical pixels square (decoded at twice
/// that for HiDPI). An unknown id gets the generic cloud icon, so a row
/// whose service is not known yet still lines up with the others.
pub fn image(id: &str, px: i32) -> gtk::Image {
    image_or(id, px, GENERIC_CLOUD)
}

/// [`image`] with the symbolic icon to show for an id we have no mark for.
pub fn image_or(id: &str, px: i32, fallback: &str) -> gtk::Image {
    let image = match texture(id, px * 2) {
        Some(t) => gtk::Image::from_paintable(Some(&t)),
        None => gtk::Image::from_icon_name(fallback),
    };
    image.set_pixel_size(px);
    image.set_valign(gtk::Align::Center);
    image
}

/// Point an existing image at a mark (or the fallback icon), for a header
/// mark that follows a picker.
pub fn set_image(image: &gtk::Image, id: &str, px: i32, fallback: &str) {
    match texture(id, px * 2) {
        Some(t) => image.set_paintable(Some(&t)),
        None => image.set_icon_name(Some(fallback)),
    }
    image.set_pixel_size(px);
}
