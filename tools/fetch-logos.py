#!/usr/bin/env python3
"""Fetch the bundled sender logos (data/logos/) from their upstream sets.

Reads data/logos/domains.txt, resolves each domain against gilbarbara/logos
(full-colour SVGs) and Simple Icons (monochrome SVGs with a brand colour),
downloads the chosen file into data/logos/<source>/, and writes
data/logos/logos.toml (what src/logo.rs embeds and consults) plus
resources/logos.gresource.xml (what build.rs compiles in).

A colour icon from gilbarbara wins when it is roughly square (an "-icon"
file, or a mark whose viewBox is no wider than 1.6:1); Simple Icons' glyph
comes next, drawn white on the brand colour; a gilbarbara wordmark is the
last resort, shown on a white tile. Both upstream sets are pinned to the
commits below; bump them and re-run to refresh.

    tools/fetch-logos.py            # needs network; commits the results
"""
import json, os, re, sys, unicodedata, urllib.request, xml.etree.ElementTree as ET

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GILBARBARA = "gilbarbara/logos"
SIMPLE = "simple-icons/simple-icons"
GB_REF = os.environ.get("GB_REF", "main")
SI_REF = os.environ.get("SI_REF", "develop")

def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": "hylki-fetch-logos"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read()

def resolve_ref(repo, ref):
    d = json.loads(fetch(f"https://api.github.com/repos/{repo}/commits/{ref}"))
    return d["sha"]

def si_slug(title):
    t = title.lower()
    for a, b in [("+", "plus"), (".", "dot"), ("&", "and"), ("đ", "d"), ("ħ", "h"), ("ı", "i"),
                 ("ĸ", "k"), ("ŀ", "l"), ("ł", "l"), ("ß", "ss"), ("ŧ", "t")]:
        t = t.replace(a, b)
    t = unicodedata.normalize("NFD", t)
    return re.sub(r"[^a-z0-9]", "", t)

def svg_aspect(data):
    try:
        root = ET.fromstring(data)
    except ET.ParseError:
        return None
    vb = root.get("viewBox")
    if vb:
        parts = re.split(r"[\s,]+", vb.strip())
        if len(parts) == 4:
            try:
                w, h = float(parts[2]), float(parts[3])
                if w > 0 and h > 0:
                    return w / h
            except ValueError:
                pass
    w, h = root.get("width"), root.get("height")
    try:
        w, h = float(re.sub(r"[a-z%]+$", "", w or "")), float(re.sub(r"[a-z%]+$", "", h or ""))
        return w / h if w > 0 and h > 0 else None
    except ValueError:
        return None

def main():
    gb_sha = resolve_ref(GILBARBARA, GB_REF)
    si_sha = resolve_ref(SIMPLE, SI_REF)
    print(f"gilbarbara/logos @ {gb_sha[:10]}, simple-icons @ {si_sha[:10]}")
    gb = json.loads(fetch(f"https://raw.githubusercontent.com/{GILBARBARA}/{gb_sha}/logos.json"))
    si = json.loads(fetch(f"https://raw.githubusercontent.com/{SIMPLE}/{si_sha}/data/simple-icons.json"))
    si = si.get("icons", si) if isinstance(si, dict) else si
    si_by_slug = {}
    for icon in si:
        si_by_slug.setdefault(si_slug(icon["title"]), icon)
        for aka in icon.get("aliases", {}).get("aka", []):
            si_by_slug.setdefault(si_slug(aka), icon)
    gb_by_host = {}
    for entry in gb:
        host = re.sub(r"^https?://(www\.)?", "", entry.get("url", "")).split("/")[0].lower()
        if host:
            gb_by_host.setdefault(host, entry)

    entries = []
    for raw in open(os.path.join(ROOT, "data/logos/domains.txt"), encoding="utf-8"):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        parts = line.split()
        domain, pick = parts[0].lower(), (parts[1] if len(parts) > 1 else None)
        chosen = None
        if pick and pick.startswith("brand:"):
            # One of the app's own service marks (data/brands/, a PNG the
            # binary already embeds): nothing to fetch.
            chosen = ("brand", pick.split(":", 1)[1], None)
        elif pick and pick.startswith("gilbarbara:"):
            chosen = ("gilbarbara", pick.split(":", 1)[1], None)
        elif pick and pick.startswith("simple:"):
            slug = pick.split(":", 1)[1]
            icon = si_by_slug.get(slug)
            if icon is None:
                print(f"  !! {domain}: no Simple Icon '{slug}'", file=sys.stderr)
                continue
            chosen = ("simple", f"{slug}.svg", icon["hex"])
        else:
            label = re.sub(r"[^a-z0-9]", "", domain.split(".")[0])
            gbe = gb_by_host.get(domain) or gb_by_host.get("www." + domain)
            gb_icon = None
            if gbe:
                icons = [f for f in gbe["files"] if f.endswith("-icon.svg")]
                gb_icon = icons[0] if icons else gbe["files"][0]
            sie = si_by_slug.get(label)
            if gbe and gb_icon.endswith("-icon.svg"):
                chosen = ("gilbarbara", gb_icon, None)
            elif sie:
                chosen = ("simple", f"{si_slug(sie['title'])}.svg", sie["hex"])
            elif gbe:
                chosen = ("gilbarbara", gb_icon, None)
        if chosen is None:
            print(f"  -- {domain}: nothing in either set", file=sys.stderr)
            continue
        source, file, hex_ = chosen
        if source == "brand":
            if not os.path.exists(os.path.join(ROOT, "data/brands", file + ".png")):
                print(f"  !! {domain}: no mark data/brands/{file}.png", file=sys.stderr)
                continue
            entries.append({"domain": domain, "source": source, "file": file, "hex": None, "aspect": 1.0})
            print(f"  {domain:28} brand:{file}")
            continue
        dest_dir = os.path.join(ROOT, "data/logos", source)
        os.makedirs(dest_dir, exist_ok=True)
        dest = os.path.join(dest_dir, file)
        if not os.path.exists(dest):
            url = (f"https://raw.githubusercontent.com/{GILBARBARA}/{gb_sha}/logos/{file}" if source == "gilbarbara"
                   else f"https://raw.githubusercontent.com/{SIMPLE}/{si_sha}/icons/{file}")
            try:
                data = fetch(url)
            except Exception as e:
                print(f"  !! {domain}: {url}: {e}", file=sys.stderr)
                continue
            open(dest, "wb").write(data)
        data = open(dest, "rb").read()
        aspect = svg_aspect(data)
        if source == "gilbarbara" and not file.endswith("-icon.svg") and aspect and aspect > 1.6:
            # A wordmark: only when Simple Icons has nothing (checked above),
            # and shown on a white tile.
            pass
        entries.append({"domain": domain, "source": source, "file": file, "hex": hex_, "aspect": aspect})
        print(f"  {domain:28} {source}:{file}" + (f" #{hex_}" if hex_ else "") + (f" ({aspect:.2f})" if aspect else ""))

    # The map the app embeds.
    lines = [f"# Generated by tools/fetch-logos.py — do not edit by hand.",
             f"# gilbarbara/logos {gb_sha}", f"# simple-icons {si_sha}", ""]
    for e in sorted(entries, key=lambda e: e["domain"]):
        lines.append("[[logo]]")
        lines.append(f'domain = "{e["domain"]}"')
        lines.append(f'source = "{e["source"]}"')
        lines.append(f'file = "{e["file"]}"')
        if e["hex"]:
            lines.append(f'color = "#{e["hex"]}"')
        lines.append("")
    open(os.path.join(ROOT, "data/logos/logos.toml"), "w", encoding="utf-8").write("\n".join(lines))

    # The gresource listing, compressed: an SVG shrinks to a fraction.
    files = sorted({(e["source"], e["file"]) for e in entries if e["source"] != "brand"})
    xml = ['<?xml version="1.0" encoding="UTF-8"?>', "<gresources>",
           '  <gresource prefix="/co/hyprlab/Hylki/logos">']
    for source, file in files:
        xml.append(f'    <file compressed="true" alias="{source}/{file}">../data/logos/{source}/{file}</file>')
    xml += ["  </gresource>", "</gresources>", ""]
    open(os.path.join(ROOT, "resources/logos.gresource.xml"), "w", encoding="utf-8").write("\n".join(xml))
    # Drop files no entry uses any more.
    for source in ("gilbarbara", "simple"):
        d = os.path.join(ROOT, "data/logos", source)
        if os.path.isdir(d):
            keep = {f for s, f in files if s == source}
            for f in os.listdir(d):
                if f not in keep:
                    os.remove(os.path.join(d, f))
    total = sum(os.path.getsize(os.path.join(ROOT, "data/logos", s, f)) for s, f in files)
    print(f"==> {len(entries)} domains, {len(files)} files, {total/1024:.0f} KB of SVG")

if __name__ == "__main__":
    main()
