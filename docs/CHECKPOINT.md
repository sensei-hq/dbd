# Checkpoint

**Slice:** 0.27.4 leftovers — on `develop`, not released.

## Done

- 0.27.4 released (crates.io, `main` 2147205e, CI green); #85 #86 #87 #88
  #89 #94 closed.
- #99 (PR #101): a dotted schema, which has no file, is counted and named as a
  schema (`1 schema is not valid`, `schema a.b`, `⚠ schema a.b is not valid`);
  `release` gives the same named refusal as `apply`/`snapshot`
  (`entity::ensure_fully_read`).
- PR #100: `make _check-ci` (pre-commit and `make bump`) runs `cargo doc`;
  `tests/pre_commit_hook.rs` requires every source check `ci.yml` runs.
- Gate on develop 8ac36511: check-ci (with cargo doc); embedded 363/363 and
  cli_live 83/83 on the #99 branch; CI green on both PRs. Branches pruned.

## Next

- Release 0.27.5 when wanted (`make bump`, release PR to `main`); #99 closes
  when `main` has it. The docs say "before 0.27.5" for the #99 change.
