# weld

CEF surface adapter: drives Chromium Embedded Framework in accelerated
off-screen rendering (OSR) mode and imports rendered frames into the host's
wgpu pipeline as GPU textures.

Current implementation checkpoint: runtime loading/subprocess probing, the
native-frame mailbox, and the Windows D3D shared-handle → `wgpu` D3D12 importer
are present. CEF client/render-handler vtable wiring and live browser creation
are still pending.

## CEF Foibles

Three constraints govern every use of this crate. Understand them before writing
any integration code.

### 1. The Subprocess Tax

CEF spawns renderer, GPU, and utility processes by **re-executing the host
binary**. The embedder must call `CefRuntime::execute_process_from` at the
absolute start of `main()`, before any other initialization, and exit immediately
if it returns `Some(exit_code)`:

```rust,no_run
fn main() {
    let cef_path = std::env::var("CEF_PATH").expect("CEF_PATH required");
    if let Some(code) = welding::CefRuntime::execute_process_from(cef_path.as_ref())
        .expect("failed to probe CEF subprocess role")
    {
        std::process::exit(code);
    }
    // ... rest of main
}
```

Forgetting this causes subprocess slots to go unfilled. The GPU process never
starts; you get a blank OSR surface and no `OnAcceleratedPaint` callbacks.

### 2. Handle Lifetime

On Windows, `OnAcceleratedPaint` provides a `HANDLE` to a shared D3D texture.
**The handle is valid only for the duration of the callback.** `weld` duplicates
it inside the callback before returning, stores the duplicate in a
`PendingFrameSlot`, and closes that duplicate after the host-side D3D12 import.
Never store CEF's raw callback handle across the callback boundary.

On macOS the analogous constraint is: the `IOSurfaceRef` passed to
`OnAcceleratedPaint` is retained for the duration of the callback. `weld` calls
`IOSurfaceIncrementUseCount` (via `objc2-io-surface`) before the callback
returns.

### 3. Distribution Path

CEF is **not** a system library. `libcef.dll` / `libcef.so` / `Chromium Embedded
Framework.framework` must be distributed alongside your application and located
at a known path. Pass that path to `CefRuntimeConfig::new`. The `weld` crate
has no link-time dependency on CEF (it uses `libloading`), so it compiles without
CEF present — the runtime panics with a clear error if the library is not found
when `CefRuntime::initialize` is called.

## Platform texture paths

| Platform | CEF output | Import path |
| --- | --- | --- |
| Windows | `HANDLE` (shared D3D texture) | D3D12 `OpenSharedHandle` → `wgpu` D3D12 texture |
| macOS | `IOSurfaceRef` | Planned: IOSurface → MTLTexture → wgpu Metal |
| Linux | native pixmap / DMABUF planes | Planned: `VK_EXT_external_memory_dma_buf` → wgpu Vulkan |

The Windows path follows the same shape as `wgpu-scry`'s `native_frame` module.
The source differs: CEF gives a callback-scoped shared handle, while scrying's
WebView2 path receives a capture-owned shared texture after WGC has copied the
composited visual.

## Producer / consumer contract

- **`weld` owns:** CEF initialization and subprocess detection, `OnAcceleratedPaint`
  callback wiring, GPU texture import, `CefSurfaceProducer` trait.
- **The host owns:** the event loop, window/HWND creation, calling
  `CefRuntime::do_message_loop_work()` on each tick (or running
  `CefRuntime::run_message_loop()` on a dedicated thread), and passing the
  resulting `ImportedTexture` to its render pipeline.

## Accelerated OSR availability

`OnAcceleratedPaint` requires CEF to be built with GPU support and
`windowless_rendering_enabled = true` in `cef_browser_settings_t`. Verify at
runtime with `CefSurfaceCapabilities::probe`. The `cpu-paint-fallback` feature
enables the slower `OnPaint` CPU-bitmap path as a fallback.
