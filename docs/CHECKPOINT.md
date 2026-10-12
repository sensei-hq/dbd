# Checkpoint

**Slice:** v0.25.1 shipped. Nothing in flight.

## Done

- v0.25.1 (`0b6b2f9a`, tag `v0.25.1`): #41 (PR #46) and the scoped-reset fix
  (PR #47). Release notes `65e59083`.
- Pre-release suite green: workspace, embedded 132, `cli_live`, clippy, fmt,
  `cargo doc -D warnings`, `cargo audit`.
- `release.yml` run 38276639619 green; `dbd-core` and `dbd-cli` 0.25.1 on
  crates.io.
- Shipped artifact (`cargo install dbd-cli --version 0.25.1`) on a private
  server: the #41 repro reconciles `0 altered` and `diff` is in sync; sensei's
  `dojo` scope resets fully (15 functions, 7 procedures, 8 views, 53 tables,
  40 enums). crates.io 0.25.0, same environment, fails both.
- PR #48 merged to `main` locally (`636528df`); `main` CI and the Workers
  production deploy green; the site serves 0.25.1; #41 closed.

## Next

    # nothing queued — docs/BACKLOG.md holds the 0.25.x follow-ups

## Open

- `6fccb0ff`/`b4f075e3` carry GitHub's noreply identity; not rewritten (an
  ancestor of the published `v0.25.0` tag). Merges are now done locally.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
- CHANGELOG Known limitations; `docs/BACKLOG.md`.
