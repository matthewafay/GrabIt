# GrabIt

<img src="assets/icons/grabit.png" alt="GrabIt logo" width="96"/>

A lightweight Windows screenshot + annotation tool. Single `.exe`, lives in
the system tray, launches with Windows.

## What it does today

**Capture**

- **Fullscreen** — `PrintScreen` hotkey or tray → *Capture fullscreen*.
  Saves PNG to `%USERPROFILE%\Pictures\GrabIt\` and copies to clipboard.
- **Region / window** — tray → *Capture region / window…*. Drag a rectangle
  in the overlay, or hover a window (green outline) and click to grab it.
  Multi-monitor and mixed-DPI aware. Cursor is captured as a separate layer.
- **Delayed** — tray → *Capture with delay* → 3 / 5 / 10s. A countdown
  window appears, then closes before the capture fires so it never lands in
  the output.

**Annotate** — tray → *Capture & annotate…*

- **Arrow** tool: click-drag to place. Color picker + thickness slider.
- **Text** tool: click to place a single-line text annotation. Enter commits,
  Escape cancels. Color picker + text-size slider. Uses JetBrains Mono.
- **Undo** (Ctrl+Z) and **Clear**.
- **Save** (Ctrl+S) writes a PNG with annotations baked in, a `.grabit`
  sidecar that preserves the editable scene graph, and updates the clipboard.

## Build

Requires Rust 1.78+ (tested on 1.95) and Visual Studio Build Tools with the
"Desktop development with C++" workload (for the MSVC linker + `rc.exe`).

```sh
cargo build --release
```

Produces `target/release/grabit.exe`, a self-contained Windows binary around
5 MB (statically linked CRT, LTO, stripped).

## Use

Run `grabit.exe`. The logo appears in the system tray. Right-click for the
menu. Left-click does nothing yet (reserved for future quick-capture).

Toggle **Launch at startup** in the tray menu to add/remove an entry under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run\GrabIt`.

Default output folder: `%USERPROFILE%\Pictures\GrabIt\`.
Settings: `%APPDATA%\GrabIt\settings.toml`.
Logs / crash dumps: `%APPDATA%\GrabIt\logs\`.

## Architecture

```
src/
  main.rs              entry; single-instance mutex; DPI + font init;
                       event loop pumping tray, hotkeys, and commands
  app/                 AppState, command dispatch, paths, single-instance
  tray/                system-tray icon + menu
  hotkeys/             global-hotkey registration + accelerator parsing
  autostart/           HKCU Run-key read/write
  platform/            DPI, monitor enumeration, embedded font registration
  capture/
    gdi.rs             GDI BitBlt (fullscreen / region)
    window_pick.rs     PrintWindow(PW_RENDERFULLCONTENT)
    cursor.rs          GetCursorInfo + DrawIconEx → separate cursor layer
    region.rs          layered-window overlay (drag / window-hover)
    delay.rs           countdown overlay
    wgc.rs             WGC stub (activated in a later milestone)
  editor/
    app.rs             eframe App: toolbar + canvas + arrow / text tools
    document.rs        Document schema (.grabit, MessagePack)
    rasterize.rs       arrow + text baking into the saved PNG
  settings/            TOML-serialized settings + hotkey bindings
  export/              PNG write + Windows clipboard (CF_DIB)
```

Editor runs on a worker thread via `eframe::run_native` with
`EventLoopBuilderExtWindows::with_any_thread(true)` so the main-thread tray
loop stays alive for concurrent captures.

## Status

| Milestone | Status | What it delivered |
|---|---|---|
| M0 | ✅ | Scaffold, tray, hotkey, GDI fullscreen capture, PNG + clipboard, Document schema |
| M1 | ✅ | Per-monitor DPI, region/window overlay, PrintWindow window capture, countdown |
| M2 | 🔶 | eframe/egui editor skeleton (pan/zoom + crop/resize/rotate deferred) |
| M3 | 🔶 | Arrow + Text tools (callouts/shapes/step/stamps/cursor-edit pending) |
| M4 | ⏳ | Blur, cut-out, borders, magnify, capture-info |
| M5 | ⏳ | Presets, per-preset hotkeys, quick styles |
| M6 | ⏳ | Menu / object (UIAutomation) capture, multi-region composites |
| M7 | ⏳ | Templates, batch processing |
| M8 | ⏳ | Virtual printer capture (Authenticode-signed port monitor) |
| M9 | ⏳ | Installer + portable zip + auto-update |

Full plan with scope, risks, and verification lives at
`.claude/plans/` outside this repo.

## Credits

- **Logo:** pixel-art TV by the project owner.
- **Font:** [JetBrains Mono](https://www.jetbrains.com/lp/mono/) Regular &
  Bold, SIL Open Font License 1.1. License text:
  `assets/fonts/OFL.txt`.
- **Rust crates** (runtime): `windows`, `eframe` / `egui`, `tray-icon`,
  `global-hotkey`, `image`, `ab_glyph`, `winreg`, `toml`, `rmp-serde`,
  `serde`, `chrono`, `parking_lot`, `crossbeam-channel`, `anyhow`,
  `thiserror`, `log` / `env_logger`, `uuid`, `rfd`, `dirs`.
- **Rust crates** (build): `embed-resource`, `ico`, `image`.
