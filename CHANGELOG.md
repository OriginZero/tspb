# Changelog

All notable changes to tspb are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.3] - 2026-11-25

### Added

- **OS listening port discovery for running processes.** Process scanning now
  collects active TCP listening sockets (`netstat` on Windows / `lsof` on Unix,
  with lightweight caching) alongside command-line `--server.port` flags,
  surfacing Nacos/config-assigned ports (e.g. `:7001`, `:7017`) in `RUNNING`.

### Fixed

- **Maven/Gradle child JVM deduplication across sibling modules.** `RunningApp`
  records OS parent-process chains (`ancestor_pids`), resolves
  `-Dmaven.multiModuleProjectDirectory=` from ancestor Maven launchers, and
  falls back to `@*.argfile` module classpath entries (`target/classes`,
  `build/classes/...`). Managed processes now deduplicate reliably even when
  sibling modules share an identical `@SpringBootApplication` main class and no
  `-Duser.dir=` is set.
- **Switching `RUNNING` services always updates the `LOG` view.** Rows carrying a
  live `pid` no longer inherit the previously viewed service's `selected_log_id`
  when they have no log file of their own, and external processes no longer
  borrow another run's managed buffer via `find_by_project`. Selection moves
  sync target stream state immediately, `find_by_project` prefers active runs,
  and `row_key` keeps `PROJECTS` cursor selection stable across launches.
- **Per-PID log isolation for shared Logback files.** When multiple external
  `[SYSTEM]` services write to the same Logback file (e.g. copied
  `jeecg-system-%d.log` patterns), switching PIDs now resets scroll/follow
  state via `(shown_log_id, shown_log_pid)` and filters lines per service using
  exclusive listening ports (such as `[http-nio-7017-*]` vs `[http-nio-7001-*]`),
  PID markers, unique main class / `spring.application.name`, and attached
  stacktrace continuations, while reporting matched counts in `DETAIL` and `LOG`.

## [0.3.2] - 2026-11-24

### Added

- **Confirmation prompt for stopping running services.** Pressing `s` (or `x`)
  on any live service (whether started inside `tspb` or externally) now prompts
  for confirmation before terminating its process tree; already exited/failed
  entries are still dismissed immediately.
- **IDEA `ALTERNATIVE_JRE_PATH` support.** IDEA run configurations with an
  enabled alternative JRE are matched against discovered JDKs when no manual
  or `--jdk` override is set.

### Fixed

- **Multi-module IDEA run configuration binding & deduplication.** `parse_run_xml`
  now binds `<module name="..." />` entries to their target module and scopes
  Maven/Gradle goal checks to each `<configuration>` block, preventing shared
  main-class modules (e.g. Spring Cloud `-start` vs `-cloud-start` modules) or
  parent aggregator POMs from inheriting another module's configuration.
  `match_running_project` also disambiguates running processes across shared
  main classes and `--wide` checkouts.
- **Shared `logs` directory isolation & config parsing.** External log discovery
  separates module-exclusive folders from shared `../logs` folders so sibling
  services do not hijack each other's log files, avoids attaching file logs to
  idle project rows, parses nested YAML (`spring.application.name`,
  `logging.file.*`) and `.properties` (`=`) files, expands `${user.home}`, and
  caches file lookups across UI frames.
- **Clear stale logs after stopping services.** Stopping an external or managed
  process—or an external process exiting on its own—now clears `selected_log_id`
  and prunes stale `file://` cache entries instead of leaving old logs pinned.
- **TUI JDK picker override.** Selecting a JDK in the TUI (`J` or `Enter` on a
  JDK row) now overrides `<java.version>` auto-selection and accurately reports
  the launched JDK in the status bar.
- **Gradle wrapper, process detection & UI polish.** Gradle projects invoke
  `gradlew`/`gradle` instead of `mvn`; process scanning recognizes Spring Boot
  3.2+ `loader.launch.*` and `-jar` launches while stripping Windows `\\?\`
  prefixes; `SCROLL` mode accounts for CJK display width and word wrapping;
  scrolled list mouse clicks respect list offset; sidebar `Running`/`All` counts
  include managed processes; reader threads get a brief flush window on exit.

## [0.3.1] - 2026-11-23

### Added

- **Live log streaming for external applications.** `tspb` now automatically
  locates active log files for applications started outside the TUI (e.g. from
  IntelliJ IDEA). It inspects `logback-spring.xml` / `logback.xml` rolling file
  patterns (`LOG_HOME`, `LOG_PATH`, `<FileNamePattern>`), Spring configuration
  properties (`logging.file.name`, `logging.file.path`), and conventional `logs`
  directories, scoring candidate files by name tokens and recency.
- The LOG pane displays the matched log file badge (e.g. `[app.log]`), and
  DETAIL reports the file name alongside live line counts.

### Fixed

- **Commented-out annotations are ignored during project scanning.** Comments
  (`//` and `/* */`) in Java and Kotlin source files are stripped before checking
  for `@SpringBootApplication`, preventing commented-out modules from being
  mistakenly listed as runnable.
- **Deduplicated managed applications.** Java processes launched by `tspb`
  runner are deduplicated against system process scans and inherit detected
  ports. Stopping a managed runner instance now cascades termination to child
  JVM processes, and exited/failed runner rows can be cleanly dismissed.
- **Log follow mode visual line wrapping.** In FOLLOW mode, the start line
  index is now computed using visual terminal row budget calculated backwards
  from the tail, ensuring wrapped lines do not push newest log output off-screen.

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

[0.3.2]: https://github.com/OriginZero/tspb/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/OriginZero/tspb/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/OriginZero/tspb/compare/v0.2.0...v0.3.0
