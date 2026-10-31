//! Living on the desktop: the session's settings and notifications, the app
//! icon and launcher, themes and fonts, running in the background, power and
//! network changes, one running instance, and the process's own log and
//! memory accounting.

pub(crate) mod app_icon;
pub(crate) mod background;
pub(crate) mod brand;
pub(crate) mod console_log;
pub(crate) mod icon_theme;
pub(crate) mod launcher_badge;
pub(crate) mod legacy;
pub(crate) mod memory_report;
pub(crate) mod nautilus_ext;
pub(crate) mod notify;
pub(crate) mod platform;
pub(crate) mod power;
pub(crate) mod session;
pub(crate) mod single_instance;
pub(crate) mod spell;
pub(crate) mod startup;
pub(crate) mod text_scale;
pub(crate) mod theme;
mod theme_palettes;
pub(crate) mod web_fonts;
pub(crate) mod x11_shm;
