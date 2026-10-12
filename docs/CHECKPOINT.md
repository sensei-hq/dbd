# Checkpoint

**Slice:** scoped reset refused by policies and triggers calling in-scope
functions (`fix/scoped-reset-policies`, off `develop`), for 0.25.1.

## Done

- Reverse apply order dropped an in-scope function before its in-scope table,
  while a policy (`policies/`) or a trigger written in the function's file
  still called it; the refusal blamed "dependents outside the scope".
- Red `1f0505ff` → fix `4a7f08dc`: the scope's tables' policies go first.
- Red `f9526ff4` → fix `7484eec1`: non-internal triggers on its tables and
  views go first.
- Review (failure-mode): a partition's clone of its parent's trigger is not
  internal on PG 13+ and cannot be dropped alone. Red `36bffabc` → fix
  `7df416ba`: clones (`tgparentid`, read via `to_jsonb`) are left to the parent.
- Docs `1f64ec0f` + CHANGELOG: guides 04/07, llms, both SKILL.md.
- Embedded 132/132. Real sensei `dojo` scope on a private server: 0.25.0
  refuses; the branch drops 12 functions, 7 procedures, 7 views, 53 tables,
  40 enums, and `deploy --scope dojo` restores 39 tables and 41 policies.

## Next

    gh pr create --base develop --head fix/scoped-reset-policies
    # CI green → git merge --no-ff locally, push (not gh pr merge)

## Open

- 0.25.1 on `develop`: #41 and this. #41 closes when it reaches `main`.
- `6fccb0ff`/`b4f075e3` carry GitHub's noreply identity; not rewritten.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
- CHANGELOG Known limitations; `docs/BACKLOG.md` (0.25.1).
