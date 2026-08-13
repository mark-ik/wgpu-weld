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

Version 0.5.0. Every "verified" below was checked by running it on that
platform's hardware, in one battery per machine: Windows 11 (this laptop),
macOS 15.7 on an Intel iMac, macOS 26.5 on an Apple Silicon M4 iMac (the
first arm64 run, at a native 2x scale factor), and Fedora on a ThinkPad
(AMD Renoir/RADV).

| | Windows | macOS | Linux |
| --- | --- | --- | --- |
| Accelerated frame import | DX12 shared texture | IOSurface to Metal | DMABUF to Vulkan [^linux] |
| Page renders end to end | verified | verified | verified [^linux] |
| Mouse, wheel, keyboard | verified | verified | verified |
| Cursor shape | verified | verified | verified |
| HiDPI scale factor | verified | verified | verified |
| Navigation and title events | verified | verified | verified |
| Console messages | verified | verified | verified |
| Cookies (read, write, delete) | verified | verified | verified |
| Script results (JS value back) | verified | verified | verified |
| Chromium command-line switches | verified | verified | verified |
| Popup widgets (`<select>`) | verified | **differs by macOS** [^macpopup] | opens, import blocked [^linux] |
| Renderer crash recovery | verified | verified | event not delivered [^linuxcrash] |
| IME composition | wired | wired | wired |
| Visibility (`set_visible`) | wired | wired | wired |
| DevTools window | wired | wired | wired |

"verified" means observed working on that platform's hardware. "wired" means
implemented and compiling there, but not yet exercised on any machine — the
last three rows are untested everywhere, not gaps in one platform.

Not implemented yet, and `CefSurfaceCapabilities::probe` will tell you so at
runtime rather than failing quietly: downloads, auth challenges, permission
requests, context menus, find-in-page, PDF and print, drag and drop, touch,
pointer/pen, zoom and user-agent settings, per-producer profile isolation, and
the DevTools protocol.

[^linux]: Linux needs the DMABUF buffer to carry an **explicit** DRM format
modifier. Intel/Mesa supplies one, and the frame import is verified there;
AMD/RADV hands over `DRM_FORMAT_MOD_INVALID`, and importing that needs
`VK_EXT_image_drm_format_modifier` enabled on the wgpu device, which wgpu does
not do. `welding` refuses such a buffer with a typed error naming the situation
rather than producing a broken texture. This is why the popup row reads
"opens": on the AMD test machine CEF offers the dropdown and reports its
geometry (`on_popup_show`, then `on_popup_size 320x197 at 0,80`), and only the
texture import is refused, by the same modifier limitation.

[^linuxcrash]: A renderer crash on Linux does not reach the host. Chromium logs
`Intentionally crashing` and then `Failed to send GetTerminationStatus request
to zygote`, and `OnRenderProcessTerminated` never fires, so `welding` has
nothing to report. The handler is registered identically on all three
platforms and fires on the other two, so this is Chromium's behaviour on Linux
rather than missing wiring; `--no-zygote` does not change it. Recovery itself
is untestable there until the event arrives.

[^macpopup]: Split by macOS generation, both sides verified the same day with
the identical scripted click. On macOS 15.7 (Intel) Chromium uses a native
menu for `<select>` and `OnPopupShow` never fires; on macOS 26.5 (Apple
Silicon M4) the same click delivers the popup through OSR and the surface
imports with real pixels. Treat dropdowns as version-dependent, and keep the
DOM fallback for hosts that must run on older macOS.

## Checking it yourself

The three demos take the same environment knobs, so one battery runs on all
three platforms and the results are comparable. Input is scripted because a
machine nobody is sitting at cannot click:

```sh
export WELD_URL=file:///path/to/testing/weld_input_probe.html
export WELD_SCALE=2                # force HiDPI on a 1x display
export WELD_CLICK_AT=100,100       # physical pixels
export WELD_WHEEL=-360             # scroll after the click
export WELD_KEY=k                  # then type one character
export WELD_SCRIPT='({dpr: window.devicePixelRatio})'
export WELD_COOKIE_URL=https://example.com/
export WELD_SWITCHES=disable-popup-blocking,lang=en-GB
export WELD_BACKGROUND=transparent # or rrggbb; unset = opaque white
```

`testing/weld_input_probe.html` reports what it received through
`document.title`, which comes back as a `TitleChanged` navigation event, so
every gesture becomes a checkable claim. `testing/weld_select_probe.html` does
the same for the popup path.

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

When the renderer dies, the browser survives and the host gets told what
happened. Recovering takes two steps, and the second one is easy to miss:

```rust,ignore
if let NavigationEvent::ContentProcessTerminated { status, .. } = event {
    // Not worth retrying an OutOfMemory: the retry reaches it again.
    if status != ProcessTerminationStatus::OutOfMemory {
        // Somewhere known-good. Reloading re-runs the page that just killed
        // the renderer, which kills its replacement too.
        producer.navigate_to_url(&home_url)?;
        // Painting is change-driven and the fresh renderer has nothing queued
        // for this surface, so without this the host presents its pre-crash
        // frame forever.
        producer.request_repaint()?;
    }
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
