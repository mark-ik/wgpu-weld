# wgpu-weld

Weld Chromium Embedded Framework rendered output into wgpu-importable GPU
textures via CEF's accelerated off-screen rendering (OSR).

This repo was created as a sibling to [`wgpu-graft`](https://github.com/mark-ik/wgpu-graft)
(Servo testbed, GL-FBO / Vulkan / Metal / D3D interop) and
[`wgpu-scry`](https://github.com/mark-ik/wgpu-scry) (system-webview frame
adapter — WebView2 / WKWebView / WebKitGTK).

`wgpu-weld` covers the CEF side: rather than using the OS's built-in webview,
the embedder bundles Chromium and routes CEF's `OnAcceleratedPaint` callback
directly into wgpu with no copy. The trade-off is binary size and a more
complex process model (see **CEF Foibles** in the `weld` crate docs) in exchange
for a single cross-platform producer, uniform browser behaviour, and direct
access to the CEF DevTools protocol.

## Workspace

| Crate | Purpose |
| --- | --- |
| [`weld`](weld/) | The library. `CefRuntime` initialization and subprocess detection, per-platform `OnAcceleratedPaint` → wgpu texture import, `CefSurfaceProducer` trait. |
| [`demo-weld-win`](demo-weld-win/) | Windows runtime probe. Exercises the CEF accelerated OSR path into a wgpu D3D12 texture under winit. |

See [`weld/README.md`](weld/README.md) for the producer/consumer contract and
the platform texture paths.

## Quick start

```bash
# Set CEF_PATH to your CEF binary distribution before building
set CEF_PATH=C:\path\to\cef_binary_133.x_windows64

cargo check -p weld
cargo run -p demo-weld-win
```

## Relationship to wgpu-scry and wgpu-graft

All three repos are structurally derived from the same pattern — native GPU
surface handles produced by an embedded browser, imported into a host wgpu
pipeline — but serve different niches:

| Repo | Engine | Distribution | Producers |
| --- | --- | --- | --- |
| `wgpu-graft` | Servo | bundled | 1 (Servo via surfman / GL-FBO) |
| `wgpu-scry` | OS webview | OS-provided | 3 (WebView2, WKWebView, WPE) |
| `wgpu-weld` | Chromium (CEF) | bundled | 1 cross-platform |

`wgpu-weld` ships the engine; `wgpu-scry` uses the OS's. CEF's single
cross-platform producer means one implementation to maintain, and
`OnAcceleratedPaint` gives a more direct texture path than WGC capture.

## License

[MPL-2.0](LICENSE)
