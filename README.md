# rustylogviewer

`rustylogviewer` is a compact Rust terminal log viewer for tracking appended lines across multiple files in one unified feed.

## Current Status

- Poll-based watcher with append/truncate/replace handling.
- Compact TUI feed (default mode) with key controls.
- Headless stdout mode for simple pipelines.
- Bounded in-memory line buffer with dropped-event counter.
- GUI frontend is planned (Windows/Linux), not implemented yet.

## Build

```bash
cargo build
```

## Run

GUI mode (desktop, Windows/Linux target):

```bash
cargo run -- --gui
```

GUI with preloaded config:

```bash
cargo run -- --gui --config ./rustylogviewer.toml
```

When `--gui` is started without `--config`, the app auto-loads the most recent config from MRU (if available).

TUI mode (default):

```bash
cargo run -- /var/log/app.log /tmp/dev.log
```

Headless mode:

```bash
cargo run -- --headless /var/log/app.log /tmp/dev.log
```

Print effective config and exit:

```bash
cargo run -- --print-config-only /var/log/app.log
```

Use TOML config:

```bash
cargo run -- --config ./rustylogviewer.toml
```

CLI values override config file values.

Regex rules:

- `blacklist_regex`: hide matching lines.
- `whitelist_regex`: force-show matching lines (takes precedence over blacklist).

## TUI Controls

- `q`: quit
- `p`: pause/resume polling
- `j` or `Down`: scroll toward newest lines
- `k` or `Up`: scroll toward older lines
- `g`: jump to oldest retained lines
- `G`: jump to newest lines
- `f`: cycle source-file filter
- `/`: enter text-filter input mode (`Enter` apply, `Esc` cancel)
- `c`: clear active text filter
- `i`: toggle case-insensitive text filter matching

## Useful Options

- `--case-insensitive-filter` and `--case-sensitive-filter`
- `--blacklist-regex "<pattern>"` (repeatable)
- `--whitelist-regex "<pattern>"` (repeatable)

## Config Example

See [`rustylogviewer.toml.example`](./rustylogviewer.toml.example).

## GUI Roadmap

A cross-platform graphical UI is planned using `eframe/egui` with:

- in-app config file chooser (`Open Config`)
- in-app config editor for tracked files and regex rule lists
- save/save-as config workflow
- merged live feed similar to current TUI behavior

Details are tracked in [`DEVELOPMENT_PLAN.md`](./DEVELOPMENT_PLAN.md).

## GUI Status

Current GUI supports:

- `Open Config`, `New Config`, `Save`, and `Save As`
- MRU recent-config list (persisted across app runs)
- selectable `Light Mode` (persisted as `gui_light_mode` in config)
- configurable GUI font size (persisted as `gui_font_size`)
- form-based editing for:
  - general settings
  - tracked files
  - blacklist/whitelist regex lists
- `Start`/`Stop` log stream controls
- merged live feed with source + text filters

Packaging and cross-platform QA hardening are still in progress.
