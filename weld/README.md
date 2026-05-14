# weld

CEF surface adapter: drives Chromium Embedded Framework in accelerated
off-screen rendering (OSR) mode and imports each rendered frame into the host's
wgpu pipeline as a GPU texture.

## CEF Foibles

Three constraints govern every use of this crate. Understand them before writing
any integration code.

### 1. The Subprocess Tax

CEF spawns renderer, GPU, and utility processes by **re-executing the host
binary**. The embedder must call `CefRuntime::execute_process_from` at the
absolute start of `main()`, before any other initialization, and exit immediately
if it returns `Some(exit_code)`:

```rust
fn main() {
    let cef_path = std::env::var("CEF_PATH").expect("CEF_PATH required");
    if let Some(code) = weld::CefRuntime::execute_process_from(cef_path.as_ref())
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

On Windows, `OnAcceleratedPaint` provides a `HANDLE` to a shared D3D11 texture.
**The handle is valid only for the duration of the callback.** `weld` imports
it synchronously (or `DuplicateHandle`s it) inside the callback before returning.
Never store the raw handle across the callback boundary.

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
| Windows | `HANDLE` (shared D3D11 texture) | D3D11 open-shared → D3D12 resource → wgpu |
| macOS | `IOSurfaceRef` | IOSurface → MTLTexture → wgpu Metal |
| Linux | DMABUF fd (planned) | VK_EXT_external_memory_dma_buf → wgpu Vulkan |

Both the Windows and macOS paths follow the same pattern as `wgpu-scry`'s
`native_frame` module. The GPU import bodies are structurally identical; only
how the handle is obtained differs (CEF callback vs WGC capture / SCKit).

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
