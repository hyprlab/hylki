//! Text size (#267): the app's own text, larger or smaller than the
//! desktop's.
//!
//! GTK sizes every font from the `gtk-xft-dpi` setting, which the desktop
//! sets (GNOME's text scaling factor lands there), so scaling that one
//! value scales the sidebar, the list and Settings alike while icons and
//! spacing keep their size. WebKit reads the same setting, so message text
//! follows too, on top of the reader's own zoom.

/// The sizes Settings offers, in percent of the desktop's.
pub const STEPS: &[u32] = &[90, 100, 110, 120, 135, 150];

/// GTK's value for "no DPI set": 96 dots per inch, in 1/1024ths.
const DEFAULT_DPI: i32 = 96 * 1024;

/// Put the app's text at `percent` of the desktop's size. 100 hands the
/// setting back to the desktop, so a change there is followed again.
pub fn apply(percent: u32) {
    let Some(settings) = gtk::Settings::default() else { return };
    // Measured from the desktop's value every time, never from a scaled
    // one, so moving between sizes does not compound.
    settings.reset_property("gtk-xft-dpi");
    if percent == 100 {
        return;
    }
    let desktop = match settings.gtk_xft_dpi() {
        dpi if dpi > 0 => dpi,
        _ => DEFAULT_DPI,
    };
    let scaled = i64::from(desktop) * i64::from(percent) / 100;
    settings.set_gtk_xft_dpi(scaled.clamp(1, i64::from(i32::MAX)) as i32);
}
