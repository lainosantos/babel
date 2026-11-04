These assets include Babel and bundled local inference engines for Linux x86_64, macOS Apple Silicon/Intel, and Windows x64/ARM64. `SHA256SUMS.txt` and `release-manifest.json` list every published payload.

For virtual audio, macOS uses separately installed BlackHole devices and Windows x64 can use Virtual Cables with the signed USB/IP transport. See the [platform setup guide](https://lainosantos.github.io/babel/docs/platforms.html).

The macOS app has an ad-hoc signature and the installer is unsigned. Windows executables are unsigned. System policy can require manual authorization or block these packages.

Local inference runtime archives are included separately as well as inside their platform installers. Their licenses and corresponding source are included in the payloads.
