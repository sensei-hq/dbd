# Checkpoint

**Slice:** workflow security hardening + qlty noise (on `develop`, unreleased).

## Done

- `release.yml`: OIDC trusted publishing for dbd-core + dbd-cli (registry
  publishers registered, no stored token), cargo cache removed (cold builds).
- `make _check-security` (zizmor 1.29, offline): prerequisite of `_check-ci`,
  CI `security` job, pre-publish in `release.yml`; `tests/security_gate.rs`
  pins the wiring. qlty exits 0 with findings, so zizmor directly.
- qlty: 0 security issues; TODO/NOTE notes 61 → 33 (all functional hits).
- zizmor/trufflehog set to `mode = "block"` (red Cloud statuses on findings).

## Next

- `cargo install zizmor --version 1.29.0 --locked` (one-time; pre-commit
  and `make bump` now require it), then `make bump patch` when ready —
  the next release carries all of this.

## Open

- Revoke the crates.io API token + delete `CARGO_REGISTRY_TOKEN` after the
  first green OIDC publish.
- Trufflehog has no CLI gate (same exit-0); Cloud red-status only — add a
  dedicated secret-scan step? Qlty Cloud red-status behavior unverified.

## Broken

- None known.
