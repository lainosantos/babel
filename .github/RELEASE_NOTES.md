These assets include the application, virtual audio drivers, and bundled local inference engines for Linux x86_64, macOS Apple Silicon/Intel, and Windows x64/ARM64. `SHA256SUMS.txt` and `release-manifest.json` list every published payload.

**Current signing status:** the macOS installer is a development package without a Developer ID distribution signature or notarization. Windows development packages contain an unsigned kernel driver, which cannot load under normal production driver-signing enforcement. Publishing a release does not change these restrictions. See the macOS and Windows installation guides in the repository before installing.

Local inference runtime archives are included separately as well as inside their platform installers. Their licenses and corresponding source are included in the payloads.
