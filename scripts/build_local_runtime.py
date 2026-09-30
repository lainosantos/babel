#!/usr/bin/env python3
"""Build the native inference payload on its target OS/architecture (CI only).

The installed application needs no Python, Git, compiler, package manager or
separately installed inference service. No model or microphone is used here.
Inputs are immutable archives checked against local_runtime.lock.json. A
complete corresponding-source copy accompanies the GPL Piper subprocess.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
LOCK = Path(__file__).with_name("local_runtime.lock.json")
PATCHES = {"whisper": "whisper-dynamic-port.patch", "llama": "llama-readiness.patch", "piper": "piper-managed.patch"}


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def run(*args, **kwargs):
    print("+", " ".join(map(str, args)), flush=True)
    return subprocess.run(list(map(str, args)), check=True, **kwargs)


def host():
    system = {"linux": "linux", "darwin": "macos", "win32": "windows"}.get(sys.platform)
    arch = {"x86_64": "x86_64", "amd64": "x86_64", "arm64": "aarch64", "aarch64": "aarch64"}.get(platform.machine().lower())
    if system is None or arch is None:
        raise ValueError("Unsupported local runtime platform")
    return system, arch


def fetch(pin, cache):
    cache.mkdir(parents=True, exist_ok=True)
    target = cache / pin["archive"]
    if target.exists():
        if target.stat().st_size != pin["size"] or digest(target) != pin["sha256"]:
            raise ValueError(f"Pinned archive checksum mismatch; existing file preserved: {target}")
        return target
    temporary = target.with_suffix(target.suffix + ".download")
    try:
        with urllib.request.urlopen(pin["url"], timeout=90) as source, temporary.open("xb") as dest:
            received = 0
            while chunk := source.read(1024 * 1024):
                received += len(chunk)
                if received > pin["size"]:
                    raise ValueError("Download exceeded the pinned size")
                dest.write(chunk)
        if temporary.stat().st_size != pin["size"] or digest(temporary) != pin["sha256"]:
            raise ValueError("Download does not match pinned size and SHA-256")
        os.replace(temporary, target)
    finally:
        temporary.unlink(missing_ok=True)
    return target


def unpack(archive, destination):
    """Strip one archive root; refuse absolute/traversing paths and links."""
    destination.mkdir(parents=True)
    def relative(name):
        path = PurePosixPath(name)
        if path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name:
            raise ValueError("Unsafe archive path")
        return Path(*path.parts[1:]) if len(path.parts) > 1 else None
    if zipfile.is_zipfile(archive):
        with zipfile.ZipFile(archive) as contents:
            for info in contents.infolist():
                path = relative(info.filename)
                if path is None or info.is_dir():
                    continue
                if (info.external_attr >> 16) & 0o170000 == 0o120000:
                    raise ValueError("Unexpected ZIP symlink")
                target = destination / path
                target.parent.mkdir(parents=True, exist_ok=True)
                with contents.open(info) as source, target.open("wb") as dest:
                    shutil.copyfileobj(source, dest)
    else:
        with tarfile.open(archive) as contents:
            members = contents.getmembers()
            for info in members:
                path = relative(info.name)
                if path is None or info.isdir():
                    continue
                target = destination / path
                target.parent.mkdir(parents=True, exist_ok=True)
                if info.issym() or info.islnk():
                    # Upstream ORT publishes SONAME aliases. Flatten verified
                    # archives into regular files so installers need no links.
                    source = contents.extractfile(info)
                elif info.isfile():
                    source = contents.extractfile(info)
                else:
                    raise ValueError("Unexpected archive special file")
                if source is None:
                    raise ValueError("Missing archive member data")
                with source, target.open("wb") as dest:
                    shutil.copyfileobj(source, dest)
                target.chmod(0o755 if info.mode & 0o111 else 0o644)


def source_tree(name, pins, cache, work):
    archive = fetch(pins[name], cache)
    patch = ROOT / "scripts/patches" / PATCHES[name] if name in PATCHES else None
    identity = {"archive": pins[name]["sha256"], "patch": digest(patch) if patch else None}
    directory = work / "sources" / name
    marker = directory / ".babel-source.json"
    if directory.exists():
        if not marker.is_file() or json.loads(marker.read_text()) != identity:
            raise ValueError(f"Source directory belongs to another build; use a fresh --work-dir: {directory}")
        if patch:
            verify_patch(directory, patch)
        return directory, archive
    unpack(archive, directory)
    if patch:
        verify_patch(directory, patch)
    marker.write_text(json.dumps(identity) + "\n")
    return directory, archive


def verify_patch(directory, patch):
    # Without this isolated repository, `git apply` can discover Babel's outer
    # repository and silently skip git-format paths beneath its cwd prefix.
    run("git", "init", "--quiet", directory)
    reverse = subprocess.run(["git", "-C", str(directory), "apply", "--reverse", "--check", str(patch)], capture_output=True)
    if reverse.returncode != 0:
        run("git", "-C", directory, "apply", "--check", patch)
        run("git", "-C", directory, "apply", patch)
    run("git", "-C", directory, "apply", "--reverse", "--check", patch)


def windows_cmake_generator():
    # Hosted Windows images can upgrade VS independently of the runner label.
    # Match the installed compiler to a generator actually supported by CMake.
    vswhere = Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")) / "Microsoft Visual Studio/Installer/vswhere.exe"
    component = "Microsoft.VisualStudio.Component.VC.Tools." + ("ARM64" if host()[1] == "aarch64" else "x86.x64")
    version = run(vswhere, "-latest", "-products", "*", "-requires", component,
                  "-property", "installationVersion", capture_output=True, text=True).stdout.strip()
    major = version.split(".")[0]
    if not major.isdigit():
        raise RuntimeError(f"Visual Studio with {component} was not found")
    capabilities = json.loads(run("cmake", "-E", "capabilities", capture_output=True, text=True).stdout)
    matches = [item["name"] for item in capabilities.get("generators", [])
               if item["name"].startswith(f"Visual Studio {major} ")]
    if len(matches) != 1:
        raise RuntimeError(f"Installed CMake has no matching generator for Visual Studio {version}; update CMake")
    return matches[0]


def cmake(source, build, extra, jobs, targets):
    # GGML passes CRT-owned FILE pointers between its shared libraries. /MT
    # creates a separate CRT per DLL and breaks _fileno/_get_osfhandle on Windows.
    # Use one shared CRT and bundle its redistributables beside every service.
    common = ["-DCMAKE_BUILD_TYPE=Release", "-DCMAKE_POLICY_DEFAULT_CMP0091=NEW", "-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL", "-DCMAKE_BUILD_WITH_INSTALL_RPATH=ON"]
    if sys.platform == "win32":
        # NMake rejects -A and does not initialize the MSVC environment itself.
        common += ["-G", windows_cmake_generator(), "-A", "ARM64" if host()[1] == "aarch64" else "x64"]
        if host()[1] == "aarch64":
            # The pinned GGML ARM backend requires Clang's ARM intrinsics and
            # explicitly rejects cl.exe. Keep the Visual Studio ABI/shared CRT.
            common += ["-T", "ClangCL"]
        # Existing native engines use narrow argv/filesystem paths. Windows
        # 10 1903+ UTF-8 activation preserves non-ASCII user/model directories.
        common += [f'-DCMAKE_EXE_LINKER_FLAGS=/MANIFEST:EMBED /MANIFESTINPUT:"{ROOT / "scripts/windows_utf8.manifest"}"']
    elif sys.platform == "darwin":
        common += ["-DCMAKE_OSX_DEPLOYMENT_TARGET=14.2", "-DCMAKE_INSTALL_RPATH=@loader_path;@loader_path/../lib"]
    else:
        common += ["-DCMAKE_INSTALL_RPATH=$ORIGIN;$ORIGIN/../lib"]
    run("cmake", "-S", source, "-B", build, *common, *extra)
    run("cmake", "--build", build, "--config", "Release", "--parallel", jobs, "--target", *targets)


def copy_regular(source, target, executable=False):
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, target)
    target.chmod(0o755 if executable else 0o644)


def is_library(name):
    return name.endswith((".dll", ".dylib")) or ".so" in name


def stage_ggml(name, source, work, destination, system, arch, jobs):
    build = work / "build" / name
    server = "whisper-server" if name == "whisper" else "llama-server"
    flags = ["-DBUILD_SHARED_LIBS=ON", "-DGGML_BACKEND_DL=ON", "-DGGML_NATIVE=OFF", "-DGGML_OPENMP=OFF", "-DGGML_CUDA=OFF", "-DGGML_VULKAN=OFF", "-DGGML_BLAS=OFF", "-DGGML_RPC=OFF", "-DGGML_CPU_KLEIDIAI=OFF", "-DGGML_CCACHE=OFF", "-DGGML_CPU_ALL_VARIANTS=" + ("ON" if arch == "x86_64" else "OFF"), "-DGGML_METAL=" + ("ON" if system == "macos" else "OFF"), "-DGGML_METAL_EMBED_LIBRARY=ON"]
    if name == "whisper":
        flags += ["-DWHISPER_BUILD_TESTS=OFF", "-DWHISPER_BUILD_IS_DEV=OFF", "-DWHISPER_CURL=OFF", "-DWHISPER_FFMPEG=OFF"]
    else:
        flags += ["-DLLAMA_BUILD_TESTS=OFF", "-DLLAMA_BUILD_EXAMPLES=OFF", "-DLLAMA_BUILD_SERVER=ON", "-DLLAMA_OPENSSL=OFF", "-DLLAMA_CURL=OFF", "-DLLAMA_BUILD_UI=OFF", "-DLLAMA_USE_PREBUILT_UI=OFF", "-DLLAMA_BUILD_IS_DEV=OFF"]
    cmake(source, build, flags, jobs, [server])
    binary = build / "bin"
    if system == "windows":
        binary /= "Release"
    executable = server + (".exe" if system == "windows" else "")
    copy_regular(binary / executable, destination / name / "bin" / executable, True)
    for library in binary.iterdir():
        if library.is_file() and is_library(library.name):
            copy_regular(library, destination / name / "bin" / library.name, True)
    return {"executable": f"{name}/bin/{executable}"}


def stage_piper(source, espeak, pins, cache, work, destination, system, arch, jobs):
    platform_name = {("linux", "x86_64"): "linux-x64", ("macos", "x86_64"): "osx-x86_64", ("macos", "aarch64"): "osx-arm64", ("windows", "x86_64"): "win-x64", ("windows", "aarch64"): "win-arm64"}.get((system, arch))
    if platform_name is None:
        raise ValueError("ONNX Runtime archive is not pinned for this architecture")
    onnx, _ = source_tree("onnx-" + platform_name, pins, cache, work)
    build, install = work / "build/piper", work / "install/piper"
    cmake(source / "libpiper", build, [f"-DONNXRUNTIME_DIR={onnx}", f"-DBABEL_ESPEAK_SOURCE={espeak}", f"-DCMAKE_INSTALL_PREFIX={install}", "-DPIPER_BUILD_TESTS=OFF", "-DENABLE_CLANG_TIDY=OFF", "-DCMAKE_INSTALL_LIBDIR=lib", "-DCMAKE_INSTALL_DATAROOTDIR=share"], jobs, ["piper_exe"])
    run("cmake", "--install", build, "--config", "Release")
    executable = "piper_exe" + (".exe" if system == "windows" else "")
    copy_regular(install / "bin" / executable, destination / "piper/bin" / executable, True)
    for library in (install / "lib").iterdir():
        if library.is_file() and is_library(library.name):
            folder = "bin" if system == "windows" else "lib"
            copy_regular(library, destination / "piper" / folder / library.name, True)
    shutil.copytree(install / "share/espeak-ng-data", destination / "piper/share/espeak-ng-data")
    licenses = destination / "licenses/onnxruntime"
    for file in onnx.iterdir():
        if file.is_file() and any(word in file.name.lower() for word in ("license", "notice", "privacy")):
            copy_regular(file, licenses / file.name)
    return {"executable": f"piper/bin/{executable}", "data": "piper/share/espeak-ng-data"}


def windows_runtime(destination, arch):
    """Share the VC runtime within each service, with no system installation."""
    vswhere = Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")) / "Microsoft Visual Studio/Installer/vswhere.exe"
    vs = run(vswhere, "-latest", "-products", "*", "-property", "installationPath", capture_output=True, text=True).stdout.strip()
    candidates = sorted((Path(vs) / "VC/Redist/MSVC").glob("*/" + ("arm64" if arch == "aarch64" else "x64") + "/Microsoft.VC*.CRT"))
    if not candidates:
        raise ValueError("Visual C++ redistributable DLLs are missing from the build toolchain")
    for dll in candidates[-1].glob("*.dll"):
        for service in ("whisper", "llama", "piper"):
            copy_regular(dll, destination / service / "bin" / dll.name, True)
    notices = list((Path(vs) / "Licenses").glob("**/*REDIST*"))
    if notices:
        for index, notice in enumerate(notices):
            if notice.is_file():
                copy_regular(notice, destination / "licenses" / f"Microsoft-REDIST-{index}{notice.suffix}")
    dumpbins = sorted((Path(vs) / "VC/Tools/MSVC").glob("*/bin/Host*/" + ("arm64" if arch == "aarch64" else "x64") + "/dumpbin.exe"))
    if not dumpbins:
        raise ValueError("MSVC dependency inspector is missing")
    for binary in destination.rglob("*"):
        if binary.suffix.lower() not in (".dll", ".exe"):
            continue
        imports = run(dumpbins[-1], "/DEPENDENTS", binary, capture_output=True, text=True).stdout
        for dependency in re.findall(r"(?im)^\s+([A-Za-z0-9_.-]+\.dll)\s*$", imports):
            if re.match(r"(?i)(vcruntime|msvcp|msvcr|concrt|onnxruntime|piper|ggml|llama|whisper)", dependency) and not (binary.parent / dependency).is_file():
                raise ValueError(f"Unbundled runtime dependency in {binary.name}: {dependency}")


def macos_relocate_and_sign(destination):
    for service in ("whisper", "llama", "piper"):
        binaries = [p for p in (destination / service).rglob("*") if p.is_file() and (p.parent.name == "bin" or p.suffix == ".dylib")]
        libraries = {p.name: p for p in binaries if p.suffix == ".dylib"}
        for binary in binaries:
            if binary.suffix == ".dylib":
                run("install_name_tool", "-id", "@rpath/" + binary.name, binary)
            lines = run("otool", "-L", binary, capture_output=True, text=True).stdout.splitlines()[1:]
            for line in lines:
                dependency = line.strip().split(" (", 1)[0]
                if dependency.startswith(("/usr/lib/", "/System/Library/")):
                    continue
                name = Path(dependency).name
                if name not in libraries:
                    raise ValueError(f"Unbundled Mach-O dependency: {dependency}")
                relative = os.path.relpath(libraries[name], binary.parent)
                run("install_name_tool", "-change", dependency, "@loader_path/" + relative, binary)
            run("codesign", "--force", "--sign", "-", binary)


def write_manifest(destination, system, arch, services):
    files = []
    for path in sorted(destination.rglob("*")):
        if path.is_symlink():
            raise ValueError("Runtime payload must contain only regular files")
        if path.is_file() and path != destination / "manifest.json":
            files.append({"path": path.relative_to(destination).as_posix(), "sha256": digest(path), "size": path.stat().st_size})
    manifest = {"version": 1, "platform": system, "arch": arch, "services": services, "files": files}
    (destination / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    return manifest


def smoke_command(command, timeout=20):
    """Keep bounded failure context without filling RAM or hiding native errors."""
    with tempfile.TemporaryFile() as output:
        try:
            run(*command, stdout=output, stderr=subprocess.STDOUT, timeout=timeout)
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
            output.seek(0, os.SEEK_END)
            output.seek(max(0, output.tell() - 8192))
            tail = output.read(8192).decode("utf-8", errors="replace").strip()
            reason = f"timed out after {timeout}s" if isinstance(error, subprocess.TimeoutExpired) else f"exited with code {error.returncode}"
            raise RuntimeError(f"Executable smoke {Path(command[0]).name} {reason}. Output tail:\n{tail or '(no output)'}") from None


def build(args):
    system, arch = host()
    for command in ("cmake", "git"):
        if shutil.which(command) is None:
            raise ValueError(f"Build-time tool missing: {command}")
    pins = json.loads(LOCK.read_text())["sources"]
    work, cache = args.work_dir.resolve(), args.download_cache.resolve()
    trees, archives = {}, {}
    for name in ("whisper", "llama", "piper", "espeak"):
        trees[name], archives[name] = source_tree(name, pins, cache, work)
    output = args.output.resolve() / f"{system}-{arch}"
    output.parent.mkdir(parents=True, exist_ok=True)
    if output.exists():
        raise ValueError(f"Output already exists; preserve it or choose another --output: {output}")
    with tempfile.TemporaryDirectory(prefix=".babel-runtime-", dir=output.parent) as temporary:
        payload = Path(temporary) / "payload"
        services = {name: stage_ggml(name, trees[name], work, payload, system, arch, args.jobs) for name in ("whisper", "llama")}
        services["piper"] = stage_piper(trees["piper"], trees["espeak"], pins, cache, work, payload, system, arch, args.jobs)
        for name in trees:
            for license_name in ("LICENSE", "COPYING", "COPYING.APACHE", "COPYING.BSD2", "COPYING.UCD"):
                path = trees[name] / license_name
                if path.is_file():
                    copy_regular(path, payload / "licenses" / name / license_name)
            license_directory = trees[name] / "licenses"
            if license_directory.is_dir():
                shutil.copytree(license_directory, payload / "licenses" / name / "third-party", symlinks=False)
        # Complete Piper/eSpeak corresponding source, modifications and scripts
        # travel with each binary instead of relying on a future source offer.
        for name in ("piper", "espeak"):
            copy_regular(archives[name], payload / "sources" / archives[name].name)
        for path in [Path(__file__), LOCK, ROOT / "scripts/local_runtime_packaging.py", ROOT / "scripts/windows_utf8.manifest"]:
            copy_regular(path, payload / "sources/scripts" / path.name)
        for patch in PATCHES.values():
            copy_regular(ROOT / "scripts/patches" / patch, payload / "sources/scripts/patches" / patch)
        (payload / "sources/README.txt").write_text("Corresponding source for the separate GPL-3.0 Piper/eSpeak subprocess is included here. Extract the two source archives and apply scripts/patches/piper-managed.patch. scripts/build_local_runtime.py and scripts/local_runtime.lock.json record the exact build flags and all dependency hashes. Build requires CMake 3.26+ (4.2+ when using Visual Studio 2026), a C++17 compiler (Visual Studio ClangCL tools on Windows ARM64), Git and Python 3.11+; none is required by the installed application. Whisper/llama.cpp/ONNX Runtime are MIT licensed. The Babel application communicates through separate-process IPC. Voice model licenses are supplied with model downloads.\n")
        if system == "windows":
            windows_runtime(payload, arch)
        elif system == "macos":
            macos_relocate_and_sign(payload)
        for path in payload.rglob("*"):
            if path.is_dir():
                path.chmod(0o755)
        write_manifest(payload, system, arch, services)
        # Executable smoke never loads a model or starts a listener.
        for name, service in services.items():
            smoke_command([payload / service["executable"], "--help"])
        shutil.move(str(payload), output)
    with tarfile.open(output.parent / (output.name + ".tar.gz"), "w:gz") as archive:
        archive.add(output, arcname=output.name)
    print(f"Native inference runtime built and smoke tested: {output}")


def extract_artifacts(directory, output):
    from local_runtime_packaging import validate
    archives = sorted(directory.glob("*.tar.gz"))
    if not archives:
        raise ValueError("No native runtime CI artifacts were downloaded")
    for archive in archives:
        folder = archive.name.removesuffix(".tar.gz")
        if not re.fullmatch(r"(linux|macos|windows)-(x86_64|aarch64)", folder):
            raise ValueError("Unexpected native runtime artifact name")
        destination = output / folder
        if destination.exists():
            raise ValueError("Native runtime extraction would overwrite files")
        unpack(archive, destination)
        system, arch = folder.split("-", 1)
        validate(destination, system, arch)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=ROOT / "artifacts/local-runtime")
    parser.add_argument("--work-dir", type=Path, default=ROOT / ".tools/local-runtime-build")
    parser.add_argument("--download-cache", type=Path, default=ROOT / ".tools/local-runtime-downloads")
    parser.add_argument("--jobs", type=int, default=min(4, os.cpu_count() or 1))
    parser.add_argument("--extract-artifacts", type=Path, help="extract/verify existing CI runtime archives without building")
    args = parser.parse_args()
    if not 1 <= args.jobs <= 32:
        parser.error("--jobs must be 1..32")
    try:
        if args.extract_artifacts:
            extract_artifacts(args.extract_artifacts, args.output)
        else:
            build(args)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Local runtime build failed: {error}\n")
