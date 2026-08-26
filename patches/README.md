# Vendored GPUI delta

`third_party/gpui` is gpui **0.2.2** from crates.io with a local patch, wired in
through `[patch.crates-io]` in the workspace `Cargo.toml`. The vendored tree is
3.3 MB, but the actual delta is small — keep it that way.

`gpui-0.2.2-zshot-overlay.patch` is that delta: **293 added lines, 17 removed,
across 8 source files**. Everything else in `third_party/gpui` is pristine
upstream (with `examples/` and `UPSTREAM.md` excluded from the diff).

## What the patch adds

| File | Change |
| --- | --- |
| `src/app.rs` | `App::gpu_generation`, `adapter_luid`, `with_gpu_context`, `on_gpu_reset_begin` |
| `src/platform.rs` | The same four on the `Platform` trait, plus `set_background_swap_chain`, `wait_for_commit_completion`, `on_app_ready_sentinel` |
| `src/window.rs` | `Window::set_background_swap_chain`, `win32_hwnd`, `wait_for_commit_completion`, `on_app_ready_sentinel` |
| `src/platform/windows/platform.rs` | Windows implementations of the trait additions |
| `src/platform/windows/window.rs` | `WindowOptions.topmost` → `WS_EX_TOPMOST` at creation |
| `src/platform/windows/directx_devices.rs` | `adapter_luid()` on the device set |
| `src/platform/windows/directx_renderer.rs` | Second DirectComposition visual + `set_freeze_swap_chain` |
| `src/platform/windows/events.rs` | `WM_APP+32` ready sentinel and the GPU-reset-begin notification |

Zshot depends on all of it: the freeze frame is DComp visual 0 under GPUI's own
UI visual, capture shares GPUI's `ID3D11Device`, and the T7 perf mark is the
`WM_APP+32` sentinel. `crate::platform::WM_APP_READY_SENTINEL` must stay equal to
the value in `events.rs`.

## Regenerating the patch

Run from the repo root after changing anything under `third_party/gpui`:

```bash
bash patches/regen.sh
```

## Upgrading gpui

1. Unpack the new version next to the old one.
2. `patch -p1 --dry-run < patches/gpui-0.2.2-zshot-overlay.patch` against the new
   tree and fix whatever rejects.
3. Replace `third_party/gpui`, delete `examples/`, keep `UPSTREAM.md` current.
4. Regenerate the patch and rename it for the new version.

The decision record (`F:\own\Zshot\docs\Phase1-GPUI-Overlay决策记录.md` §2) lists
what the patch is *not* allowed to grow into: no major-version bump, no native
surface portal, no changes to IME / text layout / atlas / the general renderer,
and no capture code inside GPUI.
