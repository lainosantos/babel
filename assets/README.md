# Babel icon

`babel.svg` is the shared icon for the dashboard and Linux desktop packages.
The tray on Linux, macOS and Windows embeds its pre-rendered 32 × 32 RGBA
version, `babel-tray.rgba`. Rendering happens only when updating the artwork;
the application needs no image decoder, renderer or external icon file.

After changing the SVG, regenerate the tray asset with librsvg's
`rsvg-convert` and ImageMagick 7 (development tools only):

```sh
rsvg-convert -w 256 -h 256 assets/babel.svg | magick png:- -resize 32x32 -depth 8 RGBA:assets/babel-tray.rgba
```

The raw asset contains 4,096 bytes (row-major, straight-alpha RGBA).
