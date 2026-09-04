# Producer Parity Plan: welding / scrying / grafting

**Date:** 2026-08-10
**Status:** W1-W9 landed. The 2026-08-30 snapshot-provenance follow-up is
landed with focused local test and Windows consumer-check receipts. The
wgpu 30.0.1 release row has a green three-platform CI matrix and a current
headed Linux/Vulkan import receipt; the saved Metal hosts were unreachable
for a same-day rerun. The 2026-09-04 pre-release hardening and Graft ownership
adaptation slices are local commits and need fresh CI/hardware reruns before
publication.
Every "verified" claim below names the machine it was verified on.
A three-platform parity battery was run on 2026-08-12; results under
"Parity battery, 2026-08-12" below.
**Scope:** cross-repo. This doc lives in wgpu-weld because welding carries most
of the gap list, but it assigns work to all three siblings.

**Release authority (2026-09-03):** This remains the capability-parity
implementation record. Release hardening, package order, and publication gates
now live once in
`wgpu-graft/design_docs/2026-09-03_wgpu_triplet_release_plan.md`.

## Goal

The three embedding lanes should feel like one product family to a host:

| Lane | Engine | Distribution |
| --- | --- | --- |
| `welding` (wgpu-weld) | Chromium via CEF, bundled | app ships CEF |
| `scrying` (wgpu-scry) | system webviews, 5 backends | OS provides |
| `grafting` (wgpu-graft) | Servo (via surfman GL) + raw GL producers | app ships Servo |

Parity here means an embedder can pick a lane for its distribution trade-offs
without silently losing table-stakes browser features. It does not mean
identical traits. The measure is: a host-side adapter over any lane
(the mere `SurfaceEngine` direction) should be mechanical to write.

Authoritative per-backend detail for scrying stays in
[wgpu-scry/docs/parity-matrix.md](../../wgpu-scry/docs/parity-matrix.md);
this doc does not duplicate its footnotes.

## Findings

### Where each lane stands

`scrying` 0.4.0 is the feature ceiling. Its `WebSurfaceProducer` trait plus
per-backend inherent APIs cover: frame tiers (imported texture, CPU snapshot,
overlay), navigation with `can_go_back`/`can_go_forward`, mouse, pointer,
keyboard, drag, cursor-shape polling, IME observability, cookies with change
handlers, custom URL schemes, script messaging, downloads with
pause/resume/cancel decisions, auth challenges, permission requests,
find-in-page, PDF, print, snapshots, content rule lists, interaction-state
serialization, `set_visible`, and `apply_settings` (zoom, UA, devtools toggle,
JS toggle, context menus, accelerator keys, inactive scheduling). Its 18
`NavigationEvent` variants include download lifecycle, auth, media capture,
context menu, accelerator keys, and text-input focus.

`welding` was 0.1.0 when this was written, with a control core whose cookie and
script-result methods were trait defaults that errored, and a thin
rendering-completeness surface. As of 0.4.0 it has closed popups, HiDPI, cursor,
IME, visibility, cookies, script results, command-line switches and the honest
probe; what remains is the host-decision surface (downloads, auth, permissions,
context menus) and the long tail. Its rendering story is still the
best-verified of the three, on real hardware for all three platforms.

`grafting` 0.4.0 is a different kind: the interop core the other two consume
(welding: DX12 open-shared; scrying: DX12 open-shared) plus the Servo/GL
producer lane. Browser features in the Servo lane belong to Servo's own API
and the demos, not to grafting. Grafting's parity rows are the interop ones:
import paths, the explicit sync seam (`InteropSynchronizer`,
`Dx12FenceSynchronizer`, `VulkanSemaphoreSynchronizer`,
`MetalSharedEventSynchronizer`), the epoch-keyed import cache, and dual
wgpu-28/wgpu-29 support. Nobody else has the sync seam or the dual-wgpu trick.

### Cross-lane matrix

Legend: Y implemented, P partial, N missing, o not applicable to the lane.
welding column verified against source this session; scrying column summarizes
its own matrix; grafting column covers the interop layer plus Servo-lane
demos.

| Capability | welding (CEF) | scrying (best backend) | grafting |
| --- | --- | --- | --- |
| GPU frame import | Y (3 platforms hw-verified; Linux needs an explicit DMABUF modifier, see below) | Y | Y |
| CPU fallback tier | P (feature-gated, never runtime-exercised) | Y | Y (readback demos) |
| Explicit sync seam | N (implicit: callback copy + keyed mutex + cache-flush) | P (in-tree sync modules) | Y (owner) |
| HiDPI scale factor | Y (all three via `WELD_SCALE`; native 2x on the M4 iMac, 2026-08-12) | unverified, per-backend | o (host concern) |
| Popup widget surfaces (select/autocomplete) | Y on Windows and on macOS 26/arm64 (2026-08-12); absent in the macOS 15.7/Intel test; Linux opens, import blocked (RADV modifier) | o (webviews self-composite) | o (Servo self-composites) |
| Navigation control | Y | Y | demo-level |
| can_go_back / can_go_forward | N | Y | o |
| Navigation events | Y (all declared variants emit; NewWindowRequested proven via a switch) | Y (18 variants) | o |
| Mouse / keyboard / wheel | Y (Windows and Linux both proven to the DOM; mac thinner) | Y (caveats per backend) | demo-level |
| Pointer (pen) input | N | Y | N |
| Touch input | N (CEF has SendTouchEvent) | unverified | N |
| Cursor-shape reporting | Y (verified on Linux with a human pointer) | Y (all 5) | N |
| IME | P (input + composition-range feedback wired; compile-only) | Y (GTK/WPE lanes; win/mac unverified) | N |
| Drag / drop | N (CEF has StartDragging + DragTarget*) | mixed | N |
| Cookies get/set/delete | Y (request/poll; verified on Windows) | Y | o |
| Cookie change events | N | Y (WKWebView) | o |
| Script exec + result | Y (renderer round trip, JSON values) | Y | o |
| Web message bridge | Y | Y | o |
| Custom URL scheme handlers | N (CEF has scheme handler factory) | Y (all 5) | o |
| Downloads (lifecycle + decisions) | N | Y (incl. pause/resume/cancel) | o |
| Auth challenges | N (CEF has GetAuthCredentials) | Y (win/mac) | o |
| Permission requests | N (CEF has CefPermissionHandler) | Y (win/mac) | o |
| Context menus | N | Y (event + toggle) | o |
| Find-in-page | N (CEF has Find) | Y (win/mac) | o |
| PDF / print | N (CEF has PrintToPDF) | Y (win/mac) | o |
| Snapshot to PNG | N (demo probe only) | Y | o |
| Zoom / UA / settings | N | Y (apply_settings) | o |
| Visibility (WasHidden / set_visible) | Y (set_visible on all three) | Y | o |
| Per-producer profile isolation | P (global root_cache_path only; CEF has RequestContext) | Y (per-producer data_dir) | o |
| Render-process crash recovery | Y (status-carrying event + `request_repaint`; frames resume after a deliberate crash on Windows and macOS, not delivered at all on Linux — 2026-08-12) | P | o |
| DevTools window | N (CEF 151 crashes accelerated windowless browsers; all producers refuse it) | Y | o |
| DevTools protocol (CDP) | N (CEF has ExecuteDevToolsMethod; unique leverage) | N (WebView2 could; not exposed) | o |
| Multi-producer per process | Y | Y (except WPE) | Y |
| Honest capability probe | Y (`devtools` corrected again for the CEF 151 regression in 0.14.1) | Y (matrix + footnotes) | o |

### The capability probe lies in both directions (fixed in W1)

Kept as the record of what was wrong, since it is the reason W1 came first.
`CefSurfaceCapabilities::probe` was stale in both directions:

- `accelerated_paint_available = cfg!(target_os = "windows")` denies the
  Linux and macOS lanes that were hardware-verified today.
- `popups: Supported` while all three paint handlers explicitly skip popup
  paint elements and no `on_before_popup` exists.
- `console_messages: Supported` with no `on_console_message` handler.
- `NavigationEvent::{NewWindowRequested, ConsoleMessage,
  ContentProcessTerminated}` are declared variants that nothing emits.
- Meanwhile `cookies`/`script_result` say "not wired yet", which is right at
  the producer level even though the trait declares the methods; keep those
  honest until the impls land.

Per the diagnostics doctrine, a capability report that overstates is worse than
a missing feature, which is why this was the first thing fixed. A unit test now
pins every `Supported` to a handler that exists, so landing a feature has to
flip the status and the test together.

### W6: the renderer-side CefApp (2026-08-10)

welding now owns a real `CefApp`, constructed identically in the browser
process and in every subprocess. It was the single thing several unrelated
features were waiting on, because a `CefApp` is the only hook that exists in
both processes and welding's was an empty stub.

Three things landed on it, all proven on Windows:

- **Script results.** `request_script_result` / `poll_script_result`, request
  and poll for the same reason cookies are: the value comes back from the
  renderer over process messages, so blocking would wait on the loop carrying
  the reply. Evaluation happens in the frame's V8 context and the value returns
  as JSON, because a script can return anything.
  `({title: document.title, sum: 2+2, h1: ...})` came back as
  `{"title":"Example Domain","sum":4,"h1":"Example Domain"}`.
- **Chromium command-line switches**, via `on_before_command_line_processing`
  and `CefRuntimeConfig::command_line_switches`. A great many Chromium
  behaviours have no CEF API and are reachable only this way.
- **W1's last residual, closed.** `NewWindowRequested` was wired in W1 but
  unprovable, because Chromium's popup blocker swallows a non-gesture
  `window.open` before `on_before_popup` runs. With
  `disable-popup-blocking` as a switch it fires exactly as designed:
  `NewWindowRequested { url: "https://example.org/popup", user_gesture: false }`.

The macOS helper now goes through `CefRuntime::run_subprocess` rather than
calling `cef_execute_process` itself, so it hands CEF the same app. A helper
that does its own thing gets a renderer with no handlers, and script results
there would simply never answer.

This also opens the page-to-host direction: a render-process handler is what a
real bridge needs, and the `weld.message` path can now be finished properly.

### Linux + implicit-modifier DMABUF is blocked upstream in wgpu

Tried on the Fedora ThinkPad (AMD Renoir / RADV, 2026-08-10), not reasoned
about. CEF supplies `DRM_FORMAT_MOD_INVALID` there; Intel/Mesa supplies a real
modifier, which is why the original Phase 4 verification passed and this never
surfaced. Both ways of importing such a buffer are closed on a **wgpu-created**
device:

1. `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` requires
   `VK_EXT_image_drm_format_modifier` enabled on the device. wgpu does not
   enable it. `vkCreateImage` answers `VK_ERROR_FORMAT_NOT_SUPPORTED`
   (`VUID-VkImageCreateInfo-tiling-parameter`), and the validation layer was
   observed **aborting the process** while formatting its own error.
2. Plain `VK_IMAGE_TILING_LINEAR` does create an image, but
   `vkGetPhysicalDeviceImageFormatProperties2` reports DMA_BUF as an
   incompatible handle type for that combination
   (`VUID-VkImageCreateInfo-pNext-00990`), and the resulting texture panics
   inside `wgpu_core` on first use. Measured, not predicted: the frame imported
   and then took the process down.

welding cannot route around this, because the imported texture has to live on
the *host's* device rather than one welding creates. So the fix is upstream:
**wgpu should enable `VK_EXT_image_drm_format_modifier`** when the adapter
supports it (RADV does). With that, this case becomes the ordinary
explicit-modifier path using `DRM_FORMAT_MOD_LINEAR`, and the code is already
shaped for it.

This sits with the other wgpu external-memory item in the structural findings
below, and grafting owns that tracking issue since it owns the import core.
Until then welding refuses implicit-modifier buffers with a typed error naming
all of the above, which is the difference between a puzzling core dump and a
one-line explanation.

### Structural findings

1. **Resolved 2026-09-04:** Scry and Weld retain their producer-specific
   capture, callback-lifetime, modifier, and synchronization policy, while all
   D3D12, Metal, and DMABUF/Vulkan wgpu wrappers live in Graft. Both published
   crates use `grafting` 0.6.0, released from
   `816f3e7857afee863200e1c25b300c43b1532aae`.
2. **Resolved for Weld 2026-09-04:** Weld uses the exact published Graft 0.6.0
   semver dependency rather than following a branch or Git revision.
3. **Resolved for the current row:** Graft acquires foreign DMABUF images into
   shader-readable layout and registers `RESOURCE` at the wgpu 30 HAL boundary.
   Rows 28 and 29 reject content-preserving foreign imports explicitly because
   their HAL call cannot register that established state.
4. **Vocabulary drift**: `CursorShape`, focus reasons, key/mouse event
   shapes, and `NavigationEvent` naming differ between welding and scrying
   for no load-bearing reason. Convergence is cheap now and expensive later.
   Per the module doctrine this is alignment by convention, not a new shared
   crate.

## Parity battery, 2026-08-12

Run on all three machines with the same environment knobs: Windows 11 (this
laptop), macOS 15.7.7 on the Intel iMac at `192.168.4.105`, and Fedora on the
ThinkPad at `192.168.4.28` (AMD Renoir/RADV). Mayola's M4 iMac was asleep and
was not part of the run, so no Apple Silicon result was claimed here. (It ran
later the same day and is green; see the 2026-08-12 M4 progress entry, which
also corrects two of this section's claims.)

Turned from "wired" to "verified" on every platform: mouse, wheel and keyboard
input; cursor shape; HiDPI scale; navigation and title events; console
messages; cookies; script results; Chromium command-line switches.

Still untested on every platform, and so still "wired": IME composition,
`set_visible`, and the DevTools window. These are coverage gaps, not parity
gaps.

Four defects surfaced, all in the test harness rather than the library, and all
of the same family: **a demo mistook a paint for a clock.**

1. The battery was gated on `frames_imported > 60`. An accelerated producer
   paints only on change, so a static page delivers one frame and goes quiet;
   the macOS battery reported frames and nothing else. Now tick-driven.
2. The scripted gestures were then spaced by tick count, which stalls the same
   way. The Windows click fired and the wheel never did. Now spaced by elapsed
   time, which is the only honest clock here.
3. `WELD_SCALE`, which exists to exercise HiDPI on a 1x display, was undone
   seconds later by winit's `ScaleFactorChanged` reporting the real 1.0.
   Traced `view_rect` answers went `640x351` (correct) then back to
   `1366x701`, and the page reported `dpr: 1`. A forced scale now outranks the
   event.
4. The demos had drifted: `WELD_SWITCHES` was Windows-only and scripted input
   was Windows and macOS only, so three "wired" rows could not be exercised at
   all on Linux. One `scripted.rs`, the same in all three.

The Linux popup result is worth recording precisely, because the first run
looked like a missing feature and was not. `on_popup_show` and `on_popup_size`
were silent, so "CEF never offered a dropdown" and "the dropdown was offered
and failed to import" produced identical logs. With a trace added, the
ThinkPad reports `on_popup_show(true)` then `on_popup_size 320x197 at 0,80`:
routing and geometry are correct, and only the texture import is refused, by
the same AMD/RADV implicit-modifier limitation that blocks the main frame on
that GPU.

## Crash recovery, 2026-08-12 (evening)

The row moves from P to Y on two platforms, and the reason it was P turned out
to be two separate things.

`ContentProcessTerminated` was a bare variant. CEF hands `OnRenderProcessTerminated`
a status, an error code and a string, and all three were dropped on the floor,
so a host learned its renderer had died but not whether it was killed, crashed
or ran out of memory. That is exactly the fact that decides whether retrying is
sensible. The event now carries all three, with an `Unknown(i32)` arm so a
newer CEF cannot silently look ordinary.

Then the recovery itself needs two steps, and the second is not obvious.
Navigating again brings the page back -- Chromium spawns a fresh renderer for
the navigation -- but **painting is change-driven and the replacement has
nothing queued for the surface**, so the host keeps presenting its pre-crash
frame. `request_repaint()` is the nudge: `was_resized` to make CEF re-query the
view, `invalidate` to ask for the paint. Same family as everything else that
went wrong this week.

Measured with `WELD_CRASH_AFTER_SECS` (navigates to `chrome://crash`) against
a control with recovery off, counting frames imported after the crash:

| | before crash | after, no recovery | after, with recovery |
| --- | --- | --- | --- |
| Windows 11 | 2 | 0 | 2 |
| macOS 26.5, M4 | 1 | 0 | 1 |

The first Windows attempt read 0 both ways and looked like a failure. It was
not: the Windows demo had no frame-import logging at all, so the instrument was
measuring nothing. Worth stating plainly because it is the third time this week
a demo's own instrument produced a confident wrong answer.

**Linux does not deliver the event.** Chromium logs `Intentionally crashing`,
so the renderer really dies, and then `Failed to send GetTerminationStatus
request to zygote` -- and `OnRenderProcessTerminated` never fires. Not a wiring
difference: `request_handler()` is registered identically on all three and
fires on the other two. `--no-zygote` does not change it, and a 70s window
rules out late delivery. Killing the renderer directly was not reachable either,
because on Linux the renderers are forked from the zygote and keep its cmdline
rather than re-execing the host binary. Recovery there is untestable until the
event arrives; the AMD test machine also imports no frames at all, so even the
metric used above is unavailable on it.

## The last three rows, 2026-08-12 (late)

`set_visible`, IME and the DevTools window had been "wired" since W4 and had
never been run anywhere. Taking them one at a time turned two of the three into
defects, and one of those was a lie the test suite was holding in place.

**`set_visible`: verified, Windows and macOS.** It needs an animating page to
mean anything -- a static page paints once, so "painting stopped" is
unfalsifiable on it. Against `testing/weld_anim_probe.html`: Windows 647 frames
visible, 0 hidden, 883 after showing; M4 329 / 0 / 501. Exactly zero while
hidden, both machines.

**DevTools: was never implemented at all.** `open_devtools` returned a
pending-wiring error on all three producers, while
`CefSurfaceCapabilities::probe` reported `devtools: Supported` and the W1
truth-pass test *asserted* that claim. The test pinned the lie rather than
catching it, which is worth remembering about tests written from the same
belief as the code. Now implemented, and it took two rounds of platform
divergence to get right:

- NULL `CefWindowInfo`: Windows opens the window, macOS crashes inside CEF
  (`EXC_BAD_ACCESS` at null+0x150, host thread).
- bounds-only `CefWindowInfo`: macOS still crashes, and Windows now *silently*
  fails -- the call returns success and a zero-style child window never
  appears. Caught only because the check was "is there a window with this
  title", not "did the call return Ok".
- Top-level style flags on Windows: window back. macOS crashes either way, so
  that producer refuses the call and `probe()` reports it unsupported there.
  A library must not segfault its embedder to report a missing feature.

**CEF 151 correction, 2026-09-04.** The later CEF release crashes the Windows
GPU process after `show_dev_tools` and segfaults Linux. Version 0.14.1 extends
the refusal to all three producers, reports the native window unsupported, and
keeps the independently verified CDP path supported. The parity battery now
passes only when the refusal is observed and the browser process stays alive.

**IME: wired, returns `Ok`, delivers nothing.** Windows and macOS both.
Dug into properly; every precondition holds and CEF still drops the input.

What was ruled out, in order:

1. *The page.* It listens for `compositionstart`, `compositionupdate`,
   `compositionend`, `textInput` and `input`. None fires.
2. *Which call.* `WELD_IME_MODE` runs `ImeSetComposition` alone,
   `ImeCommitText` alone, `ImeFinishComposingText`, or the pair. All four are
   equally silent, so this is not the composition machinery specifically.
3. *Renderer focus, at the time of the call rather than at load.* A
   once-a-second heartbeat in the page reports `hasFocus=true active=i` before
   the IME call and for thirteen seconds after it.
4. *Browser-side text-input state.* This is the one a working keypress does
   **not** prove, and the distinction matters: `SendKeyEvent` injects into the
   focused widget directly, while the IME methods go through the browser's
   text-input machinery. So concluding "focus is fine, because typing works"
   was wrong reasoning, even though the conclusion held. Implementing
   `OnVirtualKeyboardRequested` settled it: CEF calls it with
   `TEXT_INPUT_MODE_DEFAULT` immediately after the click, so the browser does
   know an editable field has focus.
5. *The host object.* The same `CefBrowserHost`, fetched by the same
   expression in the same run, delivers key events into that very field
   (`value:K`).
6. *Chromium's own account.* Nothing logged under `--vmodule=*ime*=3`.

Two hypotheses were tried and disproved, and both changes reverted rather than
left in the tree on a hunch:

- **No composition underline span.** CEF's sample client always passes one and
  `welding` passed `None`. Supplying a span spanning the whole composition
  changed nothing.
- **No parent window handle.** CEF documents that a windowless browser without
  a parent window may find "some functionality that requires a parent window
  may not function correctly", and `welding` never sets `parent_window`.
  Plumbing the host `HWND` through changed nothing for IME. Reverted because it
  was unproven and would have left a Windows-only field on one of three
  producers -- but it is still worth doing properly across all three for dialog
  and context-menu parenting and monitor info, which is a separate job with its
  own evidence.

**The comparison, 2026-08-12 (late).** `cefclient` turned out to be the wrong
instrument and an expensive one: the `cef-dll-sys` distribution is the
*minimal* archive, with no prebuilt sample and no `tests/` sources, so it would
have meant a full standard distribution plus a CMake/MSVC build -- and
`cefclient`'s OSR IME is driven by real `WM_IME_*` messages, which needs an
installed IME and a human typing. Chromium's own tracing answers the same
question directly, because CEF's OSR IME entry points carry `TRACE_EVENT0`.

With `--trace-startup=cef --trace-startup-format=json`, a run that clicks the
field and composes records:

| trace event | count |
| --- | --- |
| `CefRenderWidgetHostViewOSR::SendMouseEvent` | 62 |
| `CefRenderWidgetHostViewOSR::OnAcceleratedPaint` | 44 |
| `CefRenderWidgetHostViewOSR::SendKeyEvent` | 6 |
| `CefRenderWidgetHostViewOSR::Invalidate` | 2 |
| `CefRenderWidgetHostViewOSR::ImeSetComposition` | **0** |
| `CefRenderWidgetHostViewOSR::ImeCommitText` | **0** |

**The IME calls never reach CEF's off-screen view, while mouse and key calls on
the same `CefBrowserHost` do.** The first attempt at this measured nothing, for
the usual reason: `--trace-startup-file=trace.json` writes *protobuf*, so the
grep found no event names and the absence looked like a result. It needed
`--trace-startup-format=json`, and the control (are there any `Cef*` events at
all?) is what caught it.

Reading CEF's source alongside that, every guard on the path is satisfied:
`AlloyBrowserHostImpl::ImeSetComposition` returns early only when the browser
is not windowless, when it is off the UI thread (it reposts), or when
`platform_delegate_` is null; `CefBrowserPlatformDelegateOsr` only when
`GetOSRHostView()` is null; `CefRenderWidgetHostViewOSR` only when
`render_widget_host_` is null. Painting proves the delegate, the view and the
widget host are all alive, and OSR proves windowless.

Five hypotheses tried, all disproved, all reverted rather than left in:

1. **No composition underline span.** CEF's sample client always passes one.
2. **No parent window handle.** CEF warns windowless browsers without one may
   find "some functionality that requires a parent window may not function
   correctly". Plumbed through and *verified to arrive* as a real `HWND`
   (`parent_window = Some(43322240)`) -- the first attempt at this test never
   checked the handle was non-`None`, which would have made the negative
   worthless.
3. **The API version pin.** The bindings are generated at `CEF_API_VERSION`
   999999 (experimental) while `pin_cef_api_version` pins `CEF_API_VERSION_LAST`
   (14700). Pinning to the experimental value changed nothing, and the original
   is kept because it fixed a real crash. Worth recording anyway: bindings and
   DLL are the same build (`147.0.14+g76d2442+chromium-147.0.7727.138` on both
   sides), so this is not a provenance mismatch.
4. **The runtime style.** The IME members live on `AlloyBrowserHostImpl` and
   CEF 147 defaults to the Chrome runtime, so `CEF_RUNTIME_STYLE_ALLOY` was
   requested explicitly. No change, and OSR kept painting (53 frames), so the
   control held.
5. **The calling thread.** Windows uses `multi_threaded_message_loop`, so the
   call is posted to CEF's UI thread -- but macOS runs CEF's UI thread *as* the
   host thread, needs no post, and fails identically.

One dead end is worth recording so it is not re-run: `GetWindowlessFrameRate()`
returning 0 for a browser created at 60 looked like proof of a shifted struct
layout, and a whole vtable-divergence theory was built on it. It is documented
UI-thread-only, and on Windows CEF owns its own UI thread, so 0 is simply what
it returns off-thread. The control that killed the theory was
`is_window_rendering_disabled()`, which returned 1 correctly.

**It was the call site after all, and it is fixed.** The trace narrowed the
loss to "between the C API entry point and `AlloyBrowserHostImpl`", and the
answer was sitting in the distribution the whole time: CEF ships its own
`libcef_dll/ctocpp/browser_host_ctocpp.cc`, the wrapper a C++ client uses to
call the same C API. Reading how *CEF itself* makes the call:

```cpp
_struct->ime_set_composition(_struct, text.GetStruct(), underlinesCount,
                             underlinesList, &replacement_range,
                             &selection_range);
```

The C++ API takes `replacement_range` as `const CefRange&`, so CEF's wrapper
can only ever pass a real pointer. That makes non-null the **contract** of the
C API, and libcef's generated entry point enforces it: it verifies the by-ref
params and returns early on NULL. In a release build that check is silent.

`welding` passed `None`. Every composition and every commit was dropped before
a single line of CEF code ran -- which is exactly why the trace showed nothing,
why all four call modes behaved identically, and why no guard anywhere looked
unsatisfied. Passing CEF's invalid range `(UINT32_MAX, UINT32_MAX)`, the same
"replace nothing" value `cefclient` uses, fixes it.

Verified on all three platforms with the full lifecycle reaching the DOM --
`compositionstart`, `compositionupdate`, `textInput`, `compositionend`, and the
field holding the text: Windows 11, macOS 26.5 on the M4, and Fedora on the
ThinkPad. All three call modes work separately (composition alone, commit
alone, finish-composing).

The lesson generalises beyond IME: **any CEF C API parameter that the C++ API
takes by reference must be non-null**, and passing null buys a silent no-op
rather than an error. Worth auditing the other `Option<&T>` arguments
`welding` passes.

One incident from the verification run, fixed rather than just noted: on a GPU
that cannot import CEF's DMABUF, the Linux demo logged an error on *every*
paint, and the animating IME probe page paints forever. That filled the
ThinkPad's 7.5G tmpfs and took the run down with it. The error is rate-limited
now (first, then every 500th).

## Headed GPU-import receipts, 2026-08-13

Taken on the `agent/w8-hardware-tail` code (HEAD `1269462`), against the
animating probe page so the import path runs continuously rather than once.
These are the live foreign-texture receipts the matrix and unit tests cannot
give.

| path | machine | result |
| --- | --- | --- |
| DX12 shared texture -> wgpu | Windows 11, this laptop | **1632 frames imported**, first `1280x800 Bgra8Unorm`, no `acquire_frame` failures |
| IOSurface -> MTLTexture -> wgpu | Intel iMac, macOS 15.7.7 | **1541 frames imported**, `VALIDATION PASS: the IOSurface carried real pixels` |
| DMA-BUF -> Vulkan external memory | ThinkPad, AMD Renoir/RADV | **0 imported, by design**: `DRM_FORMAT_MOD_INVALID` refused with the typed error, 3 refusals, `LoadEnd` control passing |

The macOS receipt is the strongest of the three because it reads the imported
texture back and asserts real pixels; the Windows one proves a live import
stream but has no readback. The Linux row is a *negative* receipt and the only
one available on this hardware: a successful DMA-BUF import needs Intel/Mesa,
which supplies an explicit modifier. Apple Silicon was asleep and is not
claimed here — the Metal receipt above is Intel.

Two measurement failures on the way, both worth recording because both produced
a plausible wrong reading:

- The first Linux run reported "0 frames imported" and I nearly filed it as the
  expected RADV refusal. It was not: `LD_LIBRARY_PATH` pointed at a
  `cef_linux*` directory chosen by `find ... | head -1` that held no
  `libcef.so`, and the binary never started. The refusal line was absent
  because nothing ran, and the run before it had been truncated before being
  read. The corrected run has a `LoadEnd` control precisely so an absence can
  be told apart from a non-start.
- The CEF runtime layout on this branch puts `libcef.so`, `icudtl.dat`,
  `locales` and `resources.pak` directly in `target/debug`, not in a
  `cef_linux*` subdirectory. Any script carrying the old path silently produces
  a dead binary.

**The demos cannot produce a wgpu-28 or wgpu-30 receipt.** `welding` compiles
against all three rows, but the demos are written against wgpu 29 only: flipping
the workspace to 30 fails them on `RequestAdapterOptions::apply_limit_buckets`,
`SurfaceConfiguration::color_space` and `SurfaceTexture::present`. So the 28 and
30 rows are compile-verified and configuration-tested, and *not* live-verified.
Closing that needs the demos ported, which is its own job.

## Receipts on the wgpu-30 row, 2026-08-14

The default row flipped to 30 in `02fb1cc`, which invalidated the receipts
above: they were taken on 29. Re-taken on all three paths at `770b6bb`.

| path | machine | result |
| --- | --- | --- |
| DX12 shared texture -> wgpu | Windows 11 | **1578 frames imported**, `1280x800 Bgra8Unorm`, `LoadEnd` control passing |
| IOSurface -> MTLTexture -> wgpu | M4 iMac, macOS 26.5.1, arm64 | `VALIDATION PASS`, probe **16384/16384 non-zero** (recorded in `770b6bb`) |
| DMA-BUF -> Vulkan external memory | ThinkPad, AMD Renoir/RADV | **0 imported, by design**: `DRM_FORMAT_MOD_INVALID` refused with the typed error, `LoadEnd` control passing |

So `wgpu-30` is now hardware-verified on every path, and the caveat published
in 0.10.0's README — that the demos could not exercise 28 or 30 end to end —
is **no longer true**: the demos were ported alongside the flip. It stays true
for `wgpu-28`, which nobody has run on hardware.

Worth stating for consumers rather than leaving in the commit log: **flipping
the default row is a breaking change for anyone taking default features.**
0.10.0 defaulted to 29 and 0.11.0 defaults to 30, so an unpinned consumer moves
a wgpu major without asking. The README now says so and gives the pin.

## Plan

### W: welding phases, in bite order

- **W1, truth pass. DONE 2026-08-10.** `probe()` now reports the three verified
  platforms, downgrades `downloads`/`context_menus` to explained `Unsupported`,
  and splits `popups` into `Partial` (creation handled, widget surfaces not).
  All three dead `NavigationEvent` variants gained emitters on all three
  platforms: `on_before_popup` (new life-span handler on Linux/macOS, existing
  one on Windows), `on_console_message` on the display handler, and
  `on_render_process_terminated` on a new request handler wired into each
  client. A unit test pins every `Supported` claim to a handler that exists, so
  a future feature must flip the status and the test together. The Linux and
  macOS demos now drain and log navigation events; the Windows demo already did.

  Residual, carried into W2: popup *policy* is hardcoded to deny. Per the
  configurability doctrine that belongs in `CefSurfaceConfig` once W2 can
  actually render a popup surface.
- **W2, popup widget surfaces. DONE 2026-08-10, with a platform caveat.**
  `on_popup_show` / `on_popup_size` feed a shared `popup::PopupState`, and
  `PET_POPUP` paints route to a separate slot instead of being dropped. The
  host reads `acquire_popup` (new surface) and `popup_rect` (still open),
  exposed as `PopupSurface` + `PopupRect`. All three demos draw it over the
  view with a viewport-clipped second pass.

  **Verified on Windows**: a scripted click on a `<select>` produced
  `imported popup 200x95 at 40,80`, correctly sized and placed under the
  control.

  **macOS does not deliver popup widgets through OSR at all.** With the same
  page and a click that demonstrably landed (the view repainted),
  `on_popup_show` and `on_popup_size` never fired. Chromium uses a native menu
  for `<select>` on macOS, and windowless rendering does not reroute it. So the
  macOS lane silently has no dropdowns, and no amount of welding-side work
  changes that. A host that needs them there has to draw its own control from
  the DOM. Linux is compile-verified only; no Linux box was reachable this
  session.

  **Correction 2026-08-12: the macOS negative does not generalise.** On
  Mayola's M4 iMac (macOS 26.5.1, arm64) the same select probe and a scripted
  click delivered the full popup path — `on_popup_show(true)`,
  `on_popup_size 320x197 at 0,80`, and a POPUP PASS import with real pixels.
  The claim above was true of macOS 15.7 on the Intel iMac and stands as its
  record; see the 2026-08-12 M4 progress entry for the variables in play.

  Deviation from the original plan: the composited single-texture mode is not
  built. Compositing must go to a third welding-owned texture (the imported
  view texture is CEF's memory and must not be written), so it costs an
  allocation and two copies per frame. The separate-surface API is the flexible
  zero-copy one and the demos show the recipe; compositing can be added later
  without breaking it.
- **W3, HiDPI. DONE 2026-08-10.** `CefSurfaceConfig::scale_factor` plus a live
  `set_scale_factor`, both feeding a shared `view::ViewMetrics` that owns the
  size/scale pair under one lock. `GetViewRect` now answers in DIP,
  `GetScreenInfo` reports the real factor, mouse input converts physical to
  DIP, and popup rects convert DIP back to physical so a host can use them as a
  viewport directly. The demos read `window.scale_factor()` and follow
  `ScaleFactorChanged`; `WELD_SCALE` forces a factor on a 1x screen.

  **Verified on Windows**, using W1's console channel to make the page report
  its own layout. Same 1280x800 window:

  | `WELD_SCALE` | page reports |
  | --- | --- |
  | 1 | `innerWidth=1280 innerHeight=800 dpr=1` |
  | 2 | `innerWidth=640 innerHeight=400 dpr=2` |

  Correction to how this was framed earlier: the symptom of the old hardcoded
  1.0 was not soft text. CEF was laying out at twice the CSS width, so content
  rendered at half its intended size, and every mouse coordinate was off by the
  scale factor. Sharpness was never the issue; size and hit-testing were.
- **W4, cursor + IME. LANDED 2026-08-10, partly verified.** `CursorShape`
  adopts scrying's vocabulary verbatim, including the crossed naming that the
  unit test pins: CEF's `POINTER` is the arrow (`CursorShape::Default`), while
  `CursorShape::Pointer` is the CSS link hand, CEF's `HAND`. `poll_cursor_shape`
  and `poll_ime_composition` follow scrying's polling shape. IME input is
  `ime_set_composition` / `ime_commit_text` / `ime_finish_composing` /
  `ime_cancel_composition`; feedback is the union of CEF's per-character bounds,
  converted DIP to physical, so a host can place a candidate window.

  API note worth keeping: `OnCursorChange` is on CEF's **display** handler, not
  the render handler, and its cursor-handle parameter is typed differently on
  each platform (`cef::sys::HCURSOR`, `*mut u8`, `c_ulong`). Both cost a
  compile round-trip.

  **Verified**: the callback fires and reaches the host end to end. With
  `RUST_LOG=welding=debug` the Windows demo logs
  `on_cursor_change(CursorType(CT_POINTER))` and then applies `Default` to the
  winit window.

  **Verified end to end on Linux, with a human at the machine (2026-08-10).**
  Mark moved a real pointer over a page that was a full-window `<a>` with
  `cursor:pointer`, and reported seeing the hand. The log agrees: 8 `Pointer`,
  16 `Text` (the link's text), 24 `Default`, while the page independently
  reported `tag=A cursor=pointer` under the pointer. CEF to welding's mapping to
  the host to an actual cursor on screen, all of it.

  Getting there took six rounds because **every earlier negative was my own test
  being wrong**, not the code:

  - round 1-2: the interaction was with window chrome (a title-bar double-click)
    and the GNOME overview, so nothing was ever over page content. The repaints
    I read as "input arrives" were the resize from maximising.
  - rounds 3-5: `href="#"` pages with no confirmed hover target, chasing
    `external_message_pump`, focus, and visibility as causes. All three were
    reverted or kept on their own merits, none of them fixed anything.
  - round 6: a page that reports `elementFromPoint` and its computed cursor
    settled it in one move.

  Two things this did establish for free. **Linux input is fully working**: a
  page with `mousemove`/`click` listeners logged 30 `PAGE-SAW` events, so
  winit to welding to CEF to the DOM is proven on Linux, not just Windows. And
  `set_visible` (CEF `WasHidden`) is now implemented on all three producers,
  which closes its own row in the matrix above; it was written as a fix
  candidate, did not fix anything, and is kept because it is a real feature.

  The lesson worth carrying: when a negative result depends on where a pointer
  is, make the page report what it thinks is under the pointer before believing
  the negative.

  IME is compile-only: exercising it needs a real input method.

  Also fixed in passing: `demo-weld-win` never initialised a logger, so every
  `log::` line welding emitted was invisible. That is why the first three
  diagnostic rounds showed nothing.
- **W5, cookies. DONE 2026-08-10.** `set_cookie(url, cookie)`,
  `request_cookies` / `poll_cookies`, `delete_cookies` over CEF's global cookie
  manager on all three producers. The synchronous `get_cookies_for_url` is gone
  because it could not be honoured: CEF delivers cookies through a visitor, and
  on Linux and macOS the calling thread is CEF's UI thread, so blocking waits on
  the loop that produces the answer. Proven on Windows, writing `weld_probe=w5`
  and reading it back.

  The bug worth remembering: an empty store never calls the visitor at all, so
  counting callbacks cannot detect completion and "none" is indistinguishable
  from "not yet". Completion now rides on the visitor being released.

- **W6, the renderer-side `CefApp`. DONE 2026-08-10.** Detailed under Findings
  above. Script results, Chromium command-line switches, and W1's
  `NewWindowRequested` residual, all of which were waiting on the same missing
  render-process handler.
- **W7, host-decision surfaces. IN PROGRESS.** Downloads **done 2026-08-13**,
  verified on all three (Windows, M4, ThinkPad): the same 28-byte probe file
  lands on disk with the right bytes, and with no `download_dir` configured
  zero download events fire at all.

  The shape follows scrying's, with one deliberate difference. scrying can ask
  a host handler per download; `welding` cannot, because CEF asks for the
  destination inside a callback that cancels the download without an answer,
  and on Linux and macOS that callback runs on the thread a host reply would
  travel back on — the same reason cookies are request-then-poll. So the
  directory is policy (`CefSurfaceConfig::download_dir`, `None` refuses) and
  the steering is afterwards, via `cancel_download` / `pause_download` /
  `resume_download`. Those are recorded and applied on the download's next
  update, because `CefDownloadItemCallback` is callback-scoped like the paint
  handles.

  No resume blob, unlike scrying on macOS: CEF offers live pause and resume on
  a running download and nothing that outlives the process, so
  `DownloadCancelled` carries no resume data and says so.

  Two things worth keeping: the server's suggested filename is
  attacker-influenced and only its final component is used, with a test pinning
  that `../../.bashrc` stays inside the download directory; and CEF updates an
  item *before* it asks where to put it, so the first run reported progress on
  a download the host had not been told existed.

  **W7b, auth challenges: wired, never fires.** `GetAuthCredentials` is
  registered on all three with the whole answer path behind it, and CEF has
  never been observed to call it. A probe inside the handler counted zero
  invocations against a top-level 401 that Chromium itself failed with
  `ERR_INVALID_AUTH_CREDENTIALS`, with and without `CEF_RUNTIME_STYLE_ALLOY`,
  while other methods on that same handler fire normally. Reported `Partial`
  with that reason. Proxy auth is untested and may work; CEF reports `is_proxy`
  separately. Unlike scrying there is no page/download source split — CEF has
  one challenge channel, so the field would have been a claim it cannot fill.

  **W7c, permission requests: done 2026-08-13**, verified on all three (grant
  and deny, with the page reporting Chromium's own decision:
  `notif:granted` / `notif:denied`, and on Windows `geo:denied:1` by default).

  Probed first, on W7b's lesson — a handler with nothing but a log line, to see
  whether CEF calls it at all before building an API on top. It does, through
  *two* callbacks that answer differently: the prompt wants accept/deny, media
  wants the subset of capture bits being granted. The host gets one id and one
  pair of verbs; the module remembers which kind it was. Off by default for the
  same reason as auth.

  The two bitmasks are separate enums that both use `1 << 0`, so they get
  separate decoders and a test pins every named bit distinct; unnamed bits reach
  the host as `Other` rather than vanishing.

  Two measurement traps worth remembering, both of which briefly looked like
  bugs: Chromium **persists a permission decision per origin**, so a grant run
  after a deny run silently reuses the stored answer unless the profile is
  cleared — and the profile is per demo, so clearing the wrong one proves
  nothing. And geolocation grant looks like failure on a machine with no
  location provider: the page gets error **2** (position unavailable), not 1
  (permission denied). `Notification.requestPermission()` is the better probe,
  because it resolves with the decision itself.

  **W7d, context menus: done 2026-08-13**, verified on all three. Probed first
  again, and CEF does call both `on_before_context_menu` and `run_context_menu`.
  Its own menu has nowhere to draw itself under windowless rendering, so it is
  emptied and claimed, and the host gets `ContextMenuRequested` with the
  hit-test details to draw its own; a host that ignores the event gets what it
  got before, which is nothing.

  Coordinates are converted on the way out. CEF reports DIP and this API is
  physical throughout, so a right-click sent to physical (223,210) returns
  (224,210) — an odd physical coordinate loses half a DIP through the
  round-trip. macOS reports one extra target, `Selection`, because a right-click
  there selects the word under the cursor first.

  Aiming the click by hand wasted a round and briefly looked like a bug: the
  first attempt guessed at where the link was, missed, and picked up *stray real
  right-clicks* on the demo window (six events from one scripted click, at
  coordinates never sent). Asking the page for its own link rect, as the IME and
  cursor tests already did, gave one event and an exact answer.

  **W7 is done**, with auth the honest exception: three of the four rows are
  verified on all three platforms, and `GetAuthCredentials` is wired but never
  called by CEF. Done when: those capability rows read Y with the same event/decision
  shapes scrying uses.
- **W8, long tail. CODE COMPLETE 2026-08-13.** Find-in-page, zoom, and
  `can_go_back`/`can_go_forward` are in and verified on all three platforms.
  A page built with exactly seven matches reports
  `count: 7`, progressively, with `final_update` on the last.

  Zoom needed a control to believe. Reading `zoom_level()` straight after two
  zoom-ins returned 0.0, which reads like nothing happened — but CEF documents
  `GetZoomLevel` as UI-thread-only and Windows runs CEF's UI thread separately,
  so the *read* was invalid, not the zoom. Confirmed from the page instead: the
  viewport went 640 -> 512 CSS px on Windows, which is Chromium's two-notch
  125%. On Linux the first measurement was confounded by the window manager
  resizing the window mid-run, so it needed a no-zoom control in the same
  session: 546 unzoomed against 390 zoomed. Third time this week a UI-thread-only
  getter has produced a plausible wrong number.

  **Print to PDF and the user agent landed too**, verified on all three
  platforms: `PdfPrintFinished { ok: true }` with a real file on disk (19162
  bytes beginning `%PDF-1.4` on Windows, 8784 on Linux, and 9636 on both Macs),
  and `WeldProbe/1.0` showing up in `navigator.userAgent` where Chrome's
  product token would be.

  The user agent went on `CefRuntimeConfig`, not `CefSurfaceConfig`, because
  CEF takes it in `CefSettings`: it is process-wide and every producer under one
  runtime shares it. Putting it on the surface config would have implied a
  per-browser knob that does not exist.

  **The remaining macOS W8 core receipts landed 2026-08-14.** Both the Intel
  iMac and M4 iMac returned the terminal `FindResult { count: 7,
  final_update: true }`; their viewport titles changed from 640 to 512 CSS px
  after two zoom steps; `can_go_back=true` after a same-document history entry;
  and each wrote a 9636-byte `%PDF-1.4` file. The optional `WELD_RECEIPT` path
  records these events for Launch Services runs, where macOS discards ordinary
  stdout. It also captured the exact product-token replacement in the UA.

  **W8 tail implementation landed 2026-08-13.** Windows and the ThinkPad have
  executable receipts: a direct `TouchInput` produced `TOUCH:touchstart:1`
  then `TOUCH:touchend:1`; a staged Enter, Over, Drop produced
  `DROP:1:weld_drag_touch_probe.html`; a page source drag produced
  `PAGE_DRAG:started` then `DragStarted`; `Page.captureScreenshot` wrote a
  PNG with the eight-byte PNG signature (31606 bytes on Windows; 12892 bytes
  on ThinkPad); and a persistent producer created its own cache tree below the
  runtime root, including `Cookies` on ThinkPad. The profile boundary also
  changed the cookie helpers from CEF's global manager to the browser's own
  request context, so the public cookie API cannot leak across profiles.

  **The same tail is now hardware-verified on both Macs, 2026-08-14.** The
  Intel iMac (macOS 15.7.7, 1x) and M4 iMac (macOS 26.5.1, native 2x) each ran
  the ordered touch, file-drop, and page-drag battery against the probe page.
  Both post-gesture screenshots visibly end at `PAGE_DRAG:started`, proving
  the page-originated half after the earlier host inputs; they are valid PNGs
  (11791 and 21990 bytes respectively) and each child profile wrote `Cookies`.
  `WELD_SNAPSHOT_AFTER_SCRIPTED=1` makes that timing explicit for unattended
  evidence instead of taking the normal early compositor snapshot.

  The ThinkPad exposed a setup ordering rule. A disk-backed child
  `RequestContext` is initialized through CEF's host loop; creating its browser
  immediately made `browser_host_create_browser_sync` return `None`, despite
  CEF documenting child paths as valid. Pumping the host loop once after the
  context is created and before creating the browser completes that setup. The
  same persistent child-profile run then created, loaded, and wrote its profile.

  macOS has the same asynchronous context boundary, but a different safe pump
  point. `CefDoMessageLoopWork` inside winit's `resumed` callback re-enters
  AppKit and aborts. `prepare_profile` therefore creates the context and keeps
  its CEF readiness handler alive; the host pumps outside winit, then
  `try_new_with_prepared_profile` returns `None` until CEF calls that handler.
  Only then may synchronous browser creation run. Both Intel and Apple Silicon
  receipts passed through that path.

  Snapshot capture is intentionally asynchronous and private to the producer:
  `request_snapshot_png` starts Chromium `Page.captureScreenshot` and returns
  a typed request ID; `poll_snapshot_png` supplies that same ID with validated
  PNG bytes or a capture error. The channel admits at most 16 requests across
  waiting and completed slots, rejects a new request at that bound, and never
  evicts an admitted completion before polling. Drag keeps the two system
  ownership directions distinct: `DragInput` is host-to-page, while a
  page-originated `DragStarted` event gives the host a copied payload for its
  native drag loop and `finish_drag_source` closes it. Touch preserves contact
  IDs and phases rather than translating touches into mouse input.

  The system-printer boundary is split honestly. On Windows, `print()` opened
  CEF's native chooser and exposed the installed printer queues on 2026-08-13.
  On 2026-08-14, `WELD_PRINT=1` selected the ready `Brother HL-3170CDW series
  Printer` and submitted one W8 probe page. Windows retained job 2 as
  `Complete` with one of one pages printed (20,739 bytes), and PrintService
  event 307 confirms physical output through its WSD port. macOS calls the
  same CEF-owned native-dialog path but has no current hardware receipt.
  Linux CEF has no built-in dialog:
  it requires a complete embedder `CefPrintHandler` with printer UI and
  spooler, so welding returns an explicit unsupported error there rather than
  selecting a default printer or calling `lp`. Drag, touch, snapshot, and
  profile are hardware-verified on all three platforms; macOS printer output
  remains unmeasured.
  (`WasHidden`-backed visibility landed early, during W4.)
- **W9, CDP. DONE 2026-08-13**, verified on all three (Windows, M4, ThinkPad):
  `Browser.getVersion` returns a real protocol result
  (`{"protocolVersion":"1.3","product":"Chrome/147.0.7727.138",...}`) and
  `Page.enable` subscribes.

  Deliberately **not** a typed API. CDP is revised every Chromium release, so a
  wrapper would age badly and constrain hosts to whatever subset was modelled;
  the wire format passes through instead — JSON in via `send_devtools_message`,
  JSON out via `poll_devtools_message` — and any existing CDP client can drive
  it. `SendDevToolsMessage` takes raw bytes, so nothing has to be converted
  through `CefDictionaryValue` either.

  Opt-in via `CefSurfaceConfig::devtools_protocol`, because one `Page.enable`
  produces a steady stream whether or not anything reads it. The queue is
  bounded at 512 and counts drops; `devtools_dropped()` makes gaps visible
  rather than silent. Subscription is lazy because the browser is created
  asynchronously, and the registration lives on the producer because dropping
  it unsubscribes.

  Two things worth recording. `ExecuteDevToolsMethod` returned **0** on
  Windows, which reads like failure and is not: CEF documents the id as
  returned only when called on the UI thread, and Windows runs CEF's UI thread
  separately. And every run ends with an `Inspector.targetCrashed` event, which
  is the test's own `timeout` — it arrives immediately after welding's
  `ContentProcessTerminated { error_code: 143 }`, SIGTERM, and frames continued
  66 before the call and 1541 after, so attaching CDP does not disturb the page.
  The two independent views of the same shutdown agreeing is a decent check on
  both.

### S: scrying items (both directions)

- **S1. Done 2026-08-31.** Pin the reviewed Graft interop ABI exactly.
- **S2.** Its own matrix's unverified cells, in its order: win/mac IME
  observability, touch coverage, wk6 native input. Tracked in scry's docs;
  listed here only so the family view is complete.
- **S3. Done 2026-08-31.** DMABUF/Metal resource registration moved to Graft;
  WPE semaphore/modifier policy and SCK/WGC capture plumbing remain in Scry.
- **S4.** WebView2 CDP exposure, mirroring W8's shape.

### G: grafting items

- **G1. Done 2026-08-31.** Graft's wgpu 30 import registers the acquired
  `RESOURCE` state; older rows return a typed refusal.
- **G2.** `servo-wgpu-interop-adapter` selects grafting features without a
  wgpu major of its own default; it compiles today because a sibling demo
  unifies one in. Make the selection explicit.
- **G3.** Offer the sync seam to welding: either welding adopts a
  `Dx12FenceSynchronizer` path or documents why the callback-time copy makes
  implicit sync correct for CEF. Investigation, not a commitment.

### Sequencing

W1 and S1 are immediate and independent. W2+W3 before anything else in W;
they are what every embedder hits first. W4 through W7 in listed order,
re-verifying on the iMac and the Fedora box per phase (the readback-verdict
pattern from demo-weld-mac generalizes). G1 whenever, it gates nothing local.

## Progress

- 2026-09-04: **Graft owned-frame adaptation.** The implementation candidate
  first pinned Graft commit `d671c9681332751f8ea24dd974511b81fe6a05dd`;
  commit `c65a44e` switched the release to published `grafting` 0.6.0. Weld
  consumes Graft native frames by value. DX12 stores the weld-owned Win32
  handle as `OwnedHandle` and moves
  it exactly once into `Dx12SharedResource`. Metal moves the retained
  `MTLTexture` into Graft's `MetalTextureRef`. Linux validates each plane fd,
  maps repeated raw fd
  numbers before constructing `OwnedFd`, deduplicates distinct dup fds by kernel
  identity, and builds Graft's buffer table plus per-plane buffer indices before
  constructing `VulkanDmaBufImport`. Constructor/import failure paths close
  descriptors through RAII. This supersedes the old manual-close-after-import
  wording. The public raw handle/fd fields are private now; raw-owned entry
  points are explicitly unsafe and metadata is exposed through getters. Fresh
  DX12, Metal, and Vulkan hardware receipts are still required before
  publication.

- 2026-09-04: **pre-release hardening source slice.** The public
  `CefSurfaceProducer` trait no longer carries `Send`, matching the Linux and
  macOS producers' CEF UI-thread ownership; the Windows producer keeps its
  platform-specific `unsafe impl Send` with the existing CEF proxying
  rationale. `CefRuntimeConfig::new` now requires
  `CefSandboxMode::UnsandboxedTrustedContent`, and every subprocess entry point
  takes the same sandbox choice before passing null `sandbox_info`; the enum
  has no `Default`, so the current no-sandbox posture is not an invisible
  default. `CefSurfaceCapabilities::probe()` now reports CEF-backed browser
  features as unsupported when `cef-runtime` is not compiled in, matching the
  constructors. The `welding` crate declares
  `rust-version = "1.97.1"`, backed by the checked-in `rust-toolchain.toml` and
  the local validation toolchain. Local verification for this final API shape
  is limited to fast formatting, diff hygiene, and stale-pattern searches. A
  pre-correction `cargo test -p welding` passed 48 unit tests plus 2 doctests,
  and the first final-shape `cef-runtime` check caught and fixed the generated
  CEF binding's `*mut u8` `sandbox_info` type, but the full compile/test matrix
  is deferred to CI/hardware because of concurrent Cargo load. This first
  source slice deliberately did not adapt to the pending Graft ownership API;
  the later ownership slice above does. It invalidates the prior headed
  receipts and needs CI plus DX12/Metal/Vulkan hardware reruns before
  publication.

- 2026-08-31: **the triplet now has one native import boundary.** Graft commit
  `8106f7c6b16838eb9ec062f0293249b39c108907` owns the D3D12, Metal, and
  DMABUF/Vulkan wrappers, including fd closure, shared-buffer plane handling,
  compatible memory-type intersection, and foreign-queue acquisition. Scry
  commit `01e411ef35237d4c562dd311ace68977a5b9c78f` pins it and closes the
  WPE/RADV hard pixel gate at 4096/4096 expected pixels. Weld pins the same rev,
  retains only CEF policy, and adds required CEF demo builds on all three hosted
  operating systems. The library compiles with warnings denied on Windows,
  Apple Silicon, and AMD/RADV Linux across wgpu 28, 29, and 30.

  The corrected fresh-profile Linux run loaded Example Domain with HTTP 200,
  imported two CEF frames, and read back 16384/16384 non-zero bytes with
  `[238, 238, 238, 255]` first pixels. Khronos validation emitted no VUID. The
  demo now supplies the two first-run-suppression switches itself, so a new
  cache cannot stop inside Chromium's EULA before winit starts.

- 2026-08-30: **the published welding 0.13.0 release is green on the wgpu
  28/29/30 matrix, with current headed Vulkan and Metal imports.** Commit
  `2989de9088cef3de86c5e99c3c2cb6738b306e40` locks the complete wgpu 30
  family to 30.0.1 and CEF to `151.8.0+151.3.24`. GitHub Actions run
  [33345042921](https://github.com/merely-made/wgpu-weld/actions/runs/33345042921)
  passed rustfmt plus all nine Windows/Linux/macOS feature rows; the default
  wgpu 30 row also passed tests on each OS. Local isolated tests passed 48/48
  on each wgpu row, with 2 doc tests passed and 13 ignored per row.

  The headed run used that exact detached commit on the Fedora 44 ThinkPad,
  through the live Xwayland display (`DISPLAY=:0`), rustc 1.97.1, CEF 151.8,
  wgpu 30.0.1, and AMD Radeon Graphics / RADV Renoir (Mesa 26.1.8). CEF loaded
  `https://example.com/` with HTTP 200; welding reported the Vulkan backend
  and DMABUF import support, then imported 1280x701 and 1366x701
  `Bgra8UnormSrgb` frames. The validation verdict was 16384/16384 sampled
  bytes non-zero. The 1366x701 PPM receipt is 2,872,714 bytes with SHA-256
  `93817a42558bc0149356a37b77dba7d78f87abaf695edb7e8a6b948da23c7147`;
  visual inspection shows the rendered Example Domain heading, body, and link.

  The first link attempt correctly failed when `CEF_PATH` named the older CEF
  distribution left at `target/debug`; the 151.8 Rust bindings could not
  resolve their symbols from that library. Pointing both `CEF_PATH` and
  `LD_LIBRARY_PATH` at the 151.8 distribution downloaded by this build made
  the headed run pass. This confirms the documented exact-version boundary.

  The Metal rerun subsequently passed from fresh isolated builds of the
  published `welding-v0.13.0` tag (`e8657577703be8dabd9bf49049c5d7ef0a9e5361`)
  on both saved Macs: the M4 iMac on macOS 26.5.1/arm64 and the Intel iMac on
  macOS 15.7.7/x86_64. Each used rustc 1.97.1, CEF 151.8.0+151.3.24, and the
  complete wgpu 30.0.1 family. CEF loaded `https://example.com/` with HTTP 200;
  each run imported one 1280x800 `Bgra8Unorm` IOSurface frame through Metal,
  sampled 16384/16384 non-zero bytes with the expected `#EEEEEE` pixels, printed
  `VALIDATION PASS`, and exited 0. The durable logs are
  `/Users/markik/Code/worktrees/wgpu-weld-0.13.0-metal-m4/metal-m4-0.13.0-validation.log`
  and
  `/Users/markik/Code/worktrees/wgpu-weld-0.13.0-metal-intel/metal-intel-0.13.0-validation.log`
  on their respective machines.

- 2026-08-30: **snapshot correlation landed in source.** PNG snapshot requests
  now return `SnapshotRequestId`; polling returns `SnapshotPngCompletion` with
  the identical ID and `Result<Vec<u8>, WeldError>`. The shared channel's
  16-slot bound covers both pending requests and unpolled completions, so a
  host receives a visible admission error rather than silently losing a
  completed capture. Windows, Linux, and macOS forward the CEF message ID
  through this same contract. Focused CEF tests passed (3/3) and the Windows
  demo compiled as a public caller; this is source and compile coverage, not a
  new three-platform runtime screenshot receipt.

- 2026-08-10: surfaces of all three repos read; matrix and phases drafted.
  Same-day context: module split, macOS Phase 3 closed end to end, grafting
  0.4.0 + welding 0.1.0 published (see 2026-05-14_cef_accelerated_osr_plan.md).

- 2026-08-10: **W1 landed.** Compile-verified on Windows (`cef-runtime` via
  demo-weld-win), Linux (`cef-runtime` cross-target), and macOS (`cef-runtime`
  on the Intel iMac); unit and doc tests green.

  Runtime evidence is uneven, and the gap is worth stating rather than
  implying:

  - `ConsoleMessage` is **proven**. A `data:` page calling `console.log`
    produced `ConsoleMessage { level: 2, message: "weld-w1-console-probe", ... }`
    on the iMac.
  - `NewWindowRequested` is **wired but unproven**. Chromium's popup blocker
    suppresses a non-gesture `window.open` *before* `on_before_popup` is
    reached, so the obvious test can't reach the handler. Proving it needs
    either a real user gesture (a scripted click through `send_mouse_input`)
    or `--disable-popup-blocking` via `OnBeforeCommandLineProcessing`, which
    welding does not expose yet. That switch is itself a reasonable W7 item.
  - `ContentProcessTerminated` is **wired but unproven**. Killing the demo's
    own renderer helper produced no event within ~20s. The wiring was
    re-checked afterwards against the bindings (`ImplClient::request_handler`
    is the correct getter and is implemented), so this reads as an
    inconclusive test rather than a known defect. Needs a deliberate
    crash-recovery scenario to settle. **Settled 2026-08-12 on the M4 iMac:
    it emits.** See that progress entry, which also records the post-crash
    hang the same test surfaced.

  Method note: the first attempt at that crash test used
  `pgrep -f "Helper \(Renderer\)" | head -1`, which matches every Electron app
  on the machine and takes the lowest PID. It killed an unrelated app's
  renderer. Scope process matching to the bundle path under test.

- 2026-08-10: **W2 landed.** See the phase entry above for the Windows proof
  and the macOS structural gap.

  The scripted click (`WELD_CLICK_AT=x,y`, in the Windows and macOS demos) is
  the reusable instrument here. Anything gated on a real user gesture was
  previously unprovable without a human at the keyboard, which covers both
  `<select>` dropdowns and W1's `NewWindowRequested`. Retrying that W1 residual
  through a gesture-driven `window.open` is now cheap and should ride along
  with the next Windows run.

  **Linux ran for the first time (2026-08-10), on the Fedora 44 ThinkPad
  (AMD Renoir / RADV).** CEF initializes, example.com loads, and the Vulkan
  backend is detected, but no frame imports: CEF hands over
  `DRM_FORMAT_MOD_INVALID` for the DMABUF modifier, and welding fed that
  straight into `VkImageDrmFormatModifierExplicitCreateInfoEXT`. vkCreateImage
  answered `VK_ERROR_FORMAT_NOT_SUPPORTED` and the *validation layer* then
  aborted the process while formatting its own error. welding now rejects the
  invalid modifier with a typed `ImportError` naming the situation, so the
  failure is attributable instead of a core dump.

  That explains the Intel-versus-AMD split: the plan's original Linux
  verification was on Intel/Mesa, which supplies a real modifier. The proper
  fix is a linear-tiling import path carrying the plane stride, which needs a
  machine where the resulting pixels can be checked, so it is a follow-up
  rather than a guess. Until then, Linux + AMD is a documented no-import.

  Two method notes from that session. Reaching the ThinkPad's display over SSH
  needs `DISPLAY=:0` **plus** `XAUTHORITY=/run/user/1000/.mutter-Xwaylandauth.*`
  (read it off the running Xwayland's `-auth` argument); without the cookie CEF
  reports only "Missing X server or $DISPLAY". And a hypothesis about CEF not
  finding `icudtl.dat` / `locales/` was wrong: an A/B with the setting removed
  showed CEF resolves those from the `libcef.so` directory via
  `LD_LIBRARY_PATH`, so no `resources_dir_path` is required and the speculative
  fix was reverted.

  Also of note: running the Windows demo needs no separate CEF download.
  `cef-dll-sys` already fetched one, so
  `CEF_PATH=target/debug/build/cef-dll-sys-*/out/cef_windows_x86_64` is enough
  to launch it.

- 2026-08-12: **The Apple Silicon gap is closed: the battery is green on
  Mayola's M4 iMac** (Apple M4, macOS 26.5.1, Metal 4, native 2x display;
  arm64 CEF 148 / Chromium 147; first arm64 build of welding). Frames, wheel
  and key to the DOM, cursor shape, cookies, script results, command-line
  switches, navigation and title events: all verified in one run. Build note:
  `cef-dll-sys` fetched `cef_macos_aarch64` on its own, but the sandbox
  wrapper needs `cmake` and `ninja`, which this machine did not have —
  `brew install cmake ninja` and the bundle built first try.

  Three findings, each of which corrects the record:

  1. **HiDPI is now verified on real 2x hardware.** Every earlier HiDPI proof
     forced `WELD_SCALE=2` on a 1x panel. On the M4's native 2x display with
     no override, the page reported `dpr=2, innerWidth=640, innerHeight=400`
     in a 1280x800 physical window, and the popup rect came back converted:
     CEF said `320x197 at 0,80` DIP, the import was `640x394 at 0,160`
     physical.

  2. **macOS popup widgets work here, which overturns W2's "structurally
     absent".** A scripted click on the select probe fired
     `on_popup_show(true)`, `on_popup_size 320x197 at 0,80`, and the popup
     surface imported with real pixels (POPUP PASS, 14323/16384 non-zero).
     The 2026-08-10 negative was macOS 15.7, Intel, 1x; this run is macOS
     26.5.1, arm64, 2x, same crate code and CEF major. Which variable flips
     the behaviour is not knowable from this machine alone. Until the Intel
     iMac is retested, the honest matrix cell is "differs by macOS
     generation", not "impossible" — and a host that needs dropdowns on
     old-macOS still needs the DOM fallback.

  3. **The battery's VALIDATION verdict was lying on its own probe pages, on
     every platform.** welding never sets CEF's windowless
     `background_color`, so CEF's default (transparent) applies, and a page
     that declares no CSS background renders transparent — `[0,0,0,0]`,
     which the probe reads as "not carrying paint". Both `testing/` pages
     declared no background. The diagnosis was differential: example.com
     (whose stylesheet sets `#eee`) passed probed immediately *and* after
     being held 12s; the input probe failed even probed at import; a magenta
     `data:` URL passed. So: not an import failure, not surface staleness —
     an unset background. Both probe pages now declare `background:#fff`,
     and the re-run is green end to end (16384/16384 non-zero, white). The
     Windows and Linux 2026-08-12 battery verdicts ran the same pages and
     should be re-read with this in mind. Follow-ups: expose
     `background_color` in `CefSurfaceConfig` (configurability doctrine),
     and document that pages without a background import with premultiplied
     transparency today. A side benefit of the 12s-hold control: a held
     IOSurface frame does not rot — CEF does not recycle it under the host.

  **W1's last unproven variant is proven: `ContentProcessTerminated` emits.**
  Killing the renderer helper produced
  `weld: CEF render process terminated (code 9, 9)` and the event reached
  the host. The wiring was always right; the 2026-08-10 test was
  inconclusive for test reasons.

  **The aftermath looked like a new defect, and was not — retracted later
  the same day.** The "hang past `WELD_TIMEOUT_SECS`" was the test's own
  doing, in the same family as "a demo mistook a paint for a clock": the
  timeout only armed in unattended modes, and that crash test ran the demo
  interactively, so "runs forever" was the demo working as designed. The
  held singleton and the orphaned helpers were side effects of SIGKILLing a
  healthy process. The rerun that would have shown this was then corrupted
  by a second, real finding: on a fresh profile Chromium's Safe Storage
  asks the login keychain for a password, a modal prompt that blocks
  `CefInitialize` for as long as nobody answers — and answering Deny
  crashes the network service ("Network service crashed or was terminated,
  restarting service."), after which no page loads. A parity demo on a
  machine nobody sits at cannot type a password, so the macOS demo now
  passes `use-mock-keychain` unconditionally, and an explicit
  `WELD_TIMEOUT_SECS` arms the timeout in any mode.

  The clean re-run settles it: renderer killed mid-run,
  `ContentProcessTerminated` delivered, the loop kept ticking, the timeout
  fired, the report ran (the held pre-crash frame still probed real pixels),
  exit code 0, zero leftover processes. Post-crash on macOS is "event
  delivered, host loop fine"; the matrix row stays P only because there is
  no relaunch/recovery API, not because anything breaks.

  Drift note for the next demo pass: `WELD_TIMEOUT_SECS` exists only in the
  macOS demo; Windows and Linux have no timeout instrument at all. Both
  crash-test runs also navigated themselves to iana.org within seconds of
  launch — a click on the freshly-focused window sitting under a live
  pointer is the suspected cause (a human was at the machine both times);
  noted, not chased.

- 2026-08-12, later still: **the Intel iMac retest, over SSH.** With the
  key authorized the whole loop ran remotely — wake the machine by pinging
  `192.168.4.105`, ssh in, pull, rebuild, run — no human at that keyboard.

  - **The W2 popup negative is real, and reconfirmed.** The identical
    select probe, scripted click and `welding=debug` logging that produce
    the full popup path on the M4 produce *no* `on_popup_show` on macOS
    15.7.7/Intel, while the view renders and passes validation. Same crate
    code, same CEF 148, same day: the split is a stack difference, not a
    bad test. macOS generation remains the likely variable; Intel-vs-arm64
    stays conflated because nothing here runs macOS 26 on Intel.
  - **The Intel battery is green with the fixed probe pages** — cookies,
    script results, wheel and key to the DOM, cursor, and a VALIDATION
    PASS these pages could not produce before the background fix.
  - `use-mock-keychain` earned its keep immediately: over SSH the login
    keychain is locked, which would otherwise have been the
    blocked-`CefInitialize` failure from the keychain finding, invisible
    at the far end of an ssh session.

  Correction to the M4 entry above: "every earlier HiDPI proof forced
  `WELD_SCALE`" overstated. The Intel iMac reports `dpr=2, innerWidth=640`
  with no `WELD_SCALE` set — it is itself a native 2x panel, so the
  2026-08-12 battery there already exercised real HiDPI. What the M4 adds
  is arm64, macOS 26 and Metal 4, not the first 2x hardware.

  Method notes, in the spirit of the existing pgrep one:

  - `pkill -f` patterns are regexes: `Helper (Renderer)` matches nothing,
    because the parens group instead of matching. Bracket them
    (`[(]Renderer[)]`).
  - Scoping the kill to the absolute bundle path missed the *main* process,
    which had been launched as `./demo-weld-mac.app/...` and so carries a
    relative argv[0]. The first "main is gone, only helpers linger" read
    was wrong because of this; the hung main was alive the whole time,
    respawning what the kills removed. Launch by absolute path, or write
    patterns that survive both.
  - A battery on a machine with a human at it picks up the human: a stray
    terminal Return keyup became `key:Unidentified` right after load, and a
    mid-run click on the demo window navigated it to iana.org. The
    accidental upside: real, non-synthetic mouse input is hereby also
    proven on this stack.

  Doc correction queued by reading the code: the top-level README's
  constraint 2 ("welding copies inside the callback and exposes only owned
  resources") is not what the macOS lane does — it `CFRetain`s the
  IOSurface and wraps it zero-copy into an `MTLTexture`
  (`native_frame/metal.rs`). No misbehaviour observed from the aliasing
  (see the 12s hold above), but the claim should match the code, and G3's
  sync question now has a second concrete instance.

- 2026-08-12, later: **`background_color` landed (welding 0.5.0).**
  `CefSurfaceConfig::transparent` turned out to be declared, documented,
  defaulted to `false`, and wired to nothing — which is how every page
  without a CSS background silently rendered transparent. It is replaced by
  `background_color: Option<[u8; 3]>` — `Some(rgb)` opaque, `None`
  transparent (CEF has no partial alpha), default opaque white — fed to
  `CefBrowserSettings.background_color` on all three producers, with the
  ARGB mapping pinned by a unit test. Field removal is the semver break
  behind the 0.5.0 bump; not yet published. Verified on the M4 with a
  background-less `data:` page: unset probes white where it probed
  `[0,0,0,0]` before, `WELD_BACKGROUND=transparent` keeps the old behaviour
  on request, `WELD_BACKGROUND=ff0000` probes BGRA red. All three demos
  share the `WELD_BACKGROUND` knob.
