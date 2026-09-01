# wgpu-weld

First, a note:

With the first release of this CEF webview embedder, I would like to quickly
identify my goals.

1) I was shocked this didn't exist, and that wgpu contexts didn't have an easy
webview embedding/import solution. An attempt beats zero attempts, so I thought 
I'd try it from a few angles: embedding servo (the first path), embedding 
system webviews (the second path), and finally, embedding CEF. Ironically,
weld's seen the first release of the stack.

2) I wanna git gud with Rust, AI, and web technologies. This seemed a worthy project.

3) I have a prototype browser in my stack called turnstone. I would like users
to be able to switch between servo, chromium, the system webview, and the nascent 
native genet web engine, which will offer static->scripted->fullweb render tiers.
The real advantage is being able to choose the right engine for your platform!

4) A big thing I'm pursuing is a unified wgpu device: same version, 
same device, to make sharing textures easy. Controlling the embedding lib
allows me to ensure that happens.

The release notes and README both say "every desktop platform," which means
every platform I could test. The current headed hardware workflow adds the
previously missing NVIDIA/DX12 and native Wayland/RADV fixtures. Now, the
generated README, which, trust me, is a convenience for both you and me.

---

Weld Chromium Embedded Framework (CEF) rendered output into wgpu-importable
GPU textures via CEF's accelerated off-screen rendering. The
[`welding`](welding/) library bundles Chromium through CEF and routes its
paint output into host-owned wgpu textures, so an app can render live web
content inside its own pipeline. It is the CEF sibling of
[`wgpu-scry`](https://github.com/merely-made/wgpu-scry) (system webviews)
and [`wgpu-graft`](https://github.com/merely-made/wgpu-graft) (Servo, plus
the shared `grafting` interop core).

## Status (2026-08-31)

Prototype. `welding` 0.13.0 is published on crates.io (MPL-2.0); `main` is the
0.14.0 compatibility revision. It makes retained Metal frames move-only and
adds deterministic pixel fixtures whose mismatches fail the process. Per-platform
detail, and the difference between "verified on that hardware" and
"implemented but not yet run there", is the table in
[`welding/README.md`](welding/README.md).

All three native wrappers now delegate to the same exact Graft commit
(`59cd8a3ec017aca46b0756d2ec90fd0a62550ef4`). Welding keeps CEF's callback
lifetime, copy/retain, modifier fallback, and synchronization policy; Graft
owns D3D12, Metal, and Vulkan resource registration with wgpu. The platform
demos are required CI builds rather than optional examples.

0.12.0 is the first release where accelerated GPU import works on every
desktop platform reachable for testing. Linux had only ever worked on
Intel/Mesa; AMD/RADV was refused until wgpu 30 supplied the extension that
case needs. The trusted hardware workflow now exercises NVIDIA through the
Windows DX12 path and RADV through a native Wayland window.

0.12.0 shipped against CEF 147. Releases from 0.12.1 use CEF 151; the 0.13.0
lock resolves `cef` 151.8.0+151.3.24. That line also drops the last split in
the dependency graph: `cef` carried its own
`wgpu 29` for an importer `welding` never calls, so the demos resolved two
wgpu majors at once. On 151 the whole workspace resolves a single row at 30.
The library compiles unchanged on all three platforms. The historical
verification tables name the CEF build each battery actually measured.

A parity battery was run on all three platforms on 2026-08-12 (Windows 11,
macOS 15.7 on an Intel iMac, macOS 26.5 on an Apple Silicon M4 iMac, Fedora
on an AMD ThinkPad). Input, cursor, HiDPI, navigation, console, cookies,
script results and command-line switches were verified on every one. The
`headed parity battery` workflow now reruns those receipt-bearing cases on the
NVIDIA, RADV, M4, and Intel Mac runners whenever the demo/runtime seams change,
plus weekly. The CEF 151 DevTools-window regression, RADV crash-notification
gap, and Intel native-menu popup difference are named skips on only the hosts
where they are documented. Per-case logs are retained as workflow artifacts.

The last three untested rows were then taken the same evening, and two of them
turned out not to work. `set_visible` is verified on Windows and macOS, with
painting stopping exactly while hidden. DevTools was not implemented at all
despite the capability probe claiming otherwise; it opens a real window on
Windows now and crashes CEF on macOS, where the producer refuses the call
rather than segfault its host. IME composition was delivering nothing at all;
it is fixed and verified on all three platforms. `welding/README.md` carries
the evidence for each.

The IME bug is worth knowing about if you call CEF's C API from anywhere:
`replacement_range` must be a real pointer. CEF's own C++ wrapper takes it by
reference and therefore always passes one, so non-null is the C API's contract,
and libcef's generated entry point verifies it and returns early on NULL --
silently, in a release build. Passing null does not fail, it just drops the
call before any CEF code runs.

- All three platform import lanes are hardware-verified: Windows (D3D11
  copy, D3D12 shared handle via Graft, into wgpu), Linux (DMABUF via Graft's
  Vulkan external-memory import, Fedora 44 on Intel/Mesa and on AMD
  Renoir/RADV), macOS (IOSurface to Metal texture, then Graft, on both Intel
  and Apple Silicon).
  Linux needs a DRM format modifier to import. Intel/Mesa supplies an explicit
  one; AMD/RADV supplies `DRM_FORMAT_MOD_INVALID` instead, and importing that
  needs `VK_EXT_image_drm_format_modifier`, which wgpu enables from 30 on. So
  the `wgpu-30` row imports it as linear. CEF's DMABUF is a foreign image;
  content-preserving queue acquisition and wgpu state registration require
  the `wgpu-30` row and a device created through
  `build_dmabuf_capable_device`. Rows 28 and 29 compile but refuse this runtime
  path with a typed error.
- The capability probe reports honestly as of the 2026-08-10 truth pass: a
  unit test pins every "Supported" claim to a real handler.
- Popup widget surfaces (`<select>` dropdowns and similar) render via a
  separate `acquire_popup` surface: verified on Windows and on macOS 26
  (Apple Silicon). On macOS 15.7 (Intel) Chromium used a native menu and no
  popup was ever delivered, so the behaviour differs by macOS generation. On
  Linux the dropdown opens and reports its geometry. The RADV parity job now
  requires the `imported popup` receipt, making the first post-AMD-fix run the
  current texture-import gate rather than another manual observation.
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
- The Chrome DevTools Protocol is exposed directly
  (`send_devtools_message` / `poll_devtools_message`), opt-in because CDP is
  chatty, with a bounded queue that counts what it drops rather than growing
  behind a host that stopped reading.
- Chromium command-line switches are reachable through
  `CefRuntimeConfig::command_line_switches`, for the many behaviours with no
  CEF API.
- Three runnable demos (`demo-weld-win`, `demo-weld-linux`,
  `demo-weld-mac`); the macOS demo ships a CEF helper binary, an `.app`
  bundler, and unattended pixel-readback validation. All three take the same
  environment knobs and can script a click, a wheel scroll and a keypress, so
  the input path is provable without a human at the keyboard. CI now downloads
  the pinned CEF distribution and requires the matching demo to compile on
  each hosted operating system.
- Without the `cef-runtime` feature the library compiles with no CEF
  distribution; producer constructors return a pending-wiring error.

Current plan (`design_docs/`, 2026-08-10 parity plan): W1 through W6 have
landed. W7 is done bar one row: downloads, permission requests, and context
menus are verified on all three platforms; `GetAuthCredentials` is wired and
answerable but CEF has never been observed to call it. W8's code is complete:
host/page drag-drop, direct touch, correlated PNG snapshots, and one CEF
`RequestContext` per producer are verified on all three. System printing reaches CEF's native
dialog on Windows, is wired on macOS, and explicitly reports unavailable on
Linux, where CEF requires an embedder-owned printer UI and spooler. W9, the
Chrome DevTools Protocol, is done and verified on all three: its unwrapped JSON wire format lets
an existing CDP client drive an off-screen browser.

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
   `CEF_PATH` at a CEF 151 binary distribution, matching the `cef` crate.

The library defaults to wgpu 30 and also carries `wgpu-29` and `wgpu-28`.
Select 29 or 28 with default features disabled; combine `cef-runtime` with
the same feature list when building the real Chromium integration. The
default row changed from 29 to 30 in 0.11.0, so a consumer taking default
features moves major with it.

```toml
[dependencies]
welding = "0.13"

# or pin an older row:
# welding = { version = "0.13", default-features = false, features = ["wgpu-29"] }
```

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

**Made with AI**
