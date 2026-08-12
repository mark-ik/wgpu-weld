# Producer Parity Plan: welding / scrying / grafting

**Date:** 2026-08-10
**Status:** Findings complete (all three surfaces read this session); work phases not started
**Scope:** cross-repo. This doc lives in wgpu-weld because welding carries most
of the gap list, but it assigns work to all three siblings.

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

`welding` 0.1.0 has a solid control core (navigation, input basics, cookies
in the trait, script with results, web messages, devtools, focus, close) and
the strongest verified rendering story (all three platforms hardware-verified
today), but the rendering-completeness and host-feedback surface is thin.

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
| GPU frame import | Y (3 platforms, hw-verified) | Y | Y |
| CPU fallback tier | P (feature-gated, never runtime-exercised) | Y | Y (readback demos) |
| Explicit sync seam | N (implicit: callback copy + keyed mutex + cache-flush) | P (in-tree sync modules) | Y (owner) |
| HiDPI scale factor | N (hardcoded 1.0, all three producers) | unverified, per-backend | o (host concern) |
| Popup widget surfaces (select/autocomplete) | Y on Windows, structurally absent on macOS, unverified on Linux | o (webviews self-composite) | o (Servo self-composites) |
| Navigation control | Y | Y | demo-level |
| can_go_back / can_go_forward | N | Y | o |
| Navigation events | P (load/title/address wired; 3 declared variants never fire) | Y (18 variants) | o |
| Mouse / keyboard / wheel | Y (Win+Linux verified; mac thinner) | Y (caveats per backend) | demo-level |
| Pointer (pen) input | N | Y | N |
| Touch input | N (CEF has SendTouchEvent) | unverified | N |
| Cursor-shape reporting | N (CEF has OnCursorChange) | Y (all 5) | N |
| IME | N (CEF has ImeSetComposition + range callbacks) | Y (GTK/WPE lanes; win/mac unverified) | N |
| Drag / drop | N (CEF has StartDragging + DragTarget*) | mixed | N |
| Cookies get/set/delete | P (trait methods exist; producer impls absent, defaults error) | Y | o |
| Cookie change events | N | Y (WKWebView) | o |
| Script exec + result | Y / P (result bridge is trait-default error) | Y | o |
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
| Visibility (WasHidden / set_visible) | N | Y | o |
| Per-producer profile isolation | P (global root_cache_path only; CEF has RequestContext) | Y (per-producer data_dir) | o |
| Render-process crash recovery | N (variant declared, never emitted) | P | o |
| DevTools window | Y | Y | o |
| DevTools protocol (CDP) | N (CEF has ExecuteDevToolsMethod; unique leverage) | N (WebView2 could; not exposed) | o |
| Multi-producer per process | Y | Y (except WPE) | Y |
| Honest capability probe | N (see below) | Y (matrix + footnotes) | o |

### The capability probe lies in both directions

`CefSurfaceCapabilities::probe` (surface.rs) is stale relative to today's
state and was never fully honest:

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

Per the diagnostics doctrine, a capability report that overstates is worse
than a missing feature; this is the first thing to fix.

### Structural findings

1. **scrying still carries a full in-tree `native_frame`** (dmabuf 796 lines,
   metal, sync_dx12) while already delegating the DX12 open-shared step to
   grafting. welding made the same delegation this session. The remaining
   duplicated import paths (DMABUF/Vulkan, IOSurface/Metal) are candidates to
   converge on grafting 0.4.0, which now carries the fixed objc2 Metal path.
2. **scrying's grafting dep is still `git branch=main`**, the same shape that
   blocked welding's publish until today. grafting 0.4.0 is on crates.io, so
   the swap is now a one-liner, and it unblocks any future scrying publish.
3. **The wgpu external-texture UNDEFINED-layout gap** (scry's WPE DCC
   footnote: `create_texture_from_hal` tracks imports as UNDEFINED, first-use
   barrier may discard) affects every Vulkan import in the family. One
   upstream fix serves all three; grafting owns the tracking issue since it
   owns the import core.
4. **Vocabulary drift**: `CursorShape`, focus reasons, key/mouse event
   shapes, and `NavigationEvent` naming differ between welding and scrying
   for no load-bearing reason. Convergence is cheap now and expensive later.
   Per the module doctrine this is alignment by convention, not a new shared
   crate.

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
- **W4, cursor + IME.** `OnCursorChange` to a polled cursor shape (adopt
  scrying's `CursorShape` vocabulary); IME composition in
  (`ImeSetComposition`/`ImeCommitText`) and composition-range feedback out.
  Done when: I-beam shows over text in the demos and CJK input composes on
  Windows and Linux.
- **W5, cookies + script result truth.** Implement the already-declared trait
  methods in the producers (CEF cookie manager, result-bearing script bridge),
  then flip the probe statuses. Done when: trait defaults no longer error on
  any shipped producer.
- **W6, host-decision surfaces.** Downloads (lifecycle events + decision API,
  matching scrying's pause/resume/cancel shape), `GetAuthCredentials`,
  permission requests, context-menu events. Done when: the capability rows
  read Y with the same event/decision shapes scrying uses.
- **W7, long tail.** Drag/drop, touch, find-in-page, PDF, zoom/UA/settings,
  `WasHidden`-backed visibility, per-producer `RequestContext` profiles,
  `can_go_back`/`can_go_forward`, snapshot helper.
- **W8, CDP.** Expose `ExecuteDevToolsMethod` + the devtools message stream.
  Nothing else in the family can offer full CDP; this is the CEF lane's
  distinguishing feature, worth doing once the table stakes above exist.

### S: scrying items (both directions)

- **S1.** Swap the grafting dep to crates.io `0.4`.
- **S2.** Its own matrix's unverified cells, in its order: win/mac IME
  observability, touch coverage, wk6 native input. Tracked in scry's docs;
  listed here only so the family view is complete.
- **S3.** Evaluate migrating in-tree `native_frame` DMABUF/Metal import paths
  onto grafting; keep in-tree only what is genuinely capture-specific (WGC,
  SCK plumbing). Outcome may legitimately be "keep", but decide on a read,
  not by default.
- **S4.** WebView2 CDP exposure, mirroring W8's shape.

### G: grafting items

- **G1.** File/track the upstream wgpu initial-layout issue for imported
  textures; link scry's DCC footnote and welding's Vulkan path to it.
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
    crash-recovery scenario to settle.

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

  Also of note: running the Windows demo needs no separate CEF download.
  `cef-dll-sys` already fetched one, so
  `CEF_PATH=target/debug/build/cef-dll-sys-*/out/cef_windows_x86_64` is enough
  to launch it.
