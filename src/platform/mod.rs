//! Thin Win32 helper layer shared by modules that need DPI, monitor, or
//! window-tree information. Introduced in M1 so `capture::region` and
//! `capture::window_pick` don't each reimplement the same primitives.

pub mod dpi;
pub mod fonts;
pub mod monitors;
