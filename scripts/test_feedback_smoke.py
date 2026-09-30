#!/usr/bin/env python3
"""Render native command feedback on any host without audio or a desktop session.

Exercises the packaged binary, embedded font, localization and software renderer.
Window focus, placement and the OS notification fallback require a desktop test.
"""

import argparse
import hashlib
import json
from pathlib import Path
import platform
import subprocess


def smoke(binary, output):
    subprocess.run(
        [str(binary.resolve()), "--preview-dir", str(output.resolve())],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        check=True,
        timeout=30,
    )
    images = []
    for phase in ("activated", "processing", "succeeded", "failed"):
        for theme in ("light", "dark"):
            path = output / f"{phase}-{theme}.ppm"
            header, size, depth, pixels = path.read_bytes().split(b"\n", 3)
            if (header, size, depth) != (b"P6", b"760 208", b"255"):
                raise AssertionError(f"Unexpected native image header: {path.name}")
            if len(pixels) != 760 * 208 * 3:
                raise AssertionError(f"Incomplete native render: {path.name}")
            colors = set(zip(pixels[::3], pixels[1::3], pixels[2::3]))
            if len(colors) < 20:
                raise AssertionError(f"Empty or corrupt native render: {path.name}")
            images.append({"file": path.name, "sha256": hashlib.sha256(pixels).hexdigest()})
    if len({image["sha256"] for image in images}) != 8:
        raise AssertionError("Native command phases or themes render identically")
    return images


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--feedback", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    report = {"system": platform.system(), "status": "failed", "scope": __doc__.strip()}
    try:
        report["images"] = smoke(args.feedback, args.output_dir)
        report["status"] = "passed"
    except Exception as error:
        report["error"] = str(error)
    path = args.output_dir / "report.json"
    path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"Native feedback smoke {report['status']}: {path}")
    if "error" in report:
        print(report["error"])
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
