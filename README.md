# GrabIt

<img src="assets/icons/grabit.png" alt="GrabIt logo" width="96"/>

A lightweight Windows screenshot + annotation tool. Single `.exe`, lives in
the system tray, launches with Windows.

## Capture

- **Fullscreen** — `PrintScreen` or tray → *Capture fullscreen*. Saves PNG
  to the output folder and copies to clipboard.
- **Region / window** — tray → *Capture region / window…*. Drag a rectangle
  in the overlay, or hover a window (green outline) and click to grab it.
  Multi-monitor and mixed-DPI aware.
- **Annotate** — `Ctrl+X` or tray → *Capture & annotate…*. Drag-release a
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

Every capture saves both a PNG and a `.grabit` sidecar (the editable scene
graph) next to it.

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

Tray → *Settings…* opens a GUI window with:

- Fullscreen hotkey chord (default `PrintScreen`)
- Annotate hotkey chord (default `Ctrl+X`)
- Launch at startup
- Include cursor in captures
- Copy every capture to clipboard
- Output folder (default `%USERPROFILE%\Pictures\GrabIt`, with Browse / Reset)

Save writes `%APPDATA%\GrabIt\settings.toml` and signals the tray to
re-register hotkeys and re-sync autostart without restart. Chord parsing
accepts `Ctrl+Shift+Z`, `Alt+PrtSc`, `Win+S`, etc.

> Global hotkeys win over focused apps — while the annotate hotkey is
> `Ctrl+X` it intercepts Cut everywhere. Pick something unique (e.g.
> `Ctrl+Shift+X`) if that bothers you.

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
menu. Toggle **Launch at startup** in the tray to add/remove the
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run\GrabIt` entry.

Output folder: configurable in Settings (default `%USERPROFILE%\Pictures\GrabIt`).
Settings / presets / styles / logs: `%APPDATA%\GrabIt\`.

## Architecture

```
src/
  main.rs              entry + subprocess dispatch (--editor, --settings);
                       single-instance mutex; DPI + font init; event loop
  app/                 AppState, command dispatch, paths
  tray/                tray icon + menu (capture / presets / settings / quit)
  hotkeys/             global-hotkey registration, chord parser, runtime rebind
  autostart/           HKCU Run-key read/write
  platform/            DPI, monitor enumeration, JetBrains Mono font registration
  capture/
    gdi.rs             GDI BitBlt (fullscreen / region)
    window_pick.rs     PrintWindow(PW_RENDERFULLCONTENT)
    cursor.rs          GetCursorInfo + DrawIconEx → separate cursor layer
    region.rs          layered-window overlay (drag / window-hover)
    exact_dims.rs      fixed-size positioner overlay
    object_pick.rs     IUIAutomation element picker + WinEventHook menu pin
    delay.rs           countdown overlay
    wgc.rs             Windows.Graphics.Capture hooks (GDI fallback active)
  editor/
    app.rs             eframe App: toolbar + canvas + tool palette
    document.rs        Document schema (.grabit, MessagePack)
    commands.rs        command-pattern undo/redo history (bounded 200)
    rasterize.rs       flatten annotations + effects onto PNG export
    tools/             one module per tool
  presets/             per-preset .toml records + hotkey rebinding
  styles/              named quick-style presets per tool
  settings/
    mod.rs             settings.toml load/save
    ui.rs              eframe GUI for the --settings subprocess
  export/              PNG write + Windows clipboard (CF_DIB)
```

Each editor and settings window runs as its own `grabit.exe` subprocess
(`--editor <sidecar>` or `--settings`) because winit 0.30 refuses to
recreate its event loop inside one process — the tray would deadlock after
the first editor close otherwise. Subprocesses communicate with the tray
via marker files under `%APPDATA%\GrabIt\` (one-shot reloads for settings,
presets, and triggered preset captures).

## Credits

- **Logo:** pixel-art TV by the project owner.
- **Font:** [JetBrains Mono](https://www.jetbrains.com/lp/mono/) Regular &
  Bold, SIL Open Font License 1.1. License text: `assets/fonts/OFL.txt`.
- **Rust crates** (runtime): `windows`, `eframe` / `egui`, `tray-icon`,
  `global-hotkey`, `image`, `imageproc`, `fast_image_resize`, `ab_glyph`,
  `winreg`, `toml`, `rmp-serde`, `serde`, `chrono`, `parking_lot`,
  `crossbeam-channel`, `anyhow`, `thiserror`, `log` / `env_logger`,
  `uuid`, `rfd`, `dirs`.
- **Rust crates** (build): `embed-resource`, `ico`, `image`.
