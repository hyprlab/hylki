//! Host platform detection (distro + desktop), used to tailor the keyring /
//! Secret Service setup guidance we show the user.
//!
//! This works both for a native build and inside the Flatpak sandbox: the
//! sandbox's own `/etc/os-release` describes the *runtime*, not the host, so we
//! prefer the host copy Flatpak mounts at `/run/host/os-release`. The desktop
//! session is read from the standard XDG env vars, which Flatpak passes through.

use std::fs;

/// Whether Hylki is running inside a Flatpak sandbox.
pub fn is_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

/// Contents of the *host* os-release (falls back to the local one natively).
fn os_release() -> String {
    for path in ["/run/host/os-release", "/etc/os-release", "/usr/lib/os-release"] {
        if let Ok(text) = fs::read_to_string(path) {
            if !text.trim().is_empty() {
                return text;
            }
        }
    }
    String::new()
}

/// Value of a `KEY=value` field in os-release text, unquoted and lowercased.
fn field_in(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    text.lines().find_map(|line| {
        line.strip_prefix(&prefix)
            .map(|v| v.trim().trim_matches('"').to_ascii_lowercase())
    })
}

/// Whether the given os-release text describes Linux Mint.
fn text_is_mint(text: &str) -> bool {
    field_in(text, "ID").as_deref() == Some("linuxmint")
        || field_in(text, "ID_LIKE")
            .is_some_and(|v| v.split_whitespace().any(|id| id == "linuxmint"))
}

/// True on Linux Mint (matches `ID=linuxmint`, or Mint listed in `ID_LIKE`).
pub fn is_linux_mint() -> bool {
    text_is_mint(&os_release())
}

/// True when the current desktop session is Cinnamon (Mint's default).
pub fn is_cinnamon() -> bool {
    let has_cinnamon = |var: &str| {
        std::env::var(var)
            .map(|v| v.to_ascii_lowercase().contains("cinnamon"))
            .unwrap_or(false)
    };
    has_cinnamon("XDG_CURRENT_DESKTOP")
        || has_cinnamon("XDG_SESSION_DESKTOP")
        || has_cinnamon("DESKTOP_SESSION")
}

/// The combination we specifically tailor keyring guidance for.
pub fn is_mint_cinnamon() -> bool {
    is_linux_mint() && is_cinnamon()
}

/// The terminal command that installs the nautilus-python bindings (the
/// loader GNOME Files needs for the Hylki extension, #188) on this machine,
/// judged by the host's os-release. `None` for a distribution whose package
/// manager and package name are not known here.
pub fn nautilus_python_install_command() -> Option<&'static str> {
    install_command_for(&os_release())
}

fn install_command_for(text: &str) -> Option<&'static str> {
    let id = field_in(text, "ID").unwrap_or_default();
    let like = field_in(text, "ID_LIKE").unwrap_or_default();
    // The distribution's own id first, then the family it says it is like
    // (Mint and Pop!_OS say ubuntu/debian, Nobara says fedora, Manjaro arch).
    let families = std::iter::once(id.as_str()).chain(like.split_whitespace());
    for family in families {
        let cmd = match family {
            "fedora" | "rhel" | "centos" | "rocky" | "almalinux" | "nobara" | "ultramarine" => {
                "sudo dnf install nautilus-python"
            }
            "debian" | "ubuntu" | "linuxmint" | "pop" | "elementary" | "zorin" | "neon" => {
                "sudo apt install python3-nautilus"
            }
            "arch" | "manjaro" | "endeavouros" | "cachyos" | "garuda" => {
                "sudo pacman -S python-nautilus"
            }
            "opensuse" | "opensuse-tumbleweed" | "opensuse-leap" | "suse" | "sles" => {
                "sudo zypper install python3-nautilus"
            }
            "gentoo" => "sudo emerge dev-python/nautilus-python",
            "alpine" => "sudo apk add nautilus-python",
            "void" => "sudo xbps-install nautilus-python",
            _ => continue,
        };
        return Some(cmd);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{field_in, install_command_for, text_is_mint};

    #[test]
    fn install_command_follows_the_distribution_family() {
        assert_eq!(install_command_for(FEDORA), Some("sudo dnf install nautilus-python"));
        // Mint is not listed itself but says it is like ubuntu/debian… and
        // is listed anyway; either path gives apt.
        assert_eq!(install_command_for(MINT), Some("sudo apt install python3-nautilus"));
        assert_eq!(
            install_command_for("ID=nobara\nID_LIKE=fedora"),
            Some("sudo dnf install nautilus-python")
        );
        assert_eq!(
            install_command_for("ID=endeavouros\nID_LIKE=arch"),
            Some("sudo pacman -S python-nautilus")
        );
        assert_eq!(
            install_command_for("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\""),
            Some("sudo zypper install python3-nautilus")
        );
        assert_eq!(install_command_for("ID=nixos"), None);
        assert_eq!(install_command_for(""), None);
    }

    const MINT: &str = r#"NAME="Linux Mint"
VERSION="22.3 (Zara)"
ID=linuxmint
ID_LIKE="ubuntu debian"
PRETTY_NAME="Linux Mint 22.3""#;

    const FEDORA: &str = "NAME=Fedora Linux\nID=fedora\nPRETTY_NAME=\"Fedora Linux 44\"";

    #[test]
    fn detects_mint_by_id() {
        assert!(text_is_mint(MINT));
        assert!(!text_is_mint(FEDORA));
        assert!(!text_is_mint(""));
    }

    #[test]
    fn detects_mint_when_only_in_id_like() {
        // A derivative that identifies as something else but lists Mint in ID_LIKE.
        let text = "ID=lmde\nID_LIKE=\"linuxmint debian\"";
        assert!(text_is_mint(text));
    }

    #[test]
    fn field_is_unquoted_and_lowercased() {
        assert_eq!(field_in(MINT, "ID").as_deref(), Some("linuxmint"));
        assert_eq!(field_in(MINT, "ID_LIKE").as_deref(), Some("ubuntu debian"));
        assert_eq!(field_in(MINT, "MISSING"), None);
    }
}
