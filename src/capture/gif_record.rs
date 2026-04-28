//! GIF recorder.
//!
//! Flow:
//! 1. Pick a region with the existing capture-first frozen overlay.
//! 2. Show a small Win32 floating control bar (Pause / Stop / timer)
//!    anchored near the region. Esc and re-pressing the GIF hotkey both
//!    stop the recording.
//! 3. Drive a `WM_TIMER` at the configured FPS; each tick captures the
//!    region with `gdi::capture_region`, optionally composites the cursor,
//!    and spools the frame as PNG into a per-recording UUID directory.
//! 4. On stop, write a sidecar JSON describing the spool dir + frame
//!    timings + metadata, then return its path. Caller spawns the
//!    `--gif-editor <sidecar>` subprocess.
//!
//! The recorder runs synchronously on whichever thread invokes `run`
//! (typically `grabit-hotkey-drain`). A process-global atomic stores the
//! floating bar's HWND so a second hotkey press elsewhere can route to
//! `request_stop()` instead of starting a parallel recording.

use crate::app::paths::AppPaths;
use crate::capture::{cursor, gdi, region, Rect};
use crate::settings::Settings;
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use image::RgbaImage;
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GifSidecar {
    pub version: u32,
    pub spool_dir: PathBuf,
    pub frames: Vec<SidecarFrame>,
    pub base_size: (u32, u32),
    pub fps_target: u32,
    pub loop_count: u16,
    pub captured_at: chrono::DateTime<Utc>,
    pub metadata: crate::capture::CaptureMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarFrame {
    pub file: String,
    pub delay_ms: u32,
}

/// Floating-bar HWND while a recording is live; `0` when idle. Stored as
/// `isize` so we can use atomics — `HWND` is `*mut c_void` and not directly
/// atomic-able.
static ACTIVE: AtomicIsize = AtomicIsize::new(0);

pub fn is_recording() -> bool {
    ACTIVE.load(Ordering::SeqCst) != 0
}

/// Best-effort stop signal. Posts `WM_CLOSE` to the floating bar so the
/// recorder's message loop exits and the encoder runs the sidecar write.
/// Safe to call from any thread.
pub fn request_stop() {
    let hwnd_isize = ACTIVE.load(Ordering::SeqCst);
    if hwnd_isize == 0 {
        return;
    }
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
        let hwnd = HWND(hwnd_isize as *mut _);
        let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

/// Drop-guard that clears `ACTIVE` even if the recorder panics mid-run.
/// Without this a panic would leave `is_recording()` permanently true.
struct ActiveGuard;
impl ActiveGuard {
    fn arm(hwnd_isize: isize) -> Self {
        ACTIVE.store(hwnd_isize, Ordering::SeqCst);
        ActiveGuard
    }
}
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        ACTIVE.store(0, Ordering::SeqCst);
    }
}

/// Run the recorder. Returns `Ok(Some(sidecar_path))` once the user stops
/// the recording, `Ok(None)` if they cancelled at the region-pick stage.
#[cfg(windows)]
pub fn run(paths: &AppPaths, settings: &Settings) -> Result<Option<PathBuf>> {
    // 1. Pick region using the same capture-first idiom as the annotate flow.
    let (frozen, frozen_rect) = gdi::capture_virtual_desktop()
        .context("GDI fullscreen capture for GIF region pick")?;
    let region = match region::select_on_frozen(&frozen)? {
        region::RegionResult::Region(r) => r,
        region::RegionResult::Window(_) | region::RegionResult::Cancelled => {
            return Ok(None);
        }
    };
    let _ = frozen_rect;

    // 2. Make a unique spool dir under %APPDATA%\GrabIt\gif-record\<uuid>.
    let recording_id = uuid::Uuid::new_v4();
    let spool_dir = paths.gif_temp_dir().join(recording_id.to_string());
    std::fs::create_dir_all(&spool_dir)
        .with_context(|| format!("create gif spool dir {}", spool_dir.display()))?;

    let fps = settings.gif_fps.clamp(5, 60);
    let max_seconds = settings.gif_max_seconds.max(1);
    let include_cursor = settings.gif_record_cursor;

    info!(
        "gif: starting recording {recording_id} at {fps} fps in {}",
        spool_dir.display()
    );

    let outcome = imp::record_with_bar(region, fps, max_seconds, include_cursor, &spool_dir)?;

    // 3. Persist sidecar JSON and return its path. Even with zero captured
    // frames we still produce a sidecar so the editor opens cleanly with
    // an empty timeline (the user can see what happened and discard).
    let sidecar_path = spool_dir.join("recording.json");
    let metadata = crate::capture::CaptureMetadata {
        captured_at: Utc::now(),
        foreground_title: foreground_title(),
        foreground_process: foreground_process(),
        os_version: os_version(),
        monitors: crate::platform::monitors::enumerate(),
        capture_rect: region,
    };
    let sidecar = GifSidecar {
        version: 1,
        spool_dir: spool_dir.clone(),
        frames: outcome.frames,
        base_size: (region.width, region.height),
        fps_target: fps,
        loop_count: settings.gif_loop_count,
        captured_at: Utc::now(),
        metadata,
    };
    let body = serde_json::to_string_pretty(&sidecar)
        .context("serialize gif sidecar")?;
    std::fs::write(&sidecar_path, body)
        .with_context(|| format!("write sidecar {}", sidecar_path.display()))?;

    info!(
        "gif: recording stopped — {} frames \u{2192} {}",
        sidecar.frames.len(),
        sidecar_path.display()
    );
    Ok(Some(sidecar_path))
}

#[cfg(not(windows))]
pub fn run(_paths: &AppPaths, _settings: &Settings) -> Result<Option<PathBuf>> {
    Err(anyhow!("GIF recording is Windows-only"))
}

fn foreground_title() -> Option<String> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf);
        if len <= 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn foreground_process() -> Option<String> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
        use windows::Win32::System::ProcessStatus::GetProcessImageFileNameW;
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
        if pid == 0 {
            return None;
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; MAX_PATH as usize];
        let len = GetProcessImageFileNameW(handle, &mut buf);
        let _ = CloseHandle(handle);
        if len == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        Some(full.rsplit('\\').next().unwrap_or(&full).to_string())
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn os_version() -> String {
    #[cfg(windows)]
    {
        "Windows".to_string()
    }
    #[cfg(not(windows))]
    {
        "unknown".to_string()
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::cell::RefCell;
    use std::time::Instant;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateFontIndirectW, CreateSolidBrush, DeleteObject, EndPaint, FillRect,
        InvalidateRect, SelectObject, SetBkMode, SetTextColor, TextOutW, FW_SEMIBOLD, HBRUSH,
        HGDIOBJ, LOGFONTW, PAINTSTRUCT, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
        GetSystemMetrics, KillTimer, PeekMessageW, PostQuitMessage, RegisterClassExW,
        SetLayeredWindowAttributes, SetTimer, SetWindowPos, ShowWindow, TranslateMessage,
        UnregisterClassW, HCURSOR, HICON, HWND_TOPMOST, LWA_ALPHA, MSG, PM_REMOVE, SM_CXSCREEN,
        SM_CYSCREEN, SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNOACTIVATE, WM_CLOSE, WM_DESTROY, WM_KEYDOWN,
        WM_LBUTTONDOWN, WM_PAINT, WM_TIMER, WNDCLASSEXW, WNDCLASS_STYLES, WS_EX_LAYERED,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_SPACE};

    const CLASS_NAME: PCWSTR = w!("GrabItGifRecorder");
    const TIMER_FRAME: usize = 1;
    /// Repaint the timer label each ~100 ms so the elapsed counter stays
    /// readable independent of the (possibly slow) frame timer.
    const TIMER_REDRAW: usize = 2;

    const BAR_W: i32 = 280;
    const BAR_H: i32 = 56;
    const BTN_W: i32 = 70;

    #[derive(Default)]
    pub(super) struct Outcome {
        pub frames: Vec<SidecarFrame>,
    }

    struct State {
        rect: Rect,
        fps: u32,
        max_seconds: u32,
        include_cursor: bool,
        spool_dir: PathBuf,
        frames: Vec<SidecarFrame>,
        last_tick: Option<Instant>,
        start: Instant,
        paused: bool,
        timer_active: bool,
        idx: usize,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = RefCell::new(None);
    }

    fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
        COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
    }

    pub(super) fn record_with_bar(
        rect: Rect,
        fps: u32,
        max_seconds: u32,
        include_cursor: bool,
        spool_dir: &Path,
    ) -> Result<Outcome> {
        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(PCWSTR::null())
                .map_err(|e| anyhow!("GetModuleHandle: {e}"))?
                .into();

            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: WNDCLASS_STYLES(0),
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: HICON::default(),
                hCursor: HCURSOR::default(),
                hbrBackground: HBRUSH::default(),
                lpszMenuName: PCWSTR::null(),
                lpszClassName: CLASS_NAME,
                hIconSm: HICON::default(),
            };
            let _ = RegisterClassExW(&class);

            // Anchor the bar near the top-right of the recording rect, but
            // keep it on-screen even if the rect hugs the right or top edge
            // of the virtual desktop.
            let scr_w = GetSystemMetrics(SM_CXSCREEN);
            let scr_h = GetSystemMetrics(SM_CYSCREEN);
            let mut bar_x = rect.x + rect.width as i32 - BAR_W;
            let mut bar_y = rect.y - BAR_H - 8;
            if bar_y < 0 {
                bar_y = rect.y + 8;
            }
            if bar_x < 0 {
                bar_x = 0;
            }
            if bar_x + BAR_W > scr_w {
                bar_x = (scr_w - BAR_W).max(0);
            }
            if bar_y + BAR_H > scr_h {
                bar_y = (scr_h - BAR_H).max(0);
            }

            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS_NAME,
                w!("GrabIt"),
                WS_POPUP | WS_VISIBLE,
                bar_x,
                bar_y,
                BAR_W,
                BAR_H,
                None,
                None,
                hinstance,
                None,
            )
            .map_err(|e| anyhow!("CreateWindowEx: {e}"))?;
            if hwnd.0.is_null() {
                return Err(anyhow!("CreateWindowEx returned null"));
            }

            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 235, LWA_ALPHA);
            // SetWindowPos w/ HWND_TOPMOST makes sure we float above the
            // recently-foregrounded overlay window the region picker just
            // tore down (its TOPMOST status can outlive its destroy on
            // some shells).
            let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            STATE.with(|cell| {
                *cell.borrow_mut() = Some(State {
                    rect,
                    fps,
                    max_seconds,
                    include_cursor,
                    spool_dir: spool_dir.to_path_buf(),
                    frames: Vec::new(),
                    last_tick: None,
                    start: Instant::now(),
                    paused: false,
                    timer_active: false,
                    idx: 0,
                });
            });

            // Arm ACTIVE *after* the window is fully constructed so a stop
            // request that races our startup can find the HWND.
            let _guard = ActiveGuard::arm(hwnd.0 as isize);

            // Kick off frame timer + redraw timer.
            let interval = (1000 / fps.max(1)).max(1);
            let _ = SetTimer(hwnd, TIMER_FRAME, interval, None);
            let _ = SetTimer(hwnd, TIMER_REDRAW, 100, None);
            STATE.with(|cell| {
                if let Some(s) = cell.borrow_mut().as_mut() {
                    s.timer_active = true;
                }
            });

            // Modal message loop. Exits when the bar posts WM_QUIT (Stop /
            // Esc / WM_CLOSE / max-seconds / external request_stop).
            let mut msg = MSG::default();
            'pump: loop {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == windows::Win32::UI::WindowsAndMessaging::WM_QUIT {
                        break 'pump;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }

            let _ = KillTimer(hwnd, TIMER_FRAME);
            let _ = KillTimer(hwnd, TIMER_REDRAW);
            let _ = DestroyWindow(hwnd);
            let _ = UnregisterClassW(CLASS_NAME, hinstance);

            let outcome = STATE.with(|cell| {
                cell.borrow_mut()
                    .take()
                    .map(|s| Outcome { frames: s.frames })
                    .unwrap_or_default()
            });
            debug!("gif recorder bar exited with {} frames", outcome.frames.len());
            Ok(outcome)
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_TIMER => {
                let id = wparam.0;
                if id == TIMER_FRAME {
                    on_frame_tick(hwnd);
                } else if id == TIMER_REDRAW {
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                paint_bar(hdc);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let _y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                if x >= BAR_W - BTN_W {
                    // Stop button (right).
                    PostQuitMessage(0);
                } else if x >= BAR_W - 2 * BTN_W {
                    // Pause / Resume button (middle-right).
                    toggle_pause(hwnd);
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let key = wparam.0 as u32;
                if key == VK_ESCAPE.0 as u32 {
                    PostQuitMessage(0);
                } else if key == VK_SPACE.0 as u32 {
                    toggle_pause(hwnd);
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_DESTROY => {
                // Deliberately *do not* PostQuitMessage here. The pump
                // has already exited (caller breaks on WM_QUIT from the
                // Stop / Esc / WM_CLOSE / max-seconds paths) and we
                // call `DestroyWindow` ourselves after the pump returns.
                // Posting WM_QUIT from this arm queues a second WM_QUIT
                // on whichever thread is invoking the recorder — which,
                // for the tray-menu route, is the main thread. The next
                // tick of the tray's PeekMessageW loop would then drain
                // that orphan WM_QUIT and silently shut down the app.
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn toggle_pause(hwnd: HWND) {
        STATE.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let Some(s) = borrow.as_mut() else { return };
            if s.paused {
                let interval = (1000 / s.fps.max(1)).max(1);
                let _ = SetTimer(hwnd, TIMER_FRAME, interval, None);
                s.timer_active = true;
                s.paused = false;
                // Reset last_tick so the resume gap isn't credited as a
                // single huge inter-frame delay.
                s.last_tick = None;
            } else {
                let _ = KillTimer(hwnd, TIMER_FRAME);
                s.timer_active = false;
                s.paused = true;
            }
        });
    }

    unsafe fn on_frame_tick(hwnd: HWND) {
        // Snapshot what we need from the state, then drop the borrow before
        // calling into capture code that itself might post messages.
        let (rect, include_cursor, idx, spool_dir, last_tick, fps, elapsed_ms, max_ms) = STATE
            .with(|cell| {
                let s = cell.borrow();
                let s = s.as_ref().expect("recorder state");
                (
                    s.rect,
                    s.include_cursor,
                    s.idx,
                    s.spool_dir.clone(),
                    s.last_tick,
                    s.fps,
                    s.start.elapsed().as_millis() as u64,
                    (s.max_seconds as u64) * 1000,
                )
            });

        if elapsed_ms > max_ms {
            warn!("gif: max recording duration reached, stopping");
            PostQuitMessage(0);
            return;
        }

        let now = Instant::now();
        let delay_ms = match last_tick {
            None => (1000 / fps.max(1)).max(10),
            Some(prev) => {
                let dt = now.duration_since(prev).as_millis() as u32;
                dt.max(1)
            }
        };

        let img = match capture_frame(rect, include_cursor) {
            Ok(img) => img,
            Err(e) => {
                warn!("gif: frame {idx} capture failed: {e}");
                return;
            }
        };
        let path = spool_dir.join(format!("f{idx:05}.png"));
        if let Err(e) = img.save_with_format(&path, image::ImageFormat::Png) {
            warn!("gif: write frame {idx} failed: {e}");
            return;
        }

        STATE.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let Some(s) = borrow.as_mut() else { return };
            s.frames.push(SidecarFrame {
                file: path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| format!("f{idx:05}.png")),
                delay_ms,
            });
            s.idx += 1;
            s.last_tick = Some(now);
        });
        let _ = hwnd;
    }

    fn capture_frame(rect: Rect, include_cursor: bool) -> Result<RgbaImage> {
        let (mut img, _) = gdi::capture_region(rect)?;
        if include_cursor {
            if let Ok(Some(mut cur)) = cursor::sample() {
                cur.x -= rect.x;
                cur.y -= rect.y;
                let cw = cur.image.width() as i32;
                let ch = cur.image.height() as i32;
                let visible = cur.x + cw > 0
                    && cur.y + ch > 0
                    && cur.x < rect.width as i32
                    && cur.y < rect.height as i32;
                if visible {
                    crate::export::composite_over(&mut img, &cur);
                }
            }
        }
        Ok(img)
    }

    unsafe fn paint_bar(hdc: windows::Win32::Graphics::Gdi::HDC) {
        let mut client = RECT::default();
        // We don't have hwnd here, but we know the bar is BAR_W x BAR_H —
        // the layered window's client rect always matches the create size.
        client.right = BAR_W;
        client.bottom = BAR_H;

        // Background.
        let bg = CreateSolidBrush(rgb(28, 30, 38));
        FillRect(hdc, &client, bg);
        let _ = DeleteObject(HGDIOBJ(bg.0));

        // Snapshot timer state.
        let (paused, frames, secs) = STATE.with(|cell| {
            let s = cell.borrow();
            let s = s.as_ref();
            match s {
                Some(s) => (
                    s.paused,
                    s.idx,
                    s.start.elapsed().as_secs_f32(),
                ),
                None => (false, 0, 0.0),
            }
        });

        // Recording dot — red when active, amber when paused.
        let dot_color = if paused { rgb(220, 170, 40) } else { rgb(220, 60, 60) };
        let dot = CreateSolidBrush(dot_color);
        let dot_rect = RECT { left: 12, top: 18, right: 32, bottom: 38 };
        FillRect(hdc, &dot_rect, dot);
        let _ = DeleteObject(HGDIOBJ(dot.0));

        // Pause / Stop button backgrounds.
        let btn_pause = CreateSolidBrush(rgb(48, 52, 64));
        let btn_stop = CreateSolidBrush(rgb(140, 50, 50));
        let pause_rect = RECT {
            left: BAR_W - 2 * BTN_W,
            top: 6,
            right: BAR_W - BTN_W,
            bottom: BAR_H - 6,
        };
        let stop_rect = RECT {
            left: BAR_W - BTN_W,
            top: 6,
            right: BAR_W - 4,
            bottom: BAR_H - 6,
        };
        FillRect(hdc, &pause_rect, btn_pause);
        FillRect(hdc, &stop_rect, btn_stop);
        let _ = DeleteObject(HGDIOBJ(btn_pause.0));
        let _ = DeleteObject(HGDIOBJ(btn_stop.0));

        // Text.
        let mut lf = LOGFONTW::default();
        lf.lfHeight = -14;
        lf.lfWeight = FW_SEMIBOLD.0 as i32;
        let face_str = format!("{}\0", crate::platform::fonts::FACE_NAME);
        let face: Vec<u16> = face_str.encode_utf16().collect();
        for (i, c) in face.iter().enumerate() {
            if i < lf.lfFaceName.len() {
                lf.lfFaceName[i] = *c;
            }
        }
        let font = CreateFontIndirectW(&lf);
        let old = SelectObject(hdc, HGDIOBJ(font.0));
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, rgb(235, 235, 240));

        let timer_text = format!(
            "{:01}:{:04.1}  {} fr",
            (secs as u32) / 60,
            secs % 60.0,
            frames
        );
        let timer_wide: Vec<u16> = timer_text.encode_utf16().collect();
        let _ = TextOutW(hdc, 42, 19, &timer_wide);

        let pause_label: Vec<u16> = if paused { "Resume" } else { "Pause" }
            .encode_utf16()
            .collect();
        let _ = TextOutW(hdc, BAR_W - 2 * BTN_W + 10, 19, &pause_label);

        let stop_label: Vec<u16> = "Stop".encode_utf16().collect();
        let _ = TextOutW(hdc, BAR_W - BTN_W + 18, 19, &stop_label);

        SelectObject(hdc, old);
        let _ = DeleteObject(HGDIOBJ(font.0));
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    #[derive(Default)]
    pub(super) struct Outcome {
        pub frames: Vec<SidecarFrame>,
    }

    pub(super) fn record_with_bar(
        _rect: Rect,
        _fps: u32,
        _max_seconds: u32,
        _include_cursor: bool,
        _spool_dir: &Path,
    ) -> Result<Outcome> {
        Err(anyhow!("GIF recording is Windows-only"))
    }
}
