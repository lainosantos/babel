#!/usr/bin/env python3
"""Build Babel's static presentation and documentation for GitHub Pages."""
from __future__ import annotations

import argparse
import html
from html.parser import HTMLParser
from pathlib import Path
import re
import shutil
from urllib.parse import quote, unquote, urlsplit

import markdown
from markdown.extensions import Extension
from markdown.treeprocessors import Treeprocessor

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "https://github.com/lainosantos/babel"
GROUPS = [
    ("Getting started", [
        ("README.md", "overview", "Overview", "Install Babel, connect your devices, and start a session."),
        ("docs/configuration.md", "configuration", "Configuration", "Every setting, file destination, and session control."),
        ("docs/platforms.md", "platforms", "Platform setup", "Linux, macOS, and Windows audio configuration."),
        ("docs/autostart.md", "autostart", "Start at login", "Optional startup and tray controls for your system."),
        ("docs/localization.md", "localization", "Interface languages", "Language selection and adding a translation."),
    ]),
    ("Providers and voices", [
        ("docs/providers.md", "providers", "Gemini", "Live audio protocols, model capabilities, and limits."),
        ("docs/other-providers.md", "other-providers", "Other providers", "OpenAI, Deepgram, and external local services."),
        ("docs/local-inference.md", "local-inference", "Embedded local models", "Automatic setup, compact models, and offline use."),
        ("docs/voices.md", "voices", "Voices", "Native translation voices and legacy configuration migration."),
    ]),
    ("Capture and automation", [
        ("docs/transcription.md", "transcription", "Transcription", "Original-language text, timestamps, and speaker metadata."),
        ("docs/recording.md", "recording", "Recording", "Mixed originals, file formats, and recording limits."),
        ("docs/voice-commands.md", "voice-commands", "Voice commands", "Wake names, local recognition, Needle 3, and feedback."),
        ("docs/mcp.md", "mcp", "MCP integrations", "HTTP and stdio tools with supported authentication."),
    ]),
    ("Development", [
        ("docs/architecture.md", "architecture", "Architecture", "Routing isolation, memory safety, and audio processing."),
        ("docs/testing.md", "testing", "Testing", "Reproduce automated checks and hardware validation."),
        ("docs/ci-installers.md", "ci-installers", "CI and releases", "Checks, version tags, release assets, and signing."),
        ("docs/native-drivers.md", "native-drivers", "Native drivers", "Build and install Babel's virtual audio drivers."),
        ("CONTRIBUTING.md", "contributing", "Contributing", "English source, Conventional Commits, and contributions."),
        ("native/macos/README.md", "native-macos", "macOS driver source", "HAL driver development and installation."),
        ("native/windows/README.md", "native-windows", "Windows driver source", "Driver preparation, build, and installation."),
        ("packaging/linux/README.md", "packaging-linux", "Linux packaging", "DEB and RPM package builds."),
        ("packaging/macos/README.md", "packaging-macos", "macOS packaging", "Application bundle and package creation."),
        ("packaging/windows/README.md", "packaging-windows", "Windows packaging", "Installer creation and validation."),
        ("ui/README.md", "dashboard", "Dashboard development", "The local interface and its regression tests."),
        ("ui/DESIGN.md", "dashboard-design", "Dashboard design", "Interface design principles and structure."),
        ("assets/README.md", "assets", "Brand assets", "The Babel mark and tray icon assets."),
        ("assets/fonts/README.md", "fonts", "Fonts", "Bundled typography and licensing."),
    ]),
]
PAGES = [page for _, pages in GROUPS for page in pages]


def slugify(value: str, separator: str = "-") -> str:
    """Match ordinary GitHub heading fragments, including Unicode headings."""
    value = html.unescape(re.sub(r"<[^>]*>", "", value)).lower()
    return re.sub(r"\s", separator, re.sub(r"[^\w\- ]", "", value))


class LocalLinks(Treeprocessor):
    def __init__(self, md, source: Path, destinations: dict[Path, str]):
        super().__init__(md)
        self.source = source
        self.destinations = destinations

    def run(self, root):
        for node in root.iter():
            attribute = "href" if node.tag == "a" else "src" if node.tag == "img" else None
            if not attribute:
                continue
            value = node.get(attribute, "")
            parsed = urlsplit(value)
            if parsed.scheme or parsed.netloc or not parsed.path:
                continue
            target = (self.source.parent / unquote(parsed.path)).resolve()
            if target in self.destinations:
                node.set(attribute, self.destinations[target] + ("#" + parsed.fragment if parsed.fragment else ""))
            else:
                try:
                    relative = target.relative_to(ROOT).as_posix()
                except ValueError:
                    raise ValueError(f"Documentation link escapes the repository: {self.source}: {value}") from None
                node.set(attribute, f"{REPOSITORY}/blob/main/{quote(relative)}" + ("#" + parsed.fragment if parsed.fragment else ""))
        return root


class RepositoryLinks(Extension):
    def __init__(self, source: Path, destinations: dict[Path, str]):
        self.source, self.destinations = source, destinations
        super().__init__()

    def extendMarkdown(self, md):
        md.treeprocessors.register(LocalLinks(md, self.source, self.destinations), "repository_links", 5)


def navigation(selected: str = "") -> str:
    groups = []
    for label, pages in GROUPS:
        links = "".join(f'<a href="{slug}.html"' + (' aria-current="page"' if slug == selected else '') + f'>{html.escape(title)}</a>' for _, slug, title, _ in pages)
        groups.append(f"<h3>{label}</h3>{links}")
    return '<aside class="docs-sidebar"><details open><summary>Documentation</summary><nav aria-label="Documentation"><a href="index.html">All guides</a>' + "".join(groups) + '</nav></details></aside>'


def document(title: str, content: str, slug: str = "", source: str = "", toc: str = "") -> str:
    source_link = f'<a href="{REPOSITORY}/blob/main/{quote(source)}">View source</a>' if source else ''
    contents = f'<details class="doc-toc"><summary>On this page</summary>{toc}</details>' if toc else ''
    return f'''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{html.escape(title)} — Babel documentation</title><meta name="description" content="{html.escape(title)}: setup, capabilities, and practical guidance for Babel.">
<link rel="icon" href="../assets/babel.svg" type="image/svg+xml"><link rel="stylesheet" href="../assets/site.css"></head>
<body><a class="skip" href="#main">Skip to content</a><header class="site-header wrap"><a class="brand" href="../index.html"><img src="../assets/babel.svg" width="36" height="36" alt="">Babel</a><nav aria-label="Main navigation"><a href="../index.html#features">Features</a><a href="index.html">Documentation</a><a href="{REPOSITORY}">GitHub</a></nav></header>
<div class="docs-layout wrap">{navigation(slug)}<main id="main" class="doc-content"><div class="doc-meta"><a href="index.html">Documentation</a>{source_link}</div>{contents}{content}</main></div>
<footer class="site-footer wrap"><p>Babel · Audio across languages.</p><nav aria-label="Footer navigation"><a href="{REPOSITORY}/issues">Report an issue</a><a href="contributing.html">Contribute</a></nav></footer></body></html>'''


class LinkCollector(HTMLParser):
    def __init__(self):
        super().__init__()
        self.ids: set[str] = set()
        self.links: list[str] = []

    def handle_starttag(self, tag, attrs):
        values = dict(attrs)
        if values.get("id"):
            self.ids.add(values["id"])
        for key in ("href", "src"):
            if values.get(key):
                self.links.append(values[key])


def validate_links(output: Path) -> None:
    pages = {}
    for path in output.rglob("*.html"):
        collector = LinkCollector()
        collector.feed(path.read_text(encoding="utf-8"))
        pages[path.resolve()] = collector
    failures = []
    for source, page in pages.items():
        for link in page.links:
            url = urlsplit(link)
            if url.scheme or url.netloc:
                continue
            target = (source.parent / unquote(url.path)).resolve() if url.path else source
            if target.is_dir():
                target = target / "index.html"
            if not target.is_file():
                failures.append(f"{source.relative_to(output)}: missing {link}")
            elif url.fragment and target in pages and unquote(url.fragment) not in pages[target].ids:
                failures.append(f"{source.relative_to(output)}: missing anchor {link}")
    if failures:
        raise ValueError("Broken site links:\n" + "\n".join(failures))


def build(output: Path, check: bool = True) -> None:
    output = output.resolve()
    marker = output / ".babel-site-build"
    if output == ROOT or ROOT.is_relative_to(output):
        raise ValueError("The output must be a dedicated build directory, not the repository or its parent")
    if output.exists() and any(output.iterdir()) and not marker.is_file():
        raise ValueError("Refusing to replace a non-empty directory without the Babel site build marker")
    missing = [source for source, *_ in PAGES if not (ROOT / source).is_file()]
    if missing:
        raise ValueError("Documentation is missing: " + ", ".join(missing))
    if output.exists() and marker.is_file():
        shutil.rmtree(output)
    (output / "assets").mkdir(parents=True)
    (output / "docs").mkdir()
    marker.write_text("Generated by scripts/build_site.py\n", encoding="utf-8")
    (output / ".nojekyll").touch()
    shutil.copy2(ROOT / "site/index.html", output / "index.html")
    for name in ("site.css", "site.js"):
        shutil.copy2(ROOT / "site" / name, output / "assets" / name)
    shutil.copy2(ROOT / "assets/babel.svg", output / "assets/babel.svg")
    shutil.copy2(ROOT / "ui/fonts/manrope.ttf", output / "assets/manrope.ttf")
    shutil.copy2(ROOT / "ui/fonts/OFL.txt", output / "assets/OFL.txt")
    destinations = {(ROOT / source).resolve(): f"{slug}.html" for source, slug, *_ in PAGES}
    for source, slug, title, _ in PAGES:
        md = markdown.Markdown(extensions=["fenced_code", "tables", "sane_lists", "toc", RepositoryLinks(ROOT / source, destinations)], extension_configs={"toc": {"slugify": slugify, "permalink": "#", "toc_depth": "2-3"}})
        body = md.convert((ROOT / source).read_text(encoding="utf-8"))
        (output / "docs" / f"{slug}.html").write_text(document(title, body, slug, source, md.toc), encoding="utf-8")
    groups = []
    for label, pages in GROUPS:
        entries = "".join(f'<li><a href="{slug}.html">{title}</a><p>{description}</p></li>' for _, slug, title, description in pages)
        groups.append(f'<section class="guide-group" aria-labelledby="{slugify(label)}"><h2 id="{slugify(label)}">{label}</h2><ul class="guide-list">{entries}</ul></section>')
    index = '<div class="doc-index"><h1>Babel documentation</h1><p>Connect your audio, choose your providers, and make Babel work for your conversations.</p>' + "".join(groups) + '</div>'
    (output / "docs/index.html").write_text(document("Documentation", index), encoding="utf-8")
    if check:
        validate_links(output)
    print(f"Built {len(PAGES) + 2} pages in {output}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=ROOT / "_site")
    parser.add_argument("--no-check", action="store_true", help="Skip link validation while drafting documentation")
    args = parser.parse_args()
    build(args.output, check=not args.no_check)
