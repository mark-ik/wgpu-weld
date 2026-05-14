# wgpu-weld: CEF accelerated OSR → wgpu

**Date:** 2026-05-14  
**Status:** skeleton complete; CEF vtable wiring pending  
**Sibling crates:** `wgpu-graft` (WinRT / WebView2), `wgpu-scry` (WGC / ScreenCaptureKit)

---

## Goal

Provide a clean, cross-platform Rust crate (`weld`) that routes CEF's
`OnAcceleratedPaint` GPU texture handles into a caller-supplied wgpu pipeline —
zero-copy on all three platforms.

---

## CEF foibles that shape the design

### 1. Subprocess tax

CEF re-executes the host binary as its renderer, GPU, and utility subprocesses.
`cef_execute_process` must be the very first call in `main()`, before any
framework initialisation (winit, wgpu, thread pools). If subprocesses run host
initialisation code, the results are undefined.

**Consequence for `weld`:**  
`CefRuntime::execute_process_from(path)` is a static method that loads
`libcef` temporarily, calls `execute_process`, and returns
`Ok(Some(exit_code))` for subprocesses before any `CefRuntime` is constructed.
The demo's `main()` calls this as line one.

### 2. Handle lifetime in `OnAcceleratedPaint`

The `CefAcceleratedPaintInfo` handle is transient:

| Platform | Handle type       | Lifetime                              | weld strategy                   |
|----------|-------------------|---------------------------------------|---------------------------------|
| Windows  | Win32 `HANDLE`    | Valid only during the callback        | `DuplicateHandle` before return |
| macOS    | `IOSurfaceRef`    | Ref-counted; CEF holds one ref        | `CFRetain` before return        |
| Linux    | DMABUF fd (planned) | Valid only during the callback      | `dup(2)` before return          |

The duplicated / retained handle lives in `Arc<Mutex<FrameSlot>>` until
`acquire_frame` imports it into wgpu and releases it.

### 3. No link-time CEF dependency

CEF is not a system library — it must be bundled with the app. Statically
linking `libcef` would force every downstream crate to have CEF installed at
compile time.

**Consequence for `weld`:**  
All CEF entry points are resolved at runtime via `libloading`. `CefFunctions`
stores the raw function pointers; `CefRuntime` holds the `Arc<Library>` that
backs them. The `weld` crate compiles without CEF_PATH set.

---

## Architecture

```
                  ┌────────────────────────────────────────────────┐
                  │  host process (winit + wgpu)                   │
                  │                                                │
                  │  CefRuntime (process-scoped singleton)         │
                  │    ├─ Arc<Library>  (libcef)                   │
                  │    └─ Arc<CefFunctions>  (resolved fn ptrs)    │
                  │                                                │
                  │  PlatformCefProducer  (per browser)            │
                  │    ├─ Arc<FrameSlot>  ──────────────────────┐  │
                  │    └─ Arc<EventQueues>                       │  │
                  └─────────────────────────────────────────────┼──┘
                                                                │
             ┌──────────────────────────────────────────────────▼──┐
             │  CEF render-handler callback thread                  │
             │  OnAcceleratedPaint:                                 │
             │    Windows: DuplicateHandle → Dx12SharedTexture      │
             │    macOS:   CFRetain         → MetalTextureRef        │
             │    Linux:   dup(2)           → VulkanExternalImage    │
             │    store → FrameSlot                                 │
             └──────────────────────────────────────────────────────┘

  host calls:
    producer.acquire_frame(&host_ctx)
      → WgpuTextureImporter::import(frame, ctx)
        → Windows: D3D11::OpenSharedResource1 → D3D12 → wgpu HAL
        → macOS:   MTLDevice::newTexture(fromIOSurface) → wgpu HAL Metal
        → Linux:   vkCreateImage + VkImportMemoryFdInfoKHR → wgpu HAL Vulkan
```

---

## Module map

| Path | Content |
|------|---------|
| `weld/src/lib.rs` | Flat re-exports; `PlatformCefProducer` / `PlatformCefConfig` aliases |
| `weld/src/error.rs` | `WeldError` |
| `weld/src/runtime.rs` | `CefRuntime`, `CefRuntimeConfig`, `CefLogSeverity` |
| `weld/src/surface.rs` | `CefSurfaceProducer` trait, input types, `NavigationEvent` |
| `weld/src/native_frame/mod.rs` | `NativeFrame`, `WgpuTextureImporter`, `ImportedTexture`, `HostWgpuContext` |
| `weld/src/cef_ffi/mod.rs` | `CefFunctions` (libloading resolution) |
| `weld/src/cef_ffi/types.rs` | CEF C API types (`CefSettings`, `CefWindowInfo`, `CefAcceleratedPaintInfo`, …) |
| `weld/src/windows_cef/mod.rs` | `WindowsCefProducer`, `WindowsCefConfig` |
| `weld/src/macos_cef/mod.rs` | `MacosCefProducer`, `MacosCefConfig` |
| `weld/src/linux_cef/mod.rs` | `LinuxCefProducer`, `LinuxCefConfig` (scaffold) |
| `demo-weld-win/src/main.rs` | Windows demo: subprocess guard + stub event loop |

---

## Done conditions

### Phase 1 — CEF vtable wiring (Windows)

- [ ] Allocate `cef_client_t` + `cef_render_handler_t` in `windows_cef`  
- [ ] `OnAcceleratedPaint`: `DuplicateHandle`, store in `FrameSlot`  
- [ ] `OnAfterCreated`: capture `browser_id`  
- [ ] `acquire_frame` imports via D3D11 → D3D12 → wgpu (unblock `import_dx12` stub)  
- [ ] Demo renders a live CEF frame into a winit window  

### Phase 2 — Input and navigation

- [ ] Mouse / keyboard translation (`CefMouseEvent`, `CefKeyEvent`)  
- [ ] `resize` → `was_resized()`  
- [ ] Navigation methods (`load_url`, `go_back`, `reload`, …)  
- [ ] `execute_script` via `cef_frame_t::execute_java_script`  
- [ ] `poll_navigation_event` from `cef_load_handler_t` callbacks  
- [ ] `post_web_message` / `poll_web_message` via `cef_process_message_t`  

### Phase 3 — macOS

- [ ] `OnAcceleratedPaint`: `CFRetain(io_surface)`, store in `FrameSlot`  
- [ ] `import_metal`: `IOSurface::newTextureWithDescriptor` → wgpu HAL Metal  
- [ ] Demo on macOS  

### Phase 4 — Linux

- [ ] Track CEF Linux DMABUF API stabilisation  
- [ ] `dup(2)` pattern; `import_vulkan` via `VK_KHR_external_memory_fd`  

---

## Comparison with siblings

| | `wgpu-graft` | `wgpu-scry` | `wgpu-weld` |
|--|--|--|--|
| Engine | WebView2 (WinRT) | WKWebView / Edge via WGC | CEF (Chromium Embedded Framework) |
| Distribution | System / auto-update | System | App-bundled |
| Subprocess tax | None | None | Must call `execute_process_from` first in `main()` |
| Frame source | `ICoreWebView2AcceleratedPaintInfo` | WGC / ScreenCaptureKit | `CefAcceleratedPaintInfo` |
| Handle lifetime | Long (WinRT manages) | Long (capture session) | **Callback-scoped** — must dup/retain |
| Linux support | No | Partial (WGC = Windows only) | Planned (DMABUF) |
| CPU fallback | No | No | `cpu-paint-fallback` feature |
