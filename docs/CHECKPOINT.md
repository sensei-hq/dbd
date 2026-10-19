# Checkpoint

**Slice:** patch 0.27.4 — #85, #86, #87, #88, #89 and #94 (a DDL file that
fails to parse was cut by `snapshot` as `DROP TABLE … CASCADE`; found while
documenting #89, reproduced on 0.27.3).

## Done

- 0.27.3 shipped: crates.io, PR #82 merged to `main`, main CI green,
  #60/#72/#73 closed.
- Each issue red-first on its own branch, docs on every surface, CHANGELOG:
  PRs #90 (#85), #91 (#86), #92 (#87), #93 (#88), #95 (#89), #96 (#94).
- CI fix PR #97: the test step passes `GITHUB_TOKEN` (#95/#96 failed every
  embedded test on GitHub API 403/429, not on the code).
- Local `develop` holds all seven merged, the 0.27.4 release notes, and a doc
  fix (a dotted schema is refused by `snapshot` too). Gate on the merged tree:
  check-ci, embedded 363/363, cli_live 83/83.

## Next

1. CI green on #95/#96 reruns and #97 → `git push origin develop` (PRs close
   as merged).
2. `make bump` (0.27.4), release PR develop→main, merge locally, main CI.
3. Install, re-run each repro (#85 #86 #87 #88 #89 #94); confirm the issues
   close; prune the branches.

## Open

- LOW: the unread-files message counts an auto-added schema as a "file".
- Scratch to delete (rm -rf was refused): /tmp/dbd-* probe and log dirs.
