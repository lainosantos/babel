#!/usr/bin/env python3
"""Build and inspect Linux amd64 packages without installing or running Babel."""
from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib

HERE = Path(__file__).resolve().parent
PROJECT = HERE.parent.parent
sys.path.insert(0, str(PROJECT / "scripts"))
import local_runtime_packaging as runtime_package

PACKAGE = "babel-audio"
BINARIES = ("babel", "babel-tray")
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9][A-Za-z0-9.-]*)?(?:\+[A-Za-z0-9][A-Za-z0-9.-]*)?")
LIBC_SONAMES = {
    "libc.so.6", "libm.so.6", "libpthread.so.0", "libdl.so.2",
    "librt.so.1", "libresolv.so.2", "ld-linux-x86-64.so.2",
}


def run(*args: str | Path, **kwargs) -> subprocess.CompletedProcess:
    environment = kwargs.pop("env", {})
    return subprocess.run(
        [str(arg) for arg in args], check=True, capture_output=True,
        text=True, env={**os.environ, "LC_ALL": "C", **environment}, **kwargs,
    )


def digest(path: Path, algorithm: str = "sha256") -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, algorithm).hexdigest()


def package_version(override: str | None) -> str:
    version = override or tomllib.loads((PROJECT / "Cargo.toml").read_text())["package"]["version"]
    if not VERSION.fullmatch(version):
        raise ValueError("Version must be a plain semantic version, without paths or whitespace")
    return version


def deb_version(version: str) -> str:
    # Debian sorts '~rc' before the final release; '-' would denote a revision.
    return version.replace("-", "~", 1)


def rpm_version(version: str) -> str:
    # RPM 4.17+ understands '~' prereleases; '-' separates release in NEVRA.
    return version.replace("-", "~")


def elf_runtime(path: Path, bundled: set[str] | None = None) -> tuple[set[str], set[tuple[int, ...]]]:
    with path.open("rb") as stream:
        header = stream.read(64)
    if len(header) != 64 or header[:6] != b"\x7fELF\x02\x01" or struct.unpack_from("<H", header, 18)[0] != 62:
        raise ValueError(f"{path}: expected an ELF64 little-endian amd64 binary")
    if struct.unpack_from("<H", header, 16)[0] not in (2, 3):
        raise ValueError(f"{path}: not an ELF executable or PIE")
    needed = set(re.findall(r"Shared library: \[([^]]+)\]", run("readelf", "--wide", "--dynamic", path).stdout))
    unknown = needed - LIBC_SONAMES - {"libgcc_s.so.1", "libstdc++.so.6"} - (bundled or set())
    if unknown:
        raise ValueError(f"{path}: runtime libraries need explicit package mappings: {sorted(unknown)}")
    versions = {
        tuple(int(part) for part in value.split("."))
        for value in re.findall(r"\bGLIBC_([0-9]+(?:\.[0-9]+)+)\b", run("readelf", "--wide", "--version-info", path).stdout)
    }
    if "libc.so.6" not in needed or not versions:
        raise ValueError(f"{path}: this package recipe requires a dynamically linked glibc executable")
    return needed, versions


def copy_file(source: Path, destination: Path, mode: int = 0o644) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    destination.chmod(mode)


def files(root: Path) -> list[Path]:
    return sorted(path for path in root.rglob("*") if path.is_file())


def stage_payload(bin_dir: Path, payload: Path, version: str, runtime_dir: Path | None = None) -> dict:
    needed: set[str] = set()
    versions: set[tuple[int, ...]] = set()
    for name in BINARIES:
        source = bin_dir / name
        copied = payload / "bin" / name
        copy_file(source, copied, 0o755)
        libraries, glibc = elf_runtime(copied)
        needed.update(libraries)
        versions.update(glibc)
    if runtime_dir is not None:
        runtime_root = payload / "share/babel/local-runtime"
        runtime_package.stage(runtime_dir, runtime_root, "linux", ["x86_64"])
        runtime_files = list((runtime_root / "linux-x86_64").rglob("*"))
        bundled = {p.name for p in runtime_files if p.is_file() and ".so" in p.name}
        for path in runtime_files:
            if not path.is_file():
                continue
            with path.open("rb") as stream:
                is_elf = stream.read(4) == b"\x7fELF"
            if is_elf:
                libraries, glibc = elf_runtime(path, bundled)
                needed.update(libraries)
                versions.update(glibc)
    minimum = ".".join(map(str, max(versions)))
    dependencies = [f"libc6 (>= {minimum})"]
    if "libgcc_s.so.1" in needed:
        dependencies.append("libgcc-s1")
    if "libstdc++.so.6" in needed:
        dependencies.append("libstdc++6 (>= 11)")
    dependencies.extend(["pulseaudio-utils", "xdg-utils", "dbus-user-session | dbus-x11"])
    copy_file(HERE / "babel-launch", payload / "bin/babel-launch", 0o755)
    copy_file(HERE / "org.babel.audio.desktop", payload / "share/applications/org.babel.audio.desktop")
    copy_file(HERE / "org.babel.audio.svg", payload / "share/icons/hicolor/scalable/apps/org.babel.audio.svg")
    copy_file(HERE / "org.babel.audio.service", payload / "lib/systemd/user/org.babel.audio.service")
    rpm_requires = [f"glibc >= {minimum}", "/bin/sh", "/usr/bin/pactl", "/usr/bin/parec", "/usr/bin/pacat", "/usr/bin/xdg-open", "dbus"]
    if "libgcc_s.so.1" in needed:
        rpm_requires.append("libgcc_s.so.1()(64bit)")
    if "libstdc++.so.6" in needed:
        rpm_requires.append("libstdc++.so.6()(64bit)")
    docs = payload / "share/doc" / PACKAGE
    for source, destination in [
        (HERE / "LICENSE", "copyright"), (HERE / "README.md", "INSTALL.md"),
        (PROJECT / "README.md", "README.md"), (PROJECT / "Cargo.lock", "Cargo.lock"),
        (PROJECT / "examples/babel.example.toml", "babel.example.toml"),
    ]:
        copy_file(source, docs / destination)
    for source in sorted((PROJECT / "docs").glob("*.md")):
        copy_file(source, docs / "docs" / source.name)
    copy_file(PROJECT / "ui/fonts/OFL.txt", docs / "licenses/Manrope-OFL.txt")
    # Optional inference is configured independently. Ship its reviewed helper,
    # not a Python environment, model download, or a service-starting hook.
    copy_file(PROJECT / "scripts/needle_bridge.py", payload / "share/babel/scripts/needle_bridge.py")
    manifest = {
        "package": PACKAGE, "version": version, "architecture": "amd64",
        "license": "MIT AND GPL-3.0-or-later" if runtime_dir else "MIT", "glibc_minimum": minimum,
        "depends": dependencies, "needed_libraries": sorted(needed),
        "rpm_requires": rpm_requires,
        "files": {
            path.relative_to(payload).as_posix(): {
                "sha256": digest(path), "size": path.stat().st_size,
                "mode": f"{path.stat().st_mode & 0o7777:04o}",
            }
            for path in files(payload)
        },
    }
    (docs / "package-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    (docs / "package-manifest.json").chmod(0o644)
    for directory in payload.rglob("*"):
        if directory.is_dir():
            directory.chmod(0o755)
    return manifest


def expected_files(payload: Path, prefix: str) -> dict[str, tuple[int, str]]:
    return {
        f"{prefix}/{path.relative_to(payload).as_posix()}": (path.stat().st_mode & 0o7777, digest(path))
        for path in files(payload)
    }


def validate_archive(archive: tarfile.TarFile, expected: dict[str, tuple[int, str]]) -> None:
    seen: set[str] = set()
    for member in archive:
        path = PurePosixPath(member.name)
        if path.is_absolute() or ".." in path.parts or member.issym() or member.islnk():
            raise ValueError(f"Unexpected archive path or link: {member.name}")
        if member.isdir():
            if member.uid != 0 or member.gid != 0 or member.mode != 0o755:
                raise ValueError(f"Unexpected directory ownership/permissions: {member.name}")
            continue
        name = path.as_posix()
        if not member.isfile() or name not in expected or name in seen:
            raise ValueError(f"Unexpected or duplicate archive member: {member.name}")
        mode, sha256 = expected[name]
        if member.uid != 0 or member.gid != 0 or member.mode != mode:
            raise ValueError(f"Unexpected ownership/permissions: {name}")
        stream = archive.extractfile(member)
        if stream is None or hashlib.file_digest(stream, "sha256").hexdigest() != sha256:
            raise ValueError(f"Archive content differs from the payload: {name}")
        seen.add(name)
    if seen != expected.keys():
        raise ValueError(f"Archive files missing: {sorted(expected.keys() - seen)}")


def write_tar(payload: Path, destination: Path, prefix: str, epoch: int) -> None:
    with destination.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch) as compressed:
        with tarfile.open(fileobj=compressed, mode="w|") as archive:
            for path in [payload, *sorted(payload.rglob("*"))]:
                suffix = path.relative_to(payload).as_posix()
                name = prefix if suffix == "." else f"{prefix}/{suffix}"
                info = archive.gettarinfo(str(path), name)
                info.uid = info.gid = 0
                info.uname = info.gname = "root"
                info.mtime = epoch
                info.mode = 0o755 if path.is_dir() else path.stat().st_mode & 0o7777
                if path.is_file():
                    with path.open("rb") as stream:
                        archive.addfile(info, stream)
                else:
                    archive.addfile(info)
    with tarfile.open(destination, "r:gz") as archive:
        validate_archive(archive, expected_files(payload, prefix))


def write_deb(payload: Path, destination: Path, manifest: dict, work: Path, epoch: int) -> None:
    root = work / "deb"
    shutil.copytree(payload, root / "usr")
    control = root / "DEBIAN"
    control.mkdir(mode=0o755)
    size = (sum(path.stat().st_size for path in files(payload)) + 1023) // 1024
    (control / "control").write_text(
        f"Package: {PACKAGE}\nVersion: {deb_version(manifest['version'])}\n"
        "Section: sound\nPriority: optional\nArchitecture: amd64\n"
        "Maintainer: Babel contributors <maintainers@babel.invalid>\n"
        f"Installed-Size: {size}\nDepends: {', '.join(manifest['depends'])}\n"
        "Suggests: pipewire-pulse | pulseaudio\n"
        "Description: Virtual audio routing and bidirectional speech translation\n"
        " Babel provides a per-user tray and local settings dashboard.\n"
        " Requires an existing PulseAudio or PipeWire-pulse user session.\n"
        " Installation does not start Babel, enable autostart or create devices.\n"
    )
    (control / "md5sums").write_text("".join(
        f"{digest(path, 'md5')}  usr/{path.relative_to(payload).as_posix()}\n"
        for path in files(payload)
    ))
    for path in [root, *root.rglob("*")]:
        if path.is_dir():
            path.chmod(0o755)
        elif path.parent == control:
            path.chmod(0o644)
        os.utime(path, (epoch, epoch))
    subprocess.run(
        ["dpkg-deb", "--root-owner-group", "-Zgzip", "-z9", "--build", str(root), str(destination)],
        check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env={**os.environ, "LC_ALL": "C", "SOURCE_DATE_EPOCH": str(epoch)},
    )
    fields = run("dpkg-deb", "--field", destination, "Package", "Version", "Architecture").stdout
    for expected in [f"Package: {PACKAGE}", f"Version: {deb_version(manifest['version'])}", "Architecture: amd64"]:
        if expected not in fields.splitlines():
            raise ValueError(f"Unexpected Debian metadata: {fields}")
    # Read both standard human diagnostics and the actual binary contents.
    run("dpkg-deb", "--info", destination)
    run("dpkg-deb", "--contents", destination)
    with subprocess.Popen(["dpkg-deb", "--fsys-tarfile", str(destination)], stdout=subprocess.PIPE) as process:
        assert process.stdout is not None
        with tarfile.open(fileobj=process.stdout, mode="r|") as archive:
            validate_archive(archive, expected_files(payload, "usr"))
        process.stdout.close()
        if process.wait() != 0:
            raise ValueError("dpkg-deb failed while validating its payload")
    # No maintainer hooks may run any code during installation/removal.
    with tempfile.TemporaryDirectory(dir=work) as extracted:
        run("dpkg-deb", "--control", destination, extracted)
        if {path.name for path in Path(extracted).iterdir()} != {"control", "md5sums"}:
            raise ValueError("Unexpected Debian maintainer scripts")


def validate_cpio(stream, expected: dict[str, tuple[int, str]]) -> None:
    """Read RPM's newc payload without trusting extraction paths or shell tools."""
    def read_exact(size: int) -> bytes:
        value = stream.read(size)
        if len(value) != size:
            raise ValueError("Truncated RPM cpio payload")
        return value

    seen: set[str] = set()
    while True:
        header = read_exact(110)
        if header[:6] != b"070701":
            raise ValueError("Expected an RPM newc payload")
        try:
            values = [int(header[index:index + 8], 16) for index in range(6, 110, 8)]
        except ValueError as error:
            raise ValueError("Invalid RPM cpio header") from error
        _, mode, uid, gid, links, _, size, _, _, _, _, name_size, checksum = values
        if not 1 <= name_size <= 4096 or checksum:
            raise ValueError("Invalid RPM cpio filename/checksum")
        encoded_name = read_exact(name_size)
        read_exact(-(110 + name_size) % 4)
        if encoded_name[-1:] != b"\0" or b"\0" in encoded_name[:-1]:
            raise ValueError("Invalid RPM cpio filename")
        name = encoded_name[:-1].decode("utf-8", "strict")
        if name == "TRAILER!!!":
            if size:
                raise ValueError("Invalid RPM cpio trailer")
            # rpm2cpio may append zero padding up to the next block.
            while trailing := stream.read(65536):
                if trailing.strip(b"\0"):
                    raise ValueError("Unexpected data after RPM cpio trailer")
            break
        path = PurePosixPath(name)
        if path.is_absolute() or ".." in path.parts or uid or gid:
            raise ValueError(f"Unexpected RPM path/ownership: {name}")
        normalized = path.as_posix()
        if stat.S_ISDIR(mode):
            if size or stat.S_IMODE(mode) != 0o755:
                raise ValueError(f"Unexpected RPM directory: {name}")
            continue
        if not stat.S_ISREG(mode) or links != 1 or normalized not in expected or normalized in seen:
            raise ValueError(f"Unexpected or duplicate RPM member: {name}")
        expected_mode, expected_hash = expected[normalized]
        actual = hashlib.sha256()
        remaining = size
        while remaining:
            chunk = read_exact(min(remaining, 1024 * 1024))
            actual.update(chunk)
            remaining -= len(chunk)
        read_exact(-size % 4)
        if stat.S_IMODE(mode) != expected_mode or actual.hexdigest() != expected_hash:
            raise ValueError(f"RPM content/permissions differ: {name}")
        seen.add(normalized)
    if seen != expected.keys():
        raise ValueError(f"RPM files missing: {sorted(expected.keys() - seen)}")


def write_rpm(payload: Path, destination: Path, manifest: dict, epoch: int) -> None:
    # Keep RPM macro paths independent of user-supplied output directories,
    # which may contain spaces, percent signs, Unicode or shell metacharacters.
    with tempfile.TemporaryDirectory(prefix="babel-rpm-") as temporary:
        top = Path(temporary)
        for name in ("BUILD", "BUILDROOT", "RPMS", "SOURCES", "SPECS", "SRPMS"):
            (top / name).mkdir()
        staged = top / "SOURCES/payload"
        shutil.copytree(payload, staged)
        for path in [staged, *staged.rglob("*")]:
            os.utime(path, (epoch, epoch))
        entries = []
        for path in sorted(staged.rglob("*")):
            relative = path.relative_to(staged).as_posix()
            # Only own Babel directories; shared system directories belong to
            # the distribution. No .wants symlinks or installation scriptlets.
            if path.is_dir() and not relative.startswith(("share/doc/babel-audio", "share/babel")):
                continue
            quoted = '"/usr/' + relative.replace("%", "%%").replace('"', '\\"') + '"'
            prefix = "%dir " if path.is_dir() else "%license " if relative.endswith("/copyright") else ""
            entries.append(prefix + quoted)
        spec = top / "SPECS/babel.spec"
        spec.write_text(
            f"Name: {PACKAGE}\nVersion: {rpm_version(manifest['version'])}\nRelease: 1\n"
            "Summary: Virtual audio routing and bidirectional speech translation\n"
            f"License: {manifest['license']}\nBuildArch: x86_64\nAutoReqProv: no\n"
            + "".join(f"Requires: {dependency}\n" for dependency in manifest["rpm_requires"])
            + "\n%description\nBabel provides a per-user tray and local settings dashboard.\n"
            "Requires an existing PulseAudio or PipeWire-pulse user session.\n"
            "Installation does not start Babel or enable its optional user service.\n"
            "\n%install\nmkdir -p %{buildroot}/usr\ncp -a %{_sourcedir}/payload/. %{buildroot}/usr/\n"
            "\n%files\n%defattr(-,root,root,-)\n" + "\n".join(entries) + "\n"
        )
        run(
            "rpmbuild", "-bb", "--define", f"_topdir {top}",
            "--define", "_rpmformat 4", "--define", "_binary_payload w9.gzdio",
            "--define", "_buildhost reproducible.babel.invalid",
            "--define", "use_source_date_epoch_as_buildtime 1",
            "--define", "clamp_mtime_to_source_date_epoch 1",
            "--define", "_build_id_links none", "--define", "debug_package %{nil}",
            "--define", "__os_install_post %{nil}", spec,
            env={"SOURCE_DATE_EPOCH": str(epoch)},
        )
        packages = list((top / "RPMS").rglob("*.rpm"))
        if len(packages) != 1:
            raise ValueError("Expected exactly one binary RPM")
        shutil.copyfile(packages[0], destination)
    metadata = run("rpm", "-qp", "--qf", "%{NAME}\n%{VERSION}\n%{RELEASE}\n%{ARCH}\n%{LICENSE}\n%{BUILDTIME}\n", destination).stdout.splitlines()
    if metadata != [PACKAGE, rpm_version(manifest["version"]), "1", "x86_64", manifest["license"], str(epoch)]:
        raise ValueError(f"Unexpected RPM metadata: {metadata}")
    requirements = set(run("rpm", "-qp", "--requires", destination).stdout.splitlines())
    actual = {value for value in requirements if not value.startswith("rpmlib(")}
    if actual != set(manifest["rpm_requires"]):
        raise ValueError(f"Unexpected RPM dependencies: {sorted(requirements)}")
    for option in ("--scripts", "--triggers", "--filetriggers"):
        if run("rpm", "-qp", option, destination).stdout.strip():
            raise ValueError("Unexpected RPM install/remove scriptlets or triggers")
    run("rpm", "-qp", "--info", destination)
    run("rpm", "-qp", "--list", destination)
    with subprocess.Popen(["rpm2cpio", str(destination)], stdout=subprocess.PIPE) as process:
        assert process.stdout is not None
        try:
            validate_cpio(process.stdout, expected_files(payload, "usr"))
        except BaseException:
            process.kill()
            raise
        finally:
            process.stdout.close()
        if process.wait() != 0:
            raise ValueError("rpm2cpio failed while validating its payload")


def build(bin_dir: Path, output: Path, version: str, runtime_dir: Path | None = None) -> dict:
    version = package_version(version)
    for command in ("readelf", "dpkg-deb", "rpmbuild", "rpm", "rpm2cpio"):
        if not shutil.which(command):
            raise ValueError(f"Missing build tool: {command}")
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH", "0"))
    if not 0 <= epoch <= 0xFFFFFFFF:
        raise ValueError("SOURCE_DATE_EPOCH must fit an unsigned 32-bit timestamp")
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".babel-linux-", dir=output) as temporary:
        work = Path(temporary)
        payload = work / "payload"
        manifest = stage_payload(bin_dir.resolve(), payload, version, runtime_dir)
        base = f"{PACKAGE}-{version}-linux-amd64"
        deb_name = f"{PACKAGE}_{deb_version(version)}_amd64.deb"
        tar_name = f"{base}.tar.gz"
        rpm_name = f"{PACKAGE}-{rpm_version(version)}-1.x86_64.rpm"
        write_deb(payload, work / deb_name, manifest, work, epoch)
        write_tar(payload, work / tar_name, base, epoch)
        write_rpm(payload, work / rpm_name, manifest, epoch)
        report = {
            "package": PACKAGE, "version": version, "architecture": "amd64",
            "glibc_minimum": manifest["glibc_minimum"], "depends": manifest["depends"],
            "rpm_requires": manifest["rpm_requires"],
            "artifacts": {
                name: {"sha256": digest(work / name), "size": (work / name).stat().st_size}
                for name in (deb_name, tar_name, rpm_name)
            },
            "validation": ["ELF amd64", "dpkg-deb info/contents", "RPM metadata/dependencies/cpio payload", "all file hashes and modes", "no maintainer hooks or scriptlets"],
        }
        report_name = f"{base}.manifest.json"
        (work / report_name).write_text(json.dumps(report, indent=2) + "\n")
        for name in (deb_name, tar_name, rpm_name, report_name):
            os.replace(work / name, output / name)
        return report


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime-dir", type=Path, required=True, help="prebuilt and hashed local-runtime payloads")
    parser.add_argument("--bin-dir", type=Path, default=PROJECT / "target/release")
    parser.add_argument("--output", type=Path, default=PROJECT / "artifacts")
    parser.add_argument("--version")
    args = parser.parse_args()
    try:
        report = build(args.bin_dir, args.output, package_version(args.version), args.runtime_dir)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Linux packaging failed: {error}\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
