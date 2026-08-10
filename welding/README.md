# welding

CEF surface adapter: drives Chromium Embedded Framework in accelerated
off-screen rendering (OSR) mode and imports rendered frames into the host's
wgpu pipeline as GPU textures.

Current implementation checkpoint: runtime loading and subprocess probing, CEF
client/render-handler vtable wiring, live browser creation, input and
navigation. Windows (D3D11 shared-texture copy → `wgpu` D3D12) and Linux
(DMABUF → Vulkan external memory) are both exercised end to end by their demos.
macOS compiles but has not been run.

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
**The handle is valid only for the duration of the callback.** `welding`
duplicates it temporarily, opens the CEF resource with D3D11, and copies into a
weld-owned shared texture before returning. The host later imports only that owned copy
through D3D12. Never store either CEF's raw callback handle or its pooled
resource across the callback boundary.

On macOS the analogous constraint is: the `IOSurfaceRef` passed to
`OnAcceleratedPaint` is CEF's for the duration of the callback. `welding` calls
`CFRetain` on it before returning, and the Metal importer releases it once
`MTLDevice::newTextureWithDescriptor:iosurface:plane:` has taken its own
reference.

On Linux the planes' file descriptors are callback-scoped, so `welding` `dup(2)`s
each one before storing it. The Vulkan importer then takes ownership, since
`vkAllocateMemory` closes an imported dmabuf fd itself.

### 3. Distribution Path

CEF is **not** a system library. `libcef.dll` / `libcef.so` / `Chromium Embedded
Framework.framework` must be distributed alongside your application and located
at a known path. Pass that path to `CefRuntimeConfig::new`. The crate uses the
generated `cef` / `cef-dll-sys` bindings; `cef-runtime` builds require a
matching CEF distribution.

## Platform texture paths

Each arm lives in its own module under `native_frame`, `cfg`-gated to its
platform.

| Platform | CEF output | Import path |
| --- | --- | --- |
| Windows | `HANDLE` (pooled shared D3D texture) | callback-time D3D11 `CopyResource` → owned shared texture → D3D12 `OpenSharedHandle` → `wgpu` D3D12 texture |
| macOS | `IOSurfaceRef` | IOSurface retain → MTLTexture → wgpu Metal (compiles; not yet run) |
| Linux | native pixmap / DMABUF planes | DMABUF dup → `VK_EXT_external_memory_dma_buf` → wgpu Vulkan (verified on Fedora 44 + Intel/Mesa) |

The Windows path follows the same shape as `wgpu-scry`'s `native_frame` module.
The source differs: CEF gives a callback-scoped shared handle, while scrying's
WebView2 path receives a capture-owned shared texture after WGC has copied the
composited visual. The `OpenSharedHandle` step itself is not reimplemented here;
on Windows `welding` calls into
[`grafting`](https://crates.io/crates/grafting), the interop core shared with
`wgpu-graft`.

## Producer / consumer contract

- **`welding` owns:** CEF initialization and subprocess detection, `OnAcceleratedPaint`
  callback wiring, GPU texture import, `CefSurfaceProducer` trait.
- **The host owns:** the event loop, window/HWND creation, and passing the
  resulting `ImportedTexture` to its render pipeline. Windows uses CEF's
  dedicated message-loop thread; other platforms call
  `CefRuntime::do_message_loop_work()` on host-loop ticks.

## Accelerated OSR availability

`OnAcceleratedPaint` requires CEF to be built with GPU support and
`windowless_rendering_enabled = true` in `cef_settings_t`. Verify at
runtime with `CefSurfaceCapabilities::probe`. The `cpu-paint-fallback` feature
enables the slower `OnPaint` CPU-bitmap path as a fallback.
