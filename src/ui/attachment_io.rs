//! Opening an attachment in another application: staging its bytes in a
//! private directory and handing the file over, through GIO natively or
//! through the OpenURI portal inside Flatpak. And saving one where the user
//! picks.

use gtk::prelude::*;

use crate::i18n::{i18n, i18n_f};

/// Ask where to save one attachment ("Save Attachment", the file's own name
/// offered) and write it there. A place without a local path is passed over
/// in silence, as it always was; a failed write goes to `on_error`, which
/// each caller keeps as it had it (most drop it, the gallery logs it).
pub(crate) fn save_attachment_as(
    name: &str,
    data: Vec<u8>,
    parent: Option<&gtk::Window>,
    on_error: impl FnOnce(std::io::Error) + 'static,
) {
    let dialog = gtk::FileDialog::builder()
        .initial_name(name)
        .title(i18n("Save Attachment"))
        .build();
    dialog.save(parent, gtk::gio::Cancellable::NONE, move |res| {
        if let Ok(file) = res {
            if let Some(path) = file.path() {
                if let Err(e) = std::fs::write(path, &data) {
                    on_error(e);
                }
            }
        }
    });
}

/// File name endings that run as a program, or carry one, when opened with
/// the desktop's default handler: the ones a file manager would at least
/// ask about, and the Windows ones a Wine install hands to Wine.
const RISKY_EXTENSIONS: &[&str] = &[
    "desktop",
    "sh",
    "bash",
    "zsh",
    "ksh",
    "csh",
    "fish",
    "command",
    "run",
    "bin",
    "elf",
    "so",
    "appimage",
    "flatpakref",
    "flatpakrepo",
    "deb",
    "rpm",
    "apk",
    "snap",
    "flatpak",
    "jar",
    "jnlp",
    "py",
    "pyc",
    "pl",
    "rb",
    "php",
    "exe",
    "msi",
    "com",
    "scr",
    "pif",
    "bat",
    "cmd",
    "ps1",
    "psm1",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "hta",
    "lnk",
    "reg",
    "cpl",
    "msc",
    "dll",
    "docm",
    "xlsm",
    "pptm",
    "dotm",
    "xltm",
    "xlsb",
    "xlam",
    "ppsm",
    "potm",
    "ppam",
    "sldm",
];

/// Whether an attachment is a program or a script by its name, or by its
/// first bytes (an ELF or Windows binary, or a `#!` script) whatever it is
/// called. Its name and type come from whoever sent it, so the bytes are
/// looked at too.
fn is_risky_attachment(name: &str, data: &[u8]) -> bool {
    let name = name.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    let risky_name = name.rsplit_once('.').is_some_and(|(_, ext)| {
        RISKY_EXTENSIONS
            .iter()
            .any(|r| r.eq_ignore_ascii_case(ext.trim()))
    });
    risky_name || data.starts_with(b"\x7fELF") || data.starts_with(b"MZ") || data.starts_with(b"#!")
}

/// Open an attachment, asking first when it is a program or a script (see
/// [`is_risky_attachment`]).
pub(crate) fn open_bytes(name: &str, data: &[u8], parent: Option<&gtk::Window>) {
    if !is_risky_attachment(name, data) {
        open_now(name, data, parent);
        return;
    }
    let (owned_name, owned_data, owned_parent) = (name.to_string(), data.to_vec(), parent.cloned());
    crate::ui::dialog::confirm_destructive(
        parent,
        &crate::i18n::i18n_f("Open “{name}”?", &[("name", name)]),
        &i18n(
            "This attachment is a program or a script. Opening it can run it with your permissions. Open it only if you expected it and trust where it came from.",
        ),
        "open",
        &i18n("Open Anyway"),
        move || open_now(&owned_name, &owned_data, owned_parent.as_ref()),
    );
}

/// Write bytes to a temp file and open it in the default application.
///
/// The filename comes from an email, so it comes from whoever sent it. The
/// sanitizer below removes every path separator, which is what stops traversal;
/// the rest of the care here is about `/tmp` being shared on a native install
/// (under Flatpak it is per-app): the directory is created private and its
/// ownership checked, and each file is created fresh rather than written
/// through whatever already sits at a guessable path.
///
/// Launched through the portal's `UriLauncher` rather than
/// `AppInfo::launch_default_for_uri`: for a type with no registered default
/// (common for attachments — nothing may ever have been "set as default" for
/// a `.pdf`) the portal falls back to GNOME's own app-chooser dialog instead
/// of silently doing nothing.
fn open_now(name: &str, data: &[u8], parent: Option<&gtk::Window>) {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let Some(dir) = attachment_dir() else { return };
    let base = if safe.is_empty() {
        "attachment".to_string()
    } else {
        safe
    };
    let Some((mut file, path)) = create_private(&dir, &base) else {
        tracing::warn!("could not open a temporary file for the attachment");
        return;
    };
    // Write through the handle rather than reopening by path: the checks below
    // are about what is at that name, and going back through it would hand the
    // result to whatever is there by the time we look again.
    if let Err(e) = std::io::Write::write_all(&mut file, data) {
        tracing::warn!("could not write the attachment: {e}");
        return;
    }
    drop(file);
    let uri = format!("file://{}", path.to_string_lossy());
    // Outside Flatpak, GIO launches the default handler directly; the portal
    // route has been seen accepting an OpenURI request and then launching
    // nothing — reported success, no app, click looked dead. Inside the
    // sandbox the portal is the only road out and GIO's launcher is the one
    // that can't work. So each side leads with the road that works for it
    // and keeps the other as the fallback.
    if crate::desktop::platform::is_flatpak() {
        // Inside the sandbox the file was staged into the app's PRIVATE /tmp —
        // a path the host cannot read. Handing the portal a file:// URI string
        // therefore launches a handler pointed at a file that, host-side, does
        // not exist. FileLauncher instead passes the file as a descriptor
        // through the document portal, which exports it where the handler can
        // read it. When even that fails (a broken portal), say so instead of
        // doing nothing — a host-side AppInfo fallback can't work here.
        // Not GTK's FileLauncher: in this runtime it mis-finishes its own
        // async task in the sandboxed path (task-tag assertion), so its
        // callback never fires and every failure vanishes. Speaking the
        // portal protocol directly restores the contract.
        portal_open_file(path, false, parent.cloned());
    } else if let Err(e) =
        gtk::gio::AppInfo::launch_default_for_uri(&uri, gtk::gio::AppLaunchContext::NONE)
    {
        tracing::warn!("gio launch failed ({e}), trying the portal");
        let owned = parent.cloned();
        gtk::UriLauncher::new(&uri).launch(parent, gtk::gio::Cancellable::NONE, move |res| {
            if let Err(e) = res {
                tracing::warn!("portal launch also failed: {e}");
                launch_failed_dialog(owned.as_ref(), &e.to_string());
            }
        });
    }
}

/// Both launch roads failed — tell the user what happened and what still
/// works, rather than leaving a click that does nothing.
fn launch_failed_dialog(parent: Option<&gtk::Window>, error: &str) {
    let dialog = gtk::AlertDialog::builder()
        .message(i18n("The file could not be opened"))
        // No Flatseal toggle can help here: portal access is not a
        // permission, and the failure is inside the portal's own launcher.
        // Offer the steps that actually work.
        .detail(i18n_f(
            "The desktop portal reported: {error}\n\n• Download the file, then open it from Files\n• Updating “xdg-desktop-portal” and logging back in may fix direct opening",
            &[("error", error)],
        ))
        .modal(true)
        .build();
    dialog.show(parent);
}

/// Open a staged file through the OpenURI portal (`portal_request` in the
/// launch module speaks the protocol). Response 0 is success. On the quiet
/// attempt (`ask == false`) any failure retries once with the app chooser —
/// the backend's own dialog, whose launch machinery works even where the
/// direct default-handler launch is broken (seen on Fedora 44), and whose
/// "always open with" sticks in the permission store. A cancelled chooser
/// (1) is not an error; any other chooser failure gets the dialog.
fn portal_open_file(path: std::path::PathBuf, ask: bool, parent: Option<gtk::Window>) {
    use gtk::gio;
    use gtk::glib;
    use gtk::glib::prelude::*;
    use std::os::fd::AsFd;

    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("staged attachment vanished: {e}");
            return;
        }
    };
    let fd_list = gio::UnixFDList::new();
    let handle = match fd_list.append(file.as_fd()) {
        Ok(h) => h,
        Err(e) => {
            launch_failed_dialog(parent.as_ref(), &e.to_string());
            return;
        }
    };
    let retry_path = path.clone();
    let retry_parent = parent.clone();
    crate::ui::launch::portal_request(
        "OpenFile",
        Some(fd_list),
        move |token| {
            let options = glib::VariantDict::new(None);
            options.insert_value("handle_token", &token.to_variant());
            if ask {
                options.insert_value("ask", &true.to_variant());
            }
            // Not the tuple's ToVariant: that boxes the dict as a nested "v"
            // and the portal rejects "(shv)". tuple_from_iter splices each
            // child at its own type, producing the "(sha{sv})" the interface
            // declares.
            glib::Variant::tuple_from_iter([
                "".to_variant(),
                glib::variant::Handle(handle).to_variant(),
                options.end(),
            ])
        },
        move |res| match res {
            Err(e) => {
                tracing::warn!("portal OpenFile call failed: {e}");
                launch_failed_dialog(retry_parent.as_ref(), &e);
            }
            // The user cancelled — on EITHER attempt. When no default
            // handler is registered, the backend shows its chooser even on
            // the quiet attempt, so a cancel can arrive with ask == false
            // too; treating that as a failure re-asked and the dialog popped
            // right back up (issue #65). A cancel is an answer, never a
            // reason to ask again.
            Ok(0) | Ok(1) => {}
            Ok(code) if !ask => {
                tracing::warn!("portal open answered {code}; retrying with the chooser");
                portal_open_file(retry_path.clone(), true, retry_parent.clone());
            }
            Ok(code) => launch_failed_dialog(
                retry_parent.as_ref(),
                &format!("the portal answered response code {code}"),
            ),
        },
    );
}

/// The private directory opened attachments are staged in, created if needed.
///
/// Returns `None` if the path exists but is not a directory we own with nobody
/// else's access — an attacker who pre-creates it on a shared host would
/// otherwise get every attachment the user opens.
fn attachment_dir() -> Option<std::path::PathBuf> {
    let dir = staging_dir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        if !dir.exists() {
            let _ = std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir);
        }
        // `symlink_metadata`, so a symlink pointing at somewhere friendlier is
        // seen for what it is rather than followed.
        let md = std::fs::symlink_metadata(&dir).ok()?;
        if !md.is_dir() || md.uid() != our_uid() {
            tracing::warn!("{} is not ours; not staging attachments", dir.display());
            return None;
        }
        if md.permissions().mode() & 0o077 != 0 {
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::create_dir_all(&dir);
    }
    Some(dir)
}

/// Create a new 0600 file for `base` in `dir`, returning it open with its path.
///
/// `create_new` means an existing file — or a symlink planted at a guessable
/// name — makes the call fail rather than being written through, so the next
/// candidate name is tried instead. Overwriting the user's previous copy of the
/// same attachment silently would also be wrong.
fn create_private(
    dir: &std::path::Path,
    base: &str,
) -> Option<(std::fs::File, std::path::PathBuf)> {
    for n in 0..64 {
        let name = if n == 0 {
            base.to_string()
        } else {
            match base.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() => format!("{stem}-{n}.{ext}"),
                _ => format!("{base}-{n}"),
            }
        };
        let path = dir.join(&name);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
            // Refuse to follow a symlink instead of racing to check for one.
            // 0o400000 is O_NOFOLLOW on every Linux arch Hylki ships for; the
            // wrong constant here once passed O_DIRECTORY (0o200000) instead,
            // which made this open() fail and every attachment click a no-op.
            opts.custom_flags(0o400000 /* O_NOFOLLOW */);
        }
        if let Ok(file) = opts.open(&path) {
            return Some((file, path));
        }
    }
    None
}

/// This process's real user ID.
///
/// Hylki has no libc dependency; `getuid` is always available and cannot fail,
/// so declaring it directly is cheaper than taking one on.
#[cfg(unix)]
fn our_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

/// Where opened attachments are staged.
///
/// Inside Flatpak /tmp is PRIVATE to the sandbox: the document portal
/// validates an exported fd by re-opening its path in the HOST namespace,
/// so a /tmp-staged file fails validation and every portal open dies
/// silently before any UI. $XDG_RUNTIME_DIR/app/$FLATPAK_ID is
/// bind-mounted from the host at the SAME path — the one temp location
/// both sides of the sandbox agree on (and it's session-scoped tmpfs,
/// like /tmp). Native builds keep /tmp.
fn staging_dir() -> Option<std::path::PathBuf> {
    staging_dir_for(
        std::env::var("FLATPAK_ID").ok().as_deref(),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        &std::env::temp_dir(),
    )
}

fn staging_dir_for(
    flatpak_id: Option<&str>,
    runtime_dir: Option<&str>,
    temp_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    match flatpak_id {
        Some(app_id) => Some(
            std::path::Path::new(runtime_dir?)
                .join("app")
                .join(app_id)
                .join("vireo-attachments"),
        ),
        None => Some(temp_dir.join("vireo-attachments")),
    }
}

/// Delete anything left in the attachment staging directory.
///
/// Opened attachments used to accumulate in `/tmp` for the life of the machine —
/// decrypted, readable, and long past the point the user considers them gone.
/// Called once at startup, because the helper application the user opened a file
/// with may still have it open while Hylki is running. It clears the directory
/// [`attachment_dir`] stages in, which under Flatpak is not /tmp.
pub(crate) fn purge_attachment_dir() {
    let Some(dir) = staging_dir() else { return };
    if !dir.exists() {
        return;
    }
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => tracing::debug!("cleared staged attachments in {}", dir.display()),
        Err(e) => tracing::warn!("could not clear {}: {e}", dir.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_and_scripts_are_asked_about_by_name_or_by_bytes() {
        for (name, data, risky) in [
            ("report.pdf", &b"%PDF-1.7"[..], false),
            ("photo.JPG", b"\xff\xd8\xff", false),
            ("notes.txt", b"plain words", false),
            ("invite.ics", b"BEGIN:VCALENDAR", false),
            ("setup.desktop", b"[Desktop Entry]", true),
            ("install.sh", b"echo", true),
            ("Tool.AppImage", b"", true),
            ("app.jar", b"PK", true),
            ("invoice.pdf.exe", b"", true),
            ("invoice.exe. ", b"", true),
            ("macro.DOCM", b"PK", true),
            ("show.ppsm", b"PK", true),
            ("App.flatpak", b"", true),
            // What the name hides, the first bytes give away.
            ("invoice.pdf", b"\x7fELF\x02\x01", true),
            ("readme.txt", b"#!/bin/sh\nrm -rf ~", true),
            ("cv.doc", b"MZ\x90\x00", true),
            // The desktop types these by the ending, so they count as one.
            (".sh", b"", true),
            (".desktop", b"", true),
            ("noextension", b"", false),
        ] {
            assert_eq!(is_risky_attachment(name, data), risky, "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn staged_files_are_new_private_and_never_written_through() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("hylki-stage-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let name = |base: &str| {
            let (_, path) = create_private(&dir, base).expect("a file");
            path.file_name().unwrap().to_string_lossy().into_owned()
        };
        assert_eq!(name("a.txt"), "a.txt");
        assert_eq!(name("a.txt"), "a-1.txt");
        assert_eq!(name("a.txt"), "a-2.txt");
        assert_eq!(name(".bashrc"), ".bashrc");
        assert_eq!(name(".bashrc"), ".bashrc-1");
        assert_eq!(name("noext"), "noext");
        assert_eq!(name("noext"), "noext-1");
        let mode = std::fs::metadata(dir.join("a.txt"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // A symlink planted at the name is skipped, not followed.
        let target = dir.join("target");
        std::fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, dir.join("b.txt")).unwrap();
        assert_eq!(name("b.txt"), "b-1.txt");
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The startup purge and the staging must agree on the directory:
    /// under Flatpak the purge once cleared /tmp while the files sat in the
    /// runtime directory.
    #[test]
    fn the_purge_clears_where_attachments_are_staged() {
        let tmp = std::path::Path::new("/tmp");
        assert_eq!(
            staging_dir_for(Some("co.hyprlab.Hylki"), Some("/run/user/1000"), tmp),
            Some(std::path::PathBuf::from(
                "/run/user/1000/app/co.hyprlab.Hylki/vireo-attachments"
            ))
        );
        assert_eq!(
            staging_dir_for(None, Some("/run/user/1000"), tmp),
            Some(std::path::PathBuf::from("/tmp/vireo-attachments"))
        );
        // Without a runtime directory a Flatpak has nowhere to stage, and
        // nothing to purge.
        assert_eq!(staging_dir_for(Some("co.hyprlab.Hylki"), None, tmp), None);
    }
}
