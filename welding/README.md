# welding

Embed Chromium in a `wgpu` application. `welding` drives the Chromium Embedded
Framework in accelerated off-screen rendering mode and hands each painted frame
to the host as a GPU texture on the host's own device, with no CPU round trip.

Use it when you want a full Chromium in your renderer, and you are willing to
ship Chromium to get it. If you would rather use the webview the OS already
has, its sibling [`scrying`](https://crates.io/crates/scrying) covers that lane;
[`grafting`](https://crates.io/crates/grafting) is the texture-interop core both
of them import through.

**Made with AI**

## State, 2026-08-12

Version 0.4.0. Every claim in the table below was checked by running it, and
the platform column says where.

| | Windows | macOS | Linux |
| --- | --- | --- | --- |
| Accelerated frame import | DX12 shared texture | IOSurface to Metal | DMABUF to Vulkan [^linux] |
| Page renders end to end | verified | verified | verified [^linux] |
| Mouse, wheel, keyboard | verified | wired | verified |
| HiDPI scale factor | verified | wired | wired |
| Popup widgets (`<select>`, autocomplete) | verified | **not possible** [^macpopup] | wired |
| Cursor shape | wired | wired | verified |
| Navigation events | verified | wired | verified |
| Console messages | verified | verified | wired |
| Cookies (read, write, delete) | verified | wired | wired |
| Script results (JS value back) | verified | wired | wired |
| Chromium command-line switches | verified | wired | wired |
| IME composition | wired | wired | wired |
| Visibility (`set_visible`) | wired | wired | wired |
| DevTools window | wired | wired | wired |

"verified" means observed working on that platform's hardware. "wired" means
implemented and compiling there, but not yet exercised on that machine.

Not implemented yet, and `CefSurfaceCapabilities::probe` will tell you so at
runtime rather than failing quietly: downloads, auth challenges, permission
requests, context menus, find-in-page, PDF and print, drag and drop, touch,
pointer/pen, zoom and user-agent settings, per-producer profile isolation, and
the DevTools protocol.

[^linux]: Linux needs the DMABUF buffer to carry an **explicit** DRM format
modifier. Intel/Mesa supplies one; AMD/RADV hands over `DRM_FORMAT_MOD_INVALID`,
and importing that needs `VK_EXT_image_drm_format_modifier` enabled on the wgpu
device, which wgpu does not do. `welding` refuses such a buffer with a typed
error naming the situation rather than producing a broken texture.

[^macpopup]: Chromium uses a native menu for `<select>` on macOS and windowless
rendering does not reroute it, so `OnPopupShow` never fires. A macOS host that
needs dropdowns has to draw its own control from the DOM. This is a platform
fact, not a gap in `welding`.

## Using it

Frames, and the popup widget surface that CEF paints separately:

```rust,ignore
// Every tick, after CefRuntime::do_message_loop_work().
if let Some(frame) = producer.acquire_frame(&host_ctx)? {
    // frame.texture / frame.view live on your wgpu device.
    self.frame = Some(frame);
}

// A <select> dropdown is its own surface. Draw it over the view at rect,
// and drop it when popup_rect() goes back to None.
if let Some(popup) = producer.acquire_popup(&host_ctx)? {
    self.popup = Some(popup);
}
if producer.popup_rect().is_none() {
    self.popup = None;
}
```

Input is in **physical** pixels, the units a window system gives you.
`scale_factor` tells CEF how many of those make one CSS pixel:

```rust,ignore
let config = CefSurfaceConfig {
    initial_url: "https://example.com".into(),
    initial_size: window.inner_size(),   // physical
    scale_factor: window.scale_factor() as f32,
    ..Default::default()
};
// and when the window moves to another display:
producer.set_scale_factor(new_scale)?;
```

Cookies and script results are request-then-poll, because CEF answers both
asynchronously and on Linux and macOS the calling thread *is* CEF's UI thread,
so blocking would wait on the loop carrying the answer:

```rust,ignore
producer.set_cookie("https://example.com/", &cookie)?;
producer.request_cookies(Some("https://example.com/"))?;

let id = producer.request_script_result("({title: document.title, n: 2+2})")?;

// later, on ordinary ticks:
if let Some(cookies) = producer.poll_cookies() { /* Some(vec![]) means none */ }
if let Some(result) = producer.poll_script_result() {
    // result.value is Ok(json) or Err(exception message)
    // => {"title":"Example Domain","n":4}
}
```

Reaching Chromium behaviour that has no CEF API:

```rust,ignore
let mut config = CefRuntimeConfig::new(&cef_path);
config.command_line_switches = vec![("disable-popup-blocking".into(), None)];
```

Ask before you assume, and report the answer to your own users:

```rust,ignore
let caps = producer.capabilities();
// Every Supported here is pinned to a handler that exists; anything missing
// carries a reason string explaining why.
```

## CEF foibles

Three constraints govern every use of this crate.

### 1. The subprocess tax

CEF runs its renderer, GPU and utility work in separate processes. On Windows
and Linux it starts them by re-executing your binary, so
`CefRuntime::execute_process_from` must be the first thing in `main`, before
winit, wgpu, or any thread pool:

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

macOS is different: it launches separate helper executables from inside the
`.app` bundle instead. Those helpers must call
`CefRuntime::run_subprocess`, not `cef_execute_process` directly, or the
renderer comes up with no handlers and anything needing it (script results)
never answers. See `demo-weld-mac` for a working bundle, helper, and bundler.

### 2. Handle lifetime

The resource `OnAcceleratedPaint` hands over is callback-scoped. `welding`
copies or retains it inside the callback and only ever exposes an owned
resource: a D3D11 copy into a weld-owned shared texture on Windows, a
`CFRetain`ed `IOSurface` on macOS, `dup(2)`ed plane fds on Linux. Never hold
CEF's own handle past the callback.

### 3. Distribution

CEF is not a system library. `libcef.dll` / `libcef.so` / `Chromium Embedded
Framework.framework` ships with your application, and its path goes to
`CefRuntimeConfig`. Under the `cef-runtime` feature the `cef` / `cef-dll-sys`
crates download and link it at build time.

## Features

- `cef-runtime` (off by default) enables the real CEF integration. Without it
  the crate still compiles and every producer constructor returns a
  pending-wiring error, which keeps `cargo check` cheap for downstream crates.
- `cpu-paint-fallback` (off by default) enables the slower `OnPaint` CPU-bitmap
  path, for when accelerated OSR is unavailable.

## License

MPL-2.0.
