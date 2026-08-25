# Zshot (Rust Phase 1 PoC)

Tray-resident Windows screenshot tool. Product binary lives here; the C# app in `F:\own\Zshot` is unchanged.

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
- `--self-test` init tray/hook, print `zshot-ready`, exit 0
- `--probe-gpui` GPUI probe (`--features gpui-probe` to actually link GPUI)

PNGs go to `Pictures\Zshot`. Timing marks T0–T7 append to `%LOCALAPPDATA%\Zshot\logs\perf.log`.

## Phase 1 notes

- Capture: WGC first, `DXGI DuplicateOutput1` on failure / timeout / near-black / exclusive fullscreen
- Overlay background is a GPU texture (no full-frame CPU blit)
- Exclusive-fullscreen region: grab the game frame, then force the freeze overlay
- GPUI is not used for the screenshot overlay
- Not in this PoC: annotation editor, OCR, translation, long capture, AVIF/JXL/UHDR
