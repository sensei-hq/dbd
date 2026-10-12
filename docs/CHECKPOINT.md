# Checkpoint

**Slice:** #7 Phase 2, slices 1–2 (registry + claim, uses) on `feat/ownership-uses`
= PR #50 (into the `feat/ownership` integration branch). Phase 1 is on `develop`,
unreleased.

## Done

- Slices 1–2: `dbd.objects`/`dbd.uses`; keys from every `CREATE` (DO blocks,
  types and multiranges too) as Postgres makes them; uses namespaced, bare
  reads the first relation on the path, bare calls every reachable routine.
- Claim timing: an apply in one transaction claims last, in it, and before a
  part-way commit; reconcile, reset and a DBD_NO_TX apply claim past their
  last refusal (before-hooks planned first) and before the first write.
- Rounds 3–5 fixed test-first; latest red `fba2b27e`, fixes `20515ad4`,
  `871ad944`. CI runs `cli_live` (libossp-uuid installed for `uuid-ossp`).
- Embedded 196/196, `cli_live` 47/47, workspace 2002/0, clippy and fmt clean.

## Next

    git push origin feat/ownership-uses:feat/ownership-registry   # PR #50
    # update its body; CI green; merge #50 locally into feat/ownership;
    # then slice 3: prune rules (a)/(b), --prune-unowned, release.

## Open

- Slice 3: an empty design's meta has no uses — the not-upgraded marker.
- Pre-existing, outside #7: reconcile re-emits a mixed-case or quoted
  `nextval('App.Seq')` default every run (`canonical_default`) — needs an
  issue and a pg_get_expr probe first.
- Declined: best-effort batch rollback error; dropping the old `dbd.import_tmp`.
- A bare type of another project's is no use (dropped as a built-in).
- An apply in one transaction runs `apply.before` hooks before its claim
  (documented in the spec). `dbd.objects` still allows namespace `type`, unused.
- Phase 1 is releasable as 0.25.2 when asked.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
