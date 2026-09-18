%global appid co.hyprlab.Vireo
# The payload is a binary built outside rpmbuild (tools/build-packages.sh),
# so there is nothing to extract debuginfo from.
%global debug_package %{nil}

Name:           vireo
Version:        1.33.2
Release:        1%{?dist}
Summary:        A clean, fast GNOME-native email client
License:        AGPL-3.0-or-later
URL:            https://vireo.hyprlab.co
Source0:        %{name}-%{version}-bin.tar

# Passwords and OAuth tokens are stored via the Secret Service D-Bus API
Recommends:     gnome-keyring
# The GNOME Files right-click entry (Settings -> System -> GNOME Files) is
# a Python extension; this is the loader Files needs for it.
Recommends:     nautilus-python

# Renamed from "veem" in 1.6.0 — upgrades replace the old package
Provides:       veem = %{version}-%{release}
Obsoletes:      veem < 1.6.0

%description
Vireo is a desktop email client for Wayland that feels at home in GNOME. It
talks IMAP/SMTP directly, keeps your mail and credentials on your machine,
and blocks trackers by default - no telemetry, no analytics.

%prep
%setup -q -n %{name}-%{version}-bin

%install
install -Dm755 vireo %{buildroot}%{_bindir}/vireo
install -Dm644 %{appid}.desktop %{buildroot}%{_datadir}/applications/%{appid}.desktop
install -Dm644 %{appid}.metainfo.xml %{buildroot}%{_datadir}/metainfo/%{appid}.metainfo.xml
for size in 256x256 512x512; do
    install -Dm644 icons/$size/%{appid}.png \
        %{buildroot}%{_datadir}/icons/hicolor/$size/apps/%{appid}.png
done
install -Dm644 icons/scalable/%{appid}.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/%{appid}.svg
# Message catalogues, staged by tools/build-packages.sh as
# locale/<lang>/LC_MESSAGES/vireo.mo; the binary looks under
# %{_datadir}/locale beside itself (src/i18n.rs).
for mo in locale/*/LC_MESSAGES/vireo.mo; do
    [ -e "$mo" ] || continue
    install -Dm644 "$mo" %{buildroot}%{_datadir}/"$mo"
done
%find_lang %{name}

%files -f %{name}.lang
%license LICENSE
%{_bindir}/vireo
%{_datadir}/applications/%{appid}.desktop
%{_datadir}/metainfo/%{appid}.metainfo.xml
%{_datadir}/icons/hicolor/*/apps/%{appid}.png
%{_datadir}/icons/hicolor/scalable/apps/%{appid}.svg

%changelog
* Mon Aug 03 2026 Hyprlab <hyprlab@proton.me> - 1.6.0-1
- Veem is now Vireo: app ID co.hyprlab.Vireo, binary /usr/bin/vireo; user
  config, cache and keyring entries migrate automatically on first launch

* Mon Aug 03 2026 Hyprlab <hyprlab@proton.me> - 1.5.1-1
- Native RPM packaging (built from the prebuilt release binary)
