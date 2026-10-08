# Checkpoint

**Slice:** 0.25.0 — third re-audit fixes. `fix/audit-bugs` tip `0d6fdb8` (all
gates green). Report: `/tmp/dbd-resume/reaudit3-report.md` — every earlier
defect FIXED, ~32 new (8 high), mostly pre-existing.

## In progress (agents)

- `dbd-apply` `fix/migrations-3`: version-by-version migrations (items 2, 3),
  `apply -n` with pending migrations (1), enum add+use (9), migration/hook
  scripts on source.search_path (10), reconcile enum/matview (11, 13), lows.
- `dbd-emit` `fix/guards-3`: guards keyed on project.name (4), dry-run guards
  (14), pin notes, lows.
- `dbd-model` `fix/reverse-data-3`: merge externals (6), init --from-db
  fidelity (7, 12, 18), JSONL null (15), SQLite BLOB (16).
- `dbd-audit` `fix/format-dbml-3`: search_path regex → parsed (5), formatter
  literals (8), emit key names (17), DBML reader/writer (18).
- `dbd-docs` `docs/feature-coverage`: docs pass 2 (to 0d6fdb8) + audit-3 stale
  guide items.

## Decisions (user)

- #22 refuse older project; #25 reconcile grants+hooks; prod guard: no
  override (a full `reset --force` forgets the mark — done `c114367`).
- Full re-audit was run (3rd). Ask before a 4th.
- Repo identity: local override removed; global Jerry Thomas <me@jerrythomas.name>
  applies. After the fixes: `git filter-repo --mailmap` ALL non-Jerry identities
  (dev@sensei-hq.com, jerry.thomas.in@icloud.com, users.noreply.github.com,
  senecaglobal.com, GitHub committer) → Jerry Thomas <me@jerrythomas.name>;
  backup bundle first; confirm before any force-push.

## Next

    # merge each fix branch (all gates) → CHANGELOG → final docs pass → ask about
    # 4th audit → author rewrite → PR to develop

## Scratch (rm denied)

- /tmp/dbdaudit-*, /tmp/dbdA3*, /tmp/dbdB_*.txt, /tmp/a1, /tmp/a2, /tmp/pgq-probe,
  /tmp/dbd-{checkci,commit,core-embedded}.log, /tmp/dbdapply-*, /tmp/dbdprobe,
  /tmp/dbd{pg55444,emit,ml,comb,fmt,json,uq,note,fsp,idx}.*
