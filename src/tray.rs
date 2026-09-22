//! A tray icon, for the desktops that have a tray (issue #116).
//!
//! GNOME has none — its answer is Background Apps, see [`crate::background`] —
//! but Cinnamon, KDE, MATE, XFCE and GNOME with the AppIndicator extension all
//! speak the freedesktop **StatusNotifierItem** protocol: the app publishes an
//! item over D-Bus and registers it with `org.kde.StatusNotifierWatcher`, and
//! the panel draws it. That is what "AppIndicator" means today.
//!
//! The item is an icon that wears a red dot while any inbox has unread mail,
//! a tooltip saying how many, a menu (open, unread mail, accounts, settings, quit), and a click that brings the
//! window back. Off by default: on a desktop with no watcher nothing is drawn
//! and nothing else changes — the Background Apps listing comes from the
//! portal, which this never touches. The item keeps waiting, so enabling a
//! tray extension later picks it up without a restart.
//!
//! Icons are sent as pixel data rather than by name: the panel lives outside
//! the sandbox and may not resolve our icon theme, and the dot has to be drawn
//! on anyway. The Hylki icon is the app icon itself; the envelope variants are
//! the app's own symbolic icon in plain white or black, for panels that
//! don't recolour symbolic icons. On Cinnamon the icon is drawn smaller
//! inside the pixmap, see [`panel_fill`].

use gtk::cairo;
use gtk::gdk_pixbuf::{InterpType, Pixbuf, PixbufLoader};
use gtk::prelude::*;
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{MenuItem, StandardItem};
use ksni::{Category, Icon, Status, ToolTip, Tray};

use crate::app::AppMsg;
use crate::config::TrayIcon;
use crate::i18n::{i18n, i18n_f, ni18n_f};

/// One unread message as the tray menu shows it (issue #116): a card-like
/// row with the sender's picture, which opens it in the reader. A DBusMenu
/// is drawn by the panel, so a row is an icon and text and can carry no
/// buttons; the actions stay in the reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayMail {
    pub account_id: u32,
    pub folder_id: u32,
    pub message_id: u32,
    /// First line: sender, account when there are several, and the date.
    pub heading: String,
    pub subject: String,
    /// The list's preview text; empty when previews are off.
    pub preview: String,
    /// PNG of the sender's picture or initials, sized for a menu.
    pub icon: Vec<u8>,
}

/// How many unread messages the menu lists, newest first; past that, one
/// row offers the rest in the window.
pub const MAIL_LIMIT: usize = 5;

/// The menu's mail section: the cards shown, and how many unread inbox
/// messages there are in all (more than the cards when the list was cut).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TrayMailList {
    pub items: Vec<TrayMail>,
    pub unread: u32,
}

/// The app icon the tray shows: the one the user chose for the app (see
/// `app_icon.rs`), passed in as PNG bytes so a change follows live.
pub type AppIconPng = &'static [u8];
/// The app's symbolic icon (the opened envelope, drawn in black); its
/// fill is swapped for the chosen colour.
const ENVELOPE_SVG: &str =
    include_str!("../data/icons/hicolor/symbolic/apps/co.hyprlab.Hylki-symbolic.svg");
/// Panels ask for different sizes; a set covers them without upscaling blur.
const SIZES: [i32; 6] = [16, 22, 24, 32, 48, 64];
/// GNOME's red (`@error_color`).
const DOT_RGB: (f64, f64, f64) = (0xe0 as f64 / 255.0, 0x1b as f64 / 255.0, 0x24 as f64 / 255.0);

/// A running tray item. Dropping it does not remove the icon; call
/// [`TrayHandle::stop`].
pub struct TrayHandle {
    handle: Handle<HylkiTray>,
    last_unread: std::cell::Cell<u32>,
}

impl TrayHandle {
    /// Publish the item. `None` when the session bus can't be reached at
    /// all; a missing watcher is not that — the item then waits for one.
    /// `mail` is `None` when the menu is not to list messages at all.
    pub fn start(
        sender: relm4::Sender<AppMsg>,
        icon: TrayIcon,
        app_png: AppIconPng,
        unread: u32,
        mail: Option<TrayMailList>,
    ) -> Option<Self> {
        let tray = HylkiTray {
            plain: render_set(icon, app_png, false),
            dotted: render_set(icon, app_png, true),
            unread,
            mail,
            sender,
        };
        match tray.disable_dbus_name(true).assume_sni_available(true).spawn() {
            Ok(handle) => Some(Self { handle, last_unread: std::cell::Cell::new(unread) }),
            Err(e) => {
                tracing::info!("tray icon not shown: {e}");
                None
            }
        }
    }

    /// The unread total across inboxes: the dot is on while it is non-zero.
    pub fn set_unread(&self, unread: u32) {
        if self.last_unread.replace(unread) == unread {
            return;
        }
        self.handle.update(|t| t.unread = unread);
    }

    /// Swap the icon set.
    pub fn set_icon(&self, icon: TrayIcon, app_png: AppIconPng) {
        let plain = render_set(icon, app_png, false);
        let dotted = render_set(icon, app_png, true);
        self.handle.update(move |t| {
            t.plain = plain;
            t.dotted = dotted;
        });
    }

    /// Replace the menu's message list; `None` drops the section.
    pub fn set_mail(&self, mail: Option<TrayMailList>) {
        self.handle.update(move |t| t.mail = mail);
    }

    /// Take the item down.
    pub fn stop(self) {
        let _ = self.handle.shutdown();
    }
}

struct HylkiTray {
    plain: Vec<Icon>,
    dotted: Vec<Icon>,
    unread: u32,
    /// Unread inbox mail for the menu, or `None` when that section is off.
    mail: Option<TrayMailList>,
    sender: relm4::Sender<AppMsg>,
}

impl Tray for HylkiTray {
    fn id(&self) -> String {
        crate::APP_ID.to_string()
    }

    fn title(&self) -> String {
        "Hylki".to_string()
    }

    fn category(&self) -> Category {
        Category::Communications
    }

    fn status(&self) -> Status {
        Status::Active
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        if self.unread > 0 { self.dotted.clone() } else { self.plain.clone() }
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Hylki".to_string(),
            description: inbox_status_text(self.unread),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.sender.send(AppMsg::PresentWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = vec![StandardItem {
            label: i18n("Open Hylki"),
            activate: Box::new(|t: &mut Self| {
                let _ = t.sender.send(AppMsg::PresentWindow);
            }),
            ..Default::default()
        }
        .into()];
        if let Some(mail) = &self.mail {
            items.push(MenuItem::Separator);
            // The section names what it counts: the inboxes, not the
            // folders that filters file into.
            items.push(
                StandardItem {
                    label: inbox_status_text(mail.unread),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
            items.extend(mail.items.iter().map(mail_item));
            if mail.unread as usize > mail.items.len() {
                items.push(
                    StandardItem {
                        label: i18n_f("View all {n} unread in Inboxes…", &[("n", &mail.unread.to_string())]),
                        activate: Box::new(|t: &mut Self| {
                            let _ = t.sender.send(AppMsg::PresentWindow);
                            let _ = t.sender.send(AppMsg::TrayViewUnread);
                        }),
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }
        items.extend([
            MenuItem::Separator,
            // The settings window sits on the main window, so that comes
            // back first when it was hidden.
            StandardItem {
                label: i18n("Accounts"),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.sender.send(AppMsg::PresentWindow);
                    let _ = t.sender.send(AppMsg::OpenAccounts);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: i18n("Settings"),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.sender.send(AppMsg::PresentWindow);
                    let _ = t.sender.send(AppMsg::OpenPreferences);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: i18n("Quit"),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.sender.send(AppMsg::QuitFromTray);
                }),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

/// What the tooltip and the menu's heading say the count is: unread mail
/// in the inboxes alone.
fn inbox_status_text(unread: u32) -> String {
    match unread {
        0 => i18n("No unread mail in Inboxes"),
        n => ni18n_f("{n} unread in Inboxes", "{n} unread in Inboxes", n, &[("n", &n.to_string())]),
    }
}

/// One message's row: the card as its label and picture; a click opens it
/// in the reader, the same path a notification click takes.
fn mail_item(m: &TrayMail) -> MenuItem<HylkiTray> {
    let (account_id, folder_id, message_id) = (m.account_id, m.folder_id, m.message_id);
    let mut label = m.heading.clone();
    if !m.subject.is_empty() {
        label.push('\n');
        label.push_str(&m.subject);
    }
    if !m.preview.is_empty() {
        label.push('\n');
        label.push_str(&m.preview);
    }
    StandardItem {
        label: menu_text(&label),
        icon_data: m.icon.clone(),
        activate: Box::new(move |t: &mut HylkiTray| {
            let _ = t.sender.send(AppMsg::PresentWindow);
            let _ = t.sender.send(AppMsg::OpenMessageFromNotification {
                account_id,
                folder_id,
                message_id,
            });
        }),
        ..Default::default()
    }
    .into()
}

/// A menu label reads `_` as a mnemonic marker; mail text means it literally.
fn menu_text(s: &str) -> String {
    s.replace('_', "__")
}

/// Cut a line of mail text down to menu width, on a character boundary.
pub fn clip(s: &str, max: usize) -> String {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max {
        return s;
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The menu's picture for a sender: the contact or Gravatar picture when the
/// avatar cache has one, else their initials on a colour picked by name — the
/// same fallback the message list shows.
pub fn sender_icon(name: &str, email: &str, texture: Option<gtk::gdk::Texture>) -> Vec<u8> {
    const SIZE: i32 = 32;
    if let Some(png) = texture.and_then(|t| {
        let loader = PixbufLoader::with_type("png").ok()?;
        loader.write(&t.save_to_png_bytes()).ok()?;
        loader.close().ok()?;
        loader
            .pixbuf()?
            .scale_simple(SIZE, SIZE, InterpType::Bilinear)?
            .save_to_bufferv("png", &[])
            .ok()
    }) {
        return png;
    }
    initials_png(name, email, SIZE).unwrap_or_default()
}

fn initials_png(name: &str, email: &str, size: i32) -> Option<Vec<u8>> {
    const PALETTE: [(f64, f64, f64); 6] = [
        (0.21, 0.52, 0.89), // blue
        (0.18, 0.76, 0.49), // green
        (0.90, 0.65, 0.04), // yellow
        (0.90, 0.38, 0.00), // orange
        (0.57, 0.25, 0.67), // purple
        (0.75, 0.11, 0.16), // red
    ];
    let seed = if name.is_empty() { email } else { name };
    let hash = seed.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize));
    let (r, g, b) = PALETTE[hash % PALETTE.len()];
    let initials: String = {
        let src = if name.trim().is_empty() { email.split('@').next().unwrap_or("") } else { name };
        let mut it = src
            .split(|c: char| c.is_whitespace() || c == '.' || c == '_' || c == '-')
            .filter_map(|w| w.chars().next())
            .filter(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().next().unwrap_or(c));
        match (it.next(), it.last()) {
            (Some(a), Some(z)) => format!("{a}{z}"),
            (Some(a), None) => a.to_string(),
            _ => "?".to_string(),
        }
    };
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, size, size).ok()?;
    {
        let cr = cairo::Context::new(&surface).ok()?;
        let s = f64::from(size);
        cr.set_source_rgb(r, g, b);
        cr.arc(s / 2.0, s / 2.0, s / 2.0, 0.0, std::f64::consts::TAU);
        cr.fill().ok()?;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.select_font_face("Cantarell", cairo::FontSlant::Normal, cairo::FontWeight::Bold);
        cr.set_font_size(s * if initials.chars().count() > 1 { 0.44 } else { 0.5 });
        let ext = cr.text_extents(&initials).ok()?;
        cr.move_to(
            s / 2.0 - ext.width() / 2.0 - ext.x_bearing(),
            s / 2.0 - ext.height() / 2.0 - ext.y_bearing(),
        );
        cr.show_text(&initials).ok()?;
    }
    let mut png = Vec::new();
    surface.write_to_png(&mut png).ok()?;
    Some(png)
}

/// Every size of one icon, with or without the dot.
fn render_set(icon: TrayIcon, app_png: AppIconPng, dotted: bool) -> Vec<Icon> {
    let fill = panel_fill();
    SIZES
        .iter()
        .filter_map(|&size| render(icon, app_png, dotted, size, fill))
        .collect()
}

/// How much of each pixmap the icon fills; the rest is a clear margin.
///
/// A panel draws the pixmap at whatever size it asked for, so the icon
/// fills it. Cinnamon is the exception: its status applet takes a pixmap
/// for a full-colour icon and draws it at the panel's colour icon size,
/// while the symbolic icons beside it get the smaller symbolic size, so
/// ours towered over them. Drawing at five-eighths brings it level.
fn panel_fill() -> f64 {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let cinnamon = desktop
        .split(':')
        .any(|d| d.eq_ignore_ascii_case("x-cinnamon") || d.eq_ignore_ascii_case("cinnamon"));
    if cinnamon { 0.625 } else { 1.0 }
}

/// Decode the chosen icon, composite the dot over its top-right, and centre
/// it in a `size` canvas, of which it fills `fill`.
fn render(icon: TrayIcon, app_png: AppIconPng, dotted: bool, size: i32, fill: f64) -> Option<Icon> {
    let glyph = ((f64::from(size) * fill).round() as i32).clamp(1, size);
    let pixbuf = base_pixbuf(icon, app_png, glyph)?;
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, size, size).ok()?;
    {
        let cr = cairo::Context::new(&surface).ok()?;
        // Centre a pixbuf the loader sized under the glyph (an SVG keeps its
        // aspect, so a wide envelope comes back shorter than `glyph`).
        let x = f64::from(size - pixbuf.width()) / 2.0;
        let y = f64::from(size - pixbuf.height()) / 2.0;
        cr.set_source_pixbuf(&pixbuf, x, y);
        cr.paint().ok()?;
        if dotted {
            // The dot sits on the glyph's corner, not the canvas's.
            let s = f64::from(glyph);
            let margin = f64::from(size - glyph) / 2.0;
            let r = (s * 0.19).max(2.0);
            let (cx, cy) = (margin + s - r - s * 0.04, margin + r + s * 0.04);
            // A clear ring first, so the dot reads as sitting on top of the
            // icon rather than merging into its corner.
            cr.set_operator(cairo::Operator::Clear);
            cr.arc(cx, cy, r + (s * 0.06).max(1.0), 0.0, std::f64::consts::TAU);
            cr.fill().ok()?;
            cr.set_operator(cairo::Operator::Over);
            cr.set_source_rgb(DOT_RGB.0, DOT_RGB.1, DOT_RGB.2);
            cr.arc(cx, cy, r, 0.0, std::f64::consts::TAU);
            cr.fill().ok()?;
        }
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let mut surface = surface;
    let data = surface.data().ok()?;
    Some(Icon {
        width: size,
        height: size,
        data: argb_network_order(&data, size as usize, size as usize, stride),
    })
}

/// The icon's pixels before any dot: the app icon scaled down, or the
/// symbolic envelope rasterised in the chosen colour.
fn base_pixbuf(icon: TrayIcon, app_png: AppIconPng, size: i32) -> Option<Pixbuf> {
    match icon {
        TrayIcon::Hylki => {
            let loader = PixbufLoader::with_type("png").ok()?;
            loader.write(app_png).ok()?;
            loader.close().ok()?;
            loader.pixbuf()?.scale_simple(size, size, InterpType::Hyper)
        }
        TrayIcon::EnvelopeLight | TrayIcon::EnvelopeDark => {
            let fill = if icon == TrayIcon::EnvelopeLight { "#ffffff" } else { "#000000" };
            let svg = ENVELOPE_SVG.replace("#000000", fill);
            let loader = PixbufLoader::with_type("svg").ok()?;
            loader.set_size(size, size);
            loader.write(svg.as_bytes()).ok()?;
            loader.close().ok()?;
            loader.pixbuf()
        }
    }
}

/// Cairo's ARGB32 is premultiplied and native-endian; the protocol wants
/// straight alpha in network (big-endian) byte order.
fn argb_network_order(data: &[u8], width: usize, height: usize, stride: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let row = &data[y * stride..y * stride + width * 4];
        for px in row.chunks_exact(4) {
            let native = u32::from_ne_bytes([px[0], px[1], px[2], px[3]]);
            let a = (native >> 24) & 0xff;
            let un = |c: u32| -> u8 {
                if a == 0 { 0 } else { ((c * 255 + a / 2) / a).min(255) as u8 }
            };
            let r = un((native >> 16) & 0xff);
            let g = un((native >> 8) & 0xff);
            let b = un(native & 0xff);
            out.extend_from_slice(&[a as u8, r, g, b]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpremultiplies_into_network_order() {
        // One half-transparent pure-red pixel, premultiplied, native-endian.
        let native = (0x80u32 << 24) | (0x80 << 16);
        let px = native.to_ne_bytes();
        let out = argb_network_order(&px, 1, 1, 4);
        assert_eq!(out, vec![0x80, 0xff, 0x00, 0x00]);
        // Fully transparent stays all-zero rather than dividing by zero.
        assert_eq!(argb_network_order(&[0, 0, 0, 0], 1, 1, 4), vec![0, 0, 0, 0]);
    }

    #[test]
    fn every_icon_renders_at_every_size() {
        for icon in [TrayIcon::Hylki, TrayIcon::EnvelopeLight, TrayIcon::EnvelopeDark] {
            for dotted in [false, true] {
                let set = render_set(icon, crate::app_icon::png_for(crate::app_icon::DEFAULT_ID), dotted);
                assert_eq!(set.len(), SIZES.len(), "{icon:?} dotted={dotted}");
                for (i, size) in SIZES.iter().enumerate() {
                    assert_eq!(set[i].width, *size);
                    assert_eq!(set[i].data.len(), (size * size * 4) as usize);
                }
            }
        }
    }

    #[test]
    fn menu_text_keeps_underscores_literal() {
        assert_eq!(menu_text("snake_case"), "snake__case");
        assert_eq!(clip("  a   long\n line  of text ", 8), "a long …");
        assert_eq!(clip("short", 8), "short");
    }

    #[test]
    fn initials_render_as_a_png() {
        let png = sender_icon("Ada Lovelace", "ada@example.org", None);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert!(!sender_icon("", "solo@example.org", None).is_empty());
    }

    #[test]
    fn the_dot_is_red_and_only_when_asked() {
        let size = 32usize;
        let plain = render(TrayIcon::EnvelopeLight, crate::app_icon::png_for(crate::app_icon::DEFAULT_ID), false, size as i32, 1.0).unwrap();
        let dotted = render(TrayIcon::EnvelopeLight, crate::app_icon::png_for(crate::app_icon::DEFAULT_ID), true, size as i32, 1.0).unwrap();
        // The dot's centre, per `render`.
        let s = size as f64;
        let r = s * 0.19;
        let (cx, cy) = ((s - r - s * 0.04) as usize, (r + s * 0.04) as usize);
        let at = |icon: &Icon| {
            let i = (cy * size + cx) * 4;
            (icon.data[i], icon.data[i + 1], icon.data[i + 2], icon.data[i + 3])
        };
        let (a, red, g, b) = at(&dotted);
        assert_eq!(a, 0xff);
        assert_eq!((red, g, b), (0xe0, 0x1b, 0x24));
        assert_ne!(at(&plain), at(&dotted));
    }

    #[test]
    fn a_half_fill_leaves_a_clear_margin_and_moves_the_dot() {
        let size = 32usize;
        let icon = render(TrayIcon::Hylki, crate::app_icon::png_for(crate::app_icon::DEFAULT_ID), true, size as i32, 0.5).unwrap();
        assert_eq!(icon.data.len(), size * size * 4);
        let at = |x: usize, y: usize| {
            let i = (y * size + x) * 4;
            (icon.data[i], icon.data[i + 1], icon.data[i + 2], icon.data[i + 3])
        };
        // The outer 8px ring is empty: corner, and just inside the old dot.
        assert_eq!(at(1, 1), (0, 0, 0, 0));
        assert_eq!(at(size - 3, 2), (0, 0, 0, 0));
        // The dot sits on the 16px glyph's corner, per `render`.
        let (s, margin) = (16.0f64, 8.0f64);
        let r = s * 0.19;
        let (cx, cy) = ((margin + s - r - s * 0.04) as usize, (margin + r + s * 0.04) as usize);
        let (a, red, g, b) = at(cx, cy);
        assert_eq!(a, 0xff);
        assert_eq!((red, g, b), (0xe0, 0x1b, 0x24));
        // And the glyph itself is there, in the middle.
        assert_ne!(at(size / 2, size / 2).0, 0);
    }
}
