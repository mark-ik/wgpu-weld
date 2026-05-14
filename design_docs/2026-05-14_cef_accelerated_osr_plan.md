# wgpu-weld: CEF accelerated OSR → wgpu

**Date:** 2026-05-14  
**Status:** `cef-runtime` feature added; `cef`/`cef-dll-sys` elected as ABI layer; render-handler vtables wired for Windows (DuplicateHandle) and macOS (CFRetain); Linux scaffold under `cef-runtime`; input methods pending
**Sibling crates:** `wgpu-graft` (Servo / GL-FBO interop), `wgpu-scry` (system webviews / WGC / ScreenCaptureKit)

---

## Goal

Provide a clean, cross-platform Rust crate (`weld`) that routes CEF's
`OnAcceleratedPaint` GPU texture handles into a caller-supplied wgpu pipeline.
CEF's handles are callback-scoped, so the real contract is: duplicate/retain in
the callback, store only an owned handle, then import from the host renderer.
Windows is the first concrete import path; macOS and Linux remain planned.

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
| Linux    | native pixmap / DMABUF planes | Valid only during the callback | `dup(2)` before return |

The duplicated / retained handle lives in `Arc<Mutex<PendingFrameSlot>>` until
`acquire_frame` imports it into wgpu and releases it.

### 3. ABI layer: `cef`/`cef-dll-sys` (decided)

Hand-rolling `cef_client_t`, `cef_render_handler_t`, `CefWindowInfo`, and
`AcceleratedPaintInfo` is too dangerous — struct layouts drift with every CEF
version bump. The `cef` crate (tauri-apps/cef-rs) provides generated,
version-stamped bindings and `wrap_render_handler!` / `wrap_client!` / `wrap_app!`
macros that eliminate hand-written vtable allocation.

**Dividing line:**

- **`cef` crate owns:** runtime calls (`initialize`, `execute_process`, `shutdown`), ref-counted object wrappers, handler vtable macros, `WindowInfo`, `BrowserSettings`, `AcceleratedPaintInfo`.
- **`weld` owns:** callback handle duplication/retain policy, `PendingFrameSlot`, normalized `NativeFrame` variants, `WgpuTextureImporter`, `CefSurfaceProducer` public trait, event queues.

**Build-time tradeoff:** `cef-dll-sys` downloads/links CEF at build time (or uses
`CEF_PATH`). The old `cef_ffi/` `libloading` skeleton remains as the non-`cef-runtime`
scaffold path; it still compiles but all producer constructors return a pending-wiring
error without the feature flag. `cef_ffi/` will be deleted once `cef-runtime` matures.

**`cef-runtime` feature enables:** real `CefRuntime::initialize`, working
`WindowsCefProducer::new` / `MacosCefProducer::new` / `LinuxCefProducer::new`,
and `on_accelerated_paint` with correct handle duplication.

Reference: [godot-cef webrender.rs](https://github.com/dsh0416/godot-cef) for
the `impl cef::RenderHandler` + `on_accelerated_paint` pattern.

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
                  │    ├─ Arc<PendingFrameSlot> ────────────────┐  │
                  │    └─ Arc<EventQueues>                       │  │
                  └─────────────────────────────────────────────┼──┘
                                                                │
             ┌──────────────────────────────────────────────────▼──┐
             │  CEF render-handler callback thread                  │
             │  OnAcceleratedPaint:                                 │
             │    Windows: DuplicateHandle → Dx12SharedTexture      │
             │    macOS:   CFRetain         → MetalTextureRef        │
             │    Linux:   dup(2)           → DmaBufImage            │
             │    store → PendingFrameSlot                          │
             └──────────────────────────────────────────────────────┘

  host calls:
    producer.acquire_frame(&host_ctx)
      → WgpuTextureImporter::import(frame, ctx)
        → Windows: D3D12::OpenSharedHandle → wgpu HAL Dx12
        → macOS:   planned IOSurface → MTLTexture → wgpu HAL Metal
        → Linux:   planned vkCreateImage + VkImportMemoryFdInfoKHR → wgpu HAL Vulkan
```

---

## Module map

| Path | Content |
|------|---------|
| `weld/src/lib.rs` | Flat re-exports; `PlatformCefProducer` / `PlatformCefConfig` aliases |
| `weld/src/error.rs` | `WeldError` |
| `weld/src/runtime.rs` | `CefRuntime`, `CefRuntimeConfig`, `CefLogSeverity` |
| `weld/src/surface.rs` | `CefSurfaceProducer` trait, input types, `NavigationEvent` |
| `weld/src/native_frame/mod.rs` | `NativeFrame`, `PendingFrameSlot`, `WgpuTextureImporter`, `ImportedTexture`, `HostWgpuContext` |
| `weld/src/cef_ffi/mod.rs` | `CefFunctions` (libloading resolution) |
| `weld/src/cef_ffi/types.rs` | CEF C API types (`CefSettings`, `CefWindowInfo`, `CefAcceleratedPaintInfo`, …) |
| `weld/src/windows_cef/mod.rs` | `WindowsCefProducer`, `WindowsCefConfig` |
| `weld/src/macos_cef/mod.rs` | `MacosCefProducer`, `MacosCefConfig` |
| `weld/src/linux_cef/mod.rs` | `LinuxCefProducer`, `LinuxCefConfig` (scaffold) |
| `demo-weld-win/src/main.rs` | Windows demo: subprocess guard + stub event loop |

---

## Done conditions

### Phase 1 — CEF vtable wiring (Windows)

- [x] Binding strategy decided: `cef`/`cef-dll-sys` as ABI layer, `cef-runtime` feature gate
- [x] `wrap_render_handler!` + `wrap_client!` wired in `windows_cef` under `cef-runtime`
- [x] `OnAcceleratedPaint`: `DuplicateHandle` → `Dx12SharedTexture` → `PendingFrameSlot`
- [x] `browser_host_create_browser_sync` → `browser.identifier()` captures `browser_id`
- [x] `PendingFrameSlot` latest-frame mailbox with generation tracking
- [x] `acquire_frame` imports owned D3D shared handle via D3D12 `OpenSharedHandle` → wgpu
- [x] `CefRuntime` uses `cef::initialize` / `cef::execute_process` / `cef::shutdown`
- [x] Public producer methods return explicit errors (not panics) without `cef-runtime`
- [x] `resize` → `host.was_resized()`; navigation methods wired under `cef-runtime`
- [ ] Mouse / keyboard input translation (`CefMouseEvent`, `CefKeyEvent`)
- [ ] Demo renders a live CEF frame into a winit window  

### Phase 2 — Input and navigation

- [ ] Mouse / keyboard translation (`CefMouseEvent`, `CefKeyEvent`)  
- [ ] `resize` → `was_resized()`  
- [ ] Navigation methods (`load_url`, `go_back`, `reload`, …)  
- [ ] `execute_script` via `cef_frame_t::execute_java_script`  
- [ ] `poll_navigation_event` from `cef_load_handler_t` callbacks  
- [ ] `post_web_message` / `poll_web_message` via `cef_process_message_t`  

### Phase 3 — macOS

- [ ] `OnAcceleratedPaint`: `CFRetain(io_surface)`, store in `PendingFrameSlot`
- [ ] `import_metal`: `IOSurface::newTextureWithDescriptor` → wgpu HAL Metal  
- [ ] Demo on macOS  

### Phase 4 — Linux

- [ ] Map CEF native-pixmap plane metadata into `DmaBufImage`
- [ ] `dup(2)` pattern; `import_vulkan` via `VK_KHR_external_memory_fd`  

---

## Comparison with siblings

| | `wgpu-graft` | `wgpu-scry` | `wgpu-weld` |
|--|--|--|--|
| Engine | Servo | WebView2 / WKWebView / WPE | CEF (Chromium Embedded Framework) |
| Distribution | App-bundled Servo dependency | OS-provided webview | App-bundled Chromium |
| Subprocess tax | None | None | Must call `execute_process_from` first in `main()` |
| Frame source | Servo/surfman GL FBO | WGC / ScreenCaptureKit / WPE DMABUF | `CefAcceleratedPaintInfo` |
| Handle lifetime | Producer-owned GL/native resource | Capture/session-owned native frame | **Callback-scoped** — must dup/retain |
| Linux support | GL FBO → Vulkan external memory | WPE scaffold / DMABUF planned | Native pixmap / DMABUF planned |
| CPU fallback | Servo readback demos | snapshots / overlay fallback | `cpu-paint-fallback` feature |
