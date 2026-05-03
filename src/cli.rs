//! Headless `--capture` subcommand surface for Claude Code (and any other
//! script-driven consumer). Stdout carries one thing only: the absolute
//! output path on success, or a JSON array for `list-windows`. Diagnostic
//! noise (e.g. multi-window disambiguation notes) goes to stderr so the
//! stdout contract stays parseable.
//!
//! Verbs:
//! - `screenshot` — single PNG of fullscreen / window-by-process / region.
//! - `gif`        — fire-and-wait recording, fixed `--duration` seconds.
//! - `list-windows` — JSON enumeration of top-level windows.
//! - `help`       — prints usage to stdout, exits 0.

use crate::app::paths::AppPaths;
use crate::capture::{self, window_lookup, CaptureRequest, CaptureTarget, Rect};
use crate::export;
use crate::settings::Settings;
use anyhow::{anyhow, bail, Context, Result};
use log::info;
use std::path::PathBuf;

pub fn run(args: &[String], paths: &AppPaths) -> Result<()> {
    let i = args
        .iter()
        .position(|a| a == "--capture")
        .ok_or_else(|| anyhow!("internal: cli::run called without --capture in args"))?;
    let verb = match args.get(i + 1).map(String::as_str) {
        Some(v) => v,
        None => {
            print_help();
            return Ok(());
        }
    };
    match verb {
        "screenshot" => cmd_screenshot(args, paths),
        "gif" => cmd_gif(args, paths),
        "list-windows" => cmd_list_windows(args),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => bail!(
            "unknown --capture verb: {other:?}. Run `grabit.exe --capture help` for usage."
        ),
    }
}

fn cmd_screenshot(args: &[String], _paths: &AppPaths) -> Result<()> {
    let out = require_path(args, "--out")?;
    let target = resolve_target(args)?;
    let delay_ms = arg_u32(args, "--delay-ms").unwrap_or(0);
    let include_cursor = flag(args, "--include-cursor");

    ensure_parent_dir(&out)?;

    info!(
        "capture screenshot: target={:?} out={} delay_ms={} cursor={}",
        debug_target(&target),
        out.display(),
        delay_ms,
        include_cursor
    );

    let req = CaptureRequest { target, delay_ms, include_cursor };
    let result = capture::perform(req)?
        .ok_or_else(|| anyhow!("capture returned no result"))?;
    let written = export::save_png_to(&result, &out)?;
    println!("{}", canonical_or(&written));
    Ok(())
}

fn cmd_gif(args: &[String], paths: &AppPaths) -> Result<()> {
    let out = require_path(args, "--out")?;
    let duration = arg_u32(args, "--duration")
        .ok_or_else(|| anyhow!("--duration <seconds> is required for gif"))?;
    let fps_override = arg_u32(args, "--fps");
    let include_cursor = flag(args, "--include-cursor");
    let settings = Settings::load_or_default(paths);

    let target = resolve_target(args)?;
    let rect = match target {
        CaptureTarget::Window(hwnd) => window_lookup::window_rect(hwnd)?,
        CaptureTarget::Region(r) => r,
        CaptureTarget::Fullscreen => virtual_desktop_rect()?,
        _ => bail!("--capture gif: only fullscreen / window / region targets are supported"),
    };

    ensure_parent_dir(&out)?;

    info!(
        "capture gif: rect={:?} duration={}s fps={:?} out={} cursor={}",
        rect,
        duration,
        fps_override,
        out.display(),
        include_cursor
    );

    let frames = capture::gif_record::run_headless(
        paths,
        &settings,
        rect,
        duration,
        fps_override,
        include_cursor,
    )?;
    if frames.is_empty() {
        bail!("recording produced 0 frames; nothing to encode");
    }
    let loop_count = settings.gif_loop_count;
    export::gif::encode_to_gif(&frames, loop_count, &out, |_, _| {})
        .with_context(|| format!("encode gif {}", out.display()))?;
    println!("{}", canonical_or(&out));
    Ok(())
}

fn cmd_list_windows(args: &[String]) -> Result<()> {
    let process_filter = arg_value(args, "--process");
    let mut wins = window_lookup::enumerate_top_level();
    if let Some(name) = process_filter.as_deref() {
        let needle = if name.to_ascii_lowercase().ends_with(".exe") {
            name.to_string()
        } else {
            format!("{name}.exe")
        };
        wins.retain(|w| w.process.eq_ignore_ascii_case(&needle));
    }
    let body = serde_json::to_string(&wins).context("serialize window list")?;
    println!("{body}");
    Ok(())
}

/// Build a `CaptureTarget` from the `--target` flag and its dependent args.
fn resolve_target(args: &[String]) -> Result<CaptureTarget> {
    let kind = arg_value(args, "--target").ok_or_else(|| {
        anyhow!("--target is required (one of: fullscreen, window, region)")
    })?;
    match kind.as_str() {
        "fullscreen" => Ok(CaptureTarget::Fullscreen),
        "window" => {
            let process = arg_value(args, "--process").ok_or_else(|| {
                anyhow!("--target window requires --process <name> (e.g. code.exe)")
            })?;
            let matches = window_lookup::find_by_process(&process);
            if matches.is_empty() {
                bail!(
                    "no top-level windows found for process {:?}. Try `grabit.exe --capture list-windows` to see what's open.",
                    process
                );
            }
            let chosen = window_lookup::pick_largest(&matches)
                .expect("non-empty matches")
                .clone();
            if matches.len() > 1 {
                eprintln!(
                    "note: {} candidate windows for process {:?}; picked HWND 0x{:x} ({}x{}) titled {:?}",
                    matches.len(),
                    process,
                    chosen.hwnd,
                    chosen.rect.width,
                    chosen.rect.height,
                    chosen.title
                );
                for m in &matches {
                    if m.hwnd != chosen.hwnd {
                        eprintln!(
                            "  also: HWND 0x{:x} ({}x{}) {:?}",
                            m.hwnd, m.rect.width, m.rect.height, m.title
                        );
                    }
                }
            }
            Ok(CaptureTarget::Window(chosen.hwnd))
        }
        "region" => {
            let x = arg_i32(args, "--x")
                .ok_or_else(|| anyhow!("--target region requires --x"))?;
            let y = arg_i32(args, "--y")
                .ok_or_else(|| anyhow!("--target region requires --y"))?;
            let w = arg_u32(args, "--w")
                .ok_or_else(|| anyhow!("--target region requires --w"))?;
            let h = arg_u32(args, "--h")
                .ok_or_else(|| anyhow!("--target region requires --h"))?;
            if w == 0 || h == 0 {
                bail!("region width and height must be > 0");
            }
            Ok(CaptureTarget::Region(Rect { x, y, width: w, height: h }))
        }
        other => bail!(
            "--target {other:?}: expected one of fullscreen, window, region"
        ),
    }
}

/// Sum of all monitor rectangles (the virtual-desktop rect). Used as the
/// fullscreen GIF target so the recorder loop has a concrete `Rect`.
fn virtual_desktop_rect() -> Result<Rect> {
    let monitors = crate::platform::monitors::enumerate();
    if monitors.is_empty() {
        bail!("no monitors enumerated");
    }
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for m in &monitors {
        let r = m.rect;
        min_x = min_x.min(r.x);
        min_y = min_y.min(r.y);
        max_x = max_x.max(r.x + r.width as i32);
        max_y = max_y.max(r.y + r.height as i32);
    }
    Ok(Rect {
        x: min_x,
        y: min_y,
        width: (max_x - min_x).max(1) as u32,
        height: (max_y - min_y).max(1) as u32,
    })
}

fn ensure_parent_dir(p: &std::path::Path) -> Result<()> {
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create output dir {}", parent.display()))?;
        }
    }
    Ok(())
}

fn canonical_or(p: &std::path::Path) -> String {
    let resolved = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let s = resolved.display().to_string();
    // `canonicalize` on Windows returns the `\\?\` extended-length prefix.
    // It's harmless for tooling but ugly inside markdown links and confuses
    // some downstream consumers, so strip it for the stdout contract.
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn require_path(args: &[String], flag_name: &str) -> Result<PathBuf> {
    arg_value(args, flag_name)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("{flag_name} <path> is required"))
}

fn arg_u32(args: &[String], flag: &str) -> Option<u32> {
    arg_value(args, flag).and_then(|v| v.parse().ok())
}

fn arg_i32(args: &[String], flag: &str) -> Option<i32> {
    arg_value(args, flag).and_then(|v| v.parse().ok())
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn debug_target(t: &CaptureTarget) -> String {
    match t {
        CaptureTarget::Fullscreen => "fullscreen".into(),
        CaptureTarget::Window(h) => format!("window(0x{h:x})"),
        CaptureTarget::Region(r) => format!("region({},{},{}x{})", r.x, r.y, r.width, r.height),
        _ => "other".into(),
    }
}

fn print_help() {
    let msg = r#"GrabIt headless capture CLI

USAGE:
  grabit.exe --capture <verb> [options]

VERBS:
  screenshot     Take a single PNG.
  gif            Record a GIF for a fixed duration (no UI).
  list-windows   Print top-level windows as JSON. Filter with --process.
  help           Show this message.

SCREENSHOT:
  grabit.exe --capture screenshot --target fullscreen \
      --out C:\path\to\out.png [--include-cursor] [--delay-ms 0]
  grabit.exe --capture screenshot --target window --process code.exe \
      --out C:\path\to\out.png [--include-cursor] [--delay-ms 0]
  grabit.exe --capture screenshot --target region --x 100 --y 100 --w 800 --h 600 \
      --out C:\path\to\out.png [--include-cursor]

GIF:
  grabit.exe --capture gif --target window --process code.exe \
      --duration 8 --out C:\path\to\out.gif [--fps 15] [--include-cursor]
  grabit.exe --capture gif --target region --x 100 --y 100 --w 800 --h 600 \
      --duration 8 --out C:\path\to\out.gif [--fps 15]
  grabit.exe --capture gif --target fullscreen \
      --duration 5 --out C:\path\to\out.gif [--fps 15]

LIST-WINDOWS:
  grabit.exe --capture list-windows
  grabit.exe --capture list-windows --process notepad.exe

OUTPUT CONTRACT:
  Success: absolute output path on stdout (single line).
           list-windows: JSON array on stdout.
           Diagnostics (multi-window picks, etc.) on stderr.
  Failure: human message on stderr; exit code 1.

Parent directories of --out are auto-created.

For embedding into GitHub markdown, see CLAUDE.md and docs/CAPTURE-CLI.md.
"#;
    print!("{msg}");
}
