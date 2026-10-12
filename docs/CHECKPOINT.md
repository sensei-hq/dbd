# Checkpoint

**Slice:** 0.25.0 release — PR #45 (`develop` → `main`).

## Done

- PR #44 merged into `develop` (`6fccb0ff`). Release notes `99edd363`;
  `make bump minor` → `6d7769d9`, annotated tag `v0.25.0`.
- `release.yml` run 38253359492 green on the tagged tree (tests, clippy, fmt,
  `cargo doc`, publish). `dbd-core` and `dbd-cli` 0.25.0 are on crates.io.
- Shipped artifact verified: `cargo install dbd-cli --version 0.25.0 --locked`
  refuses `-e staging`; `emit --dialect tsql` puts `CREATE SCHEMA` and
  `CREATE VIEW` in `GO` batches of their own and keeps key names; MySQL gets
  `LONGTEXT` and reports the `PRIMARY` rename.
- History rewritten earlier (2026-10-12): every author, committer and tagger is
  Jerry Thomas <me@jerrythomas.name>. Backup:
  `~/Developer/dbd-backup-2026-10-12-pre-author-rewrite.bundle`.

## Next

    gh pr checks 45 --watch    # all green → merge #45 → main CI + Workers production build

## Open

- Cloudflare "Workers Builds: dbd" failed on `6fccb0ff`, `99edd363` and
  `6d7769d9`. `6fccb0ff`'s tree is identical to `c932b65d`, which built; a
  clean local `CF_PAGES=1 bun run build` at `6fccb0ff` passes, as does CI's
  site build. The cause is Cloudflare-side; its log is only in the dashboard
  (build `df5a6e58-bfdc-4466-9c04-74580ef4437a`).

## Known limits

- CHANGELOG Known limitations; `docs/BACKLOG.md` (0.25.1).
- Local `make _check-ci` does not run `cargo doc`; CI does (once per push).
