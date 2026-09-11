# Checkpoint

**Slice:** JS advisory backlog + CI/security hardening + library manifest (#13).
Merging to `main` as **Unreleased** — deliberately no version bump.

## Done

Five commits on `develop`; PR #14 green on all 10 checks (new site job, CodeQL
rust/ts/actions, test+clippy+coverage, qlty, Cloudflare build). CodeQL: 0 alerts.

- **devalue 5.8.1 → 5.9.2** ("reject out-of-bounds indices", AIKIDO-2026-869882).
  The fix was already inside kit's `^5.8.1`; only the lockfile was stale.
- **undici override 7.29.0 → 8.10.2.** The old pin was itself the finding — it
  dragged `jsdom@30` off its declared `^8.9.0`. Bun rejects nested overrides
  (verified: it warns and ignores), so one version must serve both parents.
- **`bun run check` was fatally broken** — svelte-check 4.x refuses a bare
  TypeScript 7. Fixed with `typescript@~6.0.3` + `@typescript/native` + `--tsgo`.
  It survived because nothing in CI ran the site. Now gated.
- **Both Rust findings were non-issues**, verified not assumed: `rand` 0.8.8/0.10.2
  are past every patched version; `der` has zero advisories and rides the
  `rsa ← sqlx-mysql` path `.cargo/audit.toml` already documents as never compiled.
- **cookie: no version change.** Pinned because kit still declares `^0.6.0` and
  every 0.6.x carries GHSA-pxg6-pf52-xh8x. Renovate now caps it `<1.0.0` — 1.0
  delegates quote-parsing to `decode` (kit passes an identity decoder), 2.0
  renames `parse`/`serialize`. It ships in the worker, so a bad bump is a prod break.
- **Repo:** secret scanning + push protection on; Renovate owns security PRs
  (`vulnerabilityAlerts` + `osvVulnerabilityAlerts`); 7-day cooldown on every rule.
- **#13:** manifest completed per spec. `install` names `dbd install` — the issue's
  suggested `dbd skills add` does not exist. `make bump` now syncs `documents`/`ref`.

**No version bump:** a tag publishes to crates.io and the Rust tree is
byte-identical to 0.13.0. `ref: v0.13.0` stays accurate — docs/llms, docs/skills
and docs/agents hash-match at the tag, on main, and in the site's served copy.

## Next

Merge PR #14 → main, confirm CI green on main. Then open the rokkit issue
mirroring this work (dep upgrades + the svelte-check/TS 7 note).

## Open questions

Secret-scanning **validity checks** could not be enabled — org `sensei-hq` is on
the free plan with Advanced Security off. Needs paid Secret Protection.

## Known-broken / carried forward

- `.cargo/audit.toml` ignores RUSTSEC-2023-0071 (`rsa` via `sqlx-mysql`, never
  compiled). Re-check on sqlx bumps.
- `site/package.json` `overrides` pin six transitive deps; `dompurify` and `sharp`
  are now redundant (parents' ranges caught up) and can be dropped.
- 8 Rust majors available (sqlx 0.9, reqwest 0.13, dirs 7, sha2 0.11, …), none
  security-driven. sqlx 0.9 deserves its own slice with real Postgres testing.
- `ARRAY[col]::t[]` where the column is already type `t` still reads as drift.
- `generate_data_sql` warns "may truncate data" on a *widening* cast.
- Local leftovers: databases `dbd_repro12` and `dbd_v13_verify` (drop when convenient).
