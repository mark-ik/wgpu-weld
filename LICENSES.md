# Licenses in this repository

**This repository: MPL-2.0.** Every file Mark wrote carries Exhibit A and the
SPDX tag `MPL-2.0`, per the
[license posture brief](../mere/design_docs/2026-08-22_license_posture_brief.md)
of 2026-08-22 (mere `design_docs/2026-08-22_license_posture_brief.md`). The
full text is in [`LICENSE`](LICENSE), and [`NOTICE`](NOTICE) records that this
workspace, unlike its siblings, owes no upstream attribution.

This file is the provenance ledger. It is the authority for what the relicense
tool (mere `scripts/relicense_headers.py`) skips: the backtick-quoted paths in
the **Retained licenses** table are never touched. Provenance comes before
license — a file gets Exhibit A only if Mark wrote it.

## What this repository is made of

**None of it is retained: 0 of 80 tracked files, 0 of 48 tracked source
files.** wgpu-weld vendors nothing and derives from nothing. All four crates —
`welding` and the three `demo-weld-*` members — are Mark's, and every manifest
already says `MPL-2.0` or inherits it from the workspace. The provenance grep
for `Copyright`, `Licensed under`, `Permission is hereby granted`,
`Apache License` and `SPDX-License-Identifier` finds nothing in any source
file; its only hits are `LICENSE`, which quotes the license text, and
`NOTICE`, whose whole content is Mark's own line plus the statement that
wgpu-weld "shares no Slint-derived code; it is original work" — the point on
which it differs from wgpu-graft and wgpu-scry, both of which carry the Slint
Servo example's SixtyFPS notice.

`welding` drives the Chromium Embedded Framework through the `cef` crate and
Mark's own `grafting`, both ordinary dependencies. No CEF, Chromium or
`grafting` source is vendored here, so neither is a provenance matter for this
tree; CEF's own terms (BSD-3-Clause, plus Chromium's) attach to the binaries a
packaged build ships, which belongs to the packaging lane rather than this
sweep.

## Retained licenses

**None.** There is no third-party code in this repository. The relicense tool
therefore skips nothing, and every tracked source file is owned.

| Path | License | Upstream | Notice files |
|---|---|---|---|

## Derivatives carrying MPL-2.0 with an upstream notice retained

**None.** No file here is a reworking of someone else's, so there is no
upstream notice to retain and `--retain-notice` has nothing to do.

## Exceptions under the fork/vendor criterion

**None.** The brief's §4 test — a crate stays MIT OR Apache-2.0 only when a
third party would need to *modify or vendor* it rather than merely link it —
admits nothing in this repository. `welding` is MPL-2.0 already, so no
published grant changes; per the sweep plan's invariant 8 no crate is
republished for the license.

## How to add a file from elsewhere

1. Do not delete or rewrite the upstream copyright or license notice, ever.
2. Add its path to **Retained licenses** above with its license, upstream URL,
   and where its notice text lives. The tool then skips it automatically.
3. If it is a substantial derivative rather than a verbatim import, the brief's
   rule is MPL-2.0 on the derivative *with the upstream notice retained* —
   record it in that section so the distinction is not lost.
4. Never add `license-file` to an owned manifest.
5. Re-run `python ../mere/scripts/relicense_headers.py --repo . --audit` and
   confirm the owned source count moved by exactly what you expected.
