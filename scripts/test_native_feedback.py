#!/usr/bin/env python3
"""Exercise the native helper on a private Xvfb display, never the user's desktop.

This optional Linux integration check uses only Python's standard library. The
cross-platform renderer snapshot smoke test does not need an X server.
"""
import argparse
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import sys
import tempfile
import time


def read_line(descriptor, timeout=15):
    deadline = time.monotonic() + timeout
    value = bytearray()
    while time.monotonic() < deadline:
        ready, _, _ = select.select([descriptor], [], [], deadline - time.monotonic())
        if not ready:
            break
        byte = os.read(descriptor, 1)
        if not byte:
            raise AssertionError("Native feedback exited before readiness")
        value.extend(byte)
        if byte == b"\n":
            return bytes(value)
        if len(value) > 128:
            raise AssertionError("Unexpected oversized readiness response")
    raise AssertionError("Timed out waiting for native feedback readiness")


def run(helper, xvfb):
    read_fd, write_fd = os.pipe()
    with tempfile.TemporaryFile() as display_log, tempfile.TemporaryFile() as helper_log:
        server = subprocess.Popen(
            [str(xvfb), "-displayfd", str(write_fd), "-screen", "0", "1280x720x24",
             "-nolisten", "tcp", "-noreset"],
            stdin=subprocess.DEVNULL, stdout=display_log, stderr=display_log,
            pass_fds=(write_fd,),
        )
        os.close(write_fd)
        child = None
        try:
            display_number = read_line(read_fd).decode("ascii").strip()
            assert display_number.isdecimal(), "Xvfb did not choose a display"
            environment = dict(os.environ, DISPLAY=f":{display_number}", BABEL_REDUCED_MOTION="1")
            environment.pop("WAYLAND_DISPLAY", None)
            child = subprocess.Popen(
                [str(helper)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=helper_log, env=environment,
            )
            assert read_line(child.stdout.fileno()) == b"BABEL_FEEDBACK_READY\n"
            for activation_id, outcome in [(1, "succeeded"), (2, "failed")]:
                for phase in ["activated", "processing", outcome]:
                    packet = {
                        "activation_id": activation_id, "phase": phase,
                        "title": "Synthetic feedback test",
                        "detail": "No microphone or tools are used.", "motion": False,
                    }
                    child.stdin.write(json.dumps(packet).encode("utf-8") + b"\n")
                    child.stdin.flush()
                # Let the event loop paint the final result after activation's
                # minimum dwell, including the UTF-8-independent JSON boundary.
                time.sleep(0.4)
                assert child.poll() is None, "Native feedback failed during rendering"
            child.stdin.write(json.dumps({
                "activation_id": 2, "phase": "dismissed", "title": "", "motion": False,
            }).encode("utf-8") + b"\n")
            child.stdin.flush()
            child.stdin.close()
            assert child.wait(timeout=10) == 0, "Native feedback did not close cleanly on stdin EOF"
            assert child.stdout.read() == b"", "Helper unexpectedly wrote more protocol output"
            print("Native feedback: private X11 readiness, four phases, dismissal and EOF passed")
        finally:
            os.close(read_fd)
            if child is not None and child.poll() is None:
                child.terminate()
                child.wait(timeout=10)
            server.terminate()
            server.wait(timeout=10)

    # Pure Wayland/no display must fail promptly so the parent can use a system
    # notification. Do not silently create an ordinary focus-stealing toplevel.
    environment = dict(os.environ)
    environment.pop("DISPLAY", None)
    environment.pop("WAYLAND_DISPLAY", None)
    result = subprocess.run([str(helper)], input=b"", capture_output=True,
                            env=environment, timeout=10)
    assert result.returncode != 0 and result.stdout == b""
    print("Native feedback: absent-X11 fallback signal passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--helper", type=Path, default=Path("target/debug/babel-feedback"))
    parser.add_argument("--xvfb", type=Path, default=shutil.which("Xvfb"))
    args = parser.parse_args()
    if not sys.platform.startswith("linux") or not args.xvfb:
        print("Native feedback X11 integration skipped (Linux and Xvfb required)")
        return
    run(args.helper.resolve(strict=True), Path(args.xvfb).resolve(strict=True))


if __name__ == "__main__":
    main()
