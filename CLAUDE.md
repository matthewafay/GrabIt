# GrabIt — guide for Claude Code

GrabIt is a Windows screenshot + GIF tool. It also exposes a **headless CLI**
(`grabit.exe --capture …`) that lets Claude Code take screenshots and record
GIFs without any GUI interaction, and embed the results into GitHub markdown.

## TL;DR

```powershell
# screenshot a window by process
grabit.exe --capture screenshot --target window --process code.exe `
    --out ./docs/screenshots/editor.png

# record a 5-second GIF of a window
grabit.exe --capture gif --target window --process code.exe `
    --duration 5 --out ./docs/screenshots/demo.gif

# fullscreen
grabit.exe --capture screenshot --target fullscreen --out ./shot.png
```

The absolute output path is printed to **stdout** on success. Diagnostics go
to **stderr**. Exit code is `0` on success, `1` on failure.

## Where the exe lives

After a release build: `target\release\grabit.exe` (relative to this repo).
If you're working in another repo and using GrabIt from there, the canonical
path on this machine is `C:\code\GrabIt\target\release\grabit.exe`.

## Building it

If `target\release\grabit.exe` is missing or out of date:

```powershell
# 1. Kill any running instance — the linker can't overwrite a running exe.
Get-Process grabit -ErrorAction SilentlyContinue | Stop-Process -Force

# 2. Build.
cargo build --release
```

(The kill step is automated for me — don't pause to ask, just run it.)

## Verbs

| Verb | What it does |
|---|---|
| `screenshot` | Single PNG. `--target fullscreen \| window \| region`. |
| `gif` | Fire-and-wait recording for `--duration` seconds. Same `--target` set. |
| `list-windows` | JSON array of top-level windows. Optional `--process` filter. |
| `help` | Usage to stdout. |

Run `grabit.exe --capture help` for the full flag matrix.

## Output contract (read this before parsing output)

- **Success**: the absolute output path is the *only* thing on stdout, on a
  single line, terminated with `\n`.
- **`list-windows`**: prints a JSON array on stdout. Schema:
  `[{ "hwnd": 12345, "pid": 6789, "process": "code.exe",
     "title": "...", "rect": { "x": …, "y": …, "width": …, "height": … } }]`
- **Diagnostics** (e.g. "3 candidate windows; picked HWND 0x… by largest area")
  go to **stderr** so the stdout path stays clean to parse.
- **Failure**: human message on stderr, exit code `1`. No PNG/GIF written.

Parent directories of `--out` are auto-created. You can pass `--out ./docs/screenshots/foo.png`
into a brand-new directory and it'll work.

## Targeting a window

`--target window --process <name>` looks up every visible top-level window
owned by a process matching `<name>` (case-insensitive; `.exe` optional).

- 0 matches → exit 1 with a clear error.
- 1 match → captured directly.
- multiple matches → the **largest visible window** wins, deterministically.
  Runners-up are logged to stderr so you can disambiguate via `list-windows`
  and re-run with explicit `--target region` if needed.

Minimized windows are filtered out automatically.

## Embedding in GitHub markdown

Drop the file directly into the repo's docs folder, then write a relative
markdown link. GitHub renders relative repo paths natively (web + PR
previews); no upload, no clipboard, no CDN.

```powershell
grabit.exe --capture screenshot --target window --process code.exe `
    --out ./docs/screenshots/editor.png
```

```markdown
![editor](./docs/screenshots/editor.png)
```

GIFs work identically — animated GIFs render inline in GitHub markdown.

## Running consumer projects

If you're using GrabIt from a different repo (i.e. capturing screenshots of
*that* project for *that* project's docs), drop a one-line `CLAUDE.md` into
that repo so future Claude sessions discover the tool:

```markdown
Screenshot/GIF tool: `C:\code\GrabIt\target\release\grabit.exe --capture …`.
See `C:\code\GrabIt\CLAUDE.md` for the full surface.
```

Or add the same line to your global `~/.claude/CLAUDE.md` to get it
everywhere without per-repo setup.

## Coexistence with the tray app

The headless CLI runs as a one-shot subprocess that **bypasses** GrabIt's
single-instance mutex, so you can capture even while the resident tray app
is running. The tray app's hotkeys and floating GIF bar are not affected.

## Full reference

For the long-form flag matrix, `list-windows` schema, exit codes,
troubleshooting, and worked markdown-embedding examples, see
[`docs/CAPTURE-CLI.md`](./docs/CAPTURE-CLI.md).
