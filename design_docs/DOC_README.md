# Wgpu-weld documentation

This is the canonical index for active documentation in this repository.

## Working principles

- Keep CEF and platform facts separate from hardware receipts. State the
  platform, path, and measurement before calling a capability verified.
- Preserve identity across asynchronous producer boundaries. A bounded queue
  may reject admission, but it must not silently discard an accepted result.
- Keep browser policy in the host. Welding owns CEF adaptation and
  wgpu-importable frames, not an application's navigation or storage policy.
- Treat `README.md` as current public guidance and update it with any public
  contract change.
- Publish and tag the exact source being described. A public API change after
  publication receives a new crate version rather than reusing the old one.

## Active documents

- [Documentation policy](DOC_POLICY.md): shared documentation rules and the
  Wgpu-weld local addendum.
- [CEF accelerated OSR plan](2026-05-14_cef_accelerated_osr_plan.md): CEF
  windowless rendering and GPU-import implementation record.
- [Producer parity plan](2026-08-10_producer_parity_plan.md): cross-lane
  capability matrix, implementation phases, and evidence log.

## Maintainer-owned description

[PROJECT_DESCRIPTION.md](PROJECT_DESCRIPTION.md) is the maintainer-owned
project description required by the documentation policy. It has not yet been
created, so the root [README](../README.md) remains the public project
description until the maintainer supplies it.
