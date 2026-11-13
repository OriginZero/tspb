# Changelog

All notable changes to tspb are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-11-13

### Added

- **The LOG pane follows the selection.** Moving the cursor onto another
  `RUNNING` entry switches DETAIL and LOG to that stream immediately — no
  `Enter` needed. Streams are matched by pid, so two launch configurations of
  the same module no longer collide.
- **`z` puts a window on its own.** Inside a window, or in PANES once a window is
  highlighted, `z` full-screens it and hides the others; `z` again restores the
  split. `prefix z` keeps working from anywhere.
- **Logs keep the app's colours.** ANSI SGR codes are parsed with
  [`ansi-to-tui`](https://crates.io/crates/ansi-to-tui) into real cell styles, so
  INFO / WARN / ERROR look the way the logger intended. Lines without codes
  (Maven, plain `println`) still use the pane's keyword colouring.
- DETAIL reports the pid, buffered line count and FOLLOW/SCROLL mode of the
  stream currently shown in LOG.

### Fixed

- Opening the LOG pane no longer disables FOLLOW; new lines scroll in on every
  tick and the cursor stays on the tail.
- `y` / `Y` no longer move the cursor or the viewport, and freshly arriving
  output does not drag a frozen view back to the bottom.
- A finished process is reported once instead of appending
  `[process exited: ...]` on every tick until the 2000-line ring buffer held
  nothing else.
- `s` on a process that already exited is a no-op instead of a `taskkill`
  failure with a misleading `[stop requested]` line.
- Colourised output no longer shows its escape parameters as a literal
  `[34mINFO [0;39m` in the pane, the clipboard or `--launch` output.
- An active zoom follows the pane you enter, so `Esc` → pick another window →
  `Enter` shows that window instead of the previously zoomed one.
- A bare `z` now reaches the handler; it used to fall through to `Action::None`.

### Changed

- ANSI styling is parsed per visible line at render time rather than stripped in
  the reader thread, which keeps the original colours and leaves the ring buffer
  byte-for-byte faithful to the child process.

## Earlier releases

- `v0.2.0` — startup progress screen, install scripts, `tspb update` with a
  GitHub mirror fallback.
- `v0.1.1`, `v0.1.0` — first releases: environment/index scanning, one-key
  launch, live log capture, process-tree stop.

[0.3.0]: https://github.com/OriginZero/tspb/compare/v0.2.0...v0.3.0
