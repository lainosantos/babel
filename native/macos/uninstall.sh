#!/bin/sh
# Explicit administrator action only. Never elevate or restart CoreAudio here.
set -eu
if [ "$(uname -s)" != Darwin ]; then
    echo 'This uninstaller runs only on macOS.' >&2
    exit 1
fi
if [ "$(id -u)" -ne 0 ]; then
    echo 'Administrator privileges are required. Run this script explicitly with sudo.' >&2
    exit 1
fi
babel_driver='/Library/Audio/Plug-Ins/HAL/BabelAudio.driver'
for babel_parent in /Library /Library/Audio /Library/Audio/Plug-Ins /Library/Audio/Plug-Ins/HAL "$babel_driver"; do
    if [ -L "$babel_parent" ]; then
        echo "Refusing to follow a symlink: $babel_parent" >&2
        exit 1
    fi
done
if [ ! -e "$babel_driver" ]; then
    echo 'BabelAudio.driver is not installed.'
    exit 0
fi
if [ ! -d "$babel_driver" ] || [ -L "$babel_driver/Contents" ] || [ -L "$babel_driver/Contents/Info.plist" ]; then
    echo 'Unexpected driver bundle layout; nothing was removed.' >&2
    exit 1
fi
babel_bundle_id=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$babel_driver/Contents/Info.plist")
if [ "$babel_bundle_id" != 'org.babel.audio.driver' ]; then
    echo 'The installed bundle does not belong to Babel; nothing was removed.' >&2
    exit 1
fi
/bin/rm -rf -- "$babel_driver"
/usr/sbin/pkgutil --forget org.babel.audio.driver.pkg >/dev/null 2>&1 || true
echo 'BabelAudio.driver removed. Restart macOS to unload the driver. Other audio devices were not changed.'
