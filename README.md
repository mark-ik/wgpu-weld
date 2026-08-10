# wgpu-weld

Weld Chromium Embedded Framework (CEF) rendered output into wgpu-importable GPU
textures via CEF's accelerated off-screen rendering (OSR).

`wgpu-weld` bundles Chromium (through CEF) and routes CEF's `OnAcceleratedPaint`
output into host-owned `wgpu` textures. CEF hands out callback-scoped native
handles, so the core rule is: copy or retain inside the paint callback, then
expose only an owned resource to the host side. The trade-off is binary size and
a more complex process model (see **CEF Foibles** below) in exchange for a single
cross-platform producer, uniform browser behaviour, and direct access to the CEF
DevTools protocol.

This repo is a sibling to
[`wgpu-graft`](https://github.com/merely-made/wgpu-graft) (Servo testbed, GL-FBO /
Vulkan / Metal / D3D interop) and
[`wgpu-scry`](https://github.com/merely-made/wgpu-scry) (system-webview frame
adapter, WebView2 / WKWebView / WebKitGTK). `wgpu-weld` covers the CEF lane:
rather than using the OS's built-in webview, the embedder ships its own Chromium.

**Made with AI**

## Status

Prototype, version `0.1.0`. Per `design_docs/2026-05-14_cef_accelerated_osr_plan.md`:

- **Windows:** Phase 1 + 2 complete. CEF vtable wiring, live browser creation,
  input/navigation, and the callback-time D3D11 copy into a weld-owned shared
  texture imported through D3D12 into wgpu are implemented and exercised by
  `demo-weld-win`.
- **Linux:** Phase 4 verified end-to-end on Fedora 44 + Intel/Mesa (X11 /
  XWayland). DMABUF planes are imported through Vulkan external memory.
  Single-plane formats (BGRA8 / RGBA8) only; multi-plane returns an error.
- **macOS:** Phase 3 complete as of 2026-08-10, verified on macOS 15.7.7 /
  `x86_64-apple-darwin`. `demo-weld-mac` renders example.com into a wgpu Metal
  texture through CEF's `IOSurfaceRef`, and proves it by reading the pixels back
  rather than by putting a window on a screen. Input forwarding is thinner than
  the other two platforms; see that demo's README for why.

Without the `cef-runtime` feature the crate still compiles, but all producer
constructors return a pending-wiring error.

## Workspace

| Crate | Purpose |
| --- | --- |
| [`welding`](welding/) | The library. `CefRuntime` initialization / subprocess detection, the native-frame mailbox, per-platform texture import, and the `CefSurfaceProducer` trait. This is the workspace `default-members` crate. |
| [`demo-weld-win`](demo-weld-win/) | Windows accelerated-OSR demo. Renders a live CEF surface through the host's wgpu pipeline and forwards mouse/keyboard input. Not published (`publish = false`). |
| [`demo-weld-linux`](demo-weld-linux/) | Linux accelerated-OSR demo (DMABUF to Vulkan). Not published. |
| [`demo-weld-mac`](demo-weld-mac/) | macOS accelerated-OSR demo (IOSurface to Metal). Ships a CEF helper binary and an `.app` bundler, because macOS needs both. Not published. |

See [`welding/README.md`](welding/README.md) for the producer/consumer contract
and the platform texture paths.

## CEF Foibles

Three constraints govern every use of this crate.

### 1. The Subprocess Tax

CEF spawns its renderer, GPU, and utility processes by re-executing the host
binary. The embedder must call `CefRuntime::execute_process_from` at the absolute
start of `main()`, before any other initialization (winit, wgpu, thread pools),
and exit immediately if it returns `Some(exit_code)`:

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

Forgetting this leaves subprocess slots unfilled. The GPU process never starts,
and you get a blank OSR surface with no `OnAcceleratedPaint` callbacks.

### 2. Handle Lifetime

CEF's `OnAcceleratedPaint` handle is callback-scoped. Never store CEF's raw
callback handle or its pooled resource across the callback boundary. `welding`
duplicates or retains the resource inside the callback and exposes only an owned
copy to the host.

### 3. Distribution Path

CEF is not a system library. `libcef.dll` / `libcef.so` / the
`Chromium Embedded Framework.framework` must be distributed alongside your
application and located at a known path, passed via `CefRuntimeConfig`. Under the
`cef-runtime` feature the `cef` / `cef-dll-sys` crate family downloads and links
libcef at build time from the configured CEF binary distribution.

## Popup widgets

CEF paints `<select>` dropdowns, autocomplete lists, and date pickers as a
**separate** OSR element rather than compositing them into the view. A host
that only draws `acquire_frame` shows a page whose dropdowns silently do
nothing, so `welding` surfaces them through `acquire_popup` (a new
`PopupSurface`) and `popup_rect` (still open, and where). Draw it over the
view; all three demos do, with a viewport-clipped second pass.

One platform caveat, verified rather than assumed: **macOS never delivers
these.** Chromium uses a native menu for `<select>` there, and windowless
rendering does not reroute it, so `OnPopupShow` and `OnPopupSize` simply never
fire. A macOS host that needs dropdowns has to draw its own control from the
DOM. Windows is verified working; Linux is implemented but not yet run.

Popup *browsers* (`window.open`, `target="_blank"`) are a different mechanism
entirely: `welding` renders one surface per producer, so those are denied and
reported as `NavigationEvent::NewWindowRequested` for the host to decide on.

## Platform texture paths

| Platform | CEF output | Import path |
| --- | --- | --- |
| Windows | `HANDLE` (pooled shared D3D texture) | callback-time D3D11 `CopyResource` to owned shared texture, then D3D12 `OpenSharedHandle` to a wgpu D3D12 texture (that second step is [`grafting`](https://crates.io/crates/grafting)) |
| macOS | `IOSurfaceRef` | IOSurface retain, MTLTexture, wgpu Metal (verified on macOS 15.7.7 + Intel) |
| Linux | DMABUF planes | DMABUF dup, `VK_EXT_external_memory_dma_buf`, wgpu Vulkan (verified on Fedora 44 + Intel/Mesa) |

## Build and run

### Prerequisites

- Rust toolchain with edition 2024 support, resolver 3.
- A CEF binary distribution matching the pinned `cef = "148"` crate. Set
  `CEF_PATH` to point at it (the directory containing `libcef.dll` on Windows,
  for example `C:\path\to\cef_binary_148.x_windows64`).

`cargo check` does not require `CEF_PATH`; runtime initialization does.

### Library

```sh
# Compiles without a CEF distribution; producers return a pending-wiring error.
cargo check -p welding

# Real CEF integration:
cargo check -p welding --features cef-runtime
```

The Metal and DMABUF arms are `cfg`-gated, so a host build never touches them.
Check them explicitly before changing anything under `native_frame`:

```sh
rustup target add x86_64-unknown-linux-gnu aarch64-apple-darwin
cargo check -p welding --target x86_64-unknown-linux-gnu
cargo check -p welding --target aarch64-apple-darwin
```

Adding `--features cef-runtime` works for the Linux target too. It does not work
for macOS from another host: `cef-dll-sys` builds the CEF wrapper with CMake and
Ninja, which needs a real Mac. Check that half on the Mac itself, where the only
prerequisites beyond Rust are `brew install cmake ninja`.

### Windows demo

```sh
set CEF_PATH=C:\path\to\cef_binary_148.x_windows64
cargo run -p demo-weld-win
```

The Windows demo reads `WELD_URL` for the initial page and falls back to
`https://example.com`.

### Linux demo

```sh
export CEF_PATH=/path/to/cef_binary_148.x_linux64
cargo run -p demo-weld-linux
```

The Linux demo currently loads `https://example.com` as its initial URL.

### macOS demo

CEF will not run from a bare executable on macOS, so build the `.app`:

```sh
cd demo-weld-mac
cargo run --bin bundle-demo-weld-mac
open ../target/bundle/demo-weld-mac.app
```

See [`demo-weld-mac/README.md`](demo-weld-mac/README.md) for the helper-binary
and event-loop constraints behind that, and for the unattended validation mode
(`WELD_EXIT_AFTER_FRAMES=1`) that reads the imported pixels back and prints a
verdict, which is how this path is checked over SSH.

## Features (`welding`)

- `cef-runtime` (off by default): enables the `cef` dependency and the working
  `CefRuntime` and platform producer implementations.
- `cpu-paint-fallback` (off by default): enables the slower `OnPaint`
  CPU-bitmap path, with a texture upload per frame, as a fallback when
  accelerated OSR is unavailable.

Accelerated OSR (`OnAcceleratedPaint`) requires CEF built with GPU support and
`windowless_rendering_enabled = true`. Verify at runtime with
`CefSurfaceCapabilities::probe`.

## Key dependencies

The graphics and windowing pins are centralized in the root `Cargo.toml`
`[workspace.dependencies]` and match the wider Mere ecosystem:

- `wgpu = "29"` (the `welding` crate enables the `metal` feature) plus
  `wgpu-hal = "29.0"`
- `winit = "0.30.13"`, `raw-window-handle = "0.6.2"`, `dpi = "0.1.2"`

CEF and the platform crates are declared on the `welding` crate itself, not in
the workspace table:

- `cef = "148"` with the `accelerated_osr` feature (only under `cef-runtime`)
- [`grafting`](https://crates.io/crates/grafting), the shared native-texture
  interop core from `wgpu-graft`, **on Windows only**. `welding` delegates the
  generic `OpenSharedHandle` to wgpu step to it rather than keeping a second
  copy; the CEF-specific callback copy and cache flush stay here. Pulled with
  `default-features = false` (no GL producer path) and the `wgpu-29` feature, so
  the imported texture shares the host's device. The Metal and DMABUF arms go
  through `wgpu-hal` directly and do not need it.
- Platform crates: `windows = "0.62"` (Win32 D3D11/D3D12/DXGI) on Windows;
  `objc2 = "0.6.3"` with `objc2-foundation` / `objc2-io-surface` /
  `objc2-metal` `= "0.3.2"` on macOS; `ash = "0.38.0"` and `libc = "0.2"` on
  Linux

## Producer / consumer contract

- **`welding` owns:** CEF initialization and subprocess detection,
  `OnAcceleratedPaint` callback wiring, the `PendingFrameSlot` latest-frame
  mailbox, GPU texture import, and the `CefSurfaceProducer` trait. The Windows
  D3D12 open-shared step is the one piece it delegates, to `grafting`.
- **The host owns:** the event loop, window / HWND creation, and passing the
  resulting `ImportedTexture` to its render pipeline. Windows uses CEF's
  dedicated message-loop thread; other platforms call
  `CefRuntime::do_message_loop_work()` on host-loop ticks.

## Relationship to wgpu-scry and wgpu-graft

The three repos share an import pattern (native GPU surface handles produced by
an embedded browser, imported into a host wgpu pipeline) but serve different
engines. `wgpu-graft` is the origin, derived from Slint's Servo embedding
example; `wgpu-scry` was extracted from `wgpu-graft` and keeps that Slint-derived
`native_frame` structure; `wgpu-weld` was written against the same import pattern
rather than copied from it, and on Windows it now links `grafting` for the
open-shared step instead of carrying its own. They serve different niches:

| Repo | Engine | Distribution | Producer backends |
| --- | --- | --- | --- |
| `wgpu-graft` | Servo | bundled | 1 (Servo via surfman / GL-FBO) |
| `wgpu-scry` | OS webview | OS-provided | 5 (WebView2, WKWebView, WebKitGTK 4.1, WebKitGTK 6.0, WPE) |
| `wgpu-weld` | Chromium (CEF) | bundled | 1 cross-platform |

`wgpu-weld` ships the engine; `wgpu-scry` uses the OS's. CEF's single
cross-platform producer means one implementation family to maintain, and
`OnAcceleratedPaint` gives a more direct texture source than WGC capture. The
hard part moves to CEF's C ABI / vtable ownership and callback-scoped handle
lifetime. The Windows import path follows the same shape as `wgpu-scry`'s
`native_frame` module; the source differs in that CEF gives a callback-scoped
shared handle while scrying receives a capture-owned shared texture after WGC has
copied the composited visual.

## License

MPL-2.0; see the [LICENSE](LICENSE) file at the repo root (also declared in
`Cargo.toml`).
