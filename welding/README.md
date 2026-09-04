# welding

Embed Chromium in a `wgpu` application. `welding` drives the Chromium Embedded
Framework in accelerated off-screen rendering mode and hands each painted frame
to the host as a GPU texture on the host's own device, with no CPU round trip.

Use it when you want a full Chromium in your renderer, and you are willing to
ship Chromium to get it. If you would rather use the webview the OS already
has, its sibling [`scrying`](https://crates.io/crates/scrying) covers that lane;
[`grafting`](https://crates.io/crates/grafting) is the texture-interop core all
three Weld platform imports use.

The crate defaults to wgpu 30 and also carries `wgpu-29` and `wgpu-28`
features. Pick the row matching the host, with default features disabled for
29 or 28. `welding::wgpu` re-exports the selected version so public device and
texture types cannot silently come from a different major.

The current CEF runtime is intentionally explicit about its security posture:
pass `CefSandboxMode::UnsandboxedTrustedContent` to the subprocess entry point
and to `CefRuntimeConfig::new`. That mode disables Chromium's process sandbox
and is for trusted-content embedding, not arbitrary untrusted browsing.

**The default row changed in 0.11.0**, from 29 to 30. A consumer taking default
features moves major with it; pin `default-features = false, features =
["wgpu-29"]` to stay where you were.

How far each row has been taken:

| row | evidence |
| --- | --- |
| `wgpu-30` (default) | live GPU-import receipts on all three paths; the current Graft-backed wrappers compile on Windows, Apple Silicon, and AMD/RADV Linux |
| `wgpu-29` | compiles on all three hosts; DX12 and Metal retain their live receipts, while content-preserving CEF DMABUF import now returns a typed version error |
| `wgpu-28` | compiles on all three hosts; **not** exercised on hardware, and CEF DMABUF import returns the same typed version error |

## State, 2026-09-04

Version 0.14.1 is the current published release. It refuses CEF 151's unsafe
native DevTools window on every platform while preserving the supported CDP
path. Version 0.14.0 made retained Metal frames move-only, and each platform
demo has an embedded dodger-blue pixel fixture that exits unsuccessfully on a
mismatch.
Version 0.12.0 was the first one where accelerated GPU import worked on **every**
desktop platform. Linux had only
ever worked on Intel/Mesa; AMD/RADV was refused with a typed error until
0.12.0, for reasons the `[^linux]` note below explains.

Welding uses published `grafting` 0.6.0 and delegates every native wgpu wrapper
to its owned, value-consuming import API. CEF-specific callback
ownership, the Windows copy, IOSurface construction, and Linux modifier policy
remain here. DX12 converts Weld's owned handle into `OwnedHandle` once, Metal
moves the retained `MTLTexture`, and Linux hands Graft a deduplicated `OwnedFd`
buffer table with per-plane indices. Required CI jobs compile the real CEF demo
on Windows, macOS, and Linux in addition to the nine-row library matrix. Trusted
runners execute the pixel fixture on NVIDIA/DX12, RADV/native Wayland, Intel
Metal, and Apple Silicon Metal.

**0.13.0 builds against CEF 151** (`cef` `151.8.0+151.3.24`); 0.12.0 shipped
CEF 147 and 0.12.1 moved the published line to 151. The library compiled
unchanged across those Chromium majors.

`scripts/parity-battery.sh` ran on all four machines on 2026-08-17:

| machine | result |
| --- | --- |
| Windows 11, RTX 4060 | 12 pass, 2 live, 1 fail |
| Fedora 44, AMD Renoir/RADV | 12 pass, 2 live, 1 fail |
| macOS 26.5, Apple M4 | 12 pass, 2 live, 1 fail |
| macOS 15.7, Intel iMac | 11 pass, 2 live, 2 fail |

Import, input, find, zoom, IME, context menu, touch, page drag,
script/PDF/UA, visibility and crash recovery hold everywhere on 151.

**Only two things fail, and neither is new behaviour discovered by accident.**
DevTools is a real 151 regression on Windows and Linux, see [^devtools]. The
Intel iMac's second failure is the `<select>` popup, which is the known macOS
split in [^macpopup] reproducing rather than anything breaking: the page
loaded, the click landed, no popup surface was delivered and nothing reported
an error. The battery cannot tell a documented platform difference from a
fault, so it reports that as a failure and this note supplies the reading.

CDP and downloads were both recorded live in the first pass because the
battery asserted nothing for them. Both work on 151:

```text
CDP sent {"id":1,"method":"Browser.getVersion"}
CDP <-   {"id":1,"result":{"protocolVersion":"1.3","product":"Chrome/151.0.7922.138",...}}
```

**CDP matters more than the DevTools row above suggests.** It is
`execute_dev_tools_method`, a different CEF call from the `show_dev_tools`
window that crashes on 151, and it is the path a host uses to drive its own
inspector pane. A host that never opens CEF's native window is unaffected by
that regression.

**Downloads work on 151. The earlier failure was a path format, and CEF hides
it.** Chased 2026-08-17. CEF on Windows **silently discards** a download whose
destination contains forward slashes: `on_before_download` is answered, the
transfer runs to completion and reports every byte, and then nothing is
written. No file, no `.crdownload` partial, `is_complete` never true and
`full_path` empty for the life of the item:

```text
DLPROBE id=1 complete=false canceled=false received=28 total=28 full_path=""
```

Handing the same download a native `C:\...` destination writes the file
immediately, with the expected payload. It was never blob-specific and never a
151 regression: a `data:` URL failed the same way, and both succeed once the
path is native.

`welding` now normalises the destination to the platform's separators, because
a host is well within its rights to configure `C:/downloads`, and one under
Git Bash or any POSIX-shaped config will. The old behaviour had no symptom
except a file that never appeared.

The battery also grew a `dl-file` row asserting the file exists on disk. The
original case asserted on `DownloadProgress`, which reported 28 of 28 bytes for
a transfer that wrote nothing, so the row passed throughout.

Rows not covered by the battery (cookies, console, auth, permissions, the
system print dialog) were last measured on 147 and have not been re-run.

Every "verified" here was checked by running it on that platform's hardware,
in one battery per machine: Windows 11 (this laptop), macOS 15.7 on an Intel
iMac, macOS 26.5 on an Apple Silicon M4 iMac (at a native 2x scale factor),
and Fedora 44 on a ThinkPad (AMD Renoir/RADV, Mesa 26.1.5).

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
| Popup widgets (`<select>`) | verified | **differs by macOS** [^macpopup] | verified on AMD/RADV |
| Renderer crash recovery | verified | verified | event not delivered [^linuxcrash] |
| Visibility (`set_visible`) | verified | verified | wired |
| DevTools window | refused by design [^devtools] | refused by design [^devtools] | refused by design [^devtools] |
| IME composition | verified | verified | verified |
| Downloads | verified [^dl] | untested on 151 | untested on 151 |
| Auth challenges | **never fires** [^auth] | wired | wired |
| Permission requests | verified | verified | verified |
| Context menus | verified | verified [^macmenu] | verified |
| DevTools protocol (CDP) | verified | verified | verified |
| Find in page | verified | verified | verified |
| Zoom | verified | verified | verified [^zoomlevel] |
| History state (`can_go_back`) | verified | verified | verified |
| Print to PDF | verified | verified | verified |
| User agent override | verified | verified | verified |
| System printer dialog | verified | wired | unavailable [^linuxprint] |
| Host/page drag-drop | verified | verified | verified |
| Direct touch | verified | verified | verified |
| PNG snapshot | verified | verified | verified |
| Per-producer profile | verified | verified | verified |

"verified" means observed working on that platform's hardware. "wired" means
implemented but not yet exercised on that platform's hardware.

The W8 tail now has concrete API shapes rather than prospective rows.
`DragInput` drives an OS-originated drag into the page and `DragStarted` hands
a page-originated payload to the host's toolkit drag loop; `TouchInput` keeps
contact identifiers and phases intact; `request_snapshot_png` /
`poll_snapshot_png` return an ID-correlated compositor PNG completion
asynchronously. Welding admits at most 16 captures awaiting completion or
polling, and rejects a further request rather than discarding an admitted
completion; every
producer owns a CEF `RequestContext`. Pointer and pen remain unmodelled.

On macOS, a disk-backed child context becomes ready asynchronously. Create it
with `MacosCefProducer::prepare_profile` in the native event callback, pump CEF
outside that callback, then retry `try_new_with_prepared_profile` until it
returns the producer. Calling CEF's pump inside winit re-enters AppKit and
aborts the process.

[^linux]: Linux needs the DMABUF buffer to carry a DRM format modifier.
Intel/Mesa supplies an explicit one and the frame import is verified there.
AMD/RADV hands over `DRM_FORMAT_MOD_INVALID` instead, and importing that needs
`VK_EXT_image_drm_format_modifier` on the wgpu device. Create the unified host
device through `welding::build_dmabuf_capable_device`; Graft adds the complete
DMA-BUF extension set, including `VK_EXT_queue_family_foreign`.

**Which row you are on decides what happens.** wgpu enables that extension from
**30** onward and exposes it as `VULKAN_EXTERNAL_MEMORY_DMA_BUF`. Graft also
acquires CEF's image from Vulkan's foreign queue family and registers the
resulting shader-readable state with wgpu. That complete content-preserving
contract is available on row 30. An implicit buffer is deliberately treated as
`DRM_FORMAT_MOD_LINEAR`, verified on RADV by dumping the whole imported texture
rather than counting non-zero bytes, which a wrongly tiled buffer would also
pass. Rows 28 and 29 cannot register the established state at the HAL boundary,
so Welding refuses CEF DMABUF import there with a typed error even when the
modifier is explicit.

The popup row reads "opens" because it was measured on the AMD machine while
that refusal was still in force: CEF offered the dropdown and reported its
geometry (`on_popup_show`, then `on_popup_size 320x197 at 0,80`) while the
texture import was refused. Popup import has not been re-measured since the
`wgpu-30` path started importing, so treat that row as untested rather than as
a known limit.

[^zoomlevel]: `zoom` works everywhere. `zoom_level`, the getter, only reads
truly where the host thread is CEF's UI thread — Linux and macOS here. CEF
documents `GetZoomLevel` as UI-thread-only and Windows runs CEF's UI thread
separately, so it reads 0.0 there however the page is actually zoomed.
`set_zoom_level`, the absolute setter, is wired on all three platforms but
not yet measured by a headed receipt on any of them; this row's grades cover
the stepping command only.

[^macmenu]: macOS reports an extra `Selection` target, because a right-click
there selects the word under the cursor first. The event is otherwise identical
on all three.

[^auth]: `GetAuthCredentials` is registered on all three producers and the
answer path is implemented — `NavigationEvent::AuthChallenged` plus
`answer_auth` / `cancel_auth`, held open only when
`CefSurfaceConfig::handle_auth_challenges` is set, and declined immediately
otherwise so an unwired host fails requests instead of hanging them. CEF has
simply never been seen to call it. A probe inside the handler counted **zero**
invocations against a top-level 401 that Chromium itself then failed with
`ERR_INVALID_AUTH_CREDENTIALS`, with and without `CEF_RUNTIME_STYLE_ALLOY`,
while other methods on that same handler fire normally. This looks like the
Chrome-bootstrap pattern seen elsewhere here: Chrome owns the login prompt and
a windowless browser has none. Proxy authentication is untested — CEF reports
`is_proxy` separately, and that path may well work. Do not rely on this row.

[^linuxprint]: Windows' native dialog was observed on 2026-08-13, then
hardware-verified on 2026-08-14: `WELD_PRINT=1` opened the CEF-owned dialog,
the ready `Brother HL-3170CDW series Printer` queue was selected, and one W8
probe page was submitted. Windows retained job 2 as `Complete`, with one of one
pages printed (20,739 bytes); PrintService event 307 confirms physical output.
Linux CEF provides no native printer dialog. It requires an
embedder-owned `CefPrintHandler` to supply both the dialog and spooler, so
`print()` returns an explained unsupported error there. `welding` does not
silently select a default printer or invoke `lp`.

[^dl]: CEF silently discards a download whose destination has forward
slashes: every byte arrives, `is_complete` stays false, `full_path` stays
empty, and no file is written. `welding` normalises to native separators
so a host configuring `C:/downloads` is not quietly broken. macOS and Linux
have not been re-measured on 151.

[^devtools]: **On CEF 151, opening DevTools for a windowless browser crashes
the host process on Windows and Linux too.** Measured 2026-08-17 by the parity
battery, on all three platforms, and it reproduces every run:

| platform | CEF 148 | CEF 151 |
| --- | --- | --- |
| Windows | opened a real DevTools window | `Failed to create shared context for virtualization`, then `FATAL: GPU process isn't usable. Goodbye.`, exit 3 |
| Linux | no crash, no window seen | segmentation fault, core dumped, exit 139 |
| macOS | crashed CEF, so the call is refused | unchanged, and the refusal is why this platform survives |

The macOS refusal was written against 148 and looked over-conservative. Three
Chromium majors later the same class of fatal failure was measured on Windows
and Linux. All producers now refuse the native window and report it unsupported;
the working DevTools protocol remains supported for host-owned inspector UIs.

The original macOS finding, unchanged: opening DevTools for a windowless
browser crashed CEF 148 from inside the framework (`EXC_BAD_ACCESS` at
null+0x150, on the host thread), with a NULL `CefWindowInfo`, with a
bounds-only one, and with every by-ref argument supplied non-null including
`inspect_element_at`. Rather
than terminate its embedder, the macOS producer refuses the call. Windows and
Linux now do the same. `probe()` reports the native window unsupported on all
three so a host can grey the button out.

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
export WELD_TOUCH_AT=100,100       # direct touch contact in physical pixels
export WELD_DROP_FILE=/tmp/example  # staged Enter, Over, Drop at that point
export WELD_PAGE_DRAG=100,100,220,100 # source drag in physical pixels
export WELD_FINISH_PAGE_DRAG=1      # finish that source drag as a copy
export WELD_SCRIPT='({dpr: window.devicePixelRatio})'
export WELD_COOKIE_URL=https://example.com/
export WELD_SNAPSHOT=/tmp/page.png # private CDP screenshot helper
export WELD_CACHE_ROOT=/tmp/weld-cache
export WELD_PROFILE=/tmp/weld-cache/person-a
export WELD_PRINT=1                 # opens the native dialog where supported
export WELD_TIMEOUT_SECS=45          # gracefully end a scripted battery
export WELD_SWITCHES=disable-popup-blocking,lang=en-GB
export WELD_SKIP_CASES="popup"       # explicit documented platform skips
export WELD_BACKGROUND=transparent # or rrggbb; unset = opaque white
```

Create the root or the profile's parent, but leave the final profile directory
absent. CEF creates that directory itself; pre-creating it makes Chrome reject
the named profile and fall back to `Default`.

The Linux demo always adds `no-first-run` and `no-default-browser-check`, so a
fresh CEF profile cannot block inside `CefInitialize` before the demo timeout
starts. `WELD_SWITCHES` appends host-specific switches to those defaults.

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

Rendering the page to a PDF, and being someone else on the wire:

```rust,ignore
producer.print_to_pdf(Path::new("/tmp/page.pdf"))?;
// Completion arrives later, since Chromium answers asynchronously:
// NavigationEvent::PdfPrintFinished { path, ok }

// The user agent is process-wide, not per producer -- CEF takes it in
// CefSettings, so every producer under one runtime shares it.
let sandbox = CefSandboxMode::UnsandboxedTrustedContent;
let mut runtime = CefRuntimeConfig::new(&cef_path, sandbox);
runtime.user_agent_product = Some("MyApp/1.0".into());  // or user_agent for all of it
```

For the remaining W8 operations:

```rust,ignore
use welding::{
    DragEventKind, DragInput, DragOperations, DragPayload, EventModifiers,
    TouchInput, TouchPhase,
};

producer.send_touch_input(TouchInput {
    id: 1,
    x: 240.0,
    y: 160.0,
    radius_x: 8.0,
    radius_y: 8.0,
    rotation_angle: 0.0,
    pressure: 1.0,
    phase: TouchPhase::Started,
    modifiers: EventModifiers::default(),
})?;

producer.send_drag_input(DragInput {
    kind: DragEventKind::Enter,
    payload: Some(DragPayload::default()),
    x: 240,
    y: 160,
    modifiers: EventModifiers::default(),
    allowed_operations: DragOperations::COPY,
})?;

let requested = producer.request_snapshot_png()?;
if let Some(completion) = producer.poll_snapshot_png() {
    assert_eq!(completion.id, requested);
    assert!(completion.result?.starts_with(b"\x89PNG\r\n\x1a\n"));
}
```

`CefSurfaceConfig::user_data_dir` opts a producer into a persistent profile.
It must be absolute and below the process-wide `CefRuntimeConfig::cache_path`;
without it, each producer still receives a separate in-memory `RequestContext`.

The Chrome DevTools Protocol goes through unwrapped — the thing a system
webview cannot offer. JSON in, JSON out, exactly as the protocol documents it,
so an existing CDP client can drive it:

```rust,ignore
let config = CefSurfaceConfig {
    devtools_protocol: true,   // off by default: CDP is chatty
    ..Default::default()
};

producer.send_devtools_message(r#"{"id":1,"method":"Page.enable"}"#)?;

// Every tick. The queue is bounded, so a host that stops polling loses the
// oldest messages -- devtools_dropped() says how many, rather than hiding it.
while let Some(json) = producer.poll_devtools_message() {
    // {"id":1,"result":{...}} and {"method":"Page.loadEventFired",...}
}
```

A right-click gets no menu from CEF — it has nowhere to draw one under
windowless rendering — so `welding` suppresses it and hands the host what it
needs to draw its own:

```rust,ignore
if let NavigationEvent::ContextMenuRequested { x, y, targets, link_url, .. } = event {
    // x, y are physical pixels, like every other coordinate here.
    // targets is e.g. [Page, Frame, Link]; several apply at once.
    my_menu.open_at(x, y, &targets, &link_url);
}
```

Permission requests — camera, microphone, location, notifications — are
reported and denied unless the host opts in, because an unanswered request
leaves the page waiting forever:

```rust,ignore
let config = CefSurfaceConfig {
    handle_permission_requests: true,   // off by default: report and deny
    ..Default::default()
};

if let NavigationEvent::PermissionRequested { id, origin, permissions, .. } = event {
    // permissions is e.g. [PermissionKind::Geolocation]; anything this build
    // does not name still arrives, as PermissionKind::Other(bit).
    if trusted(&origin) { producer.grant_permission(id)?; }
    else { producer.deny_permission(id)?; }
}
```

Chromium remembers a decision per origin in the profile, so a granted
permission stays granted across runs sharing a `cache_path`.

Downloads are refused unless the host says where they may land. CEF asks for a
destination inside a callback it cancels the download without an answer to, and
on Linux and macOS that callback runs on the thread a host reply would have to
come back on — so the directory is policy, and the steering happens afterwards:

```rust,ignore
let config = CefSurfaceConfig {
    download_dir: Some("/home/me/Downloads".into()),  // None refuses downloads
    ..Default::default()
};

// Then, on ordinary ticks:
match event {
    NavigationEvent::DownloadStarted { id, destination_path, .. } => { /* show it */ }
    NavigationEvent::DownloadProgress { id, bytes_received, .. } => { /* at most 10/s */ }
    NavigationEvent::DownloadFinished { id, error, .. } => { /* error is Some on failure */ }
    NavigationEvent::DownloadCancelled { id, .. } => {}
    _ => {}
}
producer.pause_download(id)?;   // applied on that download's next update
producer.cancel_download(id)?;
```

The server's suggested filename only contributes its final component, so it
cannot place a file outside `download_dir`.

IME composition goes in as text, and the page sees a real composition:

```rust,ignore
producer.ime_set_composition("weldime", (7, 7))?;   // composing
producer.ime_commit_text("weldime")?;               // committed
// The page gets compositionstart, compositionupdate, textInput,
// compositionend, and the field ends up holding the text.
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
let sandbox = CefSandboxMode::UnsandboxedTrustedContent;
let mut config = CefRuntimeConfig::new(&cef_path, sandbox);
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
    let sandbox = welding::CefSandboxMode::UnsandboxedTrustedContent;
    if let Some(code) = welding::CefRuntime::execute_process_from(cef_path.as_ref(), sandbox)
        .expect("failed to probe CEF subprocess role")
    {
        std::process::exit(code);
    }
    let config = welding::CefRuntimeConfig::new(cef_path, sandbox);
    // initialize CEF with config, then create browsers
}
```

macOS is different: it launches separate helper executables from inside the
`.app` bundle instead. Those helpers must call
`CefRuntime::try_run_subprocess(args, sandbox)`, not `cef_execute_process`
directly, or the renderer comes up with no handlers and anything needing it
(script results) never answers. In sandboxed mode this entry point also loads,
initializes, retains, and destroys the framework's `libcef_sandbox.dylib`
context. See `demo-weld-mac` for a working bundle, helper, and bundler.

### 2. Sandbox policy

Sandbox policy is explicit and has no default. Pass the same local value to
the subprocess entry point and `CefRuntimeConfig::new`.

`CefSandboxMode::Sandboxed` sets `CefSettings.no_sandbox = 0`. Linux then uses
CEF's native Chromium sandbox selection. macOS additionally initializes
`libcef_sandbox.dylib` in every helper through
`CefRuntime::try_run_subprocess`. Both require platform packaging and headed
hardware proof before an application treats the sandbox as part of its
security boundary.

Windows CEF 151 uses a different shape: `bootstrap.exe` creates the sandbox
context and calls an exported `RunWinMain` entry point in the application's
client DLL. That entry point borrows the supplied instance and context with
`CefWindowsSandboxContext::from_raw`, then calls its `execute_process` and
`initialize` methods. `CefRuntime::execute_process_from` cannot manufacture
the context and therefore rejects `Sandboxed` on Windows. See
`demo-weld-win/src/lib.rs` and its bundler for the complete packaging shape.

`CefSandboxMode::UnsandboxedTrustedContent` passes null `sandbox_info` and sets
`CefSettings.no_sandbox = 1`. It is intended only for trusted content and
demos, not arbitrary web content.

### 3. Handle lifetime

The resource `OnAcceleratedPaint` hands over is callback-scoped. `welding`
copies or retains it inside the callback and only ever exposes an owned
resource: a D3D11 copy into a weld-owned shared texture on Windows, a
`CFRetain`ed `IOSurface` on macOS, `dup(2)`ed plane fds on Linux. Never hold
CEF's own handle past the callback.

### 4. Distribution

CEF is not a system library. `libcef.dll` / `libcef.so` / `Chromium Embedded
Framework.framework` ships with your application, and its path goes to
`CefRuntimeConfig`. Under the `cef-runtime` feature the `cef` / `cef-dll-sys`
crates download and link it at build time.

## Features

- `cef-runtime` (off by default) enables the real CEF integration. Without it
  the crate still compiles and every producer constructor returns a
  pending-wiring error; `CefSurfaceCapabilities::probe()` also reports the
  CEF-backed browser features as unsupported, which keeps `cargo check` cheap
  without overstating runtime support.
- `cpu-paint-fallback` (off by default) enables the slower `OnPaint` CPU-bitmap
  path, for when accelerated OSR is unavailable.

## License

MPL-2.0.
