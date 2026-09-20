# Service marks

The logos in this folder identify the cloud storage services Hylki can
upload to. They are shown next to a service's name in Settings (the Service
picker, the Cloud Storage list, the account editor) to say "this works with
that service", and for nothing else.

**They are not covered by Hylki's licence.** Each is a trademark of its
owner and is used under that owner's brand guidelines, unmodified apart
from being rendered to PNG at the size shown. Hylki is not affiliated with
or endorsed by any of them. Section 7(e) of the AGPLv3 lets the project
decline to grant trademark rights, and it does: nothing here may be reused
as if it were part of the AGPL-licensed work.

| File | Mark of | Source (fetched 2026-09-10) |
| --- | --- | --- |
| `nextcloud` | Nextcloud GmbH | https://nextcloud.com/c/uploads/2022/08/nextcloud-logo-icon.svg (linked from https://nextcloud.com/trademarks/) |
| `owncloud` | ownCloud GmbH, a Kiteworks company | `packages/web-runtime/themes/owncloud/assets/owncloud-app-icon.png` in https://github.com/owncloud/web |
| `opencloud` | OpenCloud GmbH | `packages/design-system/docs/public/logo.svg` in https://github.com/opencloud-eu/web |
| `onedrive` | Microsoft Corporation | the current OneDrive product icon, as published on Wikimedia Commons (`Microsoft OneDrive Icon (2025 - present).svg`) |
| `dropbox` | Dropbox, Inc. | the glyph of the 2017 Dropbox logo, in its brand blue #0061FF, as published on Wikimedia Commons (`Dropbox logo 2017.svg`); Dropbox's brand rules are at https://www.dropbox.com/branding |
| `seafile` | Seafile Ltd. | `data/icons/scalable/apps/seafile.svg` in https://github.com/haiwen/seafile-client |
| `gmail` | Google LLC | the current Gmail icon as published on Wikimedia Commons (`Gmail icon (2026).svg`) |
| `outlook` | Microsoft Corporation | the current Outlook icon as published on Wikimedia Commons (`Microsoft Outlook Icon (2025–present).svg`) |
| `icloud` | Apple Inc. | the iCloud logo as published on Wikimedia Commons (`ICloud logo.svg`) |
| `yahoo` | Yahoo Inc. | Yahoo's own touch icon, https://www.yahoo.com/apple-touch-icon.png |
| `proton` | Proton AG | Proton's own touch icon, https://proton.me/favicons/apple-touch-icon.png |
| `fastmail` | Fastmail Pty Ltd | the Fastmail icon as published on Wikimedia Commons (`Fastmail icon 2019.svg`) |
| `aol` | Yahoo Inc. | AOL's own touch icon, https://www.aol.com/apple-touch-icon.png |
| `zoho` | Zoho Corporation | the Zoho Mail icon as published on Wikimedia Commons (`Zoho Mail-256.png`) |
| `gmx` | 1&1 Mail & Media GmbH | the GMX logo as published on Wikimedia Commons (`GMX-Logo (2018-).svg`) |
| `yandex` | Yandex LLC | the Yandex Mail icon as published on Wikimedia Commons (`Yandex Mail icon.svg`) |
| `mailcom` | 1&1 Mail & Media Inc. | mail.com's own touch icon, https://www.mail.com/apple-touch-icon.png |
| `mail` | Hylki's own blue envelope (not a third-party mark): manual IMAP/POP3 and any account on a server the app does not recognise | drawn for the app, `src/mail.svg` |
| `mail-oauth` | Hylki's own yellow envelope (not a third-party mark): custom OAuth accounts | drawn for the app, `src/mail-oauth.svg` |

`src/` keeps the files as fetched; the 128 px PNGs beside this file are
what the binary embeds (`src/brand.rs`). To refresh one, replace the source
and re-run:

```sh
magick -background none -density 400 data/brands/src/NAME.svg -resize 128x128 \
  -gravity center -extent 128x128 data/brands/NAME.png
```

The mail providers' marks (fetched 2026-09-10) are shown in the Provider
picker, the Mail Accounts list and the account editor the same way.

The square tiles (ownCloud, OpenCloud, Yahoo, Proton, AOL, mail.com) get
the corner radius the other tile-shaped icons in the app have (20 px on
128), and nothing else changes:

```sh
magick data/brands/src/NAME.* -resize 128x128 \
  \( -size 128x128 xc:none -draw "roundrectangle 0,0,127,127,20,20" \) \
  -alpha set -compose DstIn -composite data/brands/NAME.png
```
