# Checkpoint

**Slice:** patch 0.27.5 — released. #99 (an unread schema named as a schema;
`release` names each file) and the `cargo doc` pre-commit gate (#100).

## Done

- 0.27.5 on crates.io (dbd-cli, dbd-core); tag v0.27.5; release PR #102
  merged to `main` (df174b83). #99 closed.
- Installed 0.27.5 reproduces each #99 fix: `snapshot` reads `1 file(s) could
  not be parsed and 1 schema is not valid` / `schema a.b`; `graph` warns
  `schema a.b is not valid`; `release` names `t.ddl` and its error.
- Gate on develop: check-ci with cargo doc, embedded 363/363, cli_live 83/83.
- No open issues or PRs; branches pruned; one worktree.

## Next

- Nothing pending: `main` CI green on df174b83.

## Open

- None.
