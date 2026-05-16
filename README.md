# wgpu-weld

Weld Chromium Embedded Framework rendered output into wgpu-importable GPU
textures via CEF's accelerated off-screen rendering (OSR).

This repo was created as a sibling to [`wgpu-graft`](https://github.com/mark-ik/wgpu-graft)
(Servo testbed, GL-FBO / Vulkan / Metal / D3D interop) and
[`wgpu-scry`](https://github.com/mark-ik/wgpu-scry) (system-webview frame
adapter — WebView2 / WKWebView / WebKitGTK).

`wgpu-weld` covers the CEF side: rather than using the OS's built-in webview,
the embedder bundles Chromium and routes CEF's `OnAcceleratedPaint` output into
host-owned `wgpu` textures. CEF hands out callback-scoped native handles, so the
core rule is "duplicate or retain inside the paint callback, then import into
wgpu from the host side." The trade-off is binary size and a more complex
process model (see **CEF Foibles** in the `welding` crate docs) in exchange for a
single cross-platform producer, uniform browser behaviour, and direct access to
the CEF DevTools protocol.

## Workspace

| Crate | Purpose |
| --- | --- |
| [`welding`](welding/) | The library. `CefRuntime` initialization/subprocess detection, native-frame mailbox, Windows D3D shared-handle → `wgpu` D3D12 import, and the `CefSurfaceProducer` trait. CEF vtable/browser creation is still pending. |
| [`demo-weld-win`](demo-weld-win/) | Windows runtime probe. Currently validates the subprocess/runtime entry shape; live CEF frame rendering waits on the client/render-handler wiring. |

See [`welding/README.md`](welding/README.md) for the producer/consumer contract and
the platform texture paths.

## Quick start

```bash
# cargo check does not require CEF_PATH; runtime initialization does.
set CEF_PATH=C:\path\to\cef_binary_147.x_windows64

cargo check -p welding
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
cross-platform producer means one implementation family to maintain, and
`OnAcceleratedPaint` gives a more direct texture source than WGC capture. The
hard part moves to CEF's C ABI/vtable ownership and callback-scoped handle
lifetime.

## License

[MPL-2.0](LICENSE)
