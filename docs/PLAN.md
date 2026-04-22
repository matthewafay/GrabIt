# GrabIt — Lightweight Windows Screenshot Tool

## Context

Greenfield project at `C:\code\GrabIt` (currently empty). The user has a feature
list at `C:\Users\mfay\GrabIt.csv` describing a Snagit-class screenshot and
annotation tool for Windows. Goal: build a lightweight, single-`.exe` tray app
that auto-launches with Windows, listens for a global hotkey, captures the
screen, and offers a full editor for annotation, effects, batch processing, and
even virtual-printer capture.

The work is far too large for a single push — it will be executed across **ten
milestones (M0–M9)**, each independently shippable. This plan locks in the
architecture and the milestone order so implementation work can start at M0
without re-litigating design.

## Locked decisions

- **Language/stack**: Rust + `windows-rs` (Win32/WinRT) + `egui` via `eframe`
  (canvas-heavy editor). Chosen for tiny self-contained `.exe`, instant start,
  and direct Win32 access.
- **Distribution**: single `.exe`, release profile with `lto = "fat"`,
  `codegen-units = 1`, `strip`, `panic = "abort"`, `opt-level = "z"`, and
  `-C target-feature=+crt-static`. Expected <10 MB.
- **Minimum OS**: Windows 10 1903+ and Windows 11. `Windows.Graphics.Capture`
  (WGC) is the primary capture API; GDI `BitBlt` is a thin fallback only.
- **Runtime model**: tray app auto-started via
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\GrabIt`. Global hotkey
  fires capture. Editor is a separate window spawned on demand.
- **Scope**: all features from `GrabIt.csv` except **#15 Spotlight** (Mac-only
  in source) and **#7 Time-lapse capture** (user dropped). Plus one addition
  outside the CSV: **copy to clipboard on every capture**. Final feature count:
  **23 shipping features**.
- **Printer capture (#9)** must ship with v1.0 — requires Authenticode cert.

## Architecture

### Capture pipeline

- `Windows.Graphics.Capture` via `windows-rs` (WGC). `IsCursorCaptureEnabled =
  false` so we capture the cursor separately.
- Cursor captured via `GetCursorInfo` + `DrawIconEx` into its own RGBA layer in
  `CaptureResult`. This makes feature #2 "Edit cursor" mechanical instead of
  retrofit.
- Region-selector overlay uses **raw Win32 layered windows** (one per virtual-
  screen rect), not egui — egui/eframe does not handle
  `WS_EX_LAYERED | WS_EX_TRANSPARENT` + click-through + per-monitor DPI well.
- Object/menu capture (feature #5) uses `IUIAutomation` COM +
  `SetWinEventHook(EVENT_SYSTEM_MENUPOPUPSTART)` to keep menus visible during
  capture.
- Every successful capture populates `CaptureMetadata { timestamp,
  foreground_window_title, process_name, os_version, monitor_info }` at M1 —
  feature #17 (capture-info stamp) consumes this in M4 without re-plumbing.
- Every successful capture also places the PNG on the Windows clipboard (user-
  requested extra, implemented in M0 alongside disk save).

### Editor document model

- Serializable scene graph:
  `Document { base_image, cursor_layer: Option<CursorLayer>,
  annotations: Vec<AnnotationNode>, metadata }`.
- Persisted as `.grabit` using MessagePack (`rmp-serde`) — binary image data
  rules out JSON/TOML.
- Undo/redo via command pattern, not full snapshots — bounded memory on 4K
  captures with many annotations.
- Headless pipeline `batch::apply_recipe(input, recipe) -> Image` carved out in
  M2 and reused by the editor internally so M7 batch is integration, not
  rewrite.
- Multi-capture `Composition` type shared between M6 multi-region and M7
  templates.

### Process & threading

- Single instance via named mutex + `WM_COPYDATA` relay.
- Main thread: tray + hotkeys + capture dispatch. No async runtime (no network
  I/O in scope); `std::thread` + `crossbeam-channel` for pipeline back-pressure.
- Editor/settings: `eframe::run_native` on a worker thread, launched on demand.
  Cold start of the tray stays <200ms because egui does not initialize until a
  capture is produced.

### Storage

- `%APPDATA%\GrabIt\settings.toml` — TOML, human-editable.
- `%APPDATA%\GrabIt\presets\*.toml` — one file per preset.
- `%APPDATA%\GrabIt\stamps\` — user stamps; built-ins embedded via
  `include_bytes!`.
- `%APPDATA%\GrabIt\logs\` — rolling log file.
- Output defaults to `%USERPROFILE%\Pictures\GrabIt\`.

## Dependency pins

| Crate | Version | Purpose |
|---|---|---|
| `windows` | 0.58 | WinRT + Win32 (WGC, GDI, UIA, registry, shell, clipboard) |
| `windows-core` | 0.58 | WinRT primitives |
| `eframe` / `egui` | 0.29 | Editor & settings UI |
| `egui-wgpu` | 0.29 | Canvas rendering |
| `tray-icon` | 0.19 | System tray |
| `global-hotkey` | 0.6 | Global hotkey registration |
| `image` | 0.25 | PNG encode/decode |
| `imageproc` | 0.25 | Gaussian blur, torn-edge effects |
| `fast_image_resize` | 5 | SIMD resize |
| `serde` + `toml` | 1 / 0.8 | Settings + presets |
| `rmp-serde` | 1.3 | MessagePack for `.grabit` docs |
| `rfd` | 0.15 | Native file dialogs |
| `winreg` | 0.52 | Auto-start registry entry |
| `dirs` | 5 | `%APPDATA%` path resolution |
| `parking_lot` | 0.12 | Fast Mutex/RwLock |
| `crossbeam-channel` | 0.5 | Pipeline channels |
| `anyhow` / `thiserror` | 1 / 2 | Error handling |
| `log` + `env_logger` | 0.4 / 0.11 | File-sink logging |
| `uuid` | 1 | Annotation node IDs |

## Critical files to create

At M0 scaffolding:

- `C:\code\GrabIt\Cargo.toml` — deps + release profile
- `C:\code\GrabIt\.cargo\config.toml` — static CRT rustflags
- `C:\code\GrabIt\build.rs` — embed manifest (per-monitor v2 DPI, asInvoker),
  icon, version
- `C:\code\GrabIt\assets\manifest.xml`
- `C:\code\GrabIt\assets\icons\` + `assets\stamps\`
- `C:\code\GrabIt\src\main.rs` — entry, single-instance mutex, dispatch
- `C:\code\GrabIt\src\app\` — `AppState`, paths, single-instance
- `C:\code\GrabIt\src\tray\` — tray icon + menu wiring
- `C:\code\GrabIt\src\hotkeys\` — global hotkey registration
- `C:\code\GrabIt\src\autostart\` — HKCU Run key read/write
- `C:\code\GrabIt\src\capture\mod.rs` — public `CaptureRequest` / `CaptureResult`
- `C:\code\GrabIt\src\capture\wgc.rs` — WGC primary path
- `C:\code\GrabIt\src\capture\gdi.rs` — GDI fallback
- `C:\code\GrabIt\src\capture\cursor.rs` — cursor-as-layer
- `C:\code\GrabIt\src\editor\document.rs` — `Document` + serde
- `C:\code\GrabIt\src\settings\mod.rs` — `Settings` load/save
- `C:\code\GrabIt\src\export\mod.rs` — PNG save + clipboard copy

Later-milestone modules (created when their milestone lands):

- `src\capture\region.rs`, `window_pick.rs`, `object_pick.rs`,
  `multi_region.rs`, `delay.rs`, `exact_dims.rs`
- `src\editor\canvas.rs`, `commands.rs`, `tools\*.rs` (arrow, callout, shape,
  step, stamp, magnify, blur, crop, resize, rotate, cutout, border,
  capture_info, cursor_edit)
- `src\editor\styles.rs`, `template.rs`
- `src\batch\mod.rs`, `recipe.rs`
- `src\printer\` (M8 only)

## Milestones

| M# | Name | Features (CSV #) | Definition of done | Complexity |
|---|---|---|---|---|
| **M0** | Scaffolding + capture loop | — (infra) + clipboard | Tray icon appears; right-click menu works; hotkey triggers fullscreen PNG saved to `%USERPROFILE%\Pictures\GrabIt\` AND copied to clipboard; auto-start survives reboot; binary <10 MB | M |
| **M1** | Capture core + region overlay | #1 (delay), #6 (exact dims), cursor layer (part of #2) | Region selector overlay works across two monitors at mixed DPI; window & fullscreen capture paths work; countdown overlay does not appear in output; cursor lands on a separate layer in `.grabit` | L |
| **M2** | Editor skeleton | #20 (crop/resize/rotate), #25 (PNG export) | `.grabit` opens in egui editor; smooth pan/zoom at 4K; undo/redo for transforms; PNG export is bit-exact when no edits applied; headless `apply_recipe` API exists | L |
| **M3** | Primary annotations + cursor edit | #2 (full), #10, #11, #12, #13, #14 | Arrow/callout/shape/step/stamp tools draw, select, transform, restyle, persist and reload; cursor layer can be moved/resized/deleted; 50 annotations at 4K stays >30 fps | L |
| **M4** | Editor advanced effects | #16, #17, #21, #22, #23 | Blur regions with undo; cut-out torn-edge join; borders; capture-info stamp reads real metadata captured at M1; blur is non-destructive in `.grabit`, destructive on PNG export | M |
| **M5** | Presets + per-preset hotkeys + quick styles | #3, #4, #19 | Create preset "Region + 3s delay, no cursor", bind Ctrl+Shift+1, invoke, output matches; rebinding hotkeys works without restart; style presets round-trip | M |
| **M6** | Advanced capture | #5 (menu & object), #8 (multi-region) | Hover a menu item, capture as object; multi-region composes non-contiguous rects into one image with gutters; menu capture survives menu auto-dismiss via WinEventHook | L |
| **M7** | Templates + batch | #18, #24 | Template assembles N captures into a guide layout and exports PNG; batch applies a recipe to a folder of PNGs via GUI (CLI not in scope unless requested) | M |
| **M8** | Printer capture | #9 | "Print" from Notepad / Word / Chrome to "GrabIt" virtual printer opens editor with rasterized page; port monitor installs/uninstalls cleanly; Authenticode-signed | XL |
| **M9** | Packaging & polish | — | Signed `.exe`; installer (Inno Setup or MSIX) + portable zip; auto-update via GitHub Releases + `self_update`; first-run onboarding; SmartScreen reputation seeded | M |

**Features dropped vs CSV**: #7 Time-lapse (user removed), #15 Spotlight (Mac-
only source). All other 23 features ship.

## Verification

Each milestone ships independently. End-to-end verification per milestone:

- **M0**: reboot PC, confirm tray icon appears within ~200ms of login; press
  hotkey, verify PNG lands on disk AND clipboard (paste into Paint to confirm).
- **M1**: run on a multi-monitor setup with one 4K display and one 1080p
  display; capture a region spanning both; compare pixel dimensions against
  Snipping Tool; confirm countdown does NOT appear in output image; verify
  cursor layer exists separately in the `.grabit` file.
- **M2**: open `.grabit` from M1, apply crop/rotate/resize, undo each, export
  PNG; confirm unmodified export is bit-identical to M1's PNG.
- **M3**: for each annotation tool, draw → save `.grabit` → reopen →
  manipulate → undo/redo → re-export. Stress test with 50 annotations at 4K and
  check frame rate.
- **M4**: apply blur to a region of a captured screenshot, confirm reversible
  in editor but destructive on PNG export; verify capture-info stamp shows
  correct OS version, process name, and timestamp for the captured window.
- **M5**: create preset, bind hotkey, invoke from hotkey and from tray menu;
  confirm both paths produce identical output; rebind while app is running.
- **M6**: hover a Start menu item and capture it as an object; capture a menu
  that auto-dismisses on keypress (file menu in Notepad); build a multi-region
  composite with three non-contiguous areas.
- **M7**: build a template combining 3 captures; apply template with a
  different base image; run batch on a folder of 50 PNGs and confirm all
  outputs apply the recipe.
- **M8**: print from Notepad, Word, Chrome to "GrabIt" virtual printer; verify
  editor opens with the rasterized content; uninstall and confirm port monitor
  is removed from the registry.
- **M9**: install fresh on a clean Windows VM; portable zip runs from a USB
  stick without registry writes when `--portable` is passed; auto-update
  downloads and rolls over cleanly.

Unit tests: per-module where tractable (image ops, serde round-trips, hotkey
parser, recipe engine). Integration tests: headless capture + PNG compare on a
dedicated test harness monitor. Manual QA: per-milestone checklist against the
definitions above.

## Known risks

- **M8 printer capture** is the single largest unknown. Authenticode cert must
  be obtained before M8 starts (lead time is typically 1–3 business days for
  OV, longer for EV). If the cert is unavailable, M8 blocks v1.0 release.
- **Region-selector overlay** (in M1) is the hardest single piece of Win32 in
  the project — multi-monitor + DPI + click-through + loupe + Escape-to-cancel.
  Budget for it to take most of M1.
- **`global-hotkey` + `tray-icon` event loops** may fight each other or fight
  eframe's winit loop. If so, fall back to a hand-rolled message loop with raw
  `RegisterHotKey` + `Shell_NotifyIconW`.
- **UIPI**: global hotkeys registered by an unelevated app do NOT reach
  elevated foreground windows. Document this behavior in M0 release notes.
- **SmartScreen**: an unsigned or newly-signed `.exe` will show a SmartScreen
  warning until reputation is established. Plan M9 accordingly.
