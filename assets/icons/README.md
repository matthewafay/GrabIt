# Icons

`grabit.png` is the canonical source of truth for the app logo. To change
the logo, replace this file.

`grabit.ico` is generated automatically by `build.rs` from `grabit.png`
at build time (16/24/32/48/64/128/256 pixel sizes), so it's kept out of
source control. It's embedded into the `.exe` as a Win32 resource via
`../grabit.rc` so File Explorer, the taskbar, and Alt-Tab show the logo.

Runtime code (`src/tray/mod.rs`, `src/editor/mod.rs`) decodes `grabit.png`
directly via `include_bytes!` for tray and editor-window icons.
