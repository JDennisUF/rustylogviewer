# logtrak

`logtrak` is a compact Rust terminal log viewer for tracking appended lines across multiple files in one unified feed.

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
cargo run
```

Windows shortcut target with no console window:

```powershell
.\target\release\logtrak.exe
```

GUI with preloaded config:

```bash
cargo run -- --config ./logtrak.toml
```

Windows shortcut target with preloaded config and no console window:

```powershell
.\target\release\logtrak.exe --config .\logtrak.toml
```

When `logtrak.exe` is started without `--config`, startup selection is:
1. most recent MRU config (if any)
2. otherwise the first valid `logtrak` `.toml` config discovered in the executable/current directory

TUI mode (default):

```bash
cargo run -- /var/log/app.log /tmp/dev.log
```

Headless mode:

```bash
cargo run --bin logtrak-cli -- --headless /var/log/app.log /tmp/dev.log
```

Print effective config and exit:

```bash
cargo run --bin logtrak-cli -- --print-config-only /var/log/app.log
```

Use TOML config:

```bash
cargo run --bin logtrak-cli -- --config ./logtrak.toml
```

CLI values override config file values.

Tracked-file date placeholders:

- explicit tokens like `"/tmp/app_{yyyy}{mm}{dd}.log"` or `"/tmp/app_{mmdd}.log"`
- `"{today}"` as a shorthand that checks several common numeric formats for the current date:
  `yyyymmdd`, `yyyy-mm-dd`, `yyyy_mm_dd`, `mmdd`, `mm-dd`, `mm_dd`, `yymmdd`, `yy-mm-dd`, `yy_mm_dd`

Placeholders are resolved during polling, so a long-running session can roll over to the new day's log automatically.

Tracked-file wildcards:

- simple `*` and `?` wildcards are supported in the final filename segment, for example `"/var/log/app-*.log"`
- recursive `**` patterns are not supported
- matches are capped at `200` files per tracked pattern, with a warning if the cap is hit
- wildcard patterns are resolved during polling, so newly created matching files can be picked up without restarting

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
- `l`: show tracked file list (`l`/`Esc`/`Enter` to close)
- `/`: enter text-filter input mode (`Enter` apply, `Esc` cancel)
- `c`: clear active text filter
- `i`: toggle case-insensitive text filter matching
- `?`: show command help (`?`/`Esc`/`Enter` to close)

## Useful Options

- `--case-insensitive-filter` and `--case-sensitive-filter`
- `--blacklist-regex "<pattern>"` (repeatable)
- `--whitelist-regex "<pattern>"` (repeatable)

## Config Example

See [`logtrak.toml.example`](./logtrak.toml.example).

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
- startup discovery of valid app `.toml` configs in executable/current directory (merged into the config chooser list)
- selectable hardcoded GUI themes (including `Shades of Purple`, persisted as `gui_theme` in config)
- configurable word-wrap toggle for long log lines (persisted as `gui_word_wrap`)
- configurable GUI font size (persisted as `gui_font_size`)
- form-based editing for:
  - general settings
  - tracked files
  - blacklist/whitelist regex lists
- `Start`/`Stop` log stream controls
- live regex rule updates while running (blacklist/whitelist changes are applied immediately)
- merged live feed with source + text filters

Packaging and cross-platform QA hardening are still in progress.
