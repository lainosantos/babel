# Repository conventions

- Write documentation, source comments, identifiers, technical diagnostics,
  examples, commit messages, pull request text, and website copy in English.
- Keep intentional interface translations in locale catalogs and localized
  platform resources. Preserve multilingual test fixtures when they verify
  language handling, Unicode paths, or speech recognition.
- Use Conventional Commits for new commits, for example
  `feat(audio): add a routing option` or `fix(ui): stabilize the buffer timer`.
- Keep Linux, macOS, and Windows behavior in scope. Report native hardware or
  signing limitations accurately; compilation alone is not hardware validation.
- Original microphone and speaker routing must remain independent of expensive
  processing. Do not introduce network, file I/O, inference, or blocking work
  into the audio callbacks or original routing executor.
- The static site is generated with `scripts/build_site.py` from `site/` and
  repository Markdown. Keep local links and section anchors valid. Generated
  `_site/` output is not committed.
- Ordinary branch pushes and pull requests run checks, not installer builds.
  Stable `vX.Y.Z` tags matching `Cargo.toml` trigger the release pipeline.
- Never commit credentials, user configuration, recordings, model weights,
  temporary build output, or authenticated local dashboard URLs.
