# Checkpoint

**Slice:** v0.21.0 shipped and verified. `develop` and `main` level.

## Done

- **v0.21.0 released** — crates.io, merged to `main`, CI + CodeQL green.
  #18, #11 and #23 closed; only #7 (multi-tenant) remains open.
- **`dbd emit --dialect mysql|tsql|sqlite`** (#23) — a PostgreSQL schema as
  another engine's DDL. Downgrade-and-warn, never refuse: each loss is a
  comment at the site, a summary line, and `--report` JSON. Faithful mappings
  are reported nowhere. Only PostgreSQL can be the source.
- **#18 reconcile converges** — type-aware default comparison. Live-server
  measurement found more than the report: timestamptz renders in the session
  timezone, `'t'`→`true` was unlisted, and `'now'`/`'today'` can never
  converge so they stay visible as drift.
- **#11 CLI live coverage** — `tests/cli_live.rs` drives the real binary
  against embedded Postgres. `commands/mod.rs` 4.5% → 30.1%.
- **Non-Postgres dialects made honest** — `diff`/`reconcile` no longer report
  "in sync" for T-SQL/MySQL; a `mysql://` URL is refused by name.

## Next

    cargo test --workspace --all-features   # 1627 pass, clippy + fmt + doc clean

Nothing queued. #7 (multi-tenant isolation) is the only open issue — an
enhancement with a real design question, worth discussing before starting.

## Open question

Does sensei's 43,737 count occurrences or unique pairs? Until settled, a
residual difference is not a defect.

## Known-broken / carried forward

- `emit` covers tables and views only; routines are skipped and reported.
- `CREATE ROLE … IN ROLE …` is not read as a membership; `GRANT … TO …` is.
- `cmd_format --check` still calls `std::process::exit(1)`, so that path
  cannot be tested in-process (noted in #11 when it closed).
