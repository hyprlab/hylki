//! A dark (or light) hairline along one edge of popovers and floating
//! windows at a fractional display scale, such as 125 %.
//!
//! GTK tells the compositor which part of a popover or window is opaque,
//! taken from its background box, whenever that background is a solid
//! color. At a fractional scale the box's edge falls inside a screen pixel,
//! and the compositor rounds the opaque part out to whole pixels: the
//! half-covered row along the edge is then drawn without blending, as its
//! premultiplied color, which is a grey line where a soft edge should be.
//! Measured under GNOME 50's runtime: 118 where 235 belonged.
//!
//! GTK only reports an opaque region for a fully opaque background, so here
//! those backgrounds lose half a percent of their opacity, too little to see,
//! and the compositor blends the edge as it should. This applies only while
//! a monitor has a fractional scale, and never to maximized or fullscreen
//! windows, whose edges sit on whole pixels and which keep the compositor's
//! shortcuts for opaque windows.

use gtk::gdk;
use gtk::prelude::*;

const CSS: &str = "\
popover > contents, popover > arrow { \
  background-color: color-mix(in srgb, var(--popover-bg-color) 99.5%, transparent); } \
window.background:not(.maximized):not(.fullscreen) { \
  background-color: color-mix(in srgb, var(--window-bg-color) 99.5%, transparent); } \
window.welcome-window:not(.maximized):not(.fullscreen) { \
  background-color: rgba(0, 91, 255, 0.995); }";

/// Start following the monitors' scales. Call once, after the display is open.
pub(crate) fn install() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    // Above the app's stylesheet and the scheme provider, which set some of
    // the same backgrounds; the colors still come from the theme in force.
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 2,
    );
    let monitors = display.monitors();
    let update = {
        let provider = provider.clone();
        let monitors = monitors.clone();
        move || {
            let fractional = monitors
                .iter::<gdk::Monitor>()
                .flatten()
                .any(|m| (m.scale() - m.scale().round()).abs() > 0.01);
            provider.load_from_string(if fractional { CSS } else { "" });
        }
    };
    update();
    // A monitor plugged in or out, or its scale changed in Settings.
    let watch = {
        let update = update.clone();
        move |list: &gtk::gio::ListModel, position: u32, _removed: u32, added: u32| {
            for i in position..position + added {
                if let Some(monitor) = list.item(i).and_downcast::<gdk::Monitor>() {
                    let update = update.clone();
                    monitor.connect_scale_notify(move |_| update());
                }
            }
            update();
        }
    };
    watch(&monitors, 0, 0, monitors.n_items());
    monitors.connect_items_changed(watch);
}
