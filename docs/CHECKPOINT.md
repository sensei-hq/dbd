# Checkpoint

**Slice:** 0.25.0 release candidate — PR #44 (`fix/audit-bugs` → `develop`).

## Done

- Three full audits plus a focused verification; every confirmed defect fixed
  test-first and merged into `fix/audit-bugs`. CHANGELOG `[Unreleased]`, guides,
  README, llms, both SKILL.md, both pattern-verifier copies and the site match
  the code. Older defects for 0.25.1 are in `docs/BACKLOG.md`.
- History rewritten (2026-10-12): every author, committer and tagger is
  Jerry Thomas <me@jerrythomas.name>; branch and tag trees unchanged. Force-
  pushed with leases: main (incl. the 10 unpulled v0.24.1 release commits,
  rewritten), develop, fix/audit-bugs, 5 other branches, 84 tags. Backup:
  `~/Developer/dbd-backup-2026-10-12-pre-author-rewrite.bundle`.
- CI's `cargo doc` step failed on nine public docs linking private items;
  fixed (`c04b60f3`); `cargo doc --workspace --no-deps --all-features`, fmt and
  `cargo audit` clean locally.

## Next

    gh pr checks 44 --watch    # CI green → human review → merge to develop

## Open (user)

- Review and merge PR #44; then main and the 0.25.0 release (approval needed).

## Known limits

- CHANGELOG Known limitations; `docs/BACKLOG.md` (0.25.1).
- Local `make _check-ci` does not run `cargo doc`; CI does (once per push).
