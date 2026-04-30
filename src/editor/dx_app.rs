//! Annotation editor — Dioxus port of the eframe surface in `app.rs`.
//!
//! Renders the captured screenshot as an SVG `<image>` element with all
//! annotations layered on top as further SVG primitives. Selection,
//! resize handles, and in-progress drag previews are also SVG.
//!
//! The Rust core (document model, `History` command stack, `rasterize::
//! flatten`) is unchanged from the eframe version — only the rendering
//! and interaction layer is new.
//!
//! Architecture:
//!
//! - `run_blocking` decodes the document's base PNG once into an
//!   `RgbaImage`, base64-encodes it for `<image href>`, and hands both
//!   to Dioxus via context.
//! - `editor_app` holds all the live state in signals: document,
//!   history, dirty flag, current tool, selection, in-progress
//!   creation, and per-tool style.
//! - `Canvas` owns the SVG and the `pointerdown`/`pointermove`/
//!   `pointerup` flow for tool actions. Tool-specific creation and
//!   selection-resize live in `pointer.rs`-style helpers below.
//! - `Inspector` shows tool style + selected annotation properties +
//!   document-level effects.
//! - `Save` runs `rasterize::flatten` against the cached `RgbaImage`
//!   and writes both `.png` and `.grabit`.

use crate::app::paths::AppPaths;
use crate::editor::commands::{
    self, AddAnnotation, History, RemoveAnnotation, SetBorder, SetEdgeEffect, UpdateAnnotation,
};
use crate::editor::document::{
    self, AnnotationNode, ArrowHeadStyle, ArrowLineStyle, Border, CaptureInfoPosition,
    CaptureInfoStyle, Document, Edge, EdgeEffect, FieldKind, ShapeKind, TextAlign,
    TextListStyle,
};
use crate::editor::tools::Tool;
use crate::settings::Settings;
use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use dioxus::desktop::{tao::window::WindowBuilder, Config, LogicalSize};
use dioxus::events::{Key, MouseEvent};
use dioxus::prelude::*;
use image::RgbaImage;
use log::{info, warn};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

const STYLES: &str = include_str!("dx_app.css");

/// Default 8-color swatch palette used by the simple-mode color
/// pickers. Matches the eframe editor's palette.
const PALETTE: &[[u8; 4]] = &[
    [220, 38, 38, 255],   // red
    [234, 88, 12, 255],   // orange
    [234, 179, 8, 255],   // yellow
    [22, 163, 74, 255],   // green
    [37, 99, 235, 255],   // blue
    [147, 51, 234, 255],  // purple
    [10, 10, 10, 255],    // black
    [240, 240, 240, 255], // white
];

/// One-shot context bundle threaded into every component.
#[derive(Clone)]
struct EditorContext {
    paths: AppPaths,
    settings: Settings,
    /// Cached decoded base image; reused on every save so we don't
    /// re-decode the embedded PNG bytes from the document.
    base_image: Arc<RgbaImage>,
    /// `data:image/png;base64,…` URI for the base image, embedded via
    /// SVG `<image href>`. Wrapped in `Arc` so context can clone cheaply
    /// across the worker threads Dioxus uses to dispatch components.
    base_uri: Arc<String>,
    /// Where save writes the flattened PNG. Inherited from the
    /// `--editor` subprocess args.
    png_path: PathBuf,
    /// Where save writes the `.grabit` sidecar.
    grabit_path: PathBuf,
    /// Honor settings.copy_to_clipboard at save time. Mutable per
    /// session via the top-bar toggle.
    copy_on_save: bool,
}

/// Tool-specific style state. One field per tool; defaults pulled from
/// settings on launch. A single signal keeps the inspector wired to
/// every tool's defaults without one signal per field.
#[derive(Clone, Debug, PartialEq)]
struct ToolStyle {
    arrow_color: [u8; 4],
    arrow_thickness: f32,
    arrow_shadow: bool,
    arrow_line_style: ArrowLineStyle,
    arrow_head_style: ArrowHeadStyle,

    text_color: [u8; 4],
    text_size: f32,
    text_align: TextAlign,
    text_list: TextListStyle,
    text_frosted: bool,
    text_shadow: bool,

    shape_stroke: [u8; 4],
    shape_stroke_width: f32,
    shape_fill: [u8; 4],

    step_fill: [u8; 4],
    step_text_color: [u8; 4],
    step_radius: f32,
    next_step_number: u32,

    blur_radius: f32,

    magnify_circular: bool,
    magnify_border: [u8; 4],
    magnify_border_width: f32,
    magnify_zoom: f32,

    callout_fill: [u8; 4],
    callout_stroke: [u8; 4],
    callout_text_color: [u8; 4],

    capture_info_position: CaptureInfoPosition,
    capture_info_fields: Vec<FieldKind>,
}

impl ToolStyle {
    fn from_settings(s: &Settings) -> Self {
        Self {
            arrow_color: [220, 38, 38, 255],
            arrow_thickness: 4.0,
            arrow_shadow: s.arrow_shadow,
            arrow_line_style: ArrowLineStyle::Solid,
            arrow_head_style: ArrowHeadStyle::FilledTriangle,

            text_color: [10, 10, 10, 255],
            text_size: 22.0,
            text_align: TextAlign::Left,
            text_list: TextListStyle::None,
            text_frosted: false,
            text_shadow: false,

            shape_stroke: [37, 99, 235, 255],
            shape_stroke_width: 3.0,
            shape_fill: [0, 0, 0, 0],

            step_fill: [220, 38, 38, 255],
            step_text_color: [255, 255, 255, 255],
            step_radius: 18.0,
            next_step_number: 1,

            blur_radius: 12.0,

            magnify_circular: true,
            magnify_border: [10, 10, 10, 255],
            magnify_border_width: 3.0,
            magnify_zoom: 3.0,

            callout_fill: [255, 255, 240, 240],
            callout_stroke: [10, 10, 10, 255],
            callout_text_color: [10, 10, 10, 255],

            capture_info_position: CaptureInfoPosition::BottomLeft,
            capture_info_fields: vec![
                FieldKind::Timestamp,
                FieldKind::WindowTitle,
                FieldKind::OsVersion,
            ],
        }
    }
}

/// In-progress drag state — what the canvas's mouse handlers are
/// constructing right now. Cleared on `pointerup`.
#[derive(Clone, Debug, PartialEq)]
enum Pending {
    /// Two-corner drag (used by Arrow / Rect / Ellipse / Magnify /
    /// Blur / Text / Callout).
    Rect {
        start: [f32; 2],
        cur: [f32; 2],
    },
    /// Tool produces a centered-radius shape (Step). Single click;
    /// no drag.
    None,
    /// Active selection drag — moving an annotation.
    Move {
        target: Uuid,
        start_node: AnnotationNode,
        start_mouse: [f32; 2],
        cur_mouse: [f32; 2],
    },
    /// Active resize drag — one of the 8 corner/edge handles.
    Resize {
        target: Uuid,
        start_node: AnnotationNode,
        start_mouse: [f32; 2],
        cur_mouse: [f32; 2],
        handle: ResizeHandle,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResizeHandle {
    Nw,
    N,
    Ne,
    E,
    Se,
    S,
    Sw,
    W,
}

/// Subprocess entry — replaces the eframe `editor::run_blocking`.
pub fn run_blocking(
    document: Document,
    png_path: PathBuf,
    grabit_path: PathBuf,
    copy_to_clipboard: bool,
    paths: AppPaths,
    settings: Settings,
) -> Result<()> {
    // Decode the base PNG once. The cached RgbaImage drives both the
    // base64 URI for SVG and the rasterize::flatten path on save, so we
    // avoid the per-save PNG decode the eframe version did.
    let base_image = image::load_from_memory(&document.base_png)
        .context("decode base PNG")?
        .to_rgba8();
    let base_image = Arc::new(base_image);
    let base_uri = Arc::new(format!(
        "data:image/png;base64,{}",
        STANDARD.encode(&document.base_png)
    ));

    let copy_on_save = copy_to_clipboard;

    // Window sizing — clamp around the image dimensions like the eframe
    // version did, so a 1×1 capture still gives a usable window.
    const MIN_W: f32 = 1100.0;
    const MIN_H: f32 = 720.0;
    const MAX_W: f32 = 1700.0;
    const MAX_H: f32 = 1100.0;
    let want_w = ((document.base_width as f32 + 320.0).max(MIN_W)).min(MAX_W);
    let want_h = ((document.base_height as f32 + 140.0).max(MIN_H)).min(MAX_H);

    let cfg = Config::new().with_window(
        WindowBuilder::new()
            .with_title("GrabIt — Editor")
            .with_inner_size(LogicalSize::new(want_w, want_h))
            .with_min_inner_size(LogicalSize::new(820.0, 560.0)),
    );

    let initial_doc = document;

    dioxus::LaunchBuilder::desktop()
        .with_cfg(cfg)
        .with_context(EditorContext {
            paths,
            settings,
            base_image,
            base_uri,
            png_path,
            grabit_path,
            copy_on_save,
        })
        .with_context(InitialDoc(initial_doc))
        .launch(editor_app);

    Ok(())
}

/// Newtype so we can stuff the initial Document in via context without
/// accidentally pulling it back out instead of reading the live signal.
#[derive(Clone)]
struct InitialDoc(Document);

#[component]
fn editor_app() -> Element {
    let initial = use_context::<InitialDoc>();
    let ctx = use_context::<EditorContext>();

    let document = use_signal(|| initial.0.clone());
    let history = use_signal(History::new);
    let dirty = use_signal(|| false);
    let tool = use_signal(|| Tool::Select);
    let selected = use_signal(|| Option::<Uuid>::None);
    let pending = use_signal(|| Option::<Pending>::None);
    let style = use_signal(|| ToolStyle::from_settings(&ctx.settings));
    let copy_on_save = use_signal(|| ctx.copy_on_save);
    // True during text-edit foreignObject session; carries the id of
    // the text annotation being edited.
    let editing_text = use_signal(|| Option::<Uuid>::None);
    let close_modal = use_signal(|| false);

    // Keyboard shortcuts on the document level: Delete, Ctrl+Z / Y /
    // Shift+Z, Ctrl+S, Esc. Bound to a hidden focus-trap div.
    let on_global_keydown = move |evt: KeyboardEvent| {
        global_keydown(evt, document, history, dirty, selected, editing_text)
    };

    rsx! {
        style { "{STYLES}" }
        div {
            class: "app",
            tabindex: "0",
            autofocus: true,
            onkeydown: on_global_keydown,

            TopBar {
                document: document,
                history: history,
                dirty: dirty,
                copy_on_save: copy_on_save,
                close_modal: close_modal,
            }
            ToolPalette {
                tool: tool,
                selected: selected,
            }
            Canvas {
                document: document,
                history: history,
                dirty: dirty,
                tool: tool,
                selected: selected,
                pending: pending,
                style: style,
                editing_text: editing_text,
            }
            Inspector {
                document: document,
                history: history,
                dirty: dirty,
                tool: tool,
                selected: selected,
                style: style,
            }
            Footer {
                document: document,
                tool: tool,
                dirty: dirty,
                selected: selected,
            }

            if *close_modal.read() {
                CloseConfirm {
                    document: document,
                    dirty: dirty,
                    copy_on_save: copy_on_save,
                    close_modal: close_modal,
                }
            }
        }
    }
}

// ─── State helpers ───────────────────────────────────────────────────

/// Apply a command via the history stack and mark the document dirty.
/// The signal `with_mut` calls are nested so each takes its own borrow
/// in turn (Dioxus signals can't both be locked at once for separate
/// keys).
fn execute_command(
    mut document: Signal<Document>,
    mut history: Signal<History>,
    mut dirty: Signal<bool>,
    cmd: Box<dyn commands::Command>,
) {
    history.with_mut(|h| {
        document.with_mut(|d| {
            h.push(cmd, d);
        });
    });
    dirty.set(true);
}

fn do_undo(mut document: Signal<Document>, mut history: Signal<History>, mut dirty: Signal<bool>) {
    let did = history.with_mut(|h| document.with_mut(|d| h.undo(d)));
    if did {
        dirty.set(true);
    }
}

fn do_redo(mut document: Signal<Document>, mut history: Signal<History>, mut dirty: Signal<bool>) {
    let did = history.with_mut(|h| document.with_mut(|d| h.redo(d)));
    if did {
        dirty.set(true);
    }
}

fn global_keydown(
    evt: KeyboardEvent,
    document: Signal<Document>,
    history: Signal<History>,
    dirty: Signal<bool>,
    mut selected: Signal<Option<Uuid>>,
    mut editing_text: Signal<Option<Uuid>>,
) {
    let key = evt.key();
    let mods = evt.modifiers();

    // Don't intercept anything while a textarea/contenteditable is focused.
    if editing_text.read().is_some() {
        if matches!(key, Key::Escape) {
            editing_text.set(None);
        }
        return;
    }

    match key {
        Key::Delete => {
            // Copy the Option<Uuid> out before mutating so we don't
            // hold a Ref across the .set call.
            let sel_id = *selected.read();
            if let Some(id) = sel_id {
                execute_command(
                    document,
                    history,
                    dirty,
                    Box::new(RemoveAnnotation::new(id)),
                );
                selected.set(None);
            }
        }
        Key::Escape => {
            selected.set(None);
        }
        Key::Character(s) => {
            let lower = s.to_ascii_lowercase();
            if mods.ctrl() {
                match lower.as_str() {
                    "z" if mods.shift() => do_redo(document, history, dirty),
                    "z" => do_undo(document, history, dirty),
                    "y" => do_redo(document, history, dirty),
                    "s" => {
                        // Save handled at the top-bar via a closure;
                        // keyboard variant just calls the shared fn.
                        let ctx = use_context::<EditorContext>();
                        if let Err(e) = save_document(
                            &ctx,
                            &document.read(),
                            ctx.copy_on_save,
                        ) {
                            warn!("save failed: {e}");
                        } else {
                            // Same-frame dirty clear via shared helper.
                            let mut d = dirty;
                            d.set(false);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

// ─── Top bar ─────────────────────────────────────────────────────────

#[component]
fn TopBar(
    document: Signal<Document>,
    history: Signal<History>,
    dirty: Signal<bool>,
    copy_on_save: Signal<bool>,
    close_modal: Signal<bool>,
) -> Element {
    let ctx = use_context::<EditorContext>();
    let png_name = ctx
        .png_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "(unnamed)".into());
    let dims = format!(
        "{} × {} • {} annotation(s)",
        document.read().base_width,
        document.read().base_height,
        document.read().annotations.len()
    );
    let is_dirty = *dirty.read();
    let copy_state = *copy_on_save.read();
    let can_undo = history.read().can_undo();
    let can_redo = history.read().can_redo();

    let on_save = {
        let ctx = ctx.clone();
        move |_| {
            let snapshot = document.read().clone();
            match save_document(&ctx, &snapshot, *copy_on_save.read()) {
                Ok(()) => {
                    let mut d = dirty;
                    d.set(false);
                    info!("editor: saved");
                }
                Err(e) => warn!("save failed: {e}"),
            }
        }
    };

    let on_close = {
        let mut close_modal = close_modal;
        let dirty = dirty;
        move |_| {
            if *dirty.read() {
                close_modal.set(true);
            } else {
                dioxus::desktop::window().close();
            }
        }
    };

    rsx! {
        div { class: "topbar",
            div { class: "title",
                h1 {
                    "{png_name}"
                    if is_dirty {
                        span { class: "dirty", " ●" }
                    }
                }
                div { class: "sub", "{dims}" }
            }
            div { class: "actions",
                button {
                    class: "ghost",
                    disabled: !can_undo,
                    title: "Undo (Ctrl+Z)",
                    onclick: move |_| do_undo(document, history, dirty),
                    "↶ Undo"
                }
                button {
                    class: "ghost",
                    disabled: !can_redo,
                    title: "Redo (Ctrl+Y)",
                    onclick: move |_| do_redo(document, history, dirty),
                    "↷ Redo"
                }
                div { class: "divider" }
                label { class: "toggle",
                    input {
                        r#type: "checkbox",
                        checked: "{copy_state}",
                        onchange: move |evt| copy_on_save.set(evt.checked()),
                    }
                    "Copy on save"
                }
                button {
                    class: "primary",
                    title: "Save (Ctrl+S)",
                    onclick: on_save,
                    "Save"
                }
                button {
                    class: "ghost",
                    onclick: on_close,
                    "Close"
                }
            }
        }
    }
}

// ─── Tool palette ────────────────────────────────────────────────────

#[component]
fn ToolPalette(tool: Signal<Tool>, selected: Signal<Option<Uuid>>) -> Element {
    const TOOLS: &[(Tool, &str, &str)] = &[
        (Tool::Select, "↖", "Select"),
        (Tool::Arrow, "→", "Arrow"),
        (Tool::Text, "T", "Text"),
        (Tool::Rect, "▭", "Rect"),
        (Tool::Ellipse, "○", "Ellipse"),
        (Tool::Step, "❶", "Step"),
        (Tool::Magnify, "⌕", "Mag"),
        (Tool::Blur, "▦", "Blur"),
        (Tool::Callout, "💬", "Callout"),
        (Tool::CaptureInfo, "ⓘ", "Info"),
    ];

    rsx! {
        div { class: "tools",
            for (t, icon, label) in TOOLS.iter().copied() {
                {
                    let active = *tool.read() == t;
                    let cls = if active { "active" } else { "" };
                    rsx! {
                        button {
                            key: "{label}",
                            class: "{cls}",
                            title: "{label}",
                            onclick: move |_| {
                                tool.set(t);
                                if t != Tool::Select {
                                    selected.set(None);
                                }
                            },
                            div { "{icon}" }
                            div { class: "label", "{label}" }
                        }
                    }
                }
            }
        }
    }
}

// ─── Footer ──────────────────────────────────────────────────────────

#[component]
fn Footer(
    document: Signal<Document>,
    tool: Signal<Tool>,
    dirty: Signal<bool>,
    selected: Signal<Option<Uuid>>,
) -> Element {
    let tool_label = tool.read().label();
    let dirty_state = *dirty.read();
    // `read()` returns a Ref guard; deref + clone the inner Option
    // before calling combinators on it.
    let sel_label = (*selected.read())
        .map(|_| "selected".to_string())
        .unwrap_or_default();
    let dims = format!(
        "{} × {}",
        document.read().base_width,
        document.read().base_height
    );

    rsx! {
        div { class: "footer",
            div {
                span { "Tool: " }
                span { class: "pill", "{tool_label}" }
                if !sel_label.is_empty() {
                    span { "  •  {sel_label}" }
                }
            }
            div {
                span { class: if dirty_state { "status dirty" } else { "status ok" },
                    if dirty_state { "Unsaved changes" } else { "Saved" }
                }
                span { "  •  {dims}" }
            }
        }
    }
}

// ─── Save flow ───────────────────────────────────────────────────────

/// Run rasterize → write PNG → write `.grabit`. Both paths come from
/// `EditorContext`; `copy_clipboard` is honored at the end.
fn save_document(
    ctx: &EditorContext,
    doc: &Document,
    copy_clipboard: bool,
) -> Result<()> {
    use crate::editor::rasterize;

    let flat = rasterize::flatten(
        ctx.base_image.as_ref(),
        &doc.annotations,
        Some(&doc.metadata),
    );
    let composed = rasterize::apply_document_effects(flat, doc.edge_effect, doc.border);
    composed
        .save_with_format(&ctx.png_path, image::ImageFormat::Png)
        .with_context(|| format!("write PNG {}", ctx.png_path.display()))?;
    document::save(doc, &ctx.grabit_path)
        .with_context(|| format!("write sidecar {}", ctx.grabit_path.display()))?;

    if copy_clipboard {
        if let Err(e) = crate::export::copy_file_to_clipboard(&ctx.png_path) {
            warn!("editor: clipboard copy failed: {e}");
        }
    }

    info!(
        "editor: saved → {} (+ {})",
        ctx.png_path.display(),
        ctx.grabit_path.display()
    );
    Ok(())
}

// ─── Save-on-close confirmation modal ────────────────────────────────

#[component]
fn CloseConfirm(
    document: Signal<Document>,
    dirty: Signal<bool>,
    copy_on_save: Signal<bool>,
    close_modal: Signal<bool>,
) -> Element {
    let ctx = use_context::<EditorContext>();

    let on_save = {
        let ctx = ctx.clone();
        move |_| {
            let snapshot = document.read().clone();
            if let Err(e) = save_document(&ctx, &snapshot, *copy_on_save.read()) {
                warn!("save-and-close failed: {e}");
                return;
            }
            let mut d = dirty;
            d.set(false);
            dioxus::desktop::window().close();
        }
    };
    let on_discard = move |_| {
        dioxus::desktop::window().close();
    };
    let on_cancel = move |_| close_modal.set(false);

    rsx! {
        div { class: "modal-backdrop",
            div { class: "modal",
                h3 { "Unsaved changes" }
                p { "You have unsaved annotations. Save them before closing?" }
                div { class: "row-actions",
                    button { class: "ghost", onclick: on_cancel, "Cancel" }
                    button { class: "danger", onclick: on_discard, "Discard" }
                    button { class: "primary", onclick: on_save, "Save and close" }
                }
            }
        }
    }
}

// ═══ Canvas + SVG rendering + tool flows ═════════════════════════════

/// Convert a screen-space mouse coordinate (relative to the SVG
/// element's top-left) to image-space coordinates. The SVG `viewBox`
/// is set to `0 0 base_width base_height`, so the browser handles
/// the scaling for us — but pointer events report client-coordinates,
/// not viewBox-coordinates. We back into viewBox coords from the
/// element's bounding rect.
fn client_to_image(
    client_x: f64,
    client_y: f64,
    svg_rect: &dioxus::desktop::tao::dpi::PhysicalSize<u32>,
    base_w: u32,
    base_h: u32,
) -> [f32; 2] {
    // Fallback path used in tests; in practice the canvas calculates
    // from the rendered DOM element. Treat the rect as already image
    // space if dimensions match.
    let _ = svg_rect;
    [client_x as f32 * base_w as f32, client_y as f32 * base_h as f32]
}

#[component]
fn Canvas(
    document: Signal<Document>,
    history: Signal<History>,
    dirty: Signal<bool>,
    tool: Signal<Tool>,
    selected: Signal<Option<Uuid>>,
    pending: Signal<Option<Pending>>,
    style: Signal<ToolStyle>,
    editing_text: Signal<Option<Uuid>>,
) -> Element {
    let ctx = use_context::<EditorContext>();
    let base_w = document.read().base_width;
    let base_h = document.read().base_height;
    let view_box = format!("0 0 {} {}", base_w, base_h);
    let cur_tool = *tool.read();
    let wrap_class = format!("canvas-wrap tool-{}", cur_tool.label().to_lowercase());

    // Mouse handlers — convert client coordinates to image-space using
    // the event's offset_x/offset_y (which is relative to the target
    // element) and the SVG's viewBox. Since the SVG's viewBox is in
    // image-pixel units, the offset coordinates need to be divided by
    // the rendered display size and multiplied by base dimensions.
    //
    // Dioxus's `MouseEvent::client_coordinates` and friends give CSS
    // pixels relative to the viewport; what we want is the pointer
    // position inside the SVG element. The cleanest way is to use the
    // event's `offset` (target-relative) but Dioxus exposes
    // `client_coordinates()` and `element_coordinates()` — the latter
    // is what we need. Then we read the rendered SVG element's
    // bounding box via JS-injected calc, but we can avoid that by
    // letting CSS pin the SVG to a known max size and using the ratio
    // from `MouseEvent::element_coordinates()` (Dioxus normalizes by
    // the element's bounding rect).
    let on_pointerdown = move |evt: MouseEvent| {
        let p = mouse_to_image(&evt, base_w, base_h);
        canvas_pointerdown(
            p, cur_tool, document, history, dirty, selected, pending,
            style, editing_text,
        );
    };
    let on_pointermove = move |evt: MouseEvent| {
        let p = mouse_to_image(&evt, base_w, base_h);
        canvas_pointermove(p, pending, document);
    };
    let on_pointerup = move |evt: MouseEvent| {
        let p = mouse_to_image(&evt, base_w, base_h);
        canvas_pointerup(
            p, cur_tool, document, history, dirty, selected, pending,
            style, editing_text,
        );
    };

    rsx! {
        div { class: "{wrap_class}",
            svg {
                width: "{base_w}",
                height: "{base_h}",
                view_box: "{view_box}",
                preserve_aspect_ratio: "xMidYMid meet",
                onmousedown: on_pointerdown,
                onmousemove: on_pointermove,
                onmouseup: on_pointerup,
                oncontextmenu: |evt| evt.prevent_default(),

                // SVG <defs> for filters used by Blur / frosted text.
                {render_defs(&document.read())}
                // Base screenshot.
                image {
                    href: "{ctx.base_uri}",
                    x: "0",
                    y: "0",
                    width: "{base_w}",
                    height: "{base_h}",
                    preserve_aspect_ratio: "none",
                }
                // Annotation layer (committed nodes).
                {render_annotations(&document.read())}
                // Selection chrome (drawn over annotations).
                {render_selection(&document.read(), *selected.read(), *editing_text.read(), document, history, dirty)}
                // In-progress drag preview (drawn topmost).
                {render_pending(&pending.read(), &style.read(), cur_tool)}
            }
        }
    }
}

/// Translate a Dioxus mouse event into image-space coordinates.
/// Uses `element_coordinates` (target-element-relative) and the
/// rendered SVG dimensions; the SVG itself is sized via attributes
/// so its DOM size matches its viewBox, and the browser scales the
/// rendered element. We trust `element_coordinates` to already
/// account for that scaling.
fn mouse_to_image(evt: &MouseEvent, _base_w: u32, _base_h: u32) -> [f32; 2] {
    let p = evt.element_coordinates();
    [p.x as f32, p.y as f32]
}

// ─── Canvas pointer handlers ─────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn canvas_pointerdown(
    p: [f32; 2],
    tool: Tool,
    document: Signal<Document>,
    history: Signal<History>,
    dirty: Signal<bool>,
    mut selected: Signal<Option<Uuid>>,
    mut pending: Signal<Option<Pending>>,
    style: Signal<ToolStyle>,
    mut editing_text: Signal<Option<Uuid>>,
) {
    // Clear text-edit if clicking outside the editing rect.
    if editing_text.read().is_some() {
        editing_text.set(None);
    }

    if matches!(tool, Tool::Select) {
        // Hit-test from top-of-stack down; first hit wins.
        let hit = hit_test(&document.read(), p);
        if let Some(id) = hit {
            // Check if the click landed on a resize handle of the
            // already-selected annotation.
            if Some(id) == *selected.read() {
                if let Some(node) = find_node(&document.read(), id) {
                    if let Some(handle) = handle_at(p, &node) {
                        pending.set(Some(Pending::Resize {
                            target: id,
                            start_node: node.clone(),
                            start_mouse: p,
                            cur_mouse: p,
                            handle,
                        }));
                        return;
                    }
                }
            }
            selected.set(Some(id));
            // Begin a move drag.
            if let Some(node) = find_node(&document.read(), id) {
                pending.set(Some(Pending::Move {
                    target: id,
                    start_node: node.clone(),
                    start_mouse: p,
                    cur_mouse: p,
                }));
            }
        } else {
            selected.set(None);
        }
        return;
    }

    if matches!(tool, Tool::CaptureInfo) {
        // Single-click: place a capture-info banner at the chosen
        // corner. The position field on CaptureInfo is the corner; we
        // use the user's stylesheet's last position.
        let st = style.read();
        let node = AnnotationNode::CaptureInfo {
            id: Uuid::new_v4(),
            position: st.capture_info_position,
            fields: st.capture_info_fields.clone(),
            style: CaptureInfoStyle::default(),
        };
        execute_command(document, history, dirty, Box::new(AddAnnotation::new(node)));
        return;
    }

    if matches!(tool, Tool::Step) {
        // Single-click: place a step circle. Auto-increment the
        // tracked next step number.
        let st = style.read();
        let n = st.next_step_number;
        let node = AnnotationNode::Step {
            id: Uuid::new_v4(),
            center: p,
            radius: st.step_radius,
            number: n,
            fill: st.step_fill,
            text_color: st.step_text_color,
        };
        drop(st);
        execute_command(document, history, dirty, Box::new(AddAnnotation::new(node)));
        // Bump next step number for the next click.
        let mut s = style;
        s.with_mut(|st| {
            st.next_step_number = n.saturating_add(1);
        });
        return;
    }

    // All other tools start a two-corner drag.
    pending.set(Some(Pending::Rect {
        start: p,
        cur: p,
    }));
}

fn canvas_pointermove(p: [f32; 2], mut pending: Signal<Option<Pending>>, mut document: Signal<Document>) {
    let mut should_set = None;
    if let Some(pen) = pending.read().clone() {
        match pen {
            Pending::Rect { start, .. } => {
                should_set = Some(Pending::Rect { start, cur: p });
            }
            Pending::Move { target, start_node, start_mouse, .. } => {
                let dx = p[0] - start_mouse[0];
                let dy = p[1] - start_mouse[1];
                // Live update the document so the user sees the move.
                document.with_mut(|d| {
                    if let Some(node) = d.annotations.iter_mut().find(|n| n.id() == target) {
                        translate_node(node, dx, dy, &start_node);
                    }
                });
                should_set = Some(Pending::Move {
                    target,
                    start_node,
                    start_mouse,
                    cur_mouse: p,
                });
            }
            Pending::Resize { target, start_node, start_mouse, handle, .. } => {
                document.with_mut(|d| {
                    if let Some(node) = d.annotations.iter_mut().find(|n| n.id() == target) {
                        resize_node(node, &start_node, start_mouse, p, handle);
                    }
                });
                should_set = Some(Pending::Resize {
                    target,
                    start_node,
                    start_mouse,
                    cur_mouse: p,
                    handle,
                });
            }
            Pending::None => {}
        }
    }
    if let Some(s) = should_set {
        pending.set(Some(s));
    }
}

#[allow(clippy::too_many_arguments)]
fn canvas_pointerup(
    p: [f32; 2],
    tool: Tool,
    document: Signal<Document>,
    history: Signal<History>,
    dirty: Signal<bool>,
    selected: Signal<Option<Uuid>>,
    mut pending: Signal<Option<Pending>>,
    style: Signal<ToolStyle>,
    mut editing_text: Signal<Option<Uuid>>,
) {
    let pen = pending.read().clone();
    pending.set(None);

    let Some(pen) = pen else { return };
    match pen {
        Pending::Rect { start, .. } => {
            let cur = p;
            let dx = (cur[0] - start[0]).abs();
            let dy = (cur[1] - start[1]).abs();
            // Tools that need a true drag — discard tap-clicks below
            // a 4-pixel threshold so we don't make degenerate shapes.
            let min_drag = 4.0;
            if !matches!(tool, Tool::Arrow) && (dx < min_drag || dy < min_drag) {
                return;
            }
            if matches!(tool, Tool::Arrow) && (dx < min_drag && dy < min_drag) {
                return;
            }
            let node = build_node_from_drag(tool, start, cur, &style.read());
            if let Some(node) = node {
                let new_id = node.id();
                execute_command(
                    document,
                    history,
                    dirty,
                    Box::new(AddAnnotation::new(node)),
                );
                // For Text, jump straight into edit mode.
                if matches!(tool, Tool::Text) {
                    editing_text.set(Some(new_id));
                    let mut sel = selected;
                    sel.set(Some(new_id));
                }
            }
        }
        Pending::Move { target, start_node, cur_mouse, start_mouse, .. } => {
            // Finalize the move via UpdateAnnotation so undo/redo
            // captures the begin/end states. The live-translate during
            // pointermove already mutated the doc; undoing the live
            // mutations + applying the command keeps the history
            // clean.
            let dx = cur_mouse[0] - start_mouse[0];
            let dy = cur_mouse[1] - start_mouse[1];
            if dx.abs() < 0.5 && dy.abs() < 0.5 {
                // No real move — revert the pointermove mutations.
                let mut d = document;
                d.with_mut(|doc| {
                    if let Some(node) = doc.annotations.iter_mut().find(|n| n.id() == target) {
                        *node = start_node.clone();
                    }
                });
                return;
            }
            let mut after = start_node.clone();
            translate_node(&mut after, dx, dy, &start_node);
            // Reset live state to start, then push command to drive to after.
            let mut d = document;
            d.with_mut(|doc| {
                if let Some(node) = doc.annotations.iter_mut().find(|n| n.id() == target) {
                    *node = start_node.clone();
                }
            });
            execute_command(
                document,
                history,
                dirty,
                Box::new(UpdateAnnotation::new(start_node, after)),
            );
            let _ = selected;
        }
        Pending::Resize { target, start_node, cur_mouse, start_mouse, handle, .. } => {
            if (cur_mouse[0] - start_mouse[0]).abs() < 0.5
                && (cur_mouse[1] - start_mouse[1]).abs() < 0.5
            {
                let mut d = document;
                d.with_mut(|doc| {
                    if let Some(node) = doc.annotations.iter_mut().find(|n| n.id() == target) {
                        *node = start_node.clone();
                    }
                });
                return;
            }
            let mut after = start_node.clone();
            resize_node(&mut after, &start_node, start_mouse, cur_mouse, handle);
            let mut d = document;
            d.with_mut(|doc| {
                if let Some(node) = doc.annotations.iter_mut().find(|n| n.id() == target) {
                    *node = start_node.clone();
                }
            });
            execute_command(
                document,
                history,
                dirty,
                Box::new(UpdateAnnotation::new(start_node, after)),
            );
            let _ = selected;
        }
        Pending::None => {}
    }
}

// ─── Tool drag-end → AnnotationNode ──────────────────────────────────

fn build_node_from_drag(
    tool: Tool,
    start: [f32; 2],
    end: [f32; 2],
    style: &ToolStyle,
) -> Option<AnnotationNode> {
    let id = Uuid::new_v4();
    let rect = [
        start[0].min(end[0]),
        start[1].min(end[1]),
        start[0].max(end[0]),
        start[1].max(end[1]),
    ];
    Some(match tool {
        Tool::Arrow => AnnotationNode::Arrow {
            id,
            start,
            end,
            color: style.arrow_color,
            thickness: style.arrow_thickness,
            shadow: style.arrow_shadow,
            line_style: style.arrow_line_style,
            head_style: style.arrow_head_style,
            control: None,
        },
        Tool::Text => AnnotationNode::Text {
            id,
            rect,
            text: String::new(),
            color: style.text_color,
            size_px: style.text_size,
            frosted: style.text_frosted,
            shadow: style.text_shadow,
            align: style.text_align,
            list: style.text_list,
        },
        Tool::Rect => AnnotationNode::Shape {
            id,
            shape: ShapeKind::Rect,
            rect,
            stroke: style.shape_stroke,
            stroke_width: style.shape_stroke_width,
            fill: style.shape_fill,
        },
        Tool::Ellipse => AnnotationNode::Shape {
            id,
            shape: ShapeKind::Ellipse,
            rect,
            stroke: style.shape_stroke,
            stroke_width: style.shape_stroke_width,
            fill: style.shape_fill,
        },
        Tool::Blur => AnnotationNode::Blur {
            id,
            rect,
            radius_px: style.blur_radius,
        },
        Tool::Magnify => {
            // Drag rect = source. Target rect snaps to the same drag
            // rect at the chosen zoom factor relative to the source's
            // dimensions. User can drag the magnify rect later to
            // reposition.
            let src = rect;
            let sw = src[2] - src[0];
            let sh = src[3] - src[1];
            let tw = sw * style.magnify_zoom;
            let th = sh * style.magnify_zoom;
            // Place target offset to the bottom-right of the source.
            let target = [
                src[2] + 12.0,
                src[3] + 12.0,
                src[2] + 12.0 + tw,
                src[3] + 12.0 + th,
            ];
            AnnotationNode::Magnify {
                id,
                source_rect: src,
                target_rect: target,
                border: style.magnify_border,
                border_width: style.magnify_border_width,
                circular: style.magnify_circular,
            }
        }
        Tool::Callout => AnnotationNode::Callout {
            id,
            rect,
            tail: [rect[0] - 30.0, rect[3] + 30.0],
            text: String::new(),
            fill: style.callout_fill,
            stroke: style.callout_stroke,
            stroke_width: 2.0,
            text_color: style.callout_text_color,
            text_size: 16.0,
        },
        // The single-click tools handle their own creation in
        // canvas_pointerdown; reaching here means a stray drag.
        Tool::Step | Tool::CaptureInfo | Tool::Select => return None,
    })
}

// ─── Hit-testing + handles ───────────────────────────────────────────

fn hit_test(doc: &Document, p: [f32; 2]) -> Option<Uuid> {
    // Iterate top-of-stack first.
    for node in doc.annotations.iter().rev() {
        if hit_node(node, p) {
            return Some(node.id());
        }
    }
    None
}

fn hit_node(node: &AnnotationNode, p: [f32; 2]) -> bool {
    match node {
        AnnotationNode::Arrow { start, end, thickness, .. } => {
            distance_to_segment(p, *start, *end) < (*thickness * 0.6 + 6.0)
        }
        AnnotationNode::Text { rect, .. }
        | AnnotationNode::Shape { rect, .. }
        | AnnotationNode::Blur { rect, .. }
        | AnnotationNode::Callout { rect, .. } => point_in_rect(p, *rect),
        AnnotationNode::Step { center, radius, .. } => {
            let dx = p[0] - center[0];
            let dy = p[1] - center[1];
            (dx * dx + dy * dy).sqrt() <= *radius
        }
        AnnotationNode::Magnify { target_rect, .. } => point_in_rect(p, *target_rect),
        AnnotationNode::Stamp { rect, .. } => point_in_rect(p, *rect),
        AnnotationNode::CaptureInfo { .. } => false, // Not selectable for now
    }
}

fn point_in_rect(p: [f32; 2], r: [f32; 4]) -> bool {
    p[0] >= r[0] && p[0] <= r[2] && p[1] >= r[1] && p[1] <= r[3]
}

fn distance_to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let l2 = dx * dx + dy * dy;
    if l2 == 0.0 {
        let dx0 = p[0] - a[0];
        let dy0 = p[1] - a[1];
        return (dx0 * dx0 + dy0 * dy0).sqrt();
    }
    let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0);
    let cx = a[0] + t * dx;
    let cy = a[1] + t * dy;
    let dx0 = p[0] - cx;
    let dy0 = p[1] - cy;
    (dx0 * dx0 + dy0 * dy0).sqrt()
}

fn find_node(doc: &Document, id: Uuid) -> Option<&AnnotationNode> {
    doc.annotations.iter().find(|n| n.id() == id)
}

fn handle_at(p: [f32; 2], node: &AnnotationNode) -> Option<ResizeHandle> {
    let rect = match node {
        AnnotationNode::Arrow { start, end, .. } => {
            // Arrows have only two endpoint handles; treat the start
            // as NW and end as SE for this lookup.
            let r = 12.0;
            if (p[0] - start[0]).abs() < r && (p[1] - start[1]).abs() < r {
                return Some(ResizeHandle::Nw);
            }
            if (p[0] - end[0]).abs() < r && (p[1] - end[1]).abs() < r {
                return Some(ResizeHandle::Se);
            }
            return None;
        }
        AnnotationNode::Text { rect, .. }
        | AnnotationNode::Shape { rect, .. }
        | AnnotationNode::Blur { rect, .. }
        | AnnotationNode::Callout { rect, .. }
        | AnnotationNode::Stamp { rect, .. } => *rect,
        AnnotationNode::Magnify { target_rect, .. } => *target_rect,
        AnnotationNode::Step { center, radius, .. } => {
            // Single radius-handle to the east.
            let r = 12.0;
            if (p[0] - (center[0] + radius)).abs() < r && (p[1] - center[1]).abs() < r {
                return Some(ResizeHandle::E);
            }
            return None;
        }
        AnnotationNode::CaptureInfo { .. } => return None,
    };
    let hr = 12.0;
    let near = |hx: f32, hy: f32| (p[0] - hx).abs() < hr && (p[1] - hy).abs() < hr;
    let cx = (rect[0] + rect[2]) * 0.5;
    let cy = (rect[1] + rect[3]) * 0.5;
    if near(rect[0], rect[1]) { return Some(ResizeHandle::Nw); }
    if near(cx, rect[1]) { return Some(ResizeHandle::N); }
    if near(rect[2], rect[1]) { return Some(ResizeHandle::Ne); }
    if near(rect[2], cy) { return Some(ResizeHandle::E); }
    if near(rect[2], rect[3]) { return Some(ResizeHandle::Se); }
    if near(cx, rect[3]) { return Some(ResizeHandle::S); }
    if near(rect[0], rect[3]) { return Some(ResizeHandle::Sw); }
    if near(rect[0], cy) { return Some(ResizeHandle::W); }
    None
}

fn translate_node(node: &mut AnnotationNode, dx: f32, dy: f32, base: &AnnotationNode) {
    match (node, base) {
        (
            AnnotationNode::Arrow { start, end, control, .. },
            AnnotationNode::Arrow { start: bs, end: be, control: bc, .. },
        ) => {
            *start = [bs[0] + dx, bs[1] + dy];
            *end = [be[0] + dx, be[1] + dy];
            if let (Some(cp), Some(bcp)) = (control.as_mut(), bc) {
                *cp = [bcp[0] + dx, bcp[1] + dy];
            }
        }
        (AnnotationNode::Text { rect, .. }, AnnotationNode::Text { rect: br, .. })
        | (AnnotationNode::Shape { rect, .. }, AnnotationNode::Shape { rect: br, .. })
        | (AnnotationNode::Blur { rect, .. }, AnnotationNode::Blur { rect: br, .. })
        | (AnnotationNode::Stamp { rect, .. }, AnnotationNode::Stamp { rect: br, .. }) => {
            *rect = [br[0] + dx, br[1] + dy, br[2] + dx, br[3] + dy];
        }
        (
            AnnotationNode::Callout { rect, tail, .. },
            AnnotationNode::Callout { rect: br, tail: bt, .. },
        ) => {
            *rect = [br[0] + dx, br[1] + dy, br[2] + dx, br[3] + dy];
            *tail = [bt[0] + dx, bt[1] + dy];
        }
        (AnnotationNode::Step { center, .. }, AnnotationNode::Step { center: bc, .. }) => {
            *center = [bc[0] + dx, bc[1] + dy];
        }
        (
            AnnotationNode::Magnify { source_rect, target_rect, .. },
            AnnotationNode::Magnify { source_rect: bs, target_rect: bt, .. },
        ) => {
            // Move both rects together so the magnifier's offset is
            // preserved.
            *source_rect = [bs[0] + dx, bs[1] + dy, bs[2] + dx, bs[3] + dy];
            *target_rect = [bt[0] + dx, bt[1] + dy, bt[2] + dx, bt[3] + dy];
        }
        _ => {}
    }
}

fn resize_node(
    node: &mut AnnotationNode,
    base: &AnnotationNode,
    start_mouse: [f32; 2],
    cur_mouse: [f32; 2],
    handle: ResizeHandle,
) {
    let dx = cur_mouse[0] - start_mouse[0];
    let dy = cur_mouse[1] - start_mouse[1];

    if let (
        AnnotationNode::Arrow { start, end, .. },
        AnnotationNode::Arrow { start: bs, end: be, .. },
    ) = (&mut *node, base)
    {
        match handle {
            ResizeHandle::Nw => {
                *start = [bs[0] + dx, bs[1] + dy];
                *end = *be;
            }
            ResizeHandle::Se => {
                *start = *bs;
                *end = [be[0] + dx, be[1] + dy];
            }
            _ => {}
        }
        return;
    }

    if let AnnotationNode::Step { center, radius, .. } = node {
        if let AnnotationNode::Step { center: bc, radius: br, .. } = base {
            if matches!(handle, ResizeHandle::E) {
                *center = *bc;
                *radius = (br + dx).max(2.0);
            }
        }
        return;
    }

    let base_rect = match base {
        AnnotationNode::Text { rect, .. }
        | AnnotationNode::Shape { rect, .. }
        | AnnotationNode::Blur { rect, .. }
        | AnnotationNode::Callout { rect, .. }
        | AnnotationNode::Stamp { rect, .. } => *rect,
        AnnotationNode::Magnify { target_rect, .. } => *target_rect,
        _ => return,
    };
    let new_rect = resize_rect(base_rect, dx, dy, handle);

    match node {
        AnnotationNode::Text { rect, .. }
        | AnnotationNode::Shape { rect, .. }
        | AnnotationNode::Blur { rect, .. }
        | AnnotationNode::Callout { rect, .. }
        | AnnotationNode::Stamp { rect, .. } => {
            *rect = new_rect;
        }
        AnnotationNode::Magnify { target_rect, .. } => {
            *target_rect = new_rect;
        }
        _ => {}
    }
}

fn resize_rect(r: [f32; 4], dx: f32, dy: f32, handle: ResizeHandle) -> [f32; 4] {
    let mut x0 = r[0];
    let mut y0 = r[1];
    let mut x1 = r[2];
    let mut y1 = r[3];
    match handle {
        ResizeHandle::Nw => {
            x0 += dx;
            y0 += dy;
        }
        ResizeHandle::N => {
            y0 += dy;
        }
        ResizeHandle::Ne => {
            x1 += dx;
            y0 += dy;
        }
        ResizeHandle::E => {
            x1 += dx;
        }
        ResizeHandle::Se => {
            x1 += dx;
            y1 += dy;
        }
        ResizeHandle::S => {
            y1 += dy;
        }
        ResizeHandle::Sw => {
            x0 += dx;
            y1 += dy;
        }
        ResizeHandle::W => {
            x0 += dx;
        }
    }
    // Normalize so x0<x1, y0<y1.
    [x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)]
}

// ─── SVG render helpers ──────────────────────────────────────────────

fn rgba_to_css(c: [u8; 4]) -> String {
    format!(
        "rgba({}, {}, {}, {:.3})",
        c[0],
        c[1],
        c[2],
        c[3] as f32 / 255.0
    )
}

fn render_defs(_doc: &Document) -> Element {
    // SVG filter primitives like <feGaussianBlur> aren't first-class in
    // Dioxus's rsx (the macro can't take quoted hyphenated tags), and
    // building them via raw HTML is messy. The export rasterizer
    // (`crate::editor::rasterize`) already does the real shadow / blur
    // / frosted work at flatten-time; in the editor preview we approx
    // them with cheaper styling (alpha / dashed overlays / etc.).
    rsx! {
        defs {}
    }
}

fn render_annotations(doc: &Document) -> Element {
    rsx! {
        for node in doc.annotations.iter() {
            {render_node(node, doc)}
        }
    }
}

fn render_node(node: &AnnotationNode, doc: &Document) -> Element {
    match node {
        AnnotationNode::Arrow {
            id, start, end, color, thickness, shadow, line_style, head_style, control,
        } => render_arrow(*id, *start, *end, *color, *thickness, *shadow, *line_style, *head_style, *control),
        AnnotationNode::Text {
            id, rect, text, color, size_px, frosted, shadow, align, list,
        } => render_text(*id, *rect, text, *color, *size_px, *frosted, *shadow, *align, *list),
        AnnotationNode::Shape { id, shape, rect, stroke, stroke_width, fill } => {
            render_shape(*id, *shape, *rect, *stroke, *stroke_width, *fill)
        }
        AnnotationNode::Step { id, center, radius, number, fill, text_color } => {
            render_step(*id, *center, *radius, *number, *fill, *text_color)
        }
        AnnotationNode::Blur { id, rect, .. } => render_blur(*id, *rect),
        AnnotationNode::Callout { id, rect, tail, text, fill, stroke, stroke_width, text_color, text_size } => {
            render_callout(*id, *rect, *tail, text, *fill, *stroke, *stroke_width, *text_color, *text_size)
        }
        AnnotationNode::Magnify { id, source_rect, target_rect, border, border_width, circular } => {
            let ctx = use_context::<EditorContext>();
            render_magnify(
                *id,
                *source_rect,
                *target_rect,
                *border,
                *border_width,
                *circular,
                doc.base_width,
                doc.base_height,
                &ctx.base_uri,
            )
        }
        AnnotationNode::Stamp { id, rect, .. } => render_stamp_placeholder(*id, *rect),
        AnnotationNode::CaptureInfo { id, position, fields, style } => {
            render_capture_info(*id, *position, fields, *style, doc)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_arrow(
    id: Uuid,
    start: [f32; 2],
    end: [f32; 2],
    color: [u8; 4],
    thickness: f32,
    shadow: bool,
    line_style: ArrowLineStyle,
    head_style: ArrowHeadStyle,
    control: Option<[f32; 2]>,
) -> Element {
    let stroke_color = rgba_to_css(color);
    let dash = match line_style {
        ArrowLineStyle::Solid => String::new(),
        ArrowLineStyle::Dashed => format!("{:.1} {:.1}", thickness * 2.5, thickness * 1.5),
        ArrowLineStyle::Dotted => format!("0 {:.1}", thickness * 1.5),
    };
    let path_d = match control {
        Some(c) => format!(
            "M {} {} Q {} {} {} {}",
            start[0], start[1], c[0], c[1], end[0], end[1]
        ),
        None => format!("M {} {} L {} {}", start[0], start[1], end[0], end[1]),
    };
    let head_polys = compute_arrow_heads(start, end, thickness, head_style, control);
    // Real shadow lives in rasterize::draw_arrow / draw_text_shadow at
    // export time; preview just renders without the effect.
    let filter = if shadow { "" } else { "" };
    let hit_id = format!("hit-{}", id);
    rsx! {
        g {
            // Visible shaft + head
            path {
                d: "{path_d}",
                stroke: "{stroke_color}",
                "stroke-width": "{thickness}",
                "stroke-linecap": "round",
                "stroke-dasharray": "{dash}",
                fill: "none",
                filter: "{filter}",
            }
            for (i, poly) in head_polys.iter().enumerate() {
                {
                    let pts = poly_to_string(poly);
                    let key = format!("ah-{}-{}", id, i);
                    rsx! {
                        polygon {
                            key: "{key}",
                            points: "{pts}",
                            fill: "{stroke_color}",
                            stroke: "{stroke_color}",
                            "stroke-width": "1",
                            filter: "{filter}",
                        }
                    }
                }
            }
            // Wide invisible hit target (path-stroke based) so thin
            // arrows are still selectable.
            path {
                id: "{hit_id}",
                class: "annotation-hit",
                d: "{path_d}",
            }
        }
    }
}

/// Compute arrow-head triangle vertices in image-space. Returns a vec
/// because DoubleEnded yields two triangles. None for `Head::None`.
fn compute_arrow_heads(
    start: [f32; 2],
    end: [f32; 2],
    thickness: f32,
    head_style: ArrowHeadStyle,
    control: Option<[f32; 2]>,
) -> Vec<Vec<[f32; 2]>> {
    let head_len = (thickness * 4.0).max(14.0);
    let head_half = head_len * 0.55;
    // Tangent at the end-point: direction from control (or start) to end.
    let tail_anchor = control.unwrap_or(start);
    let mut polys = Vec::new();
    match head_style {
        ArrowHeadStyle::None => {}
        ArrowHeadStyle::FilledTriangle | ArrowHeadStyle::OutlineTriangle => {
            polys.push(triangle_at(end, tail_anchor, head_len, head_half));
        }
        ArrowHeadStyle::LineOnly => {
            polys.push(line_chevron_at(end, tail_anchor, head_len, head_half));
        }
        ArrowHeadStyle::DoubleEnded => {
            polys.push(triangle_at(end, tail_anchor, head_len, head_half));
            polys.push(triangle_at(start, end, head_len, head_half));
        }
    }
    polys
}

fn triangle_at(tip: [f32; 2], from: [f32; 2], len: f32, half_w: f32) -> Vec<[f32; 2]> {
    let dx = tip[0] - from[0];
    let dy = tip[1] - from[1];
    let l = (dx * dx + dy * dy).sqrt().max(1.0);
    let ux = dx / l;
    let uy = dy / l;
    // Perpendicular
    let px = -uy;
    let py = ux;
    let bx = tip[0] - ux * len;
    let by = tip[1] - uy * len;
    vec![
        tip,
        [bx + px * half_w, by + py * half_w],
        [bx - px * half_w, by - py * half_w],
    ]
}

fn line_chevron_at(tip: [f32; 2], from: [f32; 2], len: f32, half_w: f32) -> Vec<[f32; 2]> {
    let tri = triangle_at(tip, from, len, half_w);
    // Two short strokes from the base endpoints to the tip — render
    // as a thin polygon connecting them like an open chevron.
    vec![tri[1], tip, tri[2]]
}

fn poly_to_string(poly: &[[f32; 2]]) -> String {
    poly.iter()
        .map(|p| format!("{:.1},{:.1}", p[0], p[1]))
        .collect::<Vec<_>>()
        .join(" ")
}

#[allow(clippy::too_many_arguments)]
fn render_text(
    id: Uuid,
    rect: [f32; 4],
    text: &str,
    color: [u8; 4],
    size_px: f32,
    frosted: bool,
    shadow: bool,
    align: TextAlign,
    list: TextListStyle,
) -> Element {
    let x = rect[0];
    let y = rect[1];
    let w = (rect[2] - rect[0]).max(1.0);
    let h = (rect[3] - rect[1]).max(1.0);
    let fill = rgba_to_css(color);
    let anchor = match align {
        TextAlign::Left => "start",
        TextAlign::Center => "middle",
        TextAlign::Right => "end",
    };
    let tx = match align {
        TextAlign::Left => x + 4.0,
        TextAlign::Center => x + w * 0.5,
        TextAlign::Right => x + w - 4.0,
    };
    // Compose lines with markers if a list style is set.
    let lines = compose_text_lines(text, list);
    let line_h = size_px * 1.25;
    // Real shadow lives in rasterize::draw_arrow / draw_text_shadow at
    // export time; preview just renders without the effect.
    let filter = if shadow { "" } else { "" };

    // Frosted backdrop = a translucent white rect under the text. We
    // skip the actual gaussian-blur of the underlying image at preview
    // time (rasterize handles the real effect on export).
    let backdrop = if frosted {
        rsx! {
            rect {
                x: "{x}",
                y: "{y}",
                width: "{w}",
                height: "{h}",
                fill: "rgba(255,255,255,0.55)",
                rx: "2",
            }
        }
    } else {
        rsx! {}
    };

    rsx! {
        g {
            {backdrop}
            text {
                x: "{tx}",
                y: "{y + size_px * 0.95}",
                "font-family": "JetBrains Mono, Cascadia Mono, Consolas, monospace",
                "font-size": "{size_px}",
                fill: "{fill}",
                "text-anchor": "{anchor}",
                filter: "{filter}",
                for (i, line) in lines.iter().enumerate() {
                    {
                        let dy = if i == 0 { 0.0 } else { line_h };
                        let key = format!("ln-{}-{}", id, i);
                        rsx! {
                            tspan {
                                key: "{key}",
                                x: "{tx}",
                                dy: "{dy}",
                                "{line}"
                            }
                        }
                    }
                }
            }
        }
    }
}

fn compose_text_lines(text: &str, list: TextListStyle) -> Vec<String> {
    let mut out = Vec::new();
    let mut count = 1u32;
    for line in text.split('\n') {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        let prefixed = match list {
            TextListStyle::None => line.to_string(),
            TextListStyle::Bullet => format!("• {}", line),
            TextListStyle::Numbered => {
                let s = format!("{}. {}", count, line);
                count += 1;
                s
            }
        };
        out.push(prefixed);
    }
    out
}

fn render_shape(
    id: Uuid,
    shape: ShapeKind,
    rect: [f32; 4],
    stroke: [u8; 4],
    stroke_width: f32,
    fill: [u8; 4],
) -> Element {
    let stroke_css = rgba_to_css(stroke);
    let fill_css = if fill[3] == 0 { "none".to_string() } else { rgba_to_css(fill) };
    let x = rect[0];
    let y = rect[1];
    let w = (rect[2] - rect[0]).max(1.0);
    let h = (rect[3] - rect[1]).max(1.0);
    match shape {
        ShapeKind::Rect => rsx! {
            rect {
                key: "shape-{id}",
                x: "{x}",
                y: "{y}",
                width: "{w}",
                height: "{h}",
                fill: "{fill_css}",
                stroke: "{stroke_css}",
                "stroke-width": "{stroke_width}",
                rx: "2",
            }
        },
        ShapeKind::Ellipse => {
            let cx = x + w * 0.5;
            let cy = y + h * 0.5;
            let rx = w * 0.5;
            let ry = h * 0.5;
            rsx! {
                ellipse {
                    key: "shape-{id}",
                    cx: "{cx}",
                    cy: "{cy}",
                    rx: "{rx}",
                    ry: "{ry}",
                    fill: "{fill_css}",
                    stroke: "{stroke_css}",
                    "stroke-width": "{stroke_width}",
                }
            }
        }
    }
}

fn render_step(
    id: Uuid,
    center: [f32; 2],
    radius: f32,
    number: u32,
    fill: [u8; 4],
    text_color: [u8; 4],
) -> Element {
    let cx = center[0];
    let cy = center[1];
    let fill_css = rgba_to_css(fill);
    let text_css = rgba_to_css(text_color);
    let font_size = radius * 1.1;
    let label_y = cy + font_size * 0.34;
    rsx! {
        g {
            circle {
                key: "step-{id}",
                cx: "{cx}",
                cy: "{cy}",
                r: "{radius}",
                fill: "{fill_css}",
                stroke: "rgba(0,0,0,0.3)",
                "stroke-width": "1",
            }
            text {
                x: "{cx}",
                y: "{label_y}",
                "text-anchor": "middle",
                "font-family": "JetBrains Mono, Cascadia Mono, Consolas, monospace",
                "font-weight": "700",
                "font-size": "{font_size}",
                fill: "{text_css}",
                "{number}"
            }
        }
    }
}

fn render_blur(id: Uuid, rect: [f32; 4]) -> Element {
    // Preview-only stippled overlay. The real gaussian blur lives in
    // rasterize.rs and only fires at flatten/export time.
    let x = rect[0];
    let y = rect[1];
    let w = (rect[2] - rect[0]).max(1.0);
    let h = (rect[3] - rect[1]).max(1.0);
    rsx! {
        rect {
            key: "blur-{id}",
            x: "{x}",
            y: "{y}",
            width: "{w}",
            height: "{h}",
            fill: "rgba(120,120,140,0.45)",
            stroke: "rgba(120,120,180,0.85)",
            "stroke-width": "1.5",
            "stroke-dasharray": "4 3",
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_callout(
    id: Uuid,
    rect: [f32; 4],
    tail: [f32; 2],
    text: &str,
    fill: [u8; 4],
    stroke: [u8; 4],
    stroke_width: f32,
    text_color: [u8; 4],
    text_size: f32,
) -> Element {
    let x = rect[0];
    let y = rect[1];
    let w = (rect[2] - rect[0]).max(10.0);
    let h = (rect[3] - rect[1]).max(10.0);
    let fill_css = rgba_to_css(fill);
    let stroke_css = rgba_to_css(stroke);
    let text_css = rgba_to_css(text_color);

    // Tail is a triangle from a base on the rect edge nearest the tip.
    let cx = x + w * 0.5;
    let cy = y + h * 0.5;
    let dx = tail[0] - cx;
    let dy = tail[1] - cy;
    // Pick the nearest rect edge midpoint to anchor the tail base.
    let (bx, by) = if dx.abs() > dy.abs() {
        if dx > 0.0 { (x + w, cy) } else { (x, cy) }
    } else if dy > 0.0 { (cx, y + h) } else { (cx, y) };
    let perp = if dx.abs() > dy.abs() { (0.0, h * 0.18) } else { (w * 0.18, 0.0) };
    let p1 = (bx + perp.0, by + perp.1);
    let p2 = (bx - perp.0, by - perp.1);
    let tail_pts = format!(
        "{:.1},{:.1} {:.1},{:.1} {:.1},{:.1}",
        p1.0, p1.1, p2.0, p2.1, tail[0], tail[1]
    );

    rsx! {
        g {
            polygon {
                key: "ct-{id}",
                points: "{tail_pts}",
                fill: "{fill_css}",
                stroke: "{stroke_css}",
                "stroke-width": "{stroke_width}",
            }
            rect {
                x: "{x}",
                y: "{y}",
                width: "{w}",
                height: "{h}",
                rx: "8",
                fill: "{fill_css}",
                stroke: "{stroke_css}",
                "stroke-width": "{stroke_width}",
            }
            text {
                x: "{x + 8.0}",
                y: "{y + text_size + 4.0}",
                "font-family": "JetBrains Mono, Cascadia Mono, Consolas, monospace",
                "font-size": "{text_size}",
                fill: "{text_css}",
                "{text}"
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_magnify(
    id: Uuid,
    source_rect: [f32; 4],
    target_rect: [f32; 4],
    border: [u8; 4],
    border_width: f32,
    circular: bool,
    base_w: u32,
    base_h: u32,
    base_uri: &str,
) -> Element {
    // Implement the magnifier as a transformed-and-clipped <image>
    // element. Source rect's pixels are scaled to target rect via an
    // SVG transform, and a clipPath restricts visible pixels to the
    // target rect (rect or ellipse).
    let sx = source_rect[0];
    let sy = source_rect[1];
    let sw = (source_rect[2] - source_rect[0]).max(1.0);
    let sh = (source_rect[3] - source_rect[1]).max(1.0);
    let tx = target_rect[0];
    let ty = target_rect[1];
    let tw = (target_rect[2] - target_rect[0]).max(1.0);
    let th = (target_rect[3] - target_rect[1]).max(1.0);
    let scale_x = tw / sw;
    let scale_y = th / sh;
    let img_w = base_w as f32 * scale_x;
    let img_h = base_h as f32 * scale_y;
    let img_x = tx - sx * scale_x;
    let img_y = ty - sy * scale_y;
    let clip_id = format!("mag-clip-{}", id);
    let border_css = rgba_to_css(border);

    rsx! {
        g {
            defs {
                clipPath {
                    id: "{clip_id}",
                    if circular {
                        ellipse {
                            cx: "{tx + tw * 0.5}",
                            cy: "{ty + th * 0.5}",
                            rx: "{tw * 0.5}",
                            ry: "{th * 0.5}",
                        }
                    } else {
                        rect { x: "{tx}", y: "{ty}", width: "{tw}", height: "{th}", rx: "4" }
                    }
                }
            }
            // Magnified image clipped to target rect.
            image {
                href: "{base_uri}",
                x: "{img_x}",
                y: "{img_y}",
                width: "{img_w}",
                height: "{img_h}",
                "clip-path": "url(#{clip_id})",
                preserve_aspect_ratio: "none",
            }
            // Border around the target rect.
            if circular {
                ellipse {
                    cx: "{tx + tw * 0.5}",
                    cy: "{ty + th * 0.5}",
                    rx: "{tw * 0.5}",
                    ry: "{th * 0.5}",
                    fill: "none",
                    stroke: "{border_css}",
                    "stroke-width": "{border_width}",
                }
            } else {
                rect {
                    x: "{tx}",
                    y: "{ty}",
                    width: "{tw}",
                    height: "{th}",
                    rx: "4",
                    fill: "none",
                    stroke: "{border_css}",
                    "stroke-width": "{border_width}",
                }
            }
            // Source-rect indicator (thin dashed outline).
            rect {
                x: "{sx}",
                y: "{sy}",
                width: "{sw}",
                height: "{sh}",
                fill: "none",
                stroke: "{border_css}",
                "stroke-width": "{(border_width * 0.5).max(1.0)}",
                "stroke-dasharray": "4 3",
            }
        }
    }
}

fn render_stamp_placeholder(id: Uuid, rect: [f32; 4]) -> Element {
    // Stamps render correctly on export via rasterize::draw_stamp;
    // for live preview we draw a placeholder rect with a small label.
    let x = rect[0];
    let y = rect[1];
    let w = (rect[2] - rect[0]).max(10.0);
    let h = (rect[3] - rect[1]).max(10.0);
    rsx! {
        g {
            rect {
                key: "stamp-{id}",
                x: "{x}",
                y: "{y}",
                width: "{w}",
                height: "{h}",
                fill: "rgba(255,255,255,0.7)",
                stroke: "rgba(0,0,0,0.5)",
                "stroke-width": "1.5",
                "stroke-dasharray": "3 3",
            }
            text {
                x: "{x + w * 0.5}",
                y: "{y + h * 0.5 + 4.0}",
                "text-anchor": "middle",
                fill: "rgba(0,0,0,0.65)",
                "font-size": "12",
                "stamp"
            }
        }
    }
}

fn render_capture_info(
    id: Uuid,
    position: CaptureInfoPosition,
    fields: &[FieldKind],
    style: CaptureInfoStyle,
    doc: &Document,
) -> Element {
    use crate::editor::rasterize::capture_info_lines;

    let lines = capture_info_lines(Some(&doc.metadata), fields);
    if lines.is_empty() {
        return rsx! {};
    }
    let pad = style.padding;
    let line_h = style.text_size * 1.25;
    let max_w = lines
        .iter()
        .map(|l| l.len() as f32 * style.text_size * 0.6)
        .fold(0.0f32, f32::max)
        + pad * 2.0;
    let total_h = line_h * lines.len() as f32 + pad * 2.0;
    let (x, y) = match position {
        CaptureInfoPosition::TopLeft => (0.0, 0.0),
        CaptureInfoPosition::TopRight => (doc.base_width as f32 - max_w, 0.0),
        CaptureInfoPosition::BottomLeft => (0.0, doc.base_height as f32 - total_h),
        CaptureInfoPosition::BottomRight => (
            doc.base_width as f32 - max_w,
            doc.base_height as f32 - total_h,
        ),
    };
    let fill_css = rgba_to_css(style.fill);
    let text_css = rgba_to_css(style.text_color);
    rsx! {
        g {
            rect {
                key: "ci-{id}",
                x: "{x}",
                y: "{y}",
                width: "{max_w}",
                height: "{total_h}",
                fill: "{fill_css}",
                rx: "4",
            }
            text {
                x: "{x + pad}",
                y: "{y + pad + style.text_size}",
                "font-family": "JetBrains Mono, Cascadia Mono, Consolas, monospace",
                "font-size": "{style.text_size}",
                fill: "{text_css}",
                for (i, line) in lines.iter().enumerate() {
                    {
                        let dy = if i == 0 { 0.0 } else { line_h };
                        let key = format!("cl-{}-{}", id, i);
                        rsx! {
                            tspan {
                                key: "{key}",
                                x: "{x + pad}",
                                dy: "{dy}",
                                "{line}"
                            }
                        }
                    }
                }
            }
        }
    }
}

// ─── Selection chrome ────────────────────────────────────────────────

fn render_selection(
    doc: &Document,
    selected: Option<Uuid>,
    editing_text: Option<Uuid>,
    mut document: Signal<Document>,
    history: Signal<History>,
    mut dirty: Signal<bool>,
) -> Element {
    let Some(id) = selected else { return rsx! {} };
    let Some(node) = doc.annotations.iter().find(|n| n.id() == id) else { return rsx! {} };
    let bbox = bounding_box(node);
    let Some([x, y, w, h]) = bbox else { return rsx! {} };

    // If editing text and that's the selected node, render the text
    // editor in a foreignObject overlay instead of selection chrome.
    if editing_text == Some(id) {
        if let AnnotationNode::Text { text, .. } = node {
            let initial = text.clone();
            return rsx! {
                foreignObject {
                    x: "{x}",
                    y: "{y}",
                    width: "{w}",
                    height: "{h}",
                    textarea {
                        class: "text-editor",
                        autofocus: true,
                        value: "{initial}",
                        oninput: move |evt| {
                            let v = evt.value();
                            document.with_mut(|d| {
                                if let Some(n) = d.annotations.iter_mut().find(|n| n.id() == id) {
                                    if let AnnotationNode::Text { text, .. } = n {
                                        *text = v;
                                    }
                                }
                            });
                            dirty.set(true);
                        },
                    }
                }
            };
        }
    }

    let _ = history;
    rsx! {
        g {
            rect {
                class: "selection-stroke",
                x: "{x - 2.0}",
                y: "{y - 2.0}",
                width: "{w + 4.0}",
                height: "{h + 4.0}",
            }
            // 8 handles
            {render_handles(x, y, w, h)}
        }
    }
}

fn render_handles(x: f32, y: f32, w: f32, h: f32) -> Element {
    let s = 6.0;
    let half = s * 0.5;
    let cx = x + w * 0.5;
    let cy = y + h * 0.5;
    let positions = [
        ("nw", x, y, "handle"),
        ("n", cx, y, "handle n"),
        ("ne", x + w, y, "handle"),
        ("e", x + w, cy, "handle e"),
        ("se", x + w, y + h, "handle"),
        ("s", cx, y + h, "handle s"),
        ("sw", x, y + h, "handle"),
        ("w", x, cy, "handle w"),
    ];
    rsx! {
        for (id, hx, hy, cls) in positions.iter().copied() {
            rect {
                key: "h-{id}",
                class: "{cls}",
                x: "{hx - half}",
                y: "{hy - half}",
                width: "{s}",
                height: "{s}",
            }
        }
    }
}

fn bounding_box(node: &AnnotationNode) -> Option<[f32; 4]> {
    Some(match node {
        AnnotationNode::Arrow { start, end, .. } => {
            let x0 = start[0].min(end[0]);
            let y0 = start[1].min(end[1]);
            let x1 = start[0].max(end[0]);
            let y1 = start[1].max(end[1]);
            [x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0)]
        }
        AnnotationNode::Text { rect, .. }
        | AnnotationNode::Shape { rect, .. }
        | AnnotationNode::Blur { rect, .. }
        | AnnotationNode::Callout { rect, .. }
        | AnnotationNode::Stamp { rect, .. } => {
            [rect[0], rect[1], (rect[2] - rect[0]).max(0.0), (rect[3] - rect[1]).max(0.0)]
        }
        AnnotationNode::Magnify { target_rect, .. } => [
            target_rect[0],
            target_rect[1],
            (target_rect[2] - target_rect[0]).max(0.0),
            (target_rect[3] - target_rect[1]).max(0.0),
        ],
        AnnotationNode::Step { center, radius, .. } => {
            [center[0] - radius, center[1] - radius, radius * 2.0, radius * 2.0]
        }
        AnnotationNode::CaptureInfo { .. } => return None,
    })
}

// ─── Pending-drag preview ────────────────────────────────────────────

fn render_pending(pending: &Option<Pending>, style: &ToolStyle, tool: Tool) -> Element {
    let Some(pen) = pending else { return rsx! {} };
    if let Pending::Rect { start, cur } = pen {
        let s = *start;
        let c = *cur;
        let x = s[0].min(c[0]);
        let y = s[1].min(c[1]);
        let w = (s[0] - c[0]).abs();
        let h = (s[1] - c[1]).abs();
        match tool {
            Tool::Arrow => rsx! {
                line {
                    x1: "{s[0]}",
                    y1: "{s[1]}",
                    x2: "{c[0]}",
                    y2: "{c[1]}",
                    stroke: "{rgba_to_css(style.arrow_color)}",
                    "stroke-width": "{style.arrow_thickness}",
                    "stroke-linecap": "round",
                    opacity: "0.7",
                }
            },
            Tool::Ellipse => rsx! {
                ellipse {
                    cx: "{x + w * 0.5}",
                    cy: "{y + h * 0.5}",
                    rx: "{w * 0.5}",
                    ry: "{h * 0.5}",
                    fill: "none",
                    stroke: "{rgba_to_css(style.shape_stroke)}",
                    "stroke-width": "{style.shape_stroke_width}",
                    "stroke-dasharray": "4 3",
                }
            },
            _ => rsx! {
                rect {
                    x: "{x}",
                    y: "{y}",
                    width: "{w}",
                    height: "{h}",
                    fill: "none",
                    stroke: "{rgba_to_css(style.shape_stroke)}",
                    "stroke-width": "1.5",
                    "stroke-dasharray": "4 3",
                }
            },
        }
    } else {
        rsx! {}
    }
}

// ═══ Inspector ══════════════════════════════════════════════════════

#[component]
fn Inspector(
    document: Signal<Document>,
    history: Signal<History>,
    dirty: Signal<bool>,
    tool: Signal<Tool>,
    selected: Signal<Option<Uuid>>,
    style: Signal<ToolStyle>,
) -> Element {
    rsx! {
        div { class: "inspector",
            ToolStyleSection { tool: tool, style: style }
            div { class: "section-divider" }
            SelectionSection {
                document: document,
                history: history,
                dirty: dirty,
                selected: selected,
            }
            div { class: "section-divider" }
            DocumentEffectsSection {
                document: document,
                history: history,
                dirty: dirty,
            }
        }
    }
}

#[component]
fn ToolStyleSection(tool: Signal<Tool>, style: Signal<ToolStyle>) -> Element {
    let cur = *tool.read();
    rsx! {
        section {
            h2 { "{cur.label()} style" }
            match cur {
                Tool::Arrow => rsx! { ArrowStyle { style: style } },
                Tool::Text => rsx! { TextStyle { style: style } },
                Tool::Rect | Tool::Ellipse => rsx! { ShapeStyle { style: style } },
                Tool::Step => rsx! { StepStyle { style: style } },
                Tool::Magnify => rsx! { MagnifyStyle { style: style } },
                Tool::Blur => rsx! { BlurStyle { style: style } },
                Tool::Callout => rsx! { CalloutStyle { style: style } },
                Tool::CaptureInfo => rsx! { CaptureInfoStyleEditor { style: style } },
                Tool::Select => rsx! { p {
                    style: "font-size: 11px; color: #6c727a;",
                    "Click an annotation to select it. Drag to move; corner handles resize. Delete to remove."
                } },
            }
        }
    }
}

#[component]
fn ColorPalette(value: Signal<[u8; 4]>) -> Element {
    let cur = *value.read();
    rsx! {
        div { class: "palette",
            for c in PALETTE.iter().copied() {
                {
                    let active = c == cur;
                    let bg = rgba_to_css(c);
                    let cls = if active { "swatch active" } else { "swatch" };
                    rsx! {
                        button {
                            key: "{c[0]}-{c[1]}-{c[2]}",
                            class: "{cls}",
                            style: "background: {bg};",
                            onclick: move |_| value.set(c),
                        }
                    }
                }
            }
        }
    }
}

fn color_to_hex(c: [u8; 4]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn hex_to_color(hex: &str, alpha: u8) -> Option<[u8; 4]> {
    let s = hex.strip_prefix('#').unwrap_or(hex);
    if s.len() != 6 { return None; }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b, alpha])
}

#[component]
fn ColorPickerRow(label: String, value: Signal<[u8; 4]>) -> Element {
    let v = *value.read();
    let hex = color_to_hex(v);
    rsx! {
        div { class: "field",
            label { "{label}" }
            div { class: "row-pair",
                input {
                    r#type: "color",
                    value: "{hex}",
                    oninput: move |evt| {
                        if let Some(c) = hex_to_color(&evt.value(), v[3]) {
                            value.set(c);
                        }
                    },
                }
                input {
                    r#type: "text",
                    value: "{hex}",
                    oninput: move |evt| {
                        if let Some(c) = hex_to_color(&evt.value(), v[3]) {
                            value.set(c);
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn ArrowStyle(style: Signal<ToolStyle>) -> Element {
    let s = style.read().clone();
    let mut color_sig = use_signal(|| s.arrow_color);
    let mut thickness_sig = use_signal(|| s.arrow_thickness);
    let mut shadow_sig = use_signal(|| s.arrow_shadow);
    let mut line_sig = use_signal(|| s.arrow_line_style);
    let mut head_sig = use_signal(|| s.arrow_head_style);

    // Reflect signal changes back into the bundled style.
    use_effect(move || {
        let mut style = style;
        style.with_mut(|st| {
            st.arrow_color = *color_sig.read();
            st.arrow_thickness = *thickness_sig.read();
            st.arrow_shadow = *shadow_sig.read();
            st.arrow_line_style = *line_sig.read();
            st.arrow_head_style = *head_sig.read();
        });
    });

    let cur_line = *line_sig.read();
    let cur_head = *head_sig.read();
    let shadow = *shadow_sig.read();
    let thick = *thickness_sig.read();

    rsx! {
        ColorPalette { value: color_sig }
        ColorPickerRow { label: "Color".to_string(), value: color_sig }
        div { class: "field",
            label { "Thickness" }
            input {
                r#type: "number",
                min: "1",
                max: "32",
                step: "0.5",
                value: "{thick}",
                oninput: move |evt| {
                    if let Ok(v) = evt.value().parse::<f32>() {
                        thickness_sig.set(v.clamp(1.0, 32.0));
                    }
                },
            }
        }
        div { class: "field",
            label { "Line style" }
            div { class: "row-3",
                button {
                    class: if cur_line == ArrowLineStyle::Solid { "ghost active" } else { "ghost" },
                    onclick: move |_| line_sig.set(ArrowLineStyle::Solid),
                    "Solid"
                }
                button {
                    class: if cur_line == ArrowLineStyle::Dashed { "ghost active" } else { "ghost" },
                    onclick: move |_| line_sig.set(ArrowLineStyle::Dashed),
                    "Dashed"
                }
                button {
                    class: if cur_line == ArrowLineStyle::Dotted { "ghost active" } else { "ghost" },
                    onclick: move |_| line_sig.set(ArrowLineStyle::Dotted),
                    "Dotted"
                }
            }
        }
        div { class: "field",
            label { "Head" }
            select {
                value: "{head_label(cur_head)}",
                onchange: move |evt| {
                    head_sig.set(head_from_label(&evt.value()).unwrap_or(ArrowHeadStyle::FilledTriangle));
                },
                option { value: "Filled", "Filled triangle" }
                option { value: "Outline", "Outline triangle" }
                option { value: "Line", "Line chevron" }
                option { value: "None", "No head" }
                option { value: "Double", "Double-ended" }
            }
        }
        label { class: "toggle",
            input {
                r#type: "checkbox",
                checked: "{shadow}",
                onchange: move |evt| shadow_sig.set(evt.checked()),
            }
            "Drop shadow"
        }
    }
}

fn head_label(h: ArrowHeadStyle) -> &'static str {
    match h {
        ArrowHeadStyle::FilledTriangle => "Filled",
        ArrowHeadStyle::OutlineTriangle => "Outline",
        ArrowHeadStyle::LineOnly => "Line",
        ArrowHeadStyle::None => "None",
        ArrowHeadStyle::DoubleEnded => "Double",
    }
}
fn head_from_label(s: &str) -> Option<ArrowHeadStyle> {
    Some(match s {
        "Filled" => ArrowHeadStyle::FilledTriangle,
        "Outline" => ArrowHeadStyle::OutlineTriangle,
        "Line" => ArrowHeadStyle::LineOnly,
        "None" => ArrowHeadStyle::None,
        "Double" => ArrowHeadStyle::DoubleEnded,
        _ => return None,
    })
}

#[component]
fn TextStyle(style: Signal<ToolStyle>) -> Element {
    let s = style.read().clone();
    let mut color_sig = use_signal(|| s.text_color);
    let mut size_sig = use_signal(|| s.text_size);
    let mut align_sig = use_signal(|| s.text_align);
    let mut list_sig = use_signal(|| s.text_list);
    let mut frosted_sig = use_signal(|| s.text_frosted);
    let mut shadow_sig = use_signal(|| s.text_shadow);
    use_effect(move || {
        let mut style = style;
        style.with_mut(|st| {
            st.text_color = *color_sig.read();
            st.text_size = *size_sig.read();
            st.text_align = *align_sig.read();
            st.text_list = *list_sig.read();
            st.text_frosted = *frosted_sig.read();
            st.text_shadow = *shadow_sig.read();
        });
    });
    let size_val = *size_sig.read();
    let cur_align = *align_sig.read();
    let cur_list = *list_sig.read();
    let frosted = *frosted_sig.read();
    let shadow = *shadow_sig.read();
    rsx! {
        ColorPalette { value: color_sig }
        ColorPickerRow { label: "Color".to_string(), value: color_sig }
        div { class: "field",
            label { "Size (px)" }
            input {
                r#type: "number",
                min: "8",
                max: "200",
                value: "{size_val}",
                oninput: move |evt| {
                    if let Ok(v) = evt.value().parse::<f32>() {
                        size_sig.set(v.clamp(8.0, 200.0));
                    }
                },
            }
        }
        div { class: "field",
            label { "Alignment" }
            div { class: "row-3",
                button {
                    class: if cur_align == TextAlign::Left { "ghost active" } else { "ghost" },
                    onclick: move |_| align_sig.set(TextAlign::Left), "Left"
                }
                button {
                    class: if cur_align == TextAlign::Center { "ghost active" } else { "ghost" },
                    onclick: move |_| align_sig.set(TextAlign::Center), "Center"
                }
                button {
                    class: if cur_align == TextAlign::Right { "ghost active" } else { "ghost" },
                    onclick: move |_| align_sig.set(TextAlign::Right), "Right"
                }
            }
        }
        div { class: "field",
            label { "List" }
            div { class: "row-3",
                button {
                    class: if cur_list == TextListStyle::None { "ghost active" } else { "ghost" },
                    onclick: move |_| list_sig.set(TextListStyle::None), "None"
                }
                button {
                    class: if cur_list == TextListStyle::Bullet { "ghost active" } else { "ghost" },
                    onclick: move |_| list_sig.set(TextListStyle::Bullet), "•"
                }
                button {
                    class: if cur_list == TextListStyle::Numbered { "ghost active" } else { "ghost" },
                    onclick: move |_| list_sig.set(TextListStyle::Numbered), "1."
                }
            }
        }
        label { class: "toggle",
            input { r#type: "checkbox", checked: "{frosted}", onchange: move |e| frosted_sig.set(e.checked()), }
            "Frosted backdrop"
        }
        label { class: "toggle",
            input { r#type: "checkbox", checked: "{shadow}", onchange: move |e| shadow_sig.set(e.checked()), }
            "Drop shadow"
        }
    }
}

#[component]
fn ShapeStyle(style: Signal<ToolStyle>) -> Element {
    let s = style.read().clone();
    let mut stroke_sig = use_signal(|| s.shape_stroke);
    let mut sw_sig = use_signal(|| s.shape_stroke_width);
    let mut fill_sig = use_signal(|| s.shape_fill);
    let mut filled_sig = use_signal(|| s.shape_fill[3] != 0);
    use_effect(move || {
        let mut style = style;
        let filled = *filled_sig.read();
        let mut f = *fill_sig.read();
        if !filled { f[3] = 0; }
        style.with_mut(|st| {
            st.shape_stroke = *stroke_sig.read();
            st.shape_stroke_width = *sw_sig.read();
            st.shape_fill = f;
        });
    });
    let sw_val = *sw_sig.read();
    let filled = *filled_sig.read();
    rsx! {
        ColorPalette { value: stroke_sig }
        ColorPickerRow { label: "Stroke".to_string(), value: stroke_sig }
        div { class: "field",
            label { "Stroke width" }
            input {
                r#type: "number",
                min: "0.5",
                max: "32",
                step: "0.5",
                value: "{sw_val}",
                oninput: move |e| {
                    if let Ok(v) = e.value().parse::<f32>() {
                        sw_sig.set(v.clamp(0.5, 32.0));
                    }
                }
            }
        }
        label { class: "toggle",
            input { r#type: "checkbox", checked: "{filled}", onchange: move |e| filled_sig.set(e.checked()), }
            "Filled"
        }
        if filled {
            ColorPickerRow { label: "Fill".to_string(), value: fill_sig }
        }
    }
}

#[component]
fn StepStyle(style: Signal<ToolStyle>) -> Element {
    let s = style.read().clone();
    let mut fill_sig = use_signal(|| s.step_fill);
    let mut text_sig = use_signal(|| s.step_text_color);
    let mut radius_sig = use_signal(|| s.step_radius);
    let mut next_sig = use_signal(|| s.next_step_number);
    use_effect(move || {
        let mut style = style;
        style.with_mut(|st| {
            st.step_fill = *fill_sig.read();
            st.step_text_color = *text_sig.read();
            st.step_radius = *radius_sig.read();
            st.next_step_number = *next_sig.read();
        });
    });
    let r = *radius_sig.read();
    let n = *next_sig.read();
    rsx! {
        ColorPalette { value: fill_sig }
        ColorPickerRow { label: "Fill".to_string(), value: fill_sig }
        ColorPickerRow { label: "Text color".to_string(), value: text_sig }
        div { class: "field",
            label { "Radius" }
            input {
                r#type: "number", min: "6", max: "60", step: "1", value: "{r}",
                oninput: move |e| {
                    if let Ok(v) = e.value().parse::<f32>() {
                        radius_sig.set(v.clamp(6.0, 60.0));
                    }
                }
            }
        }
        div { class: "field",
            label { "Next number" }
            input {
                r#type: "number", min: "1", max: "999", value: "{n}",
                oninput: move |e| {
                    if let Ok(v) = e.value().parse::<u32>() {
                        next_sig.set(v);
                    }
                }
            }
        }
    }
}

#[component]
fn MagnifyStyle(style: Signal<ToolStyle>) -> Element {
    let s = style.read().clone();
    let mut circular_sig = use_signal(|| s.magnify_circular);
    let mut border_sig = use_signal(|| s.magnify_border);
    let mut bw_sig = use_signal(|| s.magnify_border_width);
    let mut zoom_sig = use_signal(|| s.magnify_zoom);
    use_effect(move || {
        let mut style = style;
        style.with_mut(|st| {
            st.magnify_circular = *circular_sig.read();
            st.magnify_border = *border_sig.read();
            st.magnify_border_width = *bw_sig.read();
            st.magnify_zoom = *zoom_sig.read();
        });
    });
    let circ = *circular_sig.read();
    let bw = *bw_sig.read();
    let z = *zoom_sig.read();
    rsx! {
        label { class: "toggle",
            input { r#type: "checkbox", checked: "{circ}", onchange: move |e| circular_sig.set(e.checked()), }
            "Circular"
        }
        ColorPickerRow { label: "Border".to_string(), value: border_sig }
        div { class: "field",
            label { "Border width" }
            input { r#type: "number", min: "0", max: "20", step: "0.5", value: "{bw}",
                oninput: move |e| { if let Ok(v) = e.value().parse::<f32>() { bw_sig.set(v.clamp(0.0, 20.0)); } }
            }
        }
        div { class: "field",
            label { "Zoom (×)" }
            input { r#type: "number", min: "1.5", max: "10", step: "0.25", value: "{z}",
                oninput: move |e| { if let Ok(v) = e.value().parse::<f32>() { zoom_sig.set(v.clamp(1.5, 10.0)); } }
            }
        }
    }
}

#[component]
fn BlurStyle(style: Signal<ToolStyle>) -> Element {
    let s = style.read().clone();
    let mut radius_sig = use_signal(|| s.blur_radius);
    use_effect(move || {
        let mut style = style;
        style.with_mut(|st| {
            st.blur_radius = *radius_sig.read();
        });
    });
    let r = *radius_sig.read();
    rsx! {
        div { class: "field",
            label { "Blur radius (sigma)" }
            input { r#type: "number", min: "1", max: "60", step: "1", value: "{r}",
                oninput: move |e| { if let Ok(v) = e.value().parse::<f32>() { radius_sig.set(v.clamp(1.0, 60.0)); } }
            }
        }
        p { style: "font-size: 11px; color: #6c727a;",
            "Drag a region; the blur is applied at export time."
        }
    }
}

#[component]
fn CalloutStyle(style: Signal<ToolStyle>) -> Element {
    let s = style.read().clone();
    let mut fill_sig = use_signal(|| s.callout_fill);
    let mut stroke_sig = use_signal(|| s.callout_stroke);
    let mut text_sig = use_signal(|| s.callout_text_color);
    use_effect(move || {
        let mut style = style;
        style.with_mut(|st| {
            st.callout_fill = *fill_sig.read();
            st.callout_stroke = *stroke_sig.read();
            st.callout_text_color = *text_sig.read();
        });
    });
    rsx! {
        ColorPickerRow { label: "Fill".to_string(), value: fill_sig }
        ColorPickerRow { label: "Stroke".to_string(), value: stroke_sig }
        ColorPickerRow { label: "Text".to_string(), value: text_sig }
    }
}

#[component]
fn CaptureInfoStyleEditor(style: Signal<ToolStyle>) -> Element {
    let s = style.read().clone();
    let mut pos_sig = use_signal(|| s.capture_info_position);
    let fields_sig = use_signal(|| s.capture_info_fields.clone());
    use_effect(move || {
        let mut style = style;
        style.with_mut(|st| {
            st.capture_info_position = *pos_sig.read();
            st.capture_info_fields = fields_sig.read().clone();
        });
    });
    let cur_pos = *pos_sig.read();
    rsx! {
        div { class: "field",
            label { "Position" }
            select {
                value: "{position_label(cur_pos)}",
                onchange: move |e| {
                    if let Some(p) = position_from_label(&e.value()) { pos_sig.set(p); }
                },
                option { value: "TopLeft", "Top left" }
                option { value: "TopRight", "Top right" }
                option { value: "BottomLeft", "Bottom left" }
                option { value: "BottomRight", "Bottom right" }
            }
        }
        FieldsCheckboxes { fields: fields_sig }
    }
}

fn position_label(p: CaptureInfoPosition) -> &'static str {
    match p {
        CaptureInfoPosition::TopLeft => "TopLeft",
        CaptureInfoPosition::TopRight => "TopRight",
        CaptureInfoPosition::BottomLeft => "BottomLeft",
        CaptureInfoPosition::BottomRight => "BottomRight",
    }
}
fn position_from_label(s: &str) -> Option<CaptureInfoPosition> {
    Some(match s {
        "TopLeft" => CaptureInfoPosition::TopLeft,
        "TopRight" => CaptureInfoPosition::TopRight,
        "BottomLeft" => CaptureInfoPosition::BottomLeft,
        "BottomRight" => CaptureInfoPosition::BottomRight,
        _ => return None,
    })
}

#[component]
fn FieldsCheckboxes(fields: Signal<Vec<FieldKind>>) -> Element {
    let cur = fields.read().clone();
    const ALL: &[FieldKind] = &[
        FieldKind::Timestamp,
        FieldKind::WindowTitle,
        FieldKind::ProcessName,
        FieldKind::OsVersion,
        FieldKind::MonitorInfo,
    ];
    rsx! {
        div { class: "field",
            label { "Fields" }
            for f in ALL.iter().copied() {
                {
                    let checked = cur.contains(&f);
                    let label = f.label();
                    rsx! {
                        label { class: "toggle",
                            input {
                                r#type: "checkbox",
                                checked: "{checked}",
                                onchange: move |e| {
                                    let mut v = fields.read().clone();
                                    if e.checked() {
                                        if !v.contains(&f) { v.push(f); }
                                    } else {
                                        v.retain(|x| *x != f);
                                    }
                                    fields.set(v);
                                },
                            }
                            "{label}"
                        }
                    }
                }
            }
        }
    }
}

// ─── Selection inspector ─────────────────────────────────────────────

#[component]
fn SelectionSection(
    document: Signal<Document>,
    history: Signal<History>,
    dirty: Signal<bool>,
    selected: Signal<Option<Uuid>>,
) -> Element {
    let sel_id = *selected.read();
    let Some(id) = sel_id else {
        return rsx! { section { h2 { "Selection" } p { style: "font-size: 11px; color: #6c727a;", "Nothing selected." } } };
    };
    // Extract the small bits we need from the document without holding
    // a borrow across the whole rsx — closures below capture `document`
    // (the Signal) directly and re-read on each event.
    let snapshot_node = document
        .read()
        .annotations
        .iter()
        .find(|n| n.id() == id)
        .cloned();
    let Some(node) = snapshot_node else {
        return rsx! {};
    };
    let kind_label = match &node {
        AnnotationNode::Arrow { .. } => "Arrow",
        AnnotationNode::Text { .. } => "Text",
        AnnotationNode::Shape { .. } => "Shape",
        AnnotationNode::Step { .. } => "Step",
        AnnotationNode::Magnify { .. } => "Magnifier",
        AnnotationNode::Blur { .. } => "Blur",
        AnnotationNode::Callout { .. } => "Callout",
        AnnotationNode::Stamp { .. } => "Stamp",
        AnnotationNode::CaptureInfo { .. } => "Capture info",
    };

    let on_delete = move |_| {
        execute_command(document, history, dirty, Box::new(RemoveAnnotation::new(id)));
        let mut sel = selected;
        sel.set(None);
    };

    let text_initial = match &node {
        AnnotationNode::Text { text, .. } => Some(text.clone()),
        _ => None,
    };
    let callout_initial = match &node {
        AnnotationNode::Callout { text, .. } => Some(text.clone()),
        _ => None,
    };

    rsx! {
        section {
            h2 { "Selection: {kind_label}" }
            if let Some(initial) = text_initial {
                div { class: "field",
                    label { "Text" }
                    textarea {
                        value: "{initial}",
                        oninput: move |evt| {
                            let v = evt.value();
                            let before = document
                                .read()
                                .annotations
                                .iter()
                                .find(|n| n.id() == id)
                                .cloned();
                            if let Some(before) = before {
                                let mut after = before.clone();
                                if let AnnotationNode::Text { text, .. } = &mut after {
                                    *text = v;
                                }
                                execute_command(
                                    document,
                                    history,
                                    dirty,
                                    Box::new(UpdateAnnotation::new(before, after)),
                                );
                            }
                        },
                    }
                }
            }
            if let Some(initial) = callout_initial {
                div { class: "field",
                    label { "Text" }
                    textarea {
                        value: "{initial}",
                        oninput: move |evt| {
                            let v = evt.value();
                            let before = document
                                .read()
                                .annotations
                                .iter()
                                .find(|n| n.id() == id)
                                .cloned();
                            if let Some(before) = before {
                                let mut after = before.clone();
                                if let AnnotationNode::Callout { text, .. } = &mut after {
                                    *text = v;
                                }
                                execute_command(
                                    document,
                                    history,
                                    dirty,
                                    Box::new(UpdateAnnotation::new(before, after)),
                                );
                            }
                        },
                    }
                }
            }
            button { class: "danger", style: "width: 100%; margin-top: 8px;",
                onclick: on_delete,
                "Delete (Del)"
            }
        }
    }
}

// ─── Document effects ────────────────────────────────────────────────

#[component]
fn DocumentEffectsSection(
    document: Signal<Document>,
    history: Signal<History>,
    dirty: Signal<bool>,
) -> Element {
    let doc_snapshot = document.read().clone();
    let edge = doc_snapshot.edge_effect;
    let border = doc_snapshot.border;
    let edge_on = edge.is_some();
    let border_on = border.is_some();

    let on_toggle_edge = move |_| {
        let before = edge;
        let after = if before.is_some() { None } else { Some(EdgeEffect::default()) };
        execute_command(document, history, dirty, Box::new(SetEdgeEffect::new(before, after)));
    };
    let on_toggle_border = move |_| {
        let before = border;
        let after = if before.is_some() { None } else { Some(Border::default()) };
        execute_command(document, history, dirty, Box::new(SetBorder::new(before, after)));
    };

    rsx! {
        section {
            h2 { "Document effects" }
            label { class: "toggle",
                input { r#type: "checkbox", checked: "{edge_on}", onchange: on_toggle_edge, }
                "Torn edge"
            }
            if let Some(eff) = edge {
                EdgeEffectEditor { effect: eff, document: document, history: history, dirty: dirty }
            }
            label { class: "toggle", style: "margin-top: 8px;",
                input { r#type: "checkbox", checked: "{border_on}", onchange: on_toggle_border, }
                "Border"
            }
            if let Some(b) = border {
                BorderEditor { border: b, document: document, history: history, dirty: dirty }
            }
        }
    }
}

#[component]
fn EdgeEffectEditor(
    effect: EdgeEffect,
    document: Signal<Document>,
    history: Signal<History>,
    dirty: Signal<bool>,
) -> Element {
    let on_change = move |new: EdgeEffect| {
        execute_command(
            document,
            history,
            dirty,
            Box::new(SetEdgeEffect::new(Some(effect), Some(new))),
        );
    };
    let mut top = effect.top;
    let mut bottom = effect.bottom;
    let mut left = effect.left;
    let mut right = effect.right;
    if !top && !bottom && !left && !right {
        match effect.edge {
            Edge::Top => top = true,
            Edge::Bottom => bottom = true,
            Edge::Left => left = true,
            Edge::Right => right = true,
        }
    }
    let depth = effect.depth;
    let teeth = effect.teeth;
    rsx! {
        div { class: "field",
            label { "Edges" }
            div { class: "row-pair",
                label { class: "toggle",
                    input { r#type: "checkbox", checked: "{top}",
                        onchange: move |e| on_change(EdgeEffect { top: e.checked(), ..effect }), }
                    "Top"
                }
                label { class: "toggle",
                    input { r#type: "checkbox", checked: "{bottom}",
                        onchange: move |e| on_change(EdgeEffect { bottom: e.checked(), ..effect }), }
                    "Bottom"
                }
            }
            div { class: "row-pair",
                label { class: "toggle",
                    input { r#type: "checkbox", checked: "{left}",
                        onchange: move |e| on_change(EdgeEffect { left: e.checked(), ..effect }), }
                    "Left"
                }
                label { class: "toggle",
                    input { r#type: "checkbox", checked: "{right}",
                        onchange: move |e| on_change(EdgeEffect { right: e.checked(), ..effect }), }
                    "Right"
                }
            }
        }
        div { class: "field",
            label { "Depth" }
            input { r#type: "number", min: "1", max: "120", step: "1", value: "{depth}",
                oninput: move |e| {
                    if let Ok(v) = e.value().parse::<f32>() {
                        on_change(EdgeEffect { depth: v.clamp(1.0, 120.0), ..effect });
                    }
                }
            }
        }
        div { class: "field",
            label { "Teeth period" }
            input { r#type: "number", min: "4", max: "120", step: "1", value: "{teeth}",
                oninput: move |e| {
                    if let Ok(v) = e.value().parse::<f32>() {
                        on_change(EdgeEffect { teeth: v.clamp(4.0, 120.0), ..effect });
                    }
                }
            }
        }
    }
}

#[component]
fn BorderEditor(
    border: Border,
    document: Signal<Document>,
    history: Signal<History>,
    dirty: Signal<bool>,
) -> Element {
    let on_change = move |new: Border| {
        execute_command(
            document,
            history,
            dirty,
            Box::new(SetBorder::new(Some(border), Some(new))),
        );
    };
    let width = border.width;
    let matte = border.matte_width;
    let shadow_r = border.shadow_radius;
    let color_hex = color_to_hex(border.color);
    let matte_hex = color_to_hex(border.matte_color);
    rsx! {
        div { class: "field",
            label { "Width" }
            input { r#type: "number", min: "0", max: "120", step: "1", value: "{width}",
                oninput: move |e| {
                    if let Ok(v) = e.value().parse::<f32>() {
                        on_change(Border { width: v.clamp(0.0, 120.0), ..border });
                    }
                }
            }
        }
        div { class: "field",
            label { "Color" }
            input {
                r#type: "color", value: "{color_hex}",
                oninput: move |e| {
                    if let Some(c) = hex_to_color(&e.value(), border.color[3]) {
                        on_change(Border { color: c, ..border });
                    }
                },
            }
        }
        div { class: "field",
            label { "Matte width" }
            input { r#type: "number", min: "0", max: "120", step: "1", value: "{matte}",
                oninput: move |e| {
                    if let Ok(v) = e.value().parse::<f32>() {
                        on_change(Border { matte_width: v.clamp(0.0, 120.0), ..border });
                    }
                }
            }
        }
        div { class: "field",
            label { "Matte color" }
            input {
                r#type: "color", value: "{matte_hex}",
                oninput: move |e| {
                    if let Some(c) = hex_to_color(&e.value(), border.matte_color[3]) {
                        on_change(Border { matte_color: c, ..border });
                    }
                },
            }
        }
        div { class: "field",
            label { "Shadow radius" }
            input { r#type: "number", min: "0", max: "60", step: "0.5", value: "{shadow_r}",
                oninput: move |e| {
                    if let Ok(v) = e.value().parse::<f32>() {
                        on_change(Border { shadow_radius: v.clamp(0.0, 60.0), ..border });
                    }
                }
            }
        }
    }
}

