# Backlog

Pending and future work only. **Shipped features are documented in the
[guides](guide/)** — the [commands reference](guide/04-commands.md), the
[design.yaml reference](guide/03-design-yaml.md), and
[snapshots & migrations](guide/05-snapshots-migrations.md) — with internals in
the [architecture notes](design/architecture.md). They are not tracked here.

## Now

### Failed policy files are skipped, not fatal

`apply_policies` logs a failing file to `PolicyReport.failed` and continues, so
a deploy can report success with RLS only partially applied. Worth deciding
whether it should refuse like `ensure_fully_parsed` does for DDL. The scope
filter below removed the *expected* failures that made this backstop noisy, so
a remaining `failed` entry is now much more likely to be a real one.

### Ordering within `policies/` is alphabetical

Not currently a problem, and deliberately left alone: policies run after all
DDL, so every object a policy body can reference already exists, and no policy
file creates an object another one needs.

Recorded because it was once filed as the cause of a live failure and was not.
That report — a policy on `repository_metrics` referencing `team_projects` —
turned out to be a scope bug: `policies/` ignored `--scope`, so the file ran
against a plane with no `dojo` schema. The error named a **schema**, not a
missing table, and the DDL order was correct all along
(`dojo.can_read_repository_metric` carries 6 edges including
`dojo.team_projects` and lands a layer after it). Fixed by scope-filtering
`policies/`; see [the commands reference](guide/04-commands.md#policies-under---scope).

If a policy body ever does need another policy's object, `pg_query` already
extracts body references (`parser::pg::common`) and the files could be ordered
topologically like DDL entities.

## Future

### Convex

- Per-table `export_data` via the Convex CLI — currently errors with a clear
  message because `npx convex export` only supports whole-deployment dumps.
  Revisit if the CLI grows a `--table` flag, or extract the table from the
  export zip.
