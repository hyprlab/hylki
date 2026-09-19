%global appid co.hyprlab.Hylki
# The payload is a binary built outside rpmbuild (tools/build-packages.sh),
# so there is nothing to extract debuginfo from.
%global debug_package %{nil}

Name:           hylki
Version:        1.35.3
Release:        1%{?dist}
Summary:        A clean, fast GNOME-native email client
License:        AGPL-3.0-or-later
URL:            https://hylki.hyprlab.co
Source0:        %{name}-%{version}-bin.tar

# Passwords and OAuth tokens are stored via the Secret Service D-Bus API
Recommends:     gnome-keyring
# The GNOME Files right-click entry (Settings -> System -> GNOME Files) is
# a Python extension; this is the loader Files needs for it.
Recommends:     nautilus-python

# Renamed from "veem" in 1.6.0 and from "vireo" in 1.35.0 — upgrades
# replace the old package; user data carries over on first start.
Provides:       vireo = %{version}-%{release}
Obsoletes:      vireo < 1.35.0
Provides:       veem = %{version}-%{release}
Obsoletes:      veem < 1.6.0

%description
Hylki is a desktop email client for Wayland that feels at home in GNOME. It
talks IMAP/SMTP directly, keeps your mail and credentials on your machine,
and blocks trackers by default - no telemetry, no analytics.

%prep
%setup -q -n %{name}-%{version}-bin

%install
install -Dm755 hylki %{buildroot}%{_bindir}/hylki
install -Dm644 %{appid}.desktop %{buildroot}%{_datadir}/applications/%{appid}.desktop
install -Dm644 %{appid}.metainfo.xml %{buildroot}%{_datadir}/metainfo/%{appid}.metainfo.xml
for size in 256x256 512x512; do
    install -Dm644 icons/$size/%{appid}.png \
        %{buildroot}%{_datadir}/icons/hicolor/$size/apps/%{appid}.png
done
install -Dm644 icons/scalable/%{appid}.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/%{appid}.svg
# Message catalogues, staged by tools/build-packages.sh as
# locale/<lang>/LC_MESSAGES/hylki.mo; the binary looks under
# %{_datadir}/locale beside itself (src/i18n.rs).
for mo in locale/*/LC_MESSAGES/hylki.mo; do
    [ -e "$mo" ] || continue
    install -Dm644 "$mo" %{buildroot}%{_datadir}/"$mo"
done
%find_lang %{name}

%files -f %{name}.lang
%license LICENSE
%{_bindir}/hylki
%{_datadir}/applications/%{appid}.desktop
%{_datadir}/metainfo/%{appid}.metainfo.xml
%{_datadir}/icons/hicolor/*/apps/%{appid}.png
%{_datadir}/icons/hicolor/scalable/apps/%{appid}.svg

%changelog
* Fri Sep 18 2026 Hyprlab <hyprlab@proton.me> - 1.35.0-1
- Vireo is now Hylki: app ID co.hyprlab.Hylki, binary /usr/bin/hylki; user
  config, cache and keyring entries carry over automatically on first start

* Mon Aug 03 2026 Hyprlab <hyprlab@proton.me> - 1.6.0-1
- Veem is now Vireo: app ID co.hyprlab.Vireo, binary /usr/bin/vireo; user
  config, cache and keyring entries migrate automatically on first launch

* Mon Aug 03 2026 Hyprlab <hyprlab@proton.me> - 1.5.1-1
- Native RPM packaging (built from the prebuilt release binary)
