#!/usr/bin/env python3
"""Regenerate src/theme_palettes.rs from the T3 Code theme definitions.

Hylki's appearance themes (Settings -> Appearance -> Theme) are the palettes
T3 Code ships, adapted to libadwaita's colour roles and renamed for Hylki
(see NAMES below). The upstream palettes live in a TypeScript source and are
written in OKLCH; this script converts them to sRGB hex, which is what GTK's
named colours and the reader documents both want.

Usage:
    git clone --depth 1 https://github.com/pingdotgg/t3code /tmp/t3code
    tools/gen-themes.py /tmp/t3code

T3 Code is MIT-licensed (Copyright (c) 2026 T3 Tools Inc.); the generated
file carries that notice.
"""

import math
import re
import sys
from pathlib import Path

# Hylki's own name for each upstream palette: its id, as saved in
# privacy.toml, and the name the picker shows. An upstream theme missing from
# here is not bundled -- T3 Code's own default look is one product's house
# style rather than a theme to choose, so Hylki does not carry it.
NAMES = {
    "t3-chat": ("rose", "Rose"),
    "grove": ("forest", "Forest"),
    "ocean": ("tidal", "Tidal"),
    "ember": ("earth", "Earth"),
    "iris": ("midnight", "Midnight"),
}

# The order the picker shows the themes in, by Hylki id. Upstream's own
# order is not one anybody chose; this one is.
PICKER_ORDER = ["midnight", "tidal", "rose", "earth", "forest"]

# Hylki's own departures from upstream, by (theme id, variant). A value is
# either a literal hex or the name of another role in the same palette, which
# is resolved after parsing so it tracks upstream rather than freezing a
# colour. Anything here survives a regeneration, so put a reason next to it.
#
# accent: upstream authors its dark accents very light, which reads as neon
# against Hylki's darker chrome; these three are taken down a few steps.
#
# accentForeground rides along where it has to: libadwaita paints it on the
# accent, so a dark accent needs light text on it (Midnight's would otherwise
# sit at 2.3:1, unreadable; white is 7.6:1). Forest and Tidal stay light
# enough for the dark foreground upstream gives them (6.5:1 and 6.0:1).
#
# sidebarBorder: upstream's dark value is a mid grey, several steps brighter
# than the sidebar it edges, so the divider between the sidebar and the
# message list glared. Following the palette's own hairline puts it at the
# weight of the other pane divider, which takes `border` too (see theme.rs).
# Rose is left alone: upstream already gives it a hairline-weight value.
OVERRIDES = {
    ("midnight", "dark"): {
        "accent": "#5d41a6",
        "accentForeground": "#ffffff",
        "sidebarBorder": "border",
    },
    ("forest", "dark"): {"accent": "#3eb272", "sidebarBorder": "border"},
    ("tidal", "dark"): {"accent": "#509ed8", "sidebarBorder": "border"},
    ("earth", "dark"): {"accent": "#ea8946", "sidebarBorder": "border"},
    # Rose's dark palette is not built like the other four: upstream gives it
    # grounds at hue 270 (which is Midnight's violet, so the two read alike),
    # a sidebar darker than its own canvas, and hairlines and panel tints
    # several steps too dark to see. Its shell is rebuilt here to the family's
    # saturation and lightness per role -- the median of Forest, Tidal, Earth
    # and Midnight -- at Rose's own hue, 330. Text, accent and the semantic
    # colours are upstream's still.
    ("rose", "dark"): {
        "accent": "#a63962",
        "canvas": "#291921",
        "chrome": "#291921",
        "toolbar": "#291921",
        "surface": "#291921",
        "surfaceRaised": "#43343c",
        "surfaceOverlay": "#4f4249",
        "sidebar": "#39202c",
        "sidebarControlSurface": "#58434e",
        "sidebarRowHover": "#4e293c",
        "sidebarRowActive": "#5c3046",
        "sidebarRowSelected": "#63334b",
        "border": "#654153",
        "toolbarBorder": "#6c3752",
        "toolbarControl": "#502a3d",
        "toolbarControlHover": "#63334b",
        "secondary": "#502a3d",
        "accentSurface": "#63334b",
        "messageSurface": "#6e3853",
        "codeBackground": "#36272f",
        "muted": "#422433",
        "input": "#794f64",
        "sidebarBorder": "border",
    },
    # And in light, where the rest of the palette is fine: upstream's light
    # sidebarBorder is a neutral grey while every other theme's follows its
    # border, so Rose alone had a grey line against a pink one.
    ("rose", "light"): {"accent": "#b75077", "sidebarBorder": "border"},
}

# The roles Hylki maps onto libadwaita colours (see src/theme.rs). Upstream
# carries more (terminal, message actions); those have no Hylki counterpart.
ROLES = [
    "canvas", "chrome", "toolbar", "toolbarForeground", "toolbarBorder",
    "toolbarControl", "toolbarControlForeground", "toolbarControlHover",
    "surface", "surfaceRaised", "surfaceOverlay", "text", "textMuted",
    "border", "input", "focus", "accent", "accentForeground", "secondary",
    "secondaryForeground", "muted", "mutedForeground", "placeholder",
    "error", "errorForeground", "errorSurface", "warning", "warningForeground",
    "warningSurface", "accentSurface", "accentSurfaceForeground",
    "messageSurface", "codeBackground", "codeForeground", "sidebar",
    "sidebarForeground", "sidebarMutedForeground", "sidebarControlSurface",
    "sidebarRowHover", "sidebarRowActive", "sidebarRowSelected", "sidebarBorder",
]


def snake(name: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


def oklch_to_hex(lightness: float, chroma: float, hue: float) -> str:
    """OKLCH -> sRGB hex, clipped to the gamut (these palettes sit inside it)."""
    rad = math.radians(hue)
    a = chroma * math.cos(rad)
    b = chroma * math.sin(rad)
    l_ = lightness + 0.3963377774 * a + 0.2158037573 * b
    m_ = lightness - 0.1055613458 * a - 0.0638541728 * b
    s_ = lightness - 0.0894841775 * a - 1.2914855480 * b
    l, m, s = l_ ** 3, m_ ** 3, s_ ** 3
    red = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s
    green = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s
    blue = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s

    def encode(value: float) -> int:
        value = max(0.0, min(1.0, value))
        value = 12.92 * value if value <= 0.0031308 else 1.055 * value ** (1 / 2.4) - 0.055
        return max(0, min(255, round(value * 255)))

    return "#%02x%02x%02x" % (encode(red), encode(green), encode(blue))


def to_hex(value: str) -> str:
    value = value.strip()
    if value.startswith("#"):
        return value.lower()
    match = re.match(r"oklch\(\s*([\d.]+)\s+([\d.]+)\s+([\d.-]+)\s*\)$", value)
    if not match:
        raise SystemExit(f"unhandled colour: {value}")
    return oklch_to_hex(*(float(g) for g in match.groups()))


def parse_colors(block: str) -> dict:
    return {k: to_hex(v) for k, v in re.findall(r'(\w+):\s*"([^"]+)"', block)}


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    repo = Path(sys.argv[1])
    shared = (repo / "packages/shared/src/themePalettes.ts").read_text()

    themes = []
    for match in re.finditer(r"export const \w+_THEME: ThemeDefinition = \{(.*?)\n\};", shared, re.S):
        body = match.group(1)
        upstream = re.search(r'id:\s*"([^"]+)"', body).group(1)
        if upstream not in NAMES:
            continue
        theme_id, label = NAMES[upstream]
        theme = {
            "id": theme_id,
            "label": label,
            # Every built-in is authored light, with dark as its variant.
            "light": parse_colors(re.search(r"\n  colors: \{(.*?)\n  \},", body, re.S).group(1)),
            "dark": parse_colors(re.search(r"\n    dark: \{(.*?)\n    \},", body, re.S).group(1)),
        }
        for variant in ("light", "dark"):
            palette = theme[variant]
            for role, value in OVERRIDES.get((theme_id, variant), {}).items():
                palette[role] = value if value.startswith("#") else palette[value]
        themes.append(theme)

    bundled = {t["id"] for t in themes}
    if stray := {t for t, _ in OVERRIDES} - bundled:
        raise SystemExit(f"overrides for themes that are not bundled: {sorted(stray)}")
    if bundled != set(PICKER_ORDER):
        raise SystemExit(f"PICKER_ORDER lists {sorted(PICKER_ORDER)}, bundled are {sorted(bundled)}")

    out = [
        "//! The built-in theme palettes, generated by `tools/gen-themes.py`.",
        "//!",
        "//! The colours are T3 Code's theme library (MIT, Copyright (c) 2026 T3",
        "//! Tools Inc.), converted from OKLCH to sRGB hex and renamed for Hylki.",
        "//! `theme.rs` maps them onto libadwaita's colour roles. Edit the",
        "//! generator, not this file.",
        "",
        "use crate::theme::{Palette, Theme};",
        "",
    ]
    for theme in themes:
        const = theme["id"].replace("-", "_").upper()
        out.append(f'pub const {const}: Theme = Theme {{')
        out.append(f'    id: "{theme["id"]}",')
        out.append(f'    label: "{theme["label"]}",')
        for variant in ("light", "dark"):
            out.append(f"    {variant}: Palette {{")
            for role in ROLES:
                out.append(f'        {snake(role)}: "{theme[variant][role]}",')
            out.append("    },")
        out.append("};")
        out.append("")
    out.append("/// Every bundled theme, in the order the picker shows them.")
    out.append("pub const THEMES: &[Theme] = &[")
    for theme_id in PICKER_ORDER:
        out.append(f'    {theme_id.replace("-", "_").upper()},')
    out.append("];")
    out.append("")

    dest = Path(__file__).resolve().parent.parent / "src/theme_palettes.rs"
    dest.write_text("\n".join(out))
    print(f"wrote {dest} ({len(themes)} themes)")


if __name__ == "__main__":
    main()
