"""Validate and copy an inference payload without executing native code."""
from __future__ import annotations
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import stat
import struct


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def safe_relative(value):
    if not isinstance(value, str) or not value or "\\" in value or ":" in value:
        raise ValueError("Invalid runtime relative path")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in ("", ".", "..") for part in value.split("/")):
        raise ValueError("Invalid runtime relative path")
    return Path(*path.parts)


def validate(root, system, arch):
    manifest_path = root / "manifest.json"
    if root.is_symlink() or manifest_path.is_symlink() or not manifest_path.is_file():
        raise ValueError(f"Missing native inference payload: {root}")
    if manifest_path.stat().st_size > 4 * 1024 * 1024:
        raise ValueError("Runtime manifest exceeds size limit")
    manifest = json.loads(manifest_path.read_text())
    if (manifest.get("version"), manifest.get("platform"), manifest.get("arch")) != (1, system, arch):
        raise ValueError("Native inference payload has the wrong format or architecture")
    expected = set()
    for entry in manifest["files"]:
        relative = safe_relative(entry["path"])
        if relative in expected or relative == Path("manifest.json"):
            raise ValueError("Duplicate runtime file")
        expected.add(relative)
        file = root / relative
        if file.is_symlink() or not file.is_file() or file.stat().st_size != entry["size"] or sha256(file) != entry["sha256"]:
            raise ValueError(f"Runtime file missing or checksum mismatch: {relative}")
    actual = set()
    for file in root.rglob("*"):
        if file.is_symlink():
            raise ValueError("Runtime payload contains a symlink")
        if file.is_file() and file != manifest_path:
            actual.add(file.relative_to(root))
    if actual != expected:
        raise ValueError("Runtime payload contains unlisted files")
    if set(manifest["services"]) != {"whisper", "llama", "piper"}:
        raise ValueError("Runtime payload is missing a native provider")
    for service in manifest["services"].values():
        path = safe_relative(service["executable"])
        if path not in expected:
            raise ValueError("Runtime executable is not covered by the manifest")
        if system != "windows" and not (root / path).stat().st_mode & stat.S_IXUSR:
            raise ValueError("Runtime executable lacks execute permission")
    data = safe_relative(manifest["services"]["piper"]["data"])
    if not (root / data / "phondata").is_file():
        raise ValueError("Piper phoneme data is missing")
    # Inspect every native file, including backend DLLs loaded at runtime.
    for relative in expected:
        file = root / relative
        with file.open("rb") as stream:
            header = stream.read(64)
            if header[:4] == b"\x7fELF":
                if system != "linux" or len(header) < 20 or struct.unpack_from("<H", header, 18)[0] != {"x86_64": 62, "aarch64": 183}[arch]:
                    raise ValueError("ELF inference dependency has the wrong architecture")
            elif header[:2] == b"MZ":
                if system != "windows" or len(header) < 64:
                    raise ValueError("Unexpected PE inference dependency")
                stream.seek(struct.unpack_from("<I", header, 60)[0])
                pe = stream.read(6)
                if pe[:4] != b"PE\0\0" or struct.unpack_from("<H", pe, 4)[0] != {"x86_64": 0x8664, "aarch64": 0xAA64}[arch]:
                    raise ValueError("PE inference dependency has the wrong architecture")
            elif header[:4] == b"\xcf\xfa\xed\xfe":
                if system != "macos" or struct.unpack_from("<I", header, 4)[0] != {"x86_64": 0x1000007, "aarch64": 0x100000c}[arch]:
                    raise ValueError("Mach-O inference dependency has the wrong architecture")
    return manifest


def stage(runtime_dir, destination, system, arches):
    for arch in arches:
        folder = f"{system}-{arch}"
        source = runtime_dir / folder
        validate(source, system, arch)
        shutil.copytree(source, destination / folder)
        validate(destination / folder, system, arch)


def rehash(root):
    """Re-signing Mach-O changes bytes; update before signing the outer app."""
    path = root / "manifest.json"
    manifest = json.loads(path.read_text())
    for entry in manifest["files"]:
        file = root / safe_relative(entry["path"])
        entry.update(size=file.stat().st_size, sha256=sha256(file))
    path.write_text(json.dumps(manifest, indent=2) + "\n")
