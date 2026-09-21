#!/usr/bin/env python3
"""Check the repository's documentation still holds its shape (#230).

The README grew to 667 lines one reasonable paragraph at a time, so these are
the rules that keep it, and docs/, from drifting back:

  * every relative link and #anchor in a Markdown file resolves
  * README.md stays under its line budget: detail belongs in docs/
  * every docs/*.md is listed in docs/README.md and linked from somewhere
  * docs/ holds documentation, not artwork (that is data/repo/)
  * the top level holds only the files GitHub looks for there
  * nothing but the changelog still calls the app Vireo or Veem
  * the documentation is free of em dashes (#230)
  * data/CONTRIBUTORS and data/TRANSLATORS parse the way About reads them

Run it after touching any .md, and before a release. It prints what is wrong
and exits non-zero, or says everything is in order.
"""
import pathlib, re, sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

# The README is the front door, not the manual. Raising this is a decision,
# not a formality: what pushed it over probably belongs in docs/.
README_MAX_LINES = 180

# The Markdown allowed at the top level: the front page, the history GitHub
# and the About page both read from here, and CLAUDE.md, which is read by
# whoever works on the repository rather than by anyone reading it. Everything
# else is documentation and lives in docs/ (SECURITY.md included: GitHub finds
# a security policy in docs/ as readily as at the top).
TOP_LEVEL_MD = {
    "README.md",
    "CHANGELOG.md",
    "CLAUDE.md",
}

# The files whose Vireo/Veem mentions are history rather than staleness.
OLD_NAME_OK = {"CHANGELOG.md", "RELEASE_NOTES.md"}

# An em dash is a colon, a comma or a full stop that has not decided which it
# is, and the documentation reads better without one (#230). The changelog and
# the release notes are a record of what was published and keep theirs;
# CLAUDE.md is a working agreement rather than documentation.
EM_DASH_OK = {"CHANGELOG.md", "RELEASE_NOTES.md", "CLAUDE.md"}

problems: list[str] = []


def problem(where: str, what: str) -> None:
    problems.append(f"{where}: {what}")


def markdown_files() -> list[pathlib.Path]:
    return sorted(ROOT.glob("*.md")) + sorted(ROOT.glob("docs/*.md")) + [
        ROOT / "po/README.md",
        ROOT / "data/brands/README.md",
        ROOT / "data/logos/README.md",
    ]


def anchors(path: pathlib.Path) -> set[str]:
    """The #fragments GitHub makes from a file's headings."""
    out = set()
    for line in path.read_text().splitlines():
        m = re.match(r"#+\s+(.*)", line)
        if not m:
            continue
        slug = m.group(1).lower().replace("&", "")
        slug = re.sub(r"[^\w\s-]", "", slug).strip().replace(" ", "-")
        out.add(slug)
    return out


def prose(text: str) -> str:
    """The text with its code out: a fenced block or an inline span is an
    example, and the changelog is full of paths and tags that are neither
    links nor meant to resolve."""
    text = re.sub(r"^```.*?^```", "", text, flags=re.S | re.M)
    return re.sub(r"`[^`]*`", "", text)


def links(text: str):
    """Every local target a page points at: Markdown links, and the HTML
    <img src>/<source srcset> the README's banner is built from."""
    text = prose(text)
    for m in re.finditer(r"\]\(([^)\s]+)\)", text):
        yield m.group(1)
    for m in re.finditer(r'(?:src|srcset)="([^"]+)"', text):
        yield m.group(1)


def check_links() -> None:
    for f in markdown_files():
        if not f.exists():
            problem(str(f.relative_to(ROOT)), "listed in this script but missing")
            continue
        for link in links(f.read_text()):
            if link.startswith(("http://", "https://", "mailto:", "#!")):
                continue
            path, _, frag = link.partition("#")
            target = (f.parent / path).resolve() if path else f.resolve()
            rel = f.relative_to(ROOT)
            if not target.exists():
                problem(str(rel), f"broken link: {link}")
            elif frag and target.suffix == ".md" and frag not in anchors(target):
                problem(str(rel), f"broken anchor: {link}")


def check_readme_length() -> None:
    lines = len((ROOT / "README.md").read_text().splitlines())
    if lines > README_MAX_LINES:
        problem(
            "README.md",
            f"{lines} lines, over the {README_MAX_LINES}-line budget — "
            "the detail probably belongs in docs/",
        )


def check_docs_index() -> None:
    index = ROOT / "docs/README.md"
    listed = set(re.findall(r"\]\((\w[\w-]*\.md)\)", index.read_text()))
    body = "\n".join(
        f.read_text() for f in markdown_files() if f.exists() and f != index
    )
    for doc in sorted(ROOT.glob("docs/*.md")):
        if doc == index:
            continue
        if doc.name not in listed:
            problem("docs/README.md", f"{doc.name} is not in the index")
        if doc.name not in body:
            problem(f"docs/{doc.name}", "nothing links to it")


def check_docs_holds_only_docs() -> None:
    for f in sorted((ROOT / "docs").rglob("*")):
        if f.is_dir() or f.suffix == ".md":
            continue
        problem(
            f"docs/{f.relative_to(ROOT / 'docs')}",
            "not documentation — artwork goes in data/repo/",
        )
    for f in sorted(ROOT.glob("*.md")):
        if f.name not in TOP_LEVEL_MD:
            problem(f.name, "top-level Markdown belongs in docs/")


def check_old_names() -> None:
    for f in markdown_files():
        if not f.exists() or f.name in OLD_NAME_OK:
            continue
        for n, line in enumerate(f.read_text().splitlines(), 1):
            # The README's name-change note is deliberate, and quoted.
            if f.name == "README.md" and line.startswith(">"):
                continue
            if re.search(r"\b(Vireo|Veem)\b", line):
                problem(f"{f.relative_to(ROOT)}:{n}", "still says Vireo/Veem")


def check_em_dashes() -> None:
    for f in markdown_files():
        if not f.exists() or f.name in EM_DASH_OK:
            continue
        for n, line in enumerate(f.read_text().splitlines(), 1):
            if "\u2014" in line:
                problem(
                    f"{f.relative_to(ROOT)}:{n}",
                    "em dash: a colon, a comma or a full stop replaces it",
                )


def check_credits_files() -> None:
    for name, note in (("CONTRIBUTORS", False), ("TRANSLATORS", True)):
        path = ROOT / "data" / name
        rows = 0
        for n, line in enumerate(path.read_text().splitlines(), 1):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            rows += 1
            m = re.fullmatch(r"([^<>]+?)\s*<(\S+)>\s*(?:-\s*(.+))?", line)
            if not m:
                problem(f"data/{name}:{n}", f"unreadable line: {line}")
            elif note and not m.group(3):
                problem(f"data/{name}:{n}", "a translator needs their languages")
            elif not note and m.group(3):
                problem(f"data/{name}:{n}", "what somebody did goes in docs/CREDITS.md")
        if rows == 0:
            problem(f"data/{name}", "empty")


for check in (
    check_links,
    check_readme_length,
    check_docs_index,
    check_docs_holds_only_docs,
    check_old_names,
    check_em_dashes,
    check_credits_files,
):
    check()

if problems:
    print("\n".join(problems))
    print(f"\n{len(problems)} problem(s). See tools/check-docs.py for the rules.")
    sys.exit(1)
print("docs in order")
