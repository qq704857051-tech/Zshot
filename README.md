# Zshot (Rust Phase 1 PoC)

Tray-resident Windows screenshot tool. Product binary lives here; the C# app in `F:\own\Zshot` is unchanged.

Default Release links the **GPUI Overlay** path: single HWND, dual DirectComposition visuals (freeze + UI), shared D3D11 device with GPUI.

## Build

```text
cargo build --release
```

Produces:

- `target/release/Zshot.exe`
- `target/release/Zshot_Ocr.exe` (placeholder worker; Phase 1 name avoids Cargo's `.` bin restriction)

## Run

Start with no arguments: silent tray, no main window. A second launch exits.

| Action | Default hotkey |
| --- | --- |
| Region (freeze overlay) | `PrtSc` |
| Copy-only | `Ctrl+Alt+A` |
| Fullscreen save | `Ctrl+Alt+W` |

Flags:

- `--once-capture [path]` capture the focused monitor to PNG and exit
- `--self-test` init tray/hook under GPUI, print `zshot-ready hook=... tray=...`, exit 0
- `--overlay-smoke` open a dual-visual smoke window briefly, then quit
- `--ime-probe` PopUp window with a single-line IME `TextInput` (manual typing probe)
- `--bench-overlay --mode first|warm --n N` T2→T7 overlay latency samples → `%LOCALAPPDATA%\Zshot\logs\perf-summary.json`
- `--stress-overlay N` loop overlay show/hide; sample Private Working Set at iterations 10 and 100

PNGs go to `Pictures\Zshot`. Timing marks T0–T7 append to `%LOCALAPPDATA%\Zshot\logs\perf.log`.

## Phase 1 notes

- Capture: WGC first, `DXGI DuplicateOutput1` on failure / timeout / near-black / exclusive fullscreen (same-adapter only)
- Overlay freeze is a composition swapchain on DComp visual 0 (no full-frame CPU blit)
- Exclusive-fullscreen region: grab the game frame, then force the freeze overlay
- Not in this PoC: annotation editor, OCR, translation, long capture, AVIF/JXL/UHDR

Decision record: `F:\own\Zshot\docs\Phase1-GPUI-Overlay决策记录.md`
