# Licence and third-party notices

Hylki is free software licensed under the **GNU Affero General Public License
v3.0 or later** ([AGPL-3.0-or-later](../LICENSE)). The full text is in
[LICENSE](../LICENSE) at the top of the repository.

Contributions ship under the same licence; there is no CLA
([CONTRIBUTING.md](CONTRIBUTING.md)).

## Trademarks

The Nextcloud, ownCloud, OpenCloud, OneDrive, Dropbox and Seafile marks shown
in Settings are trademarks of their owners, used only to identify those
services, and are not covered by that licence (see
[data/brands/README.md](../data/brands/README.md)). Hylki is not affiliated with
or endorsed by any of them. The same goes for the bundled sender logos that
can fill a sender's avatar: they come from
[gilbarbara/logos](https://github.com/gilbarbara/logos) (MIT) and
[Simple Icons](https://simpleicons.org) (CC0), remain their owners' marks, and
are listed with their sources in [data/logos/README.md](../data/logos/README.md).

## New-mail sounds

The built-in new-mail sounds (Click, Hum, String and Swing) are GNOME's alert
sounds from [GNOME Settings](https://gitlab.gnome.org/GNOME/gnome-control-center)
(GPL-2.0-or-later), unmodified; see
[data/sounds/README.md](../data/sounds/README.md).

## Appearance themes

The appearance themes (Rose, Forest, Tidal, Earth and Midnight) are the theme
library from [T3 Code](https://github.com/pingdotgg/t3code) (MIT, © 2026 T3
Tools Inc.), converted to sRGB, renamed, and mapped onto libadwaita's colour
roles by `tools/gen-themes.py`. Hylki is not affiliated with or endorsed by
T3 Tools.

## SQLCipher

The mail cache's encryption (Settings → Privacy → Encrypt stored mail) is
[SQLCipher](https://www.zetetic.net/sqlcipher/), built into Hylki from the
copy the `rusqlite` crate bundles, under this licence:

```text
Copyright (c) 2008-2020 Zetetic LLC
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:
    * Redistributions of source code must retain the above copyright
      notice, this list of conditions and the following disclaimer.
    * Redistributions in binary form must reproduce the above copyright
      notice, this list of conditions and the following disclaimer in the
      documentation and/or other materials provided with the distribution.
    * Neither the name of the ZETETIC LLC nor the
      names of its contributors may be used to endorse or promote products
      derived from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY ZETETIC LLC ''AS IS'' AND ANY
EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL ZETETIC LLC BE LIABLE FOR ANY
DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
(INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND
ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## Grammar checkers

Hylki does not ship a grammar checker. When asked to in Settings, it
downloads one from its publisher: [Harper](https://github.com/Automattic/harper)
(Apache-2.0), or [LanguageTool](https://languagetool.org) (LGPL-2.1-or-later)
with the [Eclipse Temurin](https://adoptium.net) Java runtime (GPL-2.0 with
the Classpath Exception). Each runs as a program of its own, under its own
licence, and is not part of Hylki.

© 2026 Hyprlab
