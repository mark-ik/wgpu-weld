# demo-weld-mac

macOS demo: a live CEF surface imported into a wgpu Metal texture through
`welding`, by way of `OnAcceleratedPaint`'s `IOSurfaceRef`.

This is the demo that closes Phase 3 of the accelerated-OSR plan. It is shaped
differently from `demo-weld-win` and `demo-weld-linux` for two reasons that are
specific to macOS.

## 1. CEF needs a real `.app`, and a separate helper binary

On Windows and Linux, CEF starts its renderer / GPU / utility processes by
re-executing the host binary, so those demos guard the top of `main` with
`execute_process_from`. macOS does not work that way. CEF launches the helper
executables inside `Contents/Frameworks/<app> Helper*.app`, so this crate ships
two binaries:

| Binary | Role |
| --- | --- |
| `demo-weld-mac` | The app. Never calls `execute_process`. |
| `demo-weld-mac-helper` | Answers for all five Helper bundles. Loads the framework relative to its own bundle, then hands off to CEF. |

`[package.metadata.cef.bundle]` names the helper so the bundler can find it.
A third binary, `bundle-demo-weld-mac`, assembles the `.app`: it builds both
targets, writes the `Info.plist`s, copies the framework in, and stamps the
helper into each Helper bundle.

```sh
cd demo-weld-mac
cargo run --bin bundle-demo-weld-mac
open ../target/bundle/demo-weld-mac.app
```

Running the bare executable will not work and says so. It needs the framework
at `../Frameworks` relative to itself, which only exists inside the bundle.

## 2. CEF and winit cannot both own the event loop

`CefDoMessageLoopWork` drains the `NSApplication` event queue itself. Call it
from inside a winit callback and winit's re-entrancy guard aborts the process:

```text
tried to handle event while another event is currently being handled
```

So this demo does not use `run_app`. It drives the loop itself with
`pump_app_events(Some(Duration::ZERO))` and does CEF's pump plus the render
*between* winit dispatches, in `DemoApp::tick`. Windows avoids the problem with
CEF's own UI thread, and Linux never had it.

One consequence worth knowing: CEF consumes some `NSEvent`s that winit would
otherwise see, so input forwarding is less complete here than on the other two
platforms. The demo's purpose is proving the texture path, not input parity.

## Unattended validation

There is no point opening a window on a machine nobody is watching, and "it did
not crash" is weak evidence. `WELD_PIXEL_FIXTURE=1` loads an embedded animated
dodger-blue page. With `WELD_EXIT_AFTER_FRAMES`, the demo reads a centered
64×64 sample back, requires every pixel to match within tolerance, and exits
unsuccessfully on a mismatch:

```sh
WELD_PIXEL_FIXTURE=1 WELD_EXIT_AFTER_FRAMES=2 RUST_LOG=info \
  ./demo-weld-mac.app/Contents/MacOS/demo-weld-mac
```

```text
PIXEL FIXTURE PASS: 4096/4096 center pixels matched [255, 144, 30, 255] ±8
```

`[255, 144, 30, 255]` is BGRA for dodger blue. This is an exact page-to-CEF-to-
IOSurface-to-wgpu content receipt rather than an arrival check.

Keep the frame count small. Accelerated OSR only paints on change, so a static
page delivers one frame and then goes quiet; asking for 30 frames of
example.com waits until `WELD_TIMEOUT_SECS` (default 60) gives up.

| Variable | Meaning |
| --- | --- |
| `WELD_URL` | Initial page. Defaults to `https://example.com`. |
| `WELD_PIXEL_FIXTURE` | Use the embedded deterministic page and make the pixel verdict affect process status. |
| `WELD_EXIT_AFTER_FRAMES` | Probe and exit after N imported frames. |
| `WELD_TIMEOUT_SECS` | Give up and report anyway. Default 60. |
| `WELD_SNAPSHOT` | Write an asynchronous Chromium screenshot to this PNG path. |
| `WELD_SNAPSHOT_AFTER_SCRIPTED` | With scripted input, wait until every enabled gesture was sent before requesting the PNG. |
| `WELD_RECEIPT` | Append navigation, script, PDF, and snapshot results here when a GUI launch has no stdout. |
| `WELD_HISTORY` | After two real seconds, record `can_go_back` and `can_go_forward` in `WELD_RECEIPT`. |

## Verified on

macOS 15.7.7, Intel iMac (`x86_64-apple-darwin`), 2026-08-10.

macOS 26.5.1, Apple M4 iMac (`aarch64-apple-darwin`, Metal 4, native 2x
display), 2026-08-12 — full battery, popup surfaces included.

macOS 26.5.1, Apple M4 iMac (`aarch64-apple-darwin`), 2026-08-16, on the
**wgpu-30 default row** at `02fb1cc`. `VALIDATION PASS`, 2 frames, probe
16384/16384 non-zero.

macOS 26.5.1/Apple M4 and macOS 15.7.7/Intel iMac, 2026-08-30, from fresh
isolated builds of the published `welding-v0.13.0` tag. Both CEF 151.8 / wgpu
30.0.1 runs loaded Example Domain with HTTP 200, imported a 1280x800
`Bgra8Unorm` IOSurface frame through Metal, probed 16384/16384 bytes non-zero,
printed `VALIDATION PASS`, and exited 0.

Two things to know when re-running this unattended. `bundle-demo-weld-mac`
replaces files in place, so the `.app` directory keeps the mtime of its first
bundling while the executables inside are current; read the mtime on
`Contents/MacOS/demo-weld-mac` to tell a fresh build from a stale one. And
`WELD_SNAPSHOT` needs `WELD_EXIT_AFTER_FRAMES` set high enough to outlive the
asynchronous PNG callback. At 1 or 2 frames the process exits first and writes
no file, even though the probe passed.
