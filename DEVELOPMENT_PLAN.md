# rustylogviewer Development Plan

## 1. Product Goals

- Build a lightweight Rust terminal app to watch multiple text files (primarily logs).
- Poll tracked files every 1-2 seconds (configurable).
- Show newly appended lines from all tracked files in a single compact view.
- Keep CPU/memory overhead low for developer VMs.
- Minimize UI noise while preserving important context (file source + timestamp).

## 2. Non-Goals (v1)

- Full-text indexing/search engine.
- Historical log storage/database.
- Complex dashboards or charts.
- Remote log ingestion (SSH, syslog, etc.).

## 3. Proposed Tech Stack

- Language: Rust stable.
- CLI parsing: `clap`.
- TUI framework: `ratatui` + `crossterm`.
- Config format: `TOML` (`serde`, `toml`).
- Error handling: `anyhow` + `thiserror`.
- Time formatting: `time`.
- Logging/tracing (internal): `tracing` (optional in v1, useful for debug mode).

## 4. High-Level Architecture

1. `Config` layer
- Load config file + CLI overrides.
- Validate paths, polling interval, and line buffer limits.

2. `FileWatcher` layer (poll-based)
- Track file metadata (`inode`/file id, size, last read offset).
- On each tick, detect append/truncate/rotation cases.
- Read only newly appended bytes and split into lines.

3. `Event` pipeline
- Convert new lines into normalized events:
  - `source_file`
  - `capture_time`
  - `line_text`
  - optional `line_number`/offset metadata

4. `Aggregation` layer
- Merge events from all watchers into one bounded ring buffer.
- Keep a compact in-memory recent window (e.g., last 5k-50k lines configurable).

5. `UI` layer
- Render a single unified scrolling feed.
- Show concise per-line prefix (time + short filename).
- Optional filters and pause mode.

## 5. Data Model Sketch

```rust
struct AppConfig {
    poll_interval_ms: u64,      // default 1000
    tracked_files: Vec<PathBuf>,
    max_buffer_lines: usize,    // default 10000
    max_line_len: usize,        // truncate display if needed
    show_timestamps: bool,
}

struct TrackedFileState {
    path: PathBuf,
    file_id: Option<FileIdentity>, // inode/dev on unix, equivalent on windows
    read_offset: u64,
    last_size: u64,
}

struct LogEvent {
    ts: SystemTime,
    source: String, // compact file label
    line: String,
}
```

## 6. File Tracking Strategy (Performance + Correctness)

- Poll every `poll_interval_ms` (default `1000`, allow `2000`).
- For each file:
  - `size > read_offset`: read appended bytes only.
  - `size < read_offset`: treat as truncate/rotation; reset offset and continue.
  - file identity changed: reopen and reset tracking intelligently.
- Use buffered I/O (`BufReader`) and avoid loading entire files.
- Maintain bounded buffers to prevent memory growth.
- Keep display updates incremental, not full expensive recomputation.

## 7. UI/UX Plan (Simple, Clean, Compact)

Primary screen (single view):
- Header row: app name, poll interval, tracked file count, dropped-line counters (if any).
- Main pane: unified recent lines across all files.
- Each line format:
  - `[HH:MM:SS] [short-file] message`
- Compact defaults:
  - minimal borders
  - no excessive color
  - subtle source coloring per file (optional)

Key controls (v1):
- `q`: quit
- `p`: pause/resume stream
- `j`/`k` or arrows: scroll
- `g`/`G`: jump top/bottom of retained buffer
- `f`: cycle source-file filter
- `/`: simple text filter (optional for late v1)

Noise-reduction defaults:
- No popups/toasts during normal updates.
- Truncate very long lines in view (full line accessible in future detail pane).
- Stable ordering by arrival time.

## 8. CLI + Config Design

Example CLI:

```bash
rustylogviewer \
  --config ./rustylogviewer.toml \
  --poll-ms 1000 \
  /var/log/app.log /tmp/dev.log
```

Example config (`rustylogviewer.toml`):

```toml
poll_interval_ms = 1000
max_buffer_lines = 10000
max_line_len = 512
show_timestamps = true

tracked_files = [
  "/var/log/app.log",
  "/tmp/dev.log",
]
```

Priority rules:
- CLI args override config file.
- If no files configured, fail with clear actionable error.

## 9. Milestone Plan

### Milestone 0: Project Bootstrap
- Initialize Cargo project.
- Add dependencies and module structure.
- Basic CLI parsing and config load.

Deliverable:
- App starts, validates input, prints effective config.

### Milestone 1: Polling Engine
- Implement tracked-file state and append-only reads.
- Handle truncation and basic rotation safely.
- Emit `LogEvent`s for new lines.

Deliverable:
- Headless mode that prints merged events to stdout.

### Milestone 2: Unified TUI Feed
- Integrate `ratatui` event/render loop.
- Show single compact merged feed with source + time.
- Add scroll and pause controls.

Deliverable:
- Usable terminal viewer for multiple files.

### Milestone 3: Performance/Noise Hardening
- Bounded ring buffer.
- Backpressure strategy when append rate spikes.
- Reduce redraw costs and avoid high CPU at idle.
- Add lightweight counters/metrics in status bar.

Deliverable:
- Stable behavior on busy logs and low-resource VMs.

### Milestone 4: Quality + Packaging
- Unit tests for parser/state transitions.
- Integration tests using temp files (append/truncate/rotate scenarios).
- `README` with usage and examples.

Deliverable:
- v0.1 release candidate.

## 10. Testing Strategy

Unit tests:
- Config parsing and CLI precedence.
- State transitions (`append`, `truncate`, `rotate`).

Integration tests:
- Multi-file append interleaving.
- Large line handling and truncation behavior.
- Poll interval behavior and event ordering.

Manual smoke checks:
- Run on low-spec VM and confirm low idle CPU.
- Tail rapidly changing logs and verify no UI stutter.

## 11. Performance Targets (Initial)

- Idle CPU: near 0-1% on typical dev VM with moderate tracked files.
- Memory: bounded and predictable via `max_buffer_lines`.
- Update latency: usually <= poll interval.

## 12. Risks and Mitigations

- Log rotation edge cases across platforms:
  - Mitigate with file identity checks + integration tests.
- Very high log throughput causing dropped updates:
  - Mitigate with bounded buffers + visible dropped counters.
- TUI redraw overhead:
  - Mitigate with incremental rendering and compact widgets.

## 13. Immediate Next Implementation Steps

1. Scaffold Cargo project and module skeleton.
2. Implement config/CLI loading with sane defaults.
3. Build polling watcher with append/truncate handling and tests.
4. Add basic merged-feed TUI and keybindings.
5. Tune performance and finalize README for v0.1.

## 14. Current State (Completed)

- Core polling engine implemented with append/truncate/replace handling.
- TUI implemented with merged feed, pause/scroll, source filter, and text filter.
- Regex blacklist/whitelist rules implemented with whitelist precedence.
- Headless mode implemented.
- Unit and integration tests in place.
- GUI scaffold implemented (`--gui`) with:
  - desktop shell layout
  - open/new/save/save-as config actions
  - simple config editor panels
  - live merged feed start/stop/pause baseline

## 15. GUI Plan (Windows + Linux)

### 15.1 GUI Goals

- Provide a graphical desktop UI that works on Windows and Linux.
- Keep the single merged log feed experience from TUI.
- Let users open/select a config file from inside the app.
- Let users create and edit config files in-app with a simple form UI.
- Keep resource usage reasonable for dev VMs.

### 15.2 Proposed GUI Stack

- GUI framework: `egui` via `eframe` (cross-platform native desktop).
- Native file dialogs: `rfd` (open/save config, add tracked log files).
- Config serialization: keep existing TOML format (`serde` + `toml`).

Rationale:
- `eframe/egui` is fast to ship, cross-platform, and integrates well with existing Rust core logic.

### 15.3 Architecture for Dual Frontends

1. Keep core behavior in library modules:
- config loading/validation
- watcher/polling engine
- regex line rules

2. Frontends:
- Existing TUI frontend stays available.
- New GUI frontend added as another runtime path.

3. Shared engine contract:
- GUI and TUI both consume a shared event stream and filtering behavior.

### 15.4 GUI Information Architecture

Main window layout:
- Top toolbar:
  - `Open Config`
  - `Save`
  - `Save As`
  - `Start/Stop`
  - `Pause/Resume`
- Left sidebar:
  - tracked file list
  - source filter controls
  - quick counters
- Main panel:
  - merged log feed table/list
  - compact line format with timestamp + source + message
- Bottom status bar:
  - polling interval
  - total seen
  - suppressed by rules
  - retained buffer

### 15.5 Config File Workflow (In-App)

Supported workflow:
- Open an existing TOML config from file picker.
- Create new config from defaults.
- Edit config in form-based panels.
- Save back to same file or Save As new file.

Form sections:
- General settings:
  - poll interval
  - max buffer lines
  - max line length
  - timestamp display
  - case-insensitive text filtering
- Tracked files:
  - add via file picker
  - remove selected
  - reorder optional (later)
- Rule lists:
  - blacklist regex entries
  - whitelist regex entries
  - inline regex validation errors

Validation UX:
- Invalid regex or invalid numeric settings shown inline.
- Disable `Start` when effective config is invalid.

### 15.6 Config Storage Decision

- Continue using external TOML config files as source of truth.
- App edits the selected TOML file directly on save.
- Add lightweight app state file later (optional) for:
  - last opened config path
  - recent config files list
  - window/layout preferences

### 15.7 New Milestones

### Milestone 5: GUI Bootstrap
- Add `eframe` app shell and run loop.
- Add entrypoint flag to start GUI mode.
- Render placeholder panes and status.

Deliverable:
- GUI app opens on Windows/Linux and shows static layout.

### Milestone 6: Live Feed in GUI
- Connect watcher engine to GUI update loop.
- Display merged live feed.
- Add pause/resume and source filter.

Deliverable:
- GUI shows live log updates in one view.

### Milestone 7: Config File Open/Save
- Add `Open Config`, `Save`, `Save As`.
- Load/validate TOML from selected path.
- Reflect loaded config in runtime behavior.

Deliverable:
- Users can select config file in app and run viewer with it.

### Milestone 8: In-App Config Editor
- Add form UI for all core config fields.
- Add file list editor and regex list editor.
- Add inline validation and dirty-state tracking.

Deliverable:
- Users can edit and save configs entirely in app.

### Milestone 9: Cross-Platform Packaging + QA
- Windows and Linux smoke tests.
- Basic packaging instructions.
- Performance pass for idle/update behavior.

Deliverable:
- Usable GUI release candidate for both platforms.

## 16. GUI Acceptance Criteria

- App launches and functions on Windows and Linux.
- User can open an existing config file from GUI.
- User can edit tracked files and regex rules via UI and save.
- Invalid regex is clearly surfaced before starting stream.
- Merged feed remains compact and responsive during live updates.
