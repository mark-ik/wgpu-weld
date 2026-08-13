# wgpu-weld

Weld Chromium Embedded Framework (CEF) rendered output into wgpu-importable
GPU textures via CEF's accelerated off-screen rendering. The
[`welding`](welding/) library bundles Chromium through CEF and routes its
paint output into host-owned wgpu textures, so an app can render live web
content inside its own pipeline. It is the CEF sibling of
[`wgpu-scry`](https://github.com/merely-made/wgpu-scry) (system webviews)
and [`wgpu-graft`](https://github.com/merely-made/wgpu-graft) (Servo, plus
the shared `grafting` interop core).

## Status (2026-08-12)

Prototype. `welding` 0.5.0 in this repo, 0.4.1 published on crates.io
(MPL-2.0). Per-platform detail, and
the difference between "verified on that hardware" and "implemented but not
yet run there", is the table in [`welding/README.md`](welding/README.md).

A parity battery was run on all three platforms on 2026-08-12 (Windows 11,
macOS 15.7 on an Intel iMac, macOS 26.5 on an Apple Silicon M4 iMac, Fedora
on an AMD ThinkPad). Input, cursor, HiDPI, navigation, console, cookies,
script results and command-line switches are now verified on every one. What
is still untested everywhere is IME composition, `set_visible`, and the
DevTools window.

- All three platform import lanes are hardware-verified: Windows (D3D11
  copy, D3D12 shared handle via the `grafting` crate, into wgpu), Linux
  (DMABUF via Vulkan external memory, Fedora 44 + Intel/Mesa), macOS
  (IOSurface via Metal, closed 2026-08-10). The Linux import needs an
  explicit DRM format modifier: Intel/Mesa supplies one, AMD/RADV does not,
  and that case is refused with a typed error pending wgpu enabling
  `VK_EXT_image_drm_format_modifier`.
- The capability probe reports honestly as of the 2026-08-10 truth pass: a
  unit test pins every "Supported" claim to a real handler.
- Popup widget surfaces (`<select>` dropdowns and similar) render via a
  separate `acquire_popup` surface: verified on Windows and on macOS 26
  (Apple Silicon). On macOS 15.7 (Intel) Chromium used a native menu and no
  popup was ever delivered, so the behaviour differs by macOS generation. On
  Linux the dropdown opens and reports its geometry, and only the texture
  import is refused, by the AMD/RADV modifier limitation above.
- HiDPI is honoured (`scale_factor` plus a live `set_scale_factor`): sizes and
  coordinates stay physical, and CEF is told how many make one CSS pixel.
  Verified on all three by forcing a 2x scale on a 1x panel and having the page
  report its own `devicePixelRatio` and `innerWidth`, and on real 2x hardware
  (M4 iMac, no override): `dpr=2, innerWidth=640` in a 1280-wide window.
- Cursor shape, IME composition, and visibility are reported to the host.
  Cursor changes are verified on all three platforms, by clicking a known
  element and reading back the shape CEF asked for.
- Cookies (`set_cookie`, `request_cookies` / `poll_cookies`, `delete_cookies`)
  and script results (`request_script_result` / `poll_script_result`, values
  returned as JSON from the renderer) both work, request-then-poll because CEF
  answers asynchronously.
- A dead renderer is survivable: `ContentProcessTerminated` now carries CEF's
  termination status, and `request_repaint()` is the nudge that gets the
  replacement renderer painting again (navigating alone leaves the host on its
  pre-crash frame). Verified on Windows and macOS by crashing on purpose and
  counting frames imported afterwards, 0 without the recovery and non-zero
  with. On Linux the crash never reaches the host at all; see the footnote in
  `welding/README.md`.
- Chromium command-line switches are reachable through
  `CefRuntimeConfig::command_line_switches`, for the many behaviours with no
  CEF API.
- Three runnable demos (`demo-weld-win`, `demo-weld-linux`,
  `demo-weld-mac`); the macOS demo ships a CEF helper binary, an `.app`
  bundler, and unattended pixel-readback validation. All three take the same
  environment knobs and can script a click, a wheel scroll and a keypress, so
  the input path is provable without a human at the keyboard.
- Without the `cef-runtime` feature the library compiles with no CEF
  distribution; producer constructors return a pending-wiring error.

Current plan (`design_docs/`, 2026-08-10 parity plan): W1 through W6 have
landed. What remains is W7, the host-decision surfaces (downloads, auth
challenges, permission requests, context menus); W8, the long tail (drag,
touch, find-in-page, PDF, zoom and UA settings, per-producer profiles); and
W9, the Chrome DevTools Protocol, which is this lane's distinguishing feature
and the one thing no sibling can offer.

## Use

For wgpu hosts that want an app-shipped Chromium lane. Three constraints
govern every consumer (details in [`welding/README.md`](welding/README.md)):

1. CEF re-executes the host binary for its subprocesses: call
   `CefRuntime::execute_process_from` first thing in `main()` and exit if it
   returns an exit code.
2. CEF's paint handles are callback-scoped: `welding` takes ownership inside
   the callback — a pixel copy on Windows, a `CFRetain`ed IOSurface wrapped
   zero-copy on macOS — and exposes only owned resources.
3. CEF is not a system library: ship libcef with the app and point
   `CEF_PATH` at a CEF 148 binary distribution.

```sh
cargo check -p welding                          # no CEF needed
cargo check -p welding --features cef-runtime   # real integration

# Windows / Linux demos (set CEF_PATH first; WELD_URL picks the page)
cargo run -p demo-weld-win
cargo run -p demo-weld-linux

# macOS needs the .app bundle
cd demo-weld-mac && cargo run --bin bundle-demo-weld-mac && open ../target/bundle/demo-weld-mac.app
```

## License

MPL-2.0 ([LICENSE](LICENSE)).

---

*This README was generated by AI and will be edited by the author upon
release.*
