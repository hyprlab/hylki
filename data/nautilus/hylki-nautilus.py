# hylki-nautilus.py
#
# GNOME Files (Nautilus) extension that adds "Send with Hylki" to the
# right-click menu: the selected files open in a new Hylki message, already
# attached (https://github.com/hyprlab/hylki, issue #188).
#
# It launches Hylki by its desktop id, so it works whether Hylki is the
# Flatpak or a native package. Hylki installs this file for you from
# Settings -> System -> GNOME Files. By hand, copy it to
#   ~/.local/share/nautilus-python/extensions/      (per user)
#   /usr/share/nautilus-python/extensions/          (system wide)
# and restart Files with `nautilus -q`.
#
# Needs the nautilus-python bindings: nautilus-python (Fedora),
# python3-nautilus (Debian, Ubuntu), python-nautilus (Arch).
#
# SPDX-License-Identifier: AGPL-3.0-or-later

import os
import sys

import gi

for _version in ("4.1", "4.0"):
    try:
        gi.require_version("Nautilus", _version)
        break
    except ValueError:
        continue

from gi.repository import Gio, GLib, GObject, Nautilus  # noqa: E402

__version__ = "1"

# The stable build first, the beta after it; whichever is installed answers.
APP_IDS = ("co.hyprlab.Hylki", "co.hyprlab.Hylki.Beta")

# Nautilus loads this file outside the app, so its translations live here
# rather than in Hylki's catalogues.
_STRINGS = {
    "label": "Send with Hylki",
    "tip": "Attach the selected files to a new message in Hylki",
}

_TRANSLATIONS = {
    "es": {
        "label": "Enviar con Hylki",
        "tip": "Adjuntar los archivos seleccionados a un nuevo mensaje en Hylki",
    },
    "fr": {
        "label": "Envoyer avec Hylki",
        "tip": "Joindre les fichiers sélectionnés à un nouveau message dans Hylki",
    },
    "hu": {
        "label": "Küldés a Vireóval",
        "tip": "A kijelölt fájlok csatolása egy új üzenethez a Vireóban",
    },
    "pt": {
        "label": "Enviar com o Hylki",
        "tip": "Anexar os ficheiros selecionados a uma nova mensagem no Hylki",
    },
    "ru": {
        "label": "Отправить через Hylki",
        "tip": "Прикрепить выбранные файлы к новому сообщению в Hylki",
    },
}


def _preferred_languages():
    base = ""
    for var in ("LC_ALL", "LC_MESSAGES", "LANG"):
        value = os.environ.get(var, "")
        if value:
            base = value
            break
    if not base or base.split(".")[0] in ("C", "POSIX"):
        return []
    language = os.environ.get("LANGUAGE", "")
    candidates = language.split(":") if language else [base]
    languages = []
    for value in candidates:
        code = value.split(".")[0].split("_")[0].strip().lower()
        if code and code not in languages:
            languages.append(code)
    return languages


def _strings():
    for code in _preferred_languages():
        if code == "en":
            break  # the source language outranks any lower preference
        if code in _TRANSLATIONS:
            return {**_STRINGS, **_TRANSLATIONS[code]}
    return _STRINGS


_S = _strings()


def _log(*values):
    print("Hylki:", *values, file=sys.stderr)


def _mark_loaded():
    # Hylki's settings cannot tell from the file alone whether Files has
    # actually loaded it (the nautilus-python bindings may be missing), so
    # note here that this copy was loaded: a hidden file next to it holding
    # the SHA-256 of this file, which Hylki compares with its own copy.
    try:
        import hashlib

        with open(__file__, "rb") as f:
            digest = hashlib.sha256(f.read()).hexdigest()
        marker = os.path.join(os.path.dirname(__file__), ".hylki-nautilus.loaded")
        with open(marker, "w") as f:
            f.write(digest + "\n")
    except OSError as error:
        _log("could not note the load:", error)


_mark_loaded()


def _launch_context():
    # Files' own launch context carries an activation token, which is what
    # lets Hylki's window come to the front over Files.
    try:
        gi.require_version("Gdk", "4.0")
        from gi.repository import Gdk

        display = Gdk.Display.get_default()
        if display is not None:
            return display.get_app_launch_context()
    except Exception as error:  # noqa: BLE001 - any failure just means no token
        _log("no launch context:", error)
    return None


def _desktop_app_info(desktop_id):
    # GLib 2.86 moved DesktopAppInfo to GioUnix; older ones only have Gio's.
    try:
        gi.require_version("GioUnix", "2.0")
        from gi.repository import GioUnix

        return GioUnix.DesktopAppInfo.new(desktop_id)
    except (ValueError, ImportError, AttributeError):
        return Gio.DesktopAppInfo.new(desktop_id)


def _appimage_path():
    """The AppImage Hylki last ran from, which the app records for us.

    An AppImage installs no `hylki` command and lives wherever the user
    keeps it, so there is nothing to find on PATH; the app writes the
    bundle's own path where we can read it. Returns None for a bundle that
    has since been moved or deleted.
    """
    path = os.path.join(GLib.get_user_config_dir(), "hylki", "launcher")
    try:
        with open(path, encoding="utf-8") as handle:
            bundle = handle.read().strip()
    except OSError:
        return None
    return bundle if bundle and os.access(bundle, os.X_OK) else None


def _launch(uris):
    for app_id in APP_IDS:
        info = _desktop_app_info(app_id + ".desktop")
        if info is None:
            continue
        try:
            info.launch_uris(uris, _launch_context())
            return
        except GLib.Error as error:
            _log("could not launch", app_id + ":", error)
    # No desktop entry found: the command line, native, AppImage or Flatpak.
    appimage = _appimage_path()
    if GLib.find_program_in_path("hylki"):
        argv = ["hylki"] + uris
    elif appimage:
        argv = [appimage] + uris
    elif GLib.find_program_in_path("flatpak"):
        argv = ["flatpak", "run", "--file-forwarding", APP_IDS[0], "@@u"] + uris + ["@@"]
    else:
        _log("Hylki is not installed")
        return
    try:
        Gio.Subprocess.new(argv, Gio.SubprocessFlags.NONE)
    except GLib.Error as error:
        _log("could not start Hylki:", error)


class HylkiMenuProvider(GObject.GObject, Nautilus.MenuProvider):
    def get_file_items(self, *args):
        files = args[-1]
        uris = []
        for f in files:
            # Folders cannot be attached; files on a mounted share (SMB,
            # SFTP, ...) reach the app through their local mount path.
            if f.is_directory():
                continue
            location = f.get_location()
            path = location.get_path() if location is not None else None
            if not path:
                continue
            uris.append(Gio.File.new_for_path(path).get_uri())
        if not uris:
            return []
        item = Nautilus.MenuItem(
            name="HylkiMenuProvider::send",
            label=_S["label"],
            tip=_S["tip"],
        )
        item.connect("activate", lambda _item: _launch(uris))
        return [item]
