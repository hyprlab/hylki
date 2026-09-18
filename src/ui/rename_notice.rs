//! The notice that Vireo has become Hylki.
//!
//! 1.34.0 is the last release under the Vireo name; updates continue as
//! Hylki (app ID `co.hyprlab.Hylki`), a separate install whose first start
//! carries this install's accounts, settings and cached mail across. This
//! dialog says so, links the site, and hands over the install command. It
//! opens on every start until "Don't Show Again", and stays reachable from
//! the main menu and the About window. Once Hylki is on the machine it
//! stops opening by itself and, when asked for, says Vireo can go.

use adw::prelude::*;

use crate::i18n::i18n;

/// Where the new app lives.
pub const SITE: &str = "https://hylki.hyprlab.co";
/// The site's account of the name: what a hylki is and why the change.
const WHY: &str = "https://hylki.hyprlab.co/#why-hylki";

fn esc(text: &str) -> String {
    gtk::glib::markup_escape_text(text).to_string()
}
const RELEASES: &str = "https://github.com/hyprlab/hylki/releases/latest";
const FLATPAK_ID: &str = "co.hyprlab.Hylki";
const UNINSTALL: &str = "flatpak uninstall co.hyprlab.Vireo";

/// Forces the notice open at start, dismissed or not, for checking it by
/// hand. Its value picks the variant shown: `flatpak`, `beta`, `native` or
/// `installed`; anything else shows what this build would.
const SHOWCASE: &str = "VIREO_SHOWCASE_RENAME";

fn showcase() -> Option<String> {
    std::env::var(SHOWCASE).ok()
}

/// How this install was made, which decides the instructions it gets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Install {
    Flatpak,
    FlatpakBeta,
    Native,
}

fn install() -> Install {
    match showcase().as_deref() {
        Some("flatpak") | Some("installed") => return Install::Flatpak,
        Some("beta") => return Install::FlatpakBeta,
        Some("native") => return Install::Native,
        _ => {}
    }
    if !crate::platform::is_flatpak() {
        Install::Native
    } else if cfg!(feature = "beta") {
        Install::FlatpakBeta
    } else {
        Install::Flatpak
    }
}

/// The one-line install command for a Flatpak install; the native package
/// is a download, not a command (see [`RELEASES`]).
fn install_command(kind: Install) -> Option<String> {
    let name = match kind {
        Install::Flatpak => FLATPAK_ID.to_string(),
        Install::FlatpakBeta => format!("{FLATPAK_ID}.Beta"),
        Install::Native => return None,
    };
    Some(format!("flatpak install --from {SITE}/flatpak/{name}.flatpakref"))
}

/// Whether Hylki is already installed on this machine: its Flatpak export
/// (user or system) or, for a native install, its binary. The user export
/// lives under the home directory, which the sandbox reads.
pub fn hylki_installed() -> bool {
    let desktop = format!("share/applications/{FLATPAK_ID}.desktop");
    let mut candidates: Vec<std::path::PathBuf> = vec![
        std::path::PathBuf::from("/var/lib/flatpak/exports").join(&desktop),
        std::path::PathBuf::from("/usr/bin/hylki"),
        std::path::PathBuf::from("/usr/local/bin/hylki"),
    ];
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from).or_else(dirs::home_dir) {
        candidates.push(home.join(".local/share/flatpak/exports").join(&desktop));
        candidates.push(home.join(".local/bin/hylki"));
    }
    candidates.iter().any(|p| p.exists())
}

/// Whether the notice should open by itself at this start.
pub fn due_at_startup() -> bool {
    if showcase().is_some() {
        return true;
    }
    if std::env::var_os("VIREO_DEMO").is_some() {
        return false;
    }
    !crate::config::rename_notice_dismissed() && !hylki_installed()
}

/// Open the notice. `startup` offers "Remind Me Later" / "Don't Show Again";
/// from a menu it just closes.
pub fn show(parent: &impl IsA<gtk::Window>, startup: bool) {
    let installed = match showcase().as_deref() {
        Some("installed") => true,
        Some(_) => false,
        None => hylki_installed(),
    };
    let kind = install();

    let heading = if installed { i18n("Hylki is installed") } else { i18n("Vireo is now Hylki") };
    let mut body = String::new();
    if installed {
        body.push_str(&esc(&i18n(
            "Vireo has a new name: Hylki. Hylki is installed on this computer, and \
             its first start carries your accounts, settings and cached mail across \
             from Vireo. Nothing in Vireo is changed or removed.",
        )));
        body.push_str("\n\n");
        body.push_str(&esc(&i18n(
            "This is the last Vireo release. Once Hylki is running with your \
             accounts, you can uninstall Vireo.",
        )));
    } else {
        body.push_str(&esc(&i18n("Vireo has a new name: Hylki.")));
        body.push(' ');
        body.push_str(&format!(
            "<a href=\"{WHY}\">{}</a>",
            esc(&i18n("(What's a 'hylki' and why the change?)"))
        ));
        body.push(' ');
        body.push_str(&esc(&i18n(
            "This is the last release under the old name; updates continue as \
             Hylki, which installs alongside Vireo.",
        )));
        body.push_str("\n\n");
        body.push_str(&esc(&i18n(
            "Install Hylki and its first start carries your accounts, settings and \
             cached mail across. Nothing in Vireo is changed or removed, and there is \
             nothing to set up again. Once Hylki is running, you can uninstall Vireo.",
        )));
    }

    let dialog = adw::MessageDialog::new(Some(parent.as_ref()), Some(&heading), None);
    // The body carries one link (Pango markup); the text around it is
    // escaped so an apostrophe or ampersand in a translation cannot break it.
    dialog.set_body_use_markup(true);
    dialog.set_body(&body);
    // Wide enough for the install command to read in one line.
    dialog.set_size_request(480, -1);

    // Below the text: the command (or the download) to run, then the site.
    let extra = gtk::Box::new(gtk::Orientation::Vertical, 10);
    extra.set_margin_top(6);

    let command = if installed { Some(UNINSTALL.to_string()) } else { install_command(kind) };
    match command {
        Some(cmd) => {
            let caption = gtk::Label::new(Some(
                if installed {
                    i18n("To remove Vireo, run:")
                } else {
                    i18n("To install Hylki, run this in a terminal (or open the site and click Install):")
                }
                .as_str(),
            ));
            caption.set_wrap(true);
            caption.set_xalign(0.0);
            caption.add_css_class("dim-label");
            caption.add_css_class("caption");
            extra.append(&caption);
            extra.append(&command_row(&cmd));
        }
        None => {
            let caption = gtk::Label::new(Some(
                i18n("This is a native package. Download the Hylki package for your \
                      distribution from the release page, install it over Vireo, and \
                      start Hylki.")
                .as_str(),
            ));
            caption.set_wrap(true);
            caption.set_xalign(0.0);
            caption.add_css_class("dim-label");
            caption.add_css_class("caption");
            extra.append(&caption);
            let releases = gtk::Button::with_label(&i18n("Open the Release Page"));
            releases.add_css_class("pill");
            releases.set_halign(gtk::Align::Center);
            let parent = parent.as_ref().clone();
            releases.connect_clicked(move |_| crate::ui::launch::open_link(RELEASES, Some(&parent)));
            extra.append(&releases);
        }
    }
    // The site last, under the command.
    let site = gtk::Button::with_label(&i18n("Open hylki.hyprlab.co"));
    site.add_css_class("pill");
    site.add_css_class("suggested-action");
    site.set_halign(gtk::Align::Center);
    site.set_margin_top(4);
    {
        let parent = parent.as_ref().clone();
        site.connect_clicked(move |_| crate::ui::launch::open_link(SITE, Some(&parent)));
    }
    extra.append(&site);
    dialog.set_extra_child(Some(&extra));

    if startup && !installed {
        dialog.add_response("never", &i18n("Don't Show Again"));
        dialog.add_response("later", &i18n("Remind Me Later"));
        dialog.set_default_response(Some("later"));
        dialog.set_close_response("later");
        dialog.connect_response(Some("never"), |_, _| crate::config::dismiss_rename_notice());
    } else {
        dialog.add_response("close", &i18n("Close"));
        dialog.set_default_response(Some("close"));
        dialog.set_close_response("close");
    }
    dialog.present();
}

/// The command in a monospace field with a Copy button, as the Files
/// extension row in Settings shows its install command.
fn command_row(cmd: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    // A wrapping label rather than an entry: the whole command stays in
    // view, and it can still be selected and copied by hand.
    let text = gtk::Label::builder()
        .label(cmd)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .xalign(0.0)
        .hexpand(true)
        .margin_start(10)
        .margin_end(10)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    text.add_css_class("monospace");
    let frame = gtk::Frame::new(None);
    frame.add_css_class("view");
    frame.set_child(Some(&text));
    frame.set_hexpand(true);
    row.append(&frame);
    let copy = gtk::Button::from_icon_name("co.hyprlab.Vireo-edit-copy-symbolic");
    copy.set_tooltip_text(Some(i18n("Copy").as_str()));
    copy.set_valign(gtk::Align::Center);
    {
        let cmd = cmd.to_string();
        copy.connect_clicked(move |b| {
            b.clipboard().set_text(&cmd);
            b.set_icon_name("co.hyprlab.Vireo-verified-checkmark-symbolic");
            let b = b.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1200), move || {
                b.set_icon_name("co.hyprlab.Vireo-edit-copy-symbolic");
            });
        });
    }
    row.append(&copy);
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_name_the_new_app() {
        assert_eq!(
            install_command(Install::Flatpak).as_deref(),
            Some("flatpak install --from https://hylki.hyprlab.co/flatpak/co.hyprlab.Hylki.flatpakref")
        );
        assert_eq!(
            install_command(Install::FlatpakBeta).as_deref(),
            Some("flatpak install --from https://hylki.hyprlab.co/flatpak/co.hyprlab.Hylki.Beta.flatpakref")
        );
        assert_eq!(install_command(Install::Native), None);
    }
}
