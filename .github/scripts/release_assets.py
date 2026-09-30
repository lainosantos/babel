#!/usr/bin/env python3
"""Validate a release tag and collect every installer/runtime artifact safely."""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import tomllib


# SemVer 2.0 permits arbitrary build identifiers but no zero-prefixed numbers in
# the version or numeric prerelease identifiers.
NUMBER = r"(?:0|[1-9][0-9]*)"
PRE_ID = r"(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
TAG = re.compile(rf"v(?P<version>{NUMBER}\.{NUMBER}\.{NUMBER}(?:-(?P<prerelease>{PRE_ID}(?:\.{PRE_ID})*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?)\Z")
EXPECTED = {
    "babel-local-runtime-linux-x86_64",
    "babel-local-runtime-macos-aarch64",
    "babel-local-runtime-macos-x86_64",
    "babel-local-runtime-windows-x86_64",
    "babel-local-runtime-windows-aarch64",
    "babel-installers-linux-amd64",
    "babel-installers-macos-universal-development",
    "babel-installers-windows-x64-development",
    "babel-installers-windows-ARM64-development",
}
RESERVED = {"SHA256SUMS.txt", "release-manifest.json"}


def required_formats(artifact: str) -> tuple[str, ...]:
    if artifact.startswith("babel-local-runtime-"):
        return (".tar.gz",)
    if artifact == "babel-installers-linux-amd64":
        return (".deb", ".rpm", ".tar.gz")
    if artifact.startswith("babel-installers-macos-"):
        return (".pkg",)
    if artifact.startswith("babel-installers-windows-"):
        return (".exe", ".zip")
    raise ValueError(f"Unsupported installer artifact: {artifact}")


def validate_tag(tag: str, cargo: Path) -> tuple[str, bool]:
    match = TAG.fullmatch(tag)
    if match is None:
        raise ValueError("Release tag must be v followed by a complete SemVer version")
    version = match["version"]
    if match["prerelease"] is not None or "+" in version:
        raise ValueError("Native installers currently require a stable vX.Y.Z tag; prerelease and build metadata are unsupported")
    if any(int(component) > 65535 for component in version.split(".")):
        raise ValueError("Native Windows installer version components must be <= 65535")
    declared = tomllib.loads(cargo.read_text(encoding="utf-8"))["package"]["version"]
    if declared != version:
        raise ValueError(f"Tag version {version} does not match Cargo.toml version {declared}")
    return version, False


def collect(source: Path, output: Path, tag: str) -> int:
    directories = {entry.name: entry for entry in source.iterdir() if entry.is_dir()}
    missing = EXPECTED.difference(directories)
    if missing:
        raise ValueError("Missing release artifacts: " + ", ".join(sorted(missing)))
    files = []
    for artifact, directory in sorted(directories.items()):
        if directory.is_symlink():
            raise ValueError("Release artifact directories cannot be symlinks")
        if not artifact.startswith(("babel-installers-", "babel-local-runtime-")):
            raise ValueError(f"Unexpected release artifact: {artifact}")
        owned = []
        for path in sorted(directory.rglob("*")):
            if path.is_symlink():
                raise ValueError(f"Release artifacts cannot contain symlinks: {artifact}")
            if path.is_file():
                if path.stat().st_size == 0:
                    raise ValueError(f"Empty release file: {artifact}/{path.name}")
                owned.append((artifact, path, path.relative_to(directory)))
        if not owned:
            raise ValueError(f"Empty release artifact: {artifact}")
        for suffix in required_formats(artifact):
            if not any(path.name.endswith(suffix) for _, path, _ in owned):
                raise ValueError(f"Missing {suffix} package in release artifact: {artifact}")
        files.extend(owned)
    if output.exists() and any(output.iterdir()):
        raise ValueError("Release asset output must be empty")
    output.mkdir(parents=True, exist_ok=True)
    counts = Counter(path.name for _, path, _ in files)
    used = set(RESERVED)
    manifest = {"tag": tag, "files": []}
    for artifact, path, relative in files:
        name = path.name
        if counts[name] > 1 or name in RESERVED:
            name = artifact + "--" + "--".join(relative.parts)
        if name in used or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+~-]*", name):
            raise ValueError(f"Ambiguous or unsupported release filename: {name}")
        used.add(name)
        destination = output / name
        shutil.copyfile(path, destination)
        with destination.open("rb") as stream:
            checksum = hashlib.file_digest(stream, "sha256").hexdigest()
        manifest["files"].append({"asset": name, "artifact": artifact, "path": relative.as_posix(), "sha256": checksum, "bytes": destination.stat().st_size})
    metadata = output / "release-manifest.json"
    metadata.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    checksums = []
    for path in sorted(output.iterdir()):
        with path.open("rb") as stream:
            checksums.append(f"{hashlib.file_digest(stream, 'sha256').hexdigest()}  {path.name}\n")
    (output / "SHA256SUMS.txt").write_text("".join(checksums), encoding="utf-8")
    return len(manifest["files"])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--cargo", type=Path, default=Path("Cargo.toml"))
    parser.add_argument("--source", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        version, prerelease = validate_tag(args.tag, args.cargo)
        if args.source or args.output:
            if not (args.source and args.output):
                raise ValueError("--source and --output are required together")
            print(f"Collected {collect(args.source, args.output, args.tag)} installer/runtime files.")
        if destination := os.environ.get("GITHUB_OUTPUT"):
            with open(destination, "a", encoding="utf-8") as stream:
                stream.write(f"version={version}\nprerelease={str(prerelease).lower()}\n")
        print(f"Validated release tag {args.tag}.")
    except (ValueError, KeyError, OSError) as error:
        print(f"Release validation failed: {error}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
