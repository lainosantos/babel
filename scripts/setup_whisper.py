#!/usr/bin/env python3
"""Build Babel's pinned whisper.cpp server; never start it or choose a port.

Requires Python 3.9+, Git, CMake and a C++17 compiler (Visual Studio C++ build
tools on Windows; Xcode command line tools on macOS). Only the Python standard
library is used. Optional GPU backends require their native toolkits already
installed. The patched server announces its OS-assigned port over stdout.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import urllib.request


ROOT = Path(__file__).resolve().parent.parent
REPOSITORY = "https://github.com/ggml-org/whisper.cpp.git"
VERSION = "v1.9.4"
COMMIT = "927cfce34f31707e17f2bff35c349632fb9e2c3a"
MODEL = json.loads((ROOT / "src/local_runtime/models.json").read_text(encoding="utf-8"))["whisper"]["base-q5_1"]
MODEL_URL = MODEL["url"]
MODEL_SIZE = MODEL["size"]
MODEL_SHA256 = MODEL["sha256"]
PATCH = ROOT / "scripts" / "patches" / "whisper-dynamic-port.patch"


def run(arguments, **kwargs):
    return subprocess.run([str(part) for part in arguments], check=True, **kwargs)


def git_output(source, *arguments):
    return run(["git", "-C", source, *arguments], capture_output=True,
               text=True, encoding="utf-8").stdout.strip()


def ensure_source(source):
    if not source.exists():
        source.parent.mkdir(parents=True, exist_ok=True)
        run(["git", "clone", "--depth", "1", "--branch", VERSION,
             "--", REPOSITORY, source])
    if not (source / ".git").exists():
        raise RuntimeError(f"Refusing to replace a directory without a Git checkout: {source}")
    actual = git_output(source, "rev-parse", "HEAD")
    if actual != COMMIT:
        raise RuntimeError(f"Expected whisper.cpp {COMMIT}, found {actual}; "
                           "use a separate --source-dir to preserve that checkout")
    changed_files = git_output(source, "diff", "--name-only", "HEAD", "--").splitlines()
    if changed_files and changed_files != ["examples/server/server.cpp"]:
        raise RuntimeError("The whisper.cpp checkout has unrelated tracked changes; "
                           "use a separate --source-dir to preserve them")
    changes = git_output(source, "diff", "--no-ext-diff", "--no-textconv", "--binary",
                         "--color=never", "--src-prefix=a/", "--dst-prefix=b/", "HEAD", "--")
    expected = PATCH.read_text(encoding="utf-8").strip()
    if changes and changes != expected:
        raise RuntimeError("The whisper.cpp checkout has changes other than Babel's "
                           "port-discovery patch; preserve them in a separate checkout")
    if not changes:
        run(["git", "-C", source, "apply", "--check", PATCH])
        run(["git", "-C", source, "apply", PATCH])
    # Validate idempotence and ensure a truncated/incorrect patch cannot be used.
    run(["git", "-C", source, "apply", "--reverse", "--check", PATCH])


def model_is_valid(path):
    if not path.is_file() or path.stat().st_size != MODEL_SIZE:
        return False
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest() == MODEL_SHA256


def ensure_model(path):
    if path.exists():
        if not model_is_valid(path):
            raise RuntimeError(f"Model checksum mismatch; existing file was preserved: {path}")
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        request = urllib.request.Request(MODEL_URL, headers={"User-Agent": "Babel-Whisper-Setup/1"})
        # The public model needs no token. Do not forward API keys or credentials.
        with urllib.request.urlopen(request, timeout=60) as response:
            with tempfile.NamedTemporaryFile(prefix=f".{MODEL['name']}-", suffix=".download",
                                             dir=path.parent, delete=False) as target:
                temporary = Path(target.name)
                received = 0
                while chunk := response.read(1024 * 1024):
                    received += len(chunk)
                    if received > MODEL_SIZE:
                        raise RuntimeError("Model download exceeded the pinned file size")
                    target.write(chunk)
        if not model_is_valid(temporary):
            raise RuntimeError("Downloaded model does not match the pinned size and SHA-256")
        if path.exists():
            if not model_is_valid(path):
                raise RuntimeError(f"A different model appeared during download; preserved: {path}")
        else:
            os.replace(temporary, path)
            temporary = None
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def build_server(source, backend, jobs):
    build = source / "build"
    run(["cmake", "-S", source, "-B", build,
         "-DCMAKE_BUILD_TYPE=Release", "-DGGML_NATIVE=ON",
         f"-DGGML_CUDA={'ON' if backend == 'cuda' else 'OFF'}",
         f"-DGGML_METAL={'ON' if backend == 'metal' else 'OFF'}",
         "-DWHISPER_BUILD_TESTS=OFF", "-DWHISPER_BUILD_IS_DEV=OFF"])
    run(["cmake", "--build", build, "--config", "Release",
         "--target", "whisper-server", "--parallel", str(jobs)])
    name = "whisper-server.exe" if sys.platform == "win32" else "whisper-server"
    # Visual Studio/Xcode are multi-config generators; Make/Ninja are not.
    candidates = [build / "bin" / "Release" / name, build / "bin" / name]
    for executable in candidates:
        if executable.is_file():
            return executable
    raise RuntimeError(f"Build completed but {name} was not found below {build / 'bin'}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, default=ROOT / ".tools" / "whisper.cpp")
    parser.add_argument("--backend", choices=["cpu", "cuda", "metal"], default="cpu",
                        help="cpu is portable; CUDA or Metal require an installed native toolkit")
    parser.add_argument("--jobs", type=int, default=min(4, os.cpu_count() or 1))
    args = parser.parse_args(argv)
    if not 1 <= args.jobs <= 256:
        parser.error("--jobs must be between 1 and 256")
    if args.backend == "metal" and sys.platform != "darwin":
        parser.error("Metal is available only on macOS")
    for command in ["git", "cmake"]:
        if shutil.which(command) is None:
            parser.error(f"{command} is required on PATH")
    source = args.source_dir.resolve()
    ensure_source(source)
    model = source / "models" / MODEL["name"]
    ensure_model(model)
    executable = build_server(source, args.backend, args.jobs)
    print(json.dumps({"service": "babel-whisper", "version": VERSION,
                      "commit": COMMIT, "executable": str(executable),
                      "model": str(model), "backend": args.backend}))
    print("Ready to launch with --host 127.0.0.1 --port 0. "
          "Read BABEL_SERVICE_READY from stdout, verify /health, "
          "and discard all other output without recording speech.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"Whisper setup failed: {error}", file=sys.stderr)
        sys.exit(1)
