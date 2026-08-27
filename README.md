# Zshot (Rust Phase 1 PoC)

Tray-resident Windows screenshot tool. Product binary lives here; the C# app in `F:\own\Zshot` is unchanged.

Default Release links the **GPUI Overlay** path: single HWND, dual DirectComposition visuals (freeze + UI), shared D3D11 device with GPUI.

## Build

```text
cargo build --release
```

Produces `target/release/Zshot.exe`.

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
- `--bench-overlay --mode first|warm --n N` T2→T7 overlay latency samples → `%LOCALAPPDATA%\Zshot\logs\perf-summary.json`
- `--stress-overlay N` loop overlay show/hide; sample Private Working Set at iterations 10 and 100

PNGs go to `Pictures\Zshot`. Timing marks T0–T7 append to `%LOCALAPPDATA%\Zshot\logs\perf.log`.

## OCR and translation

Toolbar 取字 runs PP-OCRv6 Small locally; 翻译 sends the recognized text to your
own OpenAI-compatible endpoint. Both show a read-only panel with a copy button.

Models are **not** in this repo. Ship them next to the exe as `models\v6\`
(`PP-OCRv6_det_small.onnx`, `PP-OCRv6_rec_small.onnx`, `ppocrv6_dict.txt`), or
point `ZSHOT_MODELS` at a directory holding those three for a `target\` build:

```text
set ZSHOT_MODELS=F:\own\Zshot\build\app\models\v6
```

Settings live in `%LOCALAPPDATA%\Zshot\settings.json`; every key is optional.

```json
{
  "ocr": { "modelsDir": "D:/Zshot/models/v6" },
  "translation": {
    "baseUrl": "https://api.openai.com/v1",
    "model": "gpt-4o-mini",
    "targetLanguage": "zh-CN",
    "prompt": "",
    "timeoutSeconds": 30
  }
}
```

The API key is **not** in that file. It stays in the Windows credential vault
under resource `Zshot.Translation`, account `apiKey` — the same entry the C#
build writes, so configuring it there covers both.

## Phase 1 notes

- Capture: WGC first, `DXGI DuplicateOutput1` on failure / timeout / near-black / exclusive fullscreen (same-adapter only). Each monitor gets 500 ms to hand back a frame.
- Overlay freeze is a composition swapchain on DComp visual 0 (no full-frame CPU blit)
- Exclusive-fullscreen region: grab the game frame, then force the freeze overlay
- Not in this PoC: annotation editor, long capture, AVIF/JXL/UHDR
- OCR runs in-process with a lazily built ORT session that drops after 60 s idle,
  not the separate `Zshot.Ocr.exe` worker the design doc calls for — revisit if
  resident memory does not fall back after that drop
- The vendored GPUI delta is `patches/gpui-0.2.2-zshot-overlay.patch` (293 added lines / 8 files); see `patches/README.md` before upgrading gpui

Decision record: `F:\own\Zshot\docs\Phase1-GPUI-Overlay决策记录.md`
