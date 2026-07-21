# Fonts

bladestats renders text through its own glyph atlas rather than through the system's text engine.
One file is required:

    JetBrainsMono-Regular.ttf

It is **not stored in the repository** — binary assets bloat git history, and the licence lets
you fetch it yourself.

## Where to get it

Official releases: <https://github.com/JetBrains/JetBrainsMono/releases>

Two files from the archive go into this directory:

- `JetBrainsMono-Regular.ttf` — the font itself;
- `OFL.txt` from the archive, renamed to `LICENSE-JetBrainsMono.txt`.

## Why this font

Monospaced, so digits do not shift as values change — the single most important property for a
monitor that updates ten times a second.

The licence is the SIL Open Font License 1.1 (versions before 2.0 were Apache-2.0; they are
not any more). OFL permits embedding the font in a binary and redistributing it with no fees
attached, but requires the licence text to travel with it — which is why
`LICENSE-JetBrainsMono.txt` is committed even though the `.ttf` is not.
