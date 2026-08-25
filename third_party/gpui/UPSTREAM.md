# gpui upstream

- crate: gpui
- version: 0.2.2
- registry: crates.io
- license: Apache-2.0
- checksum: 979b45cfa6ec723b6f42330915a1b3769b930d02b2d505f9697f8ca602bee707
- local patches:
  1. WindowOptions.topmost -> WS_EX_TOPMOST at creation
  2. DirectComposition dual visuals + set_background_swap_chain
  3. App.with_gpu_context / gpu_generation / adapter_luid
  4. on_gpu_reset_begin before device recreate
  5. gpu_generation fetch_add on the main thread next to DirectXDevices swap
  6. Window.win32_hwnd / wait_for_commit_completion / on_app_ready_sentinel (WM_APP+32)
