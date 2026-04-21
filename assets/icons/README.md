# Icons

Place the tray/window icon at `grabit.ico` (multi-resolution ICO,
containing at minimum 16x16, 32x32, 48x48, 256x256).

Once placed, uncomment the `IDI_TRAY` line in `../grabit.rc` to embed it
as a Win32 resource, and wire `Icon::from_resource(...)` in the tray
module.

Until a real icon is supplied, `src/tray/mod.rs` falls back to a generated
solid-color RGBA stub so the tray still displays during development.
