#!/bin/sh
# Locate the project configuration; session files use its configured absolute base.
set -eu
BABEL_PROJECT_DIR=$(CDPATH= cd "$(dirname "$0")/.." && pwd)
cd "$BABEL_PROJECT_DIR"

# Optional unpacked Linux utilities; normal installations use pulseaudio-utils on PATH.
if ! command -v pactl >/dev/null 2>&1 && [ -x "$BABEL_PROJECT_DIR/.tools/pulse/usr/bin/pactl" ]; then
    PATH="$BABEL_PROJECT_DIR/.tools/pulse/usr/bin:$PATH"
    export PATH
fi

if [ ! -x "$BABEL_PROJECT_DIR/target/release/babel" ]; then
    cargo build --release --locked
fi
exec "$BABEL_PROJECT_DIR/target/release/babel" "$@"
