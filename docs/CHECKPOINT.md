# Checkpoint

**Slice:** patch 0.27.4 — released. #85, #86, #87, #88, #89, and #94 (a DDL
file that failed to parse was cut by `snapshot` as `DROP TABLE … CASCADE`).

## Done

- 0.27.4 on crates.io (dbd-cli, dbd-core); tag v0.27.4; release PR #98
  merged to `main` (2147205e). Issues #85 #86 #87 #88 #89 #94 closed.
- PRs #90 #91 #92 #93 #95 #96 (fixes, red-first, docs on every surface) and
  #97 (CI: the test step passes `GITHUB_TOKEN`, so the embedded server's
  binary lookup is not rate-limited).
- Gate on the merged tree: check-ci, embedded 363/363, cli_live 83/83,
  cargo doc, doctests. Installed 0.27.4 passes every repro
  (/tmp/dbd-0274-repros.sh); 0.27.3 fails each.
- Branches pruned locally and on origin; one worktree.

## Next

- Confirm `main` CI is green on 2147205e.
- Consider adding `cargo doc --workspace --no-deps --all-features` to
  `make _check-ci`: the pre-commit gate skipped it, and #95 failed CI on a
  private doc link.

## Open

- LOW: the unread-files message counts an auto-added schema as a "file".
- LOW: `release` refuses entity errors without naming the file.
- Scratch to delete (rm -rf was refused): /tmp/dbd-* probe and log dirs.
