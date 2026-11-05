## What's new

- Use a custom speech-to-speech service over WebSocket or an audio Chat
  Completions endpoint.
- Switch physical audio devices directly from Routing during a session.
- Record the microphone and incoming audio as two stereo pairs in one WAV, with
  adaptive microphone leveling available.
- Include recent audio in recording and transcription when starting a session.
  Translation starts with live audio.
- End a session to stop translation and transcription immediately while accepted
  recording and transcript data finish saving in the background.

## Upgrading from 1.x

Voice commands and MCP integrations were removed. Before opening an existing
configuration in Babel 2, remove the `[agent]` table and any nested agent tables
from `babel.toml`. Keep the remaining settings.

The macOS and Windows packages use external virtual audio devices. If you used
the former Babel drivers, select two independent external paths in Babel and
your calling app. See the [quick-start guide](https://github.com/lainosantos/babel/blob/v2.0.0/README.md#getting-started).

## Downloads

These assets include Babel and bundled local inference engines for Linux x86_64, macOS Apple Silicon/Intel, and Windows x64/ARM64. `SHA256SUMS.txt` and `release-manifest.json` list every published payload.

For virtual audio, macOS uses separately installed BlackHole devices and Windows x64 can use Virtual Cables with the signed USB/IP transport. See the [platform setup guide](https://lainosantos.github.io/babel/docs/platforms.html).

The macOS app has an ad-hoc signature and the installer is unsigned. Windows executables are unsigned. System policy can require manual authorization or block these packages.

Local inference runtime archives are included separately as well as inside their platform installers. Their licenses and corresponding source are included in the payloads.
