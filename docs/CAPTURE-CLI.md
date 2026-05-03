# `grabit.exe --capture` — headless CLI reference

A scriptable surface for taking screenshots and recording GIFs without any
GUI interaction. Designed primarily for Claude Code (`grabit.exe` called via
the Bash tool, output path read from stdout, then inserted into markdown),
but useful for any script-driven consumer.

The short version is in [`../CLAUDE.md`](../CLAUDE.md). This file is the
full reference.

## Verbs

```text
grabit.exe --capture screenshot ...
grabit.exe --capture gif ...
grabit.exe --capture list-windows [--process <name>]
grabit.exe --capture help
```

## Output contract

Every verb that produces a file:

- **stdout**: a single line — the absolute path of the file, terminated
  with `\n`. Nothing else.
- **stderr**: diagnostic notes (e.g. multi-window pick disambiguation,
  warnings about dropped frames). Never required to interpret success.
- **exit code**: `0` on success, `1` on any failure.

`list-windows` writes a JSON array on stdout (still single-line).

`help` writes the usage banner on stdout. Exit `0`.

The `\\?\` Windows extended-length prefix is stripped from stdout paths so
they paste cleanly into markdown links.

## `screenshot`

Single PNG capture.

### Common flags

| Flag | Required | Description |
|---|---|---|
| `--target <kind>` | yes | One of `fullscreen`, `window`, `region`. |
| `--out <path>` | yes | Destination PNG. Parent dir auto-created. |
| `--include-cursor` | no | Composite the cursor into the output. |
| `--delay-ms <N>` | no | Sleep N ms before capturing. Default 0. |

### Per-target flags

- `--target fullscreen` — virtual desktop across all monitors.
- `--target window --process <name>` — see [Window targeting](#window-targeting).
- `--target region --x <X> --y <Y> --w <W> --h <H>` — explicit screen rect.

### Examples

```powershell
# Whole desktop
grabit.exe --capture screenshot --target fullscreen `
    --out ./shot.png

# A specific app window (by process)
grabit.exe --capture screenshot --target window --process code.exe `
    --out ./docs/screenshots/editor.png

# Explicit region
grabit.exe --capture screenshot --target region --x 100 --y 100 --w 800 --h 600 `
    --out ./crop.png

# With cursor + 500 ms delay (so you can hover something)
grabit.exe --capture screenshot --target window --process firefox.exe `
    --include-cursor --delay-ms 500 --out ./hover.png
```

## `gif`

Fire-and-wait recording. The CLI blocks for `--duration` seconds while
spooling frames, then encodes them through GrabIt's parallel GIF pipeline
(diff bbox + per-frame NeuQuant on every CPU core) and exits.

### Flags

| Flag | Required | Description |
|---|---|---|
| `--target <kind>` | yes | `fullscreen`, `window`, or `region`. |
| `--duration <secs>` | yes | Recording length, integer seconds. Min 1. |
| `--out <path>` | yes | Destination GIF. Parent dir auto-created. |
| `--fps <N>` | no | Override the FPS (clamped to 5..=60). Defaults to `gif_fps` from `settings.json` (15 out of the box). |
| `--include-cursor` | no | Composite the cursor into each frame. |

Per-target flags are identical to `screenshot`.

### Examples

```powershell
# Demo a window for 8 seconds at default FPS
grabit.exe --capture gif --target window --process code.exe `
    --duration 8 --out ./docs/screenshots/edit-demo.gif

# Region recording at 30 fps
grabit.exe --capture gif --target region --x 200 --y 200 --w 640 --h 480 `
    --duration 5 --fps 30 --out ./terminal.gif

# Whole-desktop capture
grabit.exe --capture gif --target fullscreen `
    --duration 4 --out ./ui-tour.gif
```

### Performance / size notes

- Higher `--fps` makes the recording smoother but produces larger files
  and more encoder work. 15 is fine for UI demos; 30+ is overkill unless
  you're capturing animation.
- The encoder runs in parallel across all CPU cores; encode time is
  typically a small fraction of the recording duration.
- Identical-frame compaction means a 5-second recording of a static UI
  encodes nearly as small as a single PNG.

## `list-windows`

Print every visible top-level window as a JSON array on stdout.

```powershell
grabit.exe --capture list-windows
grabit.exe --capture list-windows --process chrome.exe
```

### Schema

```json
[
  {
    "hwnd": 526944,
    "pid": 34408,
    "process": "WindowsTerminal.exe",
    "title": "PowerShell",
    "rect": { "x": 130, "y": 60, "width": 1752, "height": 996 }
  }
]
```

`hwnd` is a decimal integer (Windows handle, valid for the lifetime of
the window). `process` is the exe basename. `title` is the window title
(may be empty). `rect` is in physical screen pixels.

Cloaked windows (UWP-suspended, virtual-desktop-hidden, helper surfaces)
and minimized windows are filtered out — `list-windows` returns only
captureable surfaces.

## Window targeting

`--target window --process <name>` chooses a single HWND from every visible
top-level window owned by a process matching `<name>`:

- Match is **case-insensitive**.
- The `.exe` suffix is **optional** — `--process code` and `--process code.exe`
  both match `code.exe`.
- **0 matches** → exit `1` with the message
  `no top-level windows found for process "<name>"`. No file written.
- **1 match** → captured directly.
- **N matches** (N > 1) → the **largest visible window** wins
  (`width × height`), tied broken by HWND for determinism. The chosen
  window's HWND, dimensions, and title are noted on stderr; runners-up
  are listed below it. Disambiguate with `list-windows` and switch to
  `--target region` if the wrong one is picked.

Minimized windows are filtered out at enumeration, so a minimized Notepad
will produce "no top-level windows found" rather than a bad capture.

## Edge cases

- **Process not running** → exit 1, clear stderr message, no file.
- **Window minimized** → filtered out; treated as not running.
- **Output path's parent dir doesn't exist** → auto-created
  (`std::fs::create_dir_all` semantics).
- **HiDPI / multi-monitor** → coordinates are physical pixels in the
  virtual-desktop space. `list-windows` returns physical-pixel rects.
- **Tray app already running** → coexists fine. The CLI bypasses the
  single-instance mutex.
- **Choppy GIF** → lower `--fps`, or capture a smaller region. The
  recorder uses GDI `BitBlt` per frame; very large rects at 60 fps can
  saturate it.
- **Output path inside a directory you can't write to** → exit 1 with
  the underlying I/O error on stderr.

## Embedding into GitHub markdown

GitHub renders relative repo paths natively (both on github.com and in PR
previews), so the simplest workflow is:

1. Pick where the file should live in the repo (`docs/screenshots/`,
   `assets/`, etc.).
2. Run the CLI with `--out` pointing there.
3. Read the absolute path from stdout (you'll need it to confirm the
   write succeeded; for the markdown link itself, use a relative path).
4. Write the markdown image with a relative path:

   ```markdown
   ![editor](./docs/screenshots/editor.png)
   ```

   Or, for a GIF:

   ```markdown
   ![demo](./docs/screenshots/demo.gif)
   ```

No clipboard, no upload, no CDN. The PNG and GIF formats both render
inline in markdown rendered by GitHub.

### Worked example for Claude

You're working in `C:\code\someproject` and want to add a screenshot of
VS Code to the README.

```powershell
# Take the screenshot.
C:\code\GrabIt\target\release\grabit.exe `
    --capture screenshot --target window --process code.exe `
    --out ./docs/screenshots/editor.png

# Stdout: C:\code\someproject\docs\screenshots\editor.png

# Then edit README.md, inserting:
# ![VS Code editor](./docs/screenshots/editor.png)
```

## Logging

The CLI writes to the same log file the tray app uses:
`%APPDATA%\GrabIt\logs\grabit.log`. Each invocation logs a
`capture subprocess: ["screenshot", ...]` line plus per-stage progress.
Useful when something goes wrong and the stderr message isn't enough.

## What this CLI doesn't do

- **No annotation from the CLI.** Arrows, blur, callouts, text — those
  are editor-only. If you need an annotated screenshot, capture with
  the CLI then open the resulting `.png` (and its sibling `.grabit`
  sidecar) in the editor.
- **No async start/stop GIFs.** GIF recording is fire-and-wait by
  duration. If you need to interact with an app *while* recording,
  pre-script the interaction or use the tray-driven recorder instead.
- **No MCP wrapper.** This is a plain CLI. Claude Code calls it via
  Bash; the stdout contract is the API.
- **No filename templating or default output dir.** `--out` is required;
  you choose exactly where the file lands.
