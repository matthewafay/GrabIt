# GrabIt

<img src="assets/icons/grabit.png" alt="GrabIt logo" width="96"/>

A lightweight Windows screenshot + annotation tool. Single `.exe`, lives in
the system tray, launches with Windows.

If GrabIt is useful to you, you can support development here:
[☕ Buy me a coffee](https://buymeacoffee.com/matthewafay).

## What's new in 1.6.8

### Removed: Stamp annotation type
- The `Stamp` annotation variant (and the `StampSource` enum, the
  `tools::stamp` module, the `assets/stamps/` PNG library, and the
  `stamps_dir` path) are gone from the codebase entirely.
- Stamps were never user-creatable in any released GrabIt — the Tool
  palette never exposed a Stamp tool — so there should be no
  `.grabit` files in the wild containing Stamp annotations.
- Document schema bumped to v5. A v4 document containing Stamp
  variants (only possible from a non-released build) now fails to
  load; v4 documents without Stamps load unchanged.

## What's new in 1.6.4

### Removed: HKCU Run-key autostart
- Writes to `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`
  were one of the loudest heuristic triggers getting the unsigned
  binary flagged as `Wacatac.c!ml` by Defender. The autostart module
  + `winreg` crate are gone.
- Settings window no longer has a "Launch at startup" toggle.
- See the **Launching at startup** section below for the recommended
  manual setup (Task Manager → Startup tab, or a shortcut in the
  Startup folder).
- If you previously had the toggle enabled, your existing registry
  entry is still there but is now just an unused row — Task Manager
  → Startup tab → right-click → Disable cleans it up.

## What's new in 1.6

### Full UI port to Dioxus desktop
- **Every window is now Dioxus.** History (1.5), Settings, GIF frame
  editor, and the annotation editor all render through Wry +
  WebView2 with HTML/CSS instead of eframe/egui. Same Rust core
  underneath — capture, recorder, encoder, document model,
  rasterize, undo/redo command pattern — only the view layer
  changed.
- **eframe / egui / winit dropped from deps.** The release binary
  is actually a touch smaller (~5.5 MB) than the eframe-only build
  (~5.7 MB) despite gaining the entire Dioxus desktop stack.
- **Consistent visual language across the app.** Dark theme, same
  button styles, same hover/active treatments, same color palette
  shared between tools.

### Annotation editor
- All 9 annotation types (Arrow, Text, Shape, Step, Magnify, Blur,
  Callout, CaptureInfo) render as SVG primitives layered
  over the screenshot's `<image>` element.
- All 10 tools work: drag-to-create for shapes / arrows / text /
  blur / magnify / callout; single-click for step + capture-info.
- Selection chrome with 8 resize handles for shapes; 2 endpoint
  handles for arrows; radius handle for step. Drag-to-move,
  drag-to-resize, all routed through `UpdateAnnotation` commands so
  undo/redo (Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z) is intact.
- Inspector grew a per-selection properties panel: edit color,
  thickness, line/head style, alignment, list, fill, etc. of the
  selected annotation in place.
- Tool-style palette is fully wired: 8-color swatches, hex picker,
  every tool's defaults bind back into the inspector.
- Document effects panel: torn edge (per-edge toggles + depth +
  teeth), border (width + matte + colour + shadow). Backed by the
  existing `SetEdgeEffect` / `SetBorder` commands.
- Save (Ctrl+S) runs `rasterize::flatten` →
  `apply_document_effects` → PNG + .grabit. Optional copy-on-save.
- Save-on-close confirmation modal when there are unsaved edits.

### Known regressions vs the eframe editor
A few features didn't make this round of the port — they round-trip
through `.grabit` correctly (so existing files are safe) but can't
be created/edited in the new editor yet:
- Curved arrows (control-point mid-handle) — straight arrows only
  for now; loaded curves render correctly.
- Quick styles (per-tool named presets).
- Cursor layer editing (move / resize / remove the captured cursor).
- Aspect-locked Resize and 90° Rotate buttons in Document effects.

These are tracked for 1.7.

## What's new in 1.5

### Capture history
- **Mini history viewer** in the tray. Right-click → *History…* opens a
  thumbnail grid of the most recent PNGs and GIFs from your output
  folder (newest first, capped at 60 items).
- **One-click copy actions per item.** *Copy* puts a PNG on the
  clipboard as `CF_DIB` (paste into Office / browsers / chat) and puts
  a GIF on as `CF_HDROP` (a file drop, so chat clients paste it as the
  actual animated file instead of a still frame). *Copy path* drops
  the absolute path on as `CF_UNICODETEXT`.
- **No persistent history file.** The grid is just a directory scan of
  the output folder, so files you delete from disk drop out of the
  list naturally. `Refresh` button rescans on demand.
- Each card shows a PNG/GIF type badge, filename, file size, and a
  relative timestamp ("3m ago"). Clicking a button flashes a brief
  "Copied!" confirmation underneath.

## What's new in 1.4

### GIF recording (ScreenToGif-style)
- **Record GIF…** is now in the tray and bound to `Ctrl+Shift+G` by
  default. Drag a region; a small floating bar (Pause / Stop / timer)
  anchors next to it; re-pressing the chord while a recording is in
  progress acts as a stop toggle. Esc / WM_CLOSE / a max-seconds guard
  also stop the recording.
- **Frame editor** opens after Stop in its own subprocess. Scroll a
  thumbnail timeline, click frames to delete, set in/out trim markers,
  hit "Trim to selection" to drop everything outside, then Export.
  Playback is FPS-paced (matches what export will produce) and a
  background decoder thread pre-loads RGBA so it stays smooth.
- **Fast export.** Encoder runs NeuQuant palette quantization on every
  CPU core via rayon at speed=10; roughly 10–25× faster than the
  default `image::GifEncoder` path on a multi-core box.
- **Small files.** Diff-bounding-box encoding (each frame after the
  first writes only the rectangle of pixels that changed, with
  `DisposalMethod::Keep` so prior pixels persist) plus
  identical-frame compaction. Typical screen recordings are 3–10×
  smaller than full-frame output.
- New "GIF" section in the Settings window: hotkey, FPS (default 15),
  loop count, max-seconds cap, and a separate "include cursor" toggle
  that's independent from the still-capture cursor setting.

### Reputation metadata
- **Rich `VS_VERSION_INFO` resource** baked into the binary —
  `CompanyName`, `FileDescription`, `LegalCopyright`, `ProductVersion`,
  etc. all populated and driven from `Cargo.toml` at build time so
  they can't drift. Helps Windows Defender's heuristic ML scanner
  treat the unsigned exe more like a real application.

### Fixes
- **Tray app no longer silently closes after a tray-triggered capture
  with a Win32 overlay.** The recorder's and countdown overlay's
  `WM_DESTROY` handlers were posting an orphan `WM_QUIT` that the
  main loop's `PeekMessageW` then drained. Removed the redundant
  `PostQuitMessage(0)` from those `WM_DESTROY` arms.

## What's new in 1.3

### Document effects
- **Photo-frame matte**. Border now has a second, inner band — set
  `Matte width` + `Matte color` and you get a classic picture-frame
  look (thin dark outer rim + ivory matte hugging the image). Set
  Matte width to 0 for the old single-band behaviour.
- **Border live preview no longer clipped.** The editor canvas now
  allocates space for the full frame (border + matte + shadow
  spread + offset), so what you see on-screen matches the exported
  PNG even at thick widths.
- **Torn edge: any combination of edges.** The single-edge dropdown
  is now four checkboxes (Top / Bottom / Left / Right). Depth and
  Tooth sliders apply to every enabled edge; jitter is salted per
  edge so corners aren't suspiciously symmetric.

### Rendering polish
- **Step circles now 4× supersampled** on export (same treatment as
  arrows). Smooth circle edge + outline at any radius.

### Tray + settings
- **Hotkey labels update in place** when you rebind and save — muda's
  `set_accelerator` + `set_text` are called on the live menu items,
  no icon flicker.
- **Click-to-record Ctrl detection fix.** Reading `i.modifiers` once
  per frame instead of per-event; the Ctrl in Ctrl+Z (etc.) no
  longer gets dropped from a race between modifier-down and key-down.
- **Reset to defaults** button in the Settings window — preview the
  reset then Save or Cancel to discard.
- **New default hotkeys:** `Ctrl+Shift+S` for Capture fullscreen,
  `Ctrl+Shift+A` for Capture & annotate. Two-modifier chords so
  nothing accidentally fires from plain typing, and no PrintScreen
  key required.

### Editor
- **Callout tool removed from the toolbar.** Existing callouts in
  old `.grabit` docs still load and render; they just can't be
  created fresh.
- **Inspector panel slimmed to the Document tab** — Presets and
  Styles tabs are gone from the right panel.

## What's new in 1.2

### Arrow — full style kit
- **Line style**: Solid / Dashed / Dotted. Dashes survive along curves
  (they're walked by arc length, not per polyline segment).
- **Head style**: filled Triangle (default) / outline Triangle / Line
  chevron / No head / Double-ended.
- **Round caps** on the shaft — every capsule segment ends with a clean
  semicircle instead of a hard cut. Visible on thick arrows, dashed
  arrows, and `Head = None`.
- **Curved arrows** — when an arrow is selected, a cyan mid-handle
  appears between the two endpoint handles. Drag it to bend the arrow
  into a quadratic bezier; drag the handle back toward the line to
  straighten. Body-drag moves the whole curve; every line/head style
  applies to curves too. The head's orientation follows the curve
  tangent at the tip.
- **4× supersampled export** — arrow rasterisation now renders into a 4×
  offscreen buffer and downsamples, giving smooth edges at every angle in
  the saved PNG and the clipboard image.
- All new style fields round-trip through the `.grabit` sidecar
  (serde defaults keep pre-1.2 arrows loading as solid filled-triangle
  straight lines).

### Text
- **Resize while editing** — eight blue-outlined handles appear around
  the text rect while it's in edit mode; drag a corner or edge to resize
  the box. Word-wrap re-runs live as you drag.
- **Double-click to re-edit** — from any tool, double-click a committed
  text annotation to jump into edit mode on it. Tool switches to Text,
  the toolbar re-seeds from the node's current size/color/align/list/
  shadow/frosted, and the existing node is updated in place on commit.
- **List cursor fix** — typing inside a bullet/numbered text box no
  longer drifts the caret. Markers are applied at render/export time
  only; the live edit layouter uses the raw buffer so cursor indices
  map 1:1 to the string.

### Editor UX
- **Two-row toolbar** — tool selection on row 1, style controls on row 2.
  Each row wraps independently, so adding a tool never shoves style
  controls off-screen on a narrow window.
- **Editor window minimum size** — a tiny capture (e.g. 1×1 px) now
  opens a ≥1000×700 window with a hard 800×550 shrink floor, so toolbar
  + inspector + status always have room.
- **No more "unsaved changes" prompt after copy-to-clipboard** — a
  successful clipboard copy clears the dirty flag; subsequent edits
  re-arm the prompt as usual.

### Settings window
- **Reorganised into sections** — Hotkeys, Capture, Arrows. Each has a
  bold heading, its own grid, and separators between groups.
- **Click-to-record hotkeys** — click the field showing the current
  chord, press the combo you want, then Confirm. Esc cancels. The
  captured chord formats as `parse_chord` expects, so validation always
  accepts it.
- **Credit footer** — `GrabIt 2026 — Matthew Fay` pinned to the bottom
  right of the Settings window.
- **Arrow snap setting removed** — Shift always snaps to 15°; the
  modifier convention made a toggle redundant. Existing
  `settings.json` files with the field are read and quietly upgraded.

### Slim tray menu
The tray menu is now: **Capture fullscreen**, **Capture & annotate…**,
**Record GIF…** *(1.4)*, **Open output folder**, **History…** *(1.5)*,
**Settings…**, **Quit GrabIt**. The three capture entries show their
bound hotkey chord on the right side via a muda Accelerator.
Launch-at-startup lives in the Settings window (Capture section).

### Hotkeys don't get swallowed by menus
- **Dedicated hotkey-drain thread** — `WM_HOTKEY` events are processed
  on a worker thread, so a modal UI on the main thread (our own tray
  popup, Windows context menus) can't delay dispatch.
- **Captures run on the worker** — `CaptureFullscreen` and
  `CaptureAndAnnotate` execute inline in the worker. Settings are
  re-read from disk per capture so include-cursor / copy-to-clipboard
  are always current.
- **Capture-first frozen overlay for Annotate** — the full virtual
  desktop is BitBlt'd *before* the region overlay shows. The overlay
  paints that captured bitmap as its opaque background (dimmed outside
  the drag rect, full-brightness inside). Windows naturally dismisses
  any open popup menu when the overlay takes focus, but because the
  overlay is showing the frozen capture, transient UI — tray menus,
  context menus, hover tooltips — is preserved in the final image.
  The selected rect is cropped out of the already-captured bitmap.

## What's new in 1.1

- **Arrow polish** — shaft now renders as a proper anti-aliased line stroke
  (no more "soft" freehand arrows at oblique angles); the head stays a clean
  triangle.
- **Color-derived drop shadow on arrows** — optional per-arrow shadow is a
  darkened tint of the arrow's own colour, so it reads on both light and
  dark backgrounds. Toggle per-arrow from the toolbar or globally in
  Settings ("Default new arrows to drop shadow").
- **Shift-drag snaps arrows to 15°** — freehand by default; hold Shift
  mid-drag to lock the angle and pixel-snap endpoints.
- **Arrow color: simple vs. advanced mode** — simple mode (default) shows
  an 8-swatch palette (red, orange, yellow, green, blue, purple, black,
  white). Advanced mode unlocks the full picker plus a `#RRGGBB` hex input
  field. Toggle under Settings → "Arrow color — advanced mode".
- **Settings now persisted as JSON** — `%APPDATA%\GrabIt\settings.json`
  replaces the old `settings.toml` (the old file still migrates in on
  first launch).
- **Live settings reload** — change a setting in the Settings window and
  any already-open editor picks up the new values immediately; no restart
  required.
- **Copy-path button** — after a successful save, a one-click "Copy path"
  button next to the "Saved to …" status copies the full PNG path to the
  clipboard for pasting into chat / Explorer / a prompt.

## Capture

- **Fullscreen** — `Ctrl+Shift+S` or tray → *Capture fullscreen*. Saves PNG
  to the output folder and copies to clipboard.
- **Region / window** — tray → *Capture region / window…*. Drag a rectangle
  in the overlay, or hover a window (green outline) and click to grab it.
  Multi-monitor and mixed-DPI aware.
- **Annotate** — `Ctrl+Shift+A` or tray → *Capture & annotate…*. Drag-release a
  rectangle, then the editor opens with it.
- **Object / menu** — tray → *Capture object…*. A UIA picker highlights the
  UI element under the cursor (button, menu item, list row); F3 commits,
  Esc cancels. Menus stay pinned via a `SetWinEventHook` while you hover.
- **Exact size** — tray → *Capture exact size…* → pick a preset
  (1920×1080, 1600×900, 1280×720, 1024×768, 800×600, 640×480, 500×500). An
  overlay lets you position the fixed-size rectangle.
- **Delayed** — tray → *Capture with delay* → 3 / 5 / 10s. A countdown
  appears and closes before the capture fires.
- **Presets** — tray → *Presets*. User-defined capture profiles bundling
  target, delay, cursor, post-action, filename template. Each can bind its
  own global hotkey.
- **Record GIF** — `Ctrl+Shift+G` or tray → *Record GIF…*. Drag a region;
  a small floating bar (Pause / Stop / timer) appears; re-pressing the
  chord stops the recording. After Stop, a frame editor opens for trim
  / per-frame delete / FPS / loop tweaks before exporting an animated
  GIF.

Every still capture saves both a PNG and a `.grabit` sidecar (the editable
scene graph) next to it; GIF recordings spool frames to
`%APPDATA%\GrabIt\gif-record\<uuid>\` and only land in the output folder
on Export.

## Editor

Tools:

- **Select** — click any annotation to select, drag body to move, drag
  any edge or corner handle to resize (edges work along their full length,
  not just the midpoint). Drag different annotations without deselecting
  first. Delete removes. Ctrl+Z / Ctrl+Y for undo/redo.
- **Arrow** — drag tail-to-tip; shaft is draggable anywhere along its
  length for move, endpoints for retargeting.
- **Text** — drag a rectangle to define the text box, type inside. Enter
  adds a newline, text word-wraps at the right edge, Esc or clicking
  outside commits. Click an existing text rect to re-edit.
  - **Frosted** — gaussian-blur the pixels behind the text box (same
    effect as the Blur tool), so text stays legible over any background.
  - **Shadow** — soft offset drop shadow behind the text box for a
    floating-card look. Pairs well with Frosted.
  - **Align** — Left / Center / Right horizontal alignment inside the box.
  - **Lists** — toggle off / bullet (`•`) / numbered (`1.`) list style;
    wrapped continuation lines hanging-indent to the text column in the
    exported PNG.
  - JetBrains Mono at the chosen size.
- **Callout** — drag a rect; produces a speech-balloon with a movable tail.
- **Rect / Ellipse** — drag a shape with stroke + optional translucent fill.
- **Step** — click to place numbered markers; auto-increment per document.
- **Magnify** — drag over the region you want zoomed; a 3× loupe callout
  appears next to it. Circular toggle. Live preview shows the real zoom.
- **Blur** — drag a region. Non-destructive in `.grabit`, destructive on
  PNG export. Live preview shows the real gaussian.
- **Capture-info stamp** — pin a banner with timestamp / window title /
  process / OS version / monitor info. Real metadata, live preview.

Document-level effects (right inspector panel):

- **Torn edge** — jagged cut on any of the four edges. Live preview.
- **Border + drop shadow** — solid frame with optional soft shadow. Live
  preview.
- **Resize** — aspect-locked width/height inputs; Reset to base size.
- **Rotate** — 90° CW / CCW buttons; Shift+R shortcut.

Undo/redo via command pattern (Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y), bounded at
200 entries. Ctrl+S saves. Closing with unsaved edits prompts
Save / Discard / Cancel.

Quick styles (inspector → Styles tab): save the active tool's settings
as a named style and reapply later. Styles persist at
`%APPDATA%\GrabIt\styles.toml`.

## Settings

Tray → *Settings…* opens a GUI window grouped into three sections:

- **Hotkeys**
  - Fullscreen capture (default `Ctrl+Shift+S`)
  - Annotate (default `Ctrl+Shift+A`)

  Click a field and press the combo you want, then Confirm. Esc cancels.
- **Capture**
  - Include cursor in captures
  - Copy every capture to clipboard
  - Output folder (default `%USERPROFILE%\Pictures\GrabIt`, with Browse / Reset)
- **Arrows**
  - Default new arrows to drop shadow
  - Arrow color — advanced mode (picker + hex)

  *Tip: hold Shift while dragging an arrow to snap to 15°.*
- **GIF**
  - Hotkey (default `Ctrl+Shift+G`) — same click-to-record flow
  - Frames per second (5–60, default 15)
  - Loop count (0 = infinite, default 0)
  - Max recording length in seconds (default 30)
  - Include cursor in GIF frames (separate from the still-capture
    cursor toggle, so screencasts can show the cursor without
    affecting your stills)

Save writes `%APPDATA%\GrabIt\settings.json` and signals the tray to
re-register hotkeys without restart. Any open editor also live-reloads
its arrow/shadow flags.

> Global hotkeys win over focused apps — whatever chord you bind is
> intercepted everywhere. The defaults use two modifiers (`Ctrl+Shift`)
> to minimise collisions; a single-modifier chord like `Shift+Z` would
> also block Shift+Z from reaching the focused window. Rebind in
> Settings if that's what you want.

## Setup

### Rust toolchain

Install Rust via [`rustup`](https://rustup.rs/). On Windows open PowerShell
and run:

```powershell
# Installer prompt — accept the defaults (it picks the MSVC toolchain).
winget install Rustlang.Rustup
# or download + run: https://win.rustup.rs/x86_64
```

Then add the stable toolchain and make sure it targets MSVC:

```powershell
rustup install stable
rustup default stable-x86_64-pc-windows-msvc
```

Verify: `cargo --version` should print `cargo 1.78` or newer.

### Visual Studio Build Tools

GrabIt links against the Windows SDK and the MSVC CRT. Install the
"Desktop development with C++" workload via Visual Studio Installer (the
free **Build Tools for Visual Studio** SKU is enough). The Rust installer
offers to do this for you on first run — accept if prompted.

### Build

From the repo root:

```powershell
cargo build --release
```

Produces `target\release\grabit.exe`, a self-contained Windows binary
around 5 MB (statically linked CRT, LTO, stripped). Run it directly; no
installer is required.

If a build fails with `error: failed to remove file … Access is denied`,
a previous `grabit.exe` is still running. Quit it from the system tray
(and close any open editor / settings windows — each is a subprocess of
the same `.exe`) and rerun.

## Use

Run `grabit.exe`. The logo appears in the system tray. Right-click for the
menu.

Output folder: configurable in Settings (default `%USERPROFILE%\Pictures\GrabIt`).
Settings / presets / styles / logs: `%APPDATA%\GrabIt\`.

### Use from Claude Code (headless `--capture` CLI)

GrabIt also exposes a scriptable `--capture` mode so Claude Code (and any
other automation) can take screenshots and record GIFs without touching
the GUI, then drop the files straight into a repo's docs.

```powershell
# Screenshot a window by process
grabit.exe --capture screenshot --target window --process code.exe `
    --out ./docs/screenshots/editor.png

# Record a 5-second GIF
grabit.exe --capture gif --target window --process code.exe `
    --duration 5 --out ./docs/screenshots/demo.gif
```

The absolute output path is printed to stdout; diagnostics go to stderr;
exit `0` on success / `1` on failure. The CLI runs as a one-shot
subprocess that bypasses the single-instance mutex, so it coexists with
the resident tray app.

See [`CLAUDE.md`](./CLAUDE.md) for the discovery doc Claude reads
automatically, or [`docs/CAPTURE-CLI.md`](./docs/CAPTURE-CLI.md) for the
full flag matrix, schemas, and embedding workflow.

### Launching at startup

GrabIt no longer manages its own startup entry — registry writes to
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run` were one of the
heuristic triggers getting the unsigned binary flagged as
`Wacatac.c!ml`. To run on login, set it up yourself one of these ways:

- **Task Manager → Startup tab → enable a manual entry**, or
- **`Win+R` → `shell:startup` → drop a shortcut to `grabit.exe`** (the
  Startup folder is read on login and runs every shortcut inside).

If you previously toggled "Launch at startup" in an older GrabIt, the
old `GrabIt` entry under that registry key still exists but is harmless
— just an unused row pointing at the exe. Task Manager → Startup tab
→ right-click the row → Disable removes it from the boot path.

## Architecture

```
src/
  main.rs              entry + subprocess dispatch (--editor / --settings /
                       --gif-editor / --history); single-instance mutex;
                       DPI + font init; event loop
  app/                 AppState, command dispatch, paths
  tray/                tray icon + menu (capture / GIF / history / settings)
  hotkeys/             global-hotkey registration, chord parser, runtime rebind
  platform/            DPI, monitor enumeration, JetBrains Mono font
                       registration, tao window-icon loader
  history.rs           thumbnail-grid history viewer (--history subprocess)
  capture/
    gdi.rs             GDI BitBlt (fullscreen / region)
    window_pick.rs     PrintWindow(PW_RENDERFULLCONTENT)
    cursor.rs          GetCursorInfo + DrawIconEx → separate cursor layer
    region.rs          layered-window overlay (drag / window-hover)
    exact_dims.rs      fixed-size positioner overlay
    object_pick.rs     IUIAutomation element picker + WinEventHook menu pin
    delay.rs           countdown overlay
    wgc.rs             Windows.Graphics.Capture hooks (GDI fallback active)
    gif_record.rs      GIF recorder: region pick + Win32 floating bar
                       + WM_TIMER frame loop + spool dir + sidecar JSON
  editor/
    dx_app.rs          Dioxus annotation editor: SVG canvas, tool
                       palette, inspector. Drives the --editor subprocess.
    document.rs        Document schema (.grabit, MessagePack)
    commands.rs        command-pattern undo/redo history (bounded 200)
    rasterize.rs       flatten annotations + effects onto PNG export
    gif_app.rs         Dioxus GIF frame editor (--gif-editor subprocess)
    tools/             tool enum + tool-specific helpers
  presets/             per-preset .toml records + hotkey rebinding
  styles/              named quick-style presets per tool
  settings/
    mod.rs             settings.json load/save (legacy settings.toml auto-migrates)
    ui.rs              eframe GUI for the --settings subprocess
  export/
    mod.rs             PNG write + Windows clipboard
                       (CF_DIB / CF_HDROP / CF_UNICODETEXT)
    gif.rs             rayon-parallel decode + NeuQuant + diff-bbox writer
```

Each editor / settings / GIF-editor / history window runs as its own
`grabit.exe` subprocess (`--editor <sidecar>`, `--settings`,
`--gif-editor <sidecar>`, or `--history`). The original constraint
was winit refusing to recreate its event loop in one process; the
Dioxus desktop ports (1.5 + 1.6) inherit the same per-window
subprocess pattern because Wry/WebView2 has the same restriction.
Subprocesses communicate with the tray via marker files under
`%APPDATA%\GrabIt\` (one-shot reloads for settings, presets, and
triggered preset captures).

## Credits

- **Logo:** pixel-art TV by the project owner.
- **Font:** [JetBrains Mono](https://www.jetbrains.com/lp/mono/) Regular &
  Bold, SIL Open Font License 1.1. License text: `assets/fonts/OFL.txt`.
- **Rust crates** (runtime): `windows`, `dioxus` (desktop, Wry +
  WebView2), `tray-icon`, `global-hotkey`, `image`, `gif`,
  `imageproc`, `fast_image_resize`, `rayon`, `tokio` (time only),
  `base64`, `ab_glyph`, `toml`, `serde_json`, `rmp-serde`, `serde`,
  `chrono`, `parking_lot`, `crossbeam-channel`, `anyhow`,
  `thiserror`, `log` / `env_logger`, `uuid`, `rfd`, `dirs`.
- **Rust crates** (build): `embed-resource`, `ico`, `image`.
