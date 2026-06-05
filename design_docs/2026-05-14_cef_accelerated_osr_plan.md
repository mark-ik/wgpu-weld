# wgpu-weld: CEF accelerated OSR → wgpu

**Date:** 2026-05-14 (revised 2026-06-02: Windows pooled-resource lifetime correction)
**Status:** Phase 1 + 2 complete (Windows); crate renamed `welding`; Phase 3 handler wiring + `import_metal` implemented (pending macOS compile validation); Phase 4 (Linux) verified end-to-end on Fedora 44 + Intel/Mesa — example.com renders into the wgpu surface, mouse input routes through to CEF navigation
**Sibling crates:** `wgpu-graft` (Servo / GL-FBO interop), `wgpu-scry` (system webviews / WGC / ScreenCaptureKit)

---

## Goal

Provide a clean, cross-platform Rust crate (`welding`) that routes CEF's
`OnAcceleratedPaint` GPU texture handles into a caller-supplied wgpu pipeline.
CEF's handles are callback-scoped, so the real contract is: duplicate/retain in
the callback, store only an owned resource, then import from the host renderer.
Windows is the first concrete path; macOS import code exists pending runtime
validation, and Linux DMABUF import has been verified on Fedora 44 + Intel/Mesa.

**2026-06-02 Windows correction:** CEF's pooled Windows resource must not escape
`OnAcceleratedPaint`, even through a duplicated handle. Windows now duplicates
the handle only long enough to open the CEF D3D11 resource, copies into a
weld-owned shared texture inside the callback, and exposes only that owned copy
to the D3D12/wgpu host path.

---

## CEF foibles that shape the design

### 1. Subprocess tax

CEF re-executes the host binary as its renderer, GPU, and utility subprocesses.
`cef_execute_process` must be the very first call in `main()`, before any
framework initialisation (winit, wgpu, thread pools). If subprocesses run host
initialisation code, the results are undefined.

**Consequence for `welding`:**  
`CefRuntime::execute_process_from(path)` is a static method that loads
`libcef` temporarily, calls `execute_process`, and returns
`Ok(Some(exit_code))` for subprocesses before any `CefRuntime` is constructed.
The demo's `main()` calls this as line one.

### 2. Handle lifetime in `OnAcceleratedPaint`

The `CefAcceleratedPaintInfo` handle is transient:

| Platform | Handle type       | Lifetime                              | weld strategy                   |
|----------|-------------------|---------------------------------------|---------------------------------|
| Windows  | Win32 `HANDLE`    | Resource valid only during callback   | D3D11 copy into weld-owned shared texture before return |
| macOS    | `IOSurfaceRef`    | Ref-counted; CEF holds one ref        | `CFRetain` before return        |
| Linux    | native pixmap / DMABUF planes | Valid only during the callback | `dup(2)` before return |

The retained macOS and duplicated Linux resources live in
`Arc<Mutex<PendingFrameSlot>>` until `acquire_frame` imports them into wgpu and
releases them. Windows stores an already imported weld-owned copy.

### 3. ABI layer: `cef`/`cef-dll-sys` (decided)

Hand-rolling `cef_client_t`, `cef_render_handler_t`, `CefWindowInfo`, and
`AcceleratedPaintInfo` is too dangerous — struct layouts drift with every CEF
version bump. The `cef` crate (tauri-apps/cef-rs) provides generated,
version-stamped bindings and `wrap_render_handler!` / `wrap_client!` / `wrap_app!`
macros that eliminate hand-written vtable allocation.

**Dividing line:**

- **`cef` crate owns:** runtime calls (`initialize`, `execute_process`, `shutdown`), ref-counted object wrappers, handler vtable macros, `WindowInfo`, `BrowserSettings`, `AcceleratedPaintInfo`.
- **`welding` owns:** callback handle duplication/retain policy, `PendingFrameSlot`, normalized `NativeFrame` variants, `WgpuTextureImporter`, `CefSurfaceProducer` public trait, event queues.

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
             │    Windows: D3D11 copy → owned Dx12SharedTexture     │
             │    macOS:   CFRetain         → MetalTextureRef        │
             │    Linux:   dup(2)           → DmaBufImage            │
             │    store → PendingFrameSlot                          │
             └──────────────────────────────────────────────────────┘

  host calls:
    producer.acquire_frame(&host_ctx)
      → WgpuTextureImporter::import(frame, ctx)
        → Windows: already imported callback-time D3D11 copy
        → macOS:   IOSurface → MTLTexture → wgpu HAL Metal
        → Linux:   vkCreateImage + VkImportMemoryFdInfoKHR → wgpu HAL Vulkan
```

---

## Module map

| Path | Content |
|------|---------|
| `welding/src/lib.rs` | Flat re-exports; `PlatformCefProducer` / `PlatformCefConfig` aliases |
| `welding/src/error.rs` | `WeldError` |
| `welding/src/runtime.rs` | `CefRuntime`, `CefRuntimeConfig`, `CefLogSeverity` |
| `welding/src/surface.rs` | `CefSurfaceProducer` trait, input types, `NavigationEvent` |
| `welding/src/native_frame/mod.rs` | `NativeFrame`, `PendingFrameSlot`, `WgpuTextureImporter`, `ImportedTexture`, `HostWgpuContext` |
| `welding/src/cef_ffi/mod.rs` | `CefFunctions` (libloading resolution) |
| `welding/src/cef_ffi/types.rs` | CEF C API types (`CefSettings`, `CefWindowInfo`, `CefAcceleratedPaintInfo`, …) |
| `welding/src/windows_cef/mod.rs` | `WindowsCefProducer`, `WindowsCefConfig` |
| `welding/src/macos_cef/mod.rs` | `MacosCefProducer`, `MacosCefConfig` |
| `welding/src/linux_cef/mod.rs` | `LinuxCefProducer`, `LinuxCefConfig` |
| `demo-weld-win/src/main.rs` | Windows demo: subprocess guard + stub event loop |

---

## Done conditions

### Phase 1 — CEF vtable wiring (Windows)

- [x] Binding strategy decided: `cef`/`cef-dll-sys` as ABI layer, `cef-runtime` feature gate
- [x] `wrap_render_handler!` + `wrap_client!` wired in `windows_cef` under `cef-runtime`
- [x] `OnAcceleratedPaint`: callback-scoped D3D11 source → copied weld-owned shared texture → callback-time D3D12/wgpu import → frame slot
- [x] `browser_host_create_browser` → `on_after_created` installs browser + captures `browser_id`
- [x] `PendingFrameSlot` latest-frame mailbox with generation tracking
- [x] `acquire_frame` imports owned D3D shared handle via D3D12 `OpenSharedHandle` → wgpu
- [x] `CefRuntime` uses `cef::initialize` / `cef::execute_process` / `cef::shutdown`
- [x] Public producer methods return explicit errors (not panics) without `cef-runtime`
- [x] `resize` → `host.was_resized()`; navigation methods wired under `cef-runtime`
- [x] Mouse / keyboard input translation (`cef::MouseEvent`, `cef::KeyEvent`, `cef_input` module)
- [x] Demo renders a live CEF frame into a winit window (demo-weld-win: full blit pipeline, keyboard/mouse forwarding)  

### Phase 2 — Input and navigation

- [x] `resize` → `was_resized()` (done in Phase 1; left here for cross-reference)
- [x] Navigation methods (done in Phase 1; left here for cross-reference)
- [x] `execute_script` via `cef_frame_t::execute_java_script`
- [x] `poll_navigation_event` from `cef_load_handler_t` / `WeldLoadHandler` + `WeldDisplayHandler` callbacks
- [x] `post_web_message` / `poll_web_message` via JS `dispatchEvent` + `cef_process_message_t`

### Phase 3 — macOS

- [x] `OnAcceleratedPaint`: `CFRetain(io_surface)`, store in `PendingFrameSlot`
- [x] `import_metal`: `IOSurface::newTextureWithDescriptor_iosurface_plane` → `wgpu_hal::metal::Device::texture_from_raw` → wgpu HAL Metal
- [x] All handler fixes: `CefStringUserfree` conversions, `ImplBrowser/Host/Frame` imports, `#[allow]` on impl
- [ ] Demo on macOS (validates `import_metal` at runtime)  

### Phase 4 — Linux

**Upstream status (verified 2026-05-15):** the Linux DMABUF accelerated-OSR API
has been in CEF since `260dd0ca` (2024-03-08, "osr: Implement shared texture
support", fixes #1006, #2575) — same PR that landed Windows. The Linux variant
of `cef_accelerated_paint_info_t` in `include/internal/cef_types_linux.h`
defines:

- `planes[kAcceleratedPaintMaxPlanes]` (max 4): `{ fd, stride, offset, size }`
- `plane_count`
- `modifier` (DRM format modifier; `DRM_FORMAT_MOD_INVALID` if none)
- `format` (`cef_color_type_t`, currently BGRA8/RGBA8)
- `extra` (common metadata: timestamp, coded_size, visible_rect, etc.)

Commit `189b2472` (2024-12-03, fixes #3730) added partial-update info to the
callback. The Linux variant is fully defined in the master branch and stable
release branches (`>= 6261` ≈ Chromium 124+). The `cef = "148"` crate exposes
`AcceleratedPaintInfo.planes` / `plane_count` / `modifier` as named fields,
so no raw-sys access is needed.

> **Note on `cef_window_info_t.shared_texture_enabled` docstring.** The header
> comment still reads *"Currently only supported on Windows (D3D11)"* — that
> note is stale (cef#3687); the field works on Linux when CEF was built with
> GPU acceleration enabled.

**Wild-caught implementation notes** (cef#3687 thread, `adriannepilleboue`,
CEF 127.3.5 + Vulkan + X11 + GLFW + Intel):

- Vulkan `vkImportMemoryFdInfoKHR` **takes ownership of and closes** the
  passed fd. CEF expects the fd to remain valid for the duration of the
  callback. Therefore `dup(2)` the fd at the start of `OnAcceleratedPaint`
  before handing it to the importer (matches our planned strategy).
- CEF's accelerated path renders directly to an sRGB texture. Import the
  Vulkan image with `VK_FORMAT_B8G8R8A8_SRGB` (or `R8G8B8A8_SRGB` for
  `RGBA_8888`), not the `_UNORM` variant — otherwise colours are wrong.
- **NVIDIA proprietary driver: not viable** with DMABUF + Vulkan today.
  Mesa/Intel is the validated path; AMDGPU should work but is untested upstream.
- **GTK3 vs GTK4 conflict:** CEF Linux pulls in GTK3 for its native UI bits.
  Apps that use GTK4 for windowing (e.g. for Vulkan) will hit a runtime clash.
  GLFW / winit / SDL on X11 sidestep this; we target winit, matching the
  Windows/macOS demos.
- cefclient still has no Linux example for `OnAcceleratedPaint` (cef#3687
  remains open). The public C/C++ API is the contract; no reference client.

**Implementation status (2026-05-17):**

- [x] CEF C API names + types verified on `cef = "148"` (`AcceleratedPaintInfo.planes`, `.plane_count`, `.modifier`)
- [x] CEF 148 API version pinning via `cef::api_hash(CEF_API_VERSION_LAST, 0)` in `runtime.rs` (required on all platforms; previously missing)
- [x] `linux_cef::cef_backed::WeldRenderHandler::on_accelerated_paint`:
      iterates `planes[..plane_count]`, `libc::dup(fd)` per plane, maps
      `ColorType` → `wgpu::TextureFormat::{Bgra8UnormSrgb,Rgba8UnormSrgb}`,
      packages into `DmaBufImage`, stores in `PendingFrameSlot`
- [x] `WgpuTextureImporter::import_vulkan` (ported from `wgpu-graft`):
      `vkCreateImage` w/ `DRM_FORMAT_MODIFIER_EXT` tiling, `vkAllocateMemory`
      w/ `ImportMemoryFdInfoKHR` + `MemoryDedicatedAllocateInfo`,
      `texture_from_raw` w/ `wgpu_hal::vulkan::TextureMemory::External`
- [x] `DmaBufImage::Drop` closes unconsumed fds; `forget_fds` for the
      Vulkan-takes-ownership success path
- [x] Single-plane Phase-4 constraint (BGRA8/RGBA8); multi-plane returns
      a typed error
- [x] `demo-weld-linux` (mirrors `demo-weld-win`, forces Vulkan backend)
- [x] Smoke test on Fedora 44 + Intel iGPU: example.com rendered, mouse input round-trips through to navigation
- [ ] Wayland-native (currently runs through XWayland), NVIDIA proprietary, multi-plane formats (deferred)

Additional fix discovered during Phase 4: `external_begin_frame_enabled` was
set to 1 in all three producers but no caller invokes `SendExternalBeginFrame`,
so CEF was waiting forever for a host-driven vsync and emitting zero paints.
Flipped to 0 across Linux, Windows, and macOS so CEF self-drives at
`windowless_frame_rate`.

---

## Comparison with siblings

| | `wgpu-graft` | `wgpu-scry` | `wgpu-weld` |
|--|--|--|--|
| Engine | Servo | WebView2 / WKWebView / WPE | CEF (Chromium Embedded Framework) |
| Distribution | App-bundled Servo dependency | OS-provided webview | App-bundled Chromium |
| Subprocess tax | None | None | Must call `execute_process_from` first in `main()` |
| Frame source | Servo/surfman GL FBO | WGC / ScreenCaptureKit / WPE DMABUF | `CefAcceleratedPaintInfo` |
| Handle lifetime | Producer-owned GL/native resource | Capture/session-owned native frame | **Callback-scoped** — must dup/retain |
| Linux support | GL FBO → Vulkan external memory | WPE scaffold / DMABUF planned | DMABUF + Vulkan import verified (Intel/Mesa + X11/XWayland) |
| CPU fallback | Servo readback demos | snapshots / overlay fallback | `cpu-paint-fallback` feature |
