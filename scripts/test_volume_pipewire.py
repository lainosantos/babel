#!/usr/bin/env python3
"""Opt-in volume smoke test on a private PipeWire server with no hardware modules.

Requires pipewire and the PulseAudio client tools on PATH. No running audio
session, real device, default selection, or user configuration is modified.
"""

import argparse
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


CONFIG = """
context.properties = { core.daemon = true core.name = pipewire-0 support.dbus = false }
context.spa-libs = {
    audio.convert.* = audioconvert/libspa-audioconvert
    support.* = support/libspa-support
}
context.modules = [
    { name = libpipewire-module-protocol-native }
    { name = libpipewire-module-metadata }
    { name = libpipewire-module-spa-node-factory }
    { name = libpipewire-module-client-node }
    { name = libpipewire-module-access args = { access.force = unrestricted } }
    { name = libpipewire-module-adapter }
    { name = libpipewire-module-link-factory }
    { name = libpipewire-module-protocol-pulse }
]
context.objects = [
    { factory = metadata args = { metadata.name = default } }
    { factory = spa-node-factory args = {
        factory.name = support.node.driver node.name = Dummy-Driver
        node.group = pipewire.dummy priority.driver = 20000
    } }
]
pulse.properties = { server.address = [ "unix:native" ] }
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--test-binary", type=Path, help="Reuse an already-built Rust library test binary")
    args = parser.parse_args()
    for program in ("pipewire", "pactl"):
        if not shutil.which(program):
            parser.error(f"{program} must be available on PATH")
    repo = Path(__file__).resolve().parents[1]
    test = "audio::volume::linux::tests::isolated_pipewire_service_tracks_real_endpoint_controls"
    if args.test_binary:
        available = subprocess.check_output(
            [str(args.test_binary.resolve()), "--list"], text=True, timeout=10
        )
        if f"{test}: test" not in available.splitlines():
            parser.error("The supplied test binary does not include the isolated volume smoke; rebuild it first")
    command = (
        [str(args.test_binary.resolve()), test, "--ignored", "--exact", "--nocapture"]
        if args.test_binary
        else ["cargo", "test", "--lib", test, "--", "--ignored", "--exact", "--nocapture"]
    )
    with tempfile.TemporaryDirectory(prefix="babel-volume-smoke-") as directory:
        root = Path(directory)
        config = root / "server.conf"
        config.write_text(CONFIG, encoding="utf-8")
        env = {name: os.environ[name] for name in (
            "PATH", "LD_LIBRARY_PATH", "HOME", "RUSTUP_HOME", "CARGO_HOME", "RUSTUP_TOOLCHAIN", "LANG"
        ) if name in os.environ}
        env.update({
            "XDG_RUNTIME_DIR": directory,
            "PIPEWIRE_RUNTIME_DIR": directory,
            "PIPEWIRE_CONFIG_DIR": directory,
            "PIPEWIRE_REMOTE": "pipewire-0",
            "PULSE_RUNTIME_PATH": str(root / "pulse"),
            "PULSE_SERVER": f"unix:{root}/pulse/native",
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_STATE_HOME": str(root / "state"),
            "BABEL_PRIVATE_VOLUME_TEST": "1",
        })
        with (root / "server.log").open("w+") as log:
            server = subprocess.Popen(["pipewire", "-c", str(config)], env=env, stdout=log, stderr=log)
            try:
                for _ in range(60):
                    result = subprocess.run(["pactl", "info"], env=env, capture_output=True, timeout=2)
                    if result.returncode == 0:
                        break
                    if server.poll() is not None:
                        raise RuntimeError("The private PipeWire server exited during startup")
                    time.sleep(0.1)
                else:
                    raise RuntimeError("The private PipeWire server did not become ready")
                subprocess.run(command, cwd=repo, env=env, check=True, timeout=600)
                print("Private PipeWire volume smoke passed; no hardware endpoints were loaded.")
            finally:
                server.terminate()
                try:
                    server.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


if __name__ == "__main__":
    main()
