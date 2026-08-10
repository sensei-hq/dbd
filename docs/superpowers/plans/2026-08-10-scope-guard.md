# Scope Guard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pin each database to the scope it was built with (stored in `_dbd_meta.scope`) and refuse `apply`/`deploy`/`reconcile`/`reset` runs that request a different scope, unless the operator passes `--allow-scope-change`.

**Architecture:** Add a nullable `scope` column to `_dbd_meta` and a `scope: Option<String>` field on `ProjectMeta`. `set_project_meta` gains a `scope` argument; the two core write paths (`apply`'s `SetVersion` step and `reconcile`'s tail) pass the resolved scope name, so every successful write **pins** the DB. A pure `Design::check_scope_guard` associated function does the comparison; the four CLI handlers fetch `get_project_meta()` and call it before writing (this keeps the core write-method signatures — and their ~30 test call sites — untouched). Backward compatible: existing DBs have `scope = NULL` (unpinned) and never block until first re-pinned.

**Tech Stack:** Rust workspace (`dbd-core` library + `dbd-cli` binary), `sqlx` (Postgres/SQLite), `clap`, `async_trait`, `tokio`.

**Spec:** `docs/superpowers/specs/2026-08-10-scope-guard-design.md`

**Canonical commands** (from `Makefile:_check-ci`, also run by the pre-commit hook on every `git commit`):
- Full: `cargo test --workspace --quiet` and `cargo clippy --workspace --all-targets --quiet -- -D warnings`
- Focused core: `cargo test -p dbd-core <name>` — add `--features sqlite` / `--features postgres` for adapter tests
- Focused CLI: `cargo test -p dbd-cli <name>`

> **Deviations from an earlier spec draft** (already reconciled into the spec): (1) all four scope guards are invoked from the **CLI handlers** via the pure helper, not threaded through core `apply`/`deploy`/`reconcile`/`reset` signatures; (2) `reset` is **guard-only** — it does not clear the pin (it doesn't write `_dbd_meta` today).

---

## Task 1: Plumb `scope` through `ProjectMeta` and `set_project_meta`

Adds the field + column, changes the `set_project_meta` signature across the trait and all four adapters, wires the **pin** at the two core call sites, and fixes existing callers so the workspace compiles. This is one atomic compile unit (a trait-signature change) — all edits land together.

**Files:**
- Modify: `crates/dbd-core/src/adapter/mod.rs:22` (struct), `:239` (trait method)
- Modify: `crates/dbd-core/src/adapter/mock.rs:88`, `:231`, `:303`
- Modify: `crates/dbd-core/src/adapter/postgres.rs:1708`, `:1722`, `:1748`
- Modify: `crates/dbd-core/src/adapter/sqlite.rs:429`, `:442`, `:461`, `:810`, `:1119`
- Modify: `crates/dbd-core/src/adapter/convex.rs:330`, `:731`, `:739`, `:901`
- Modify: `crates/dbd-core/src/design.rs:1122`, `:1474`
- Modify: `crates/dbd-core/tests/embedded_test.rs:2305`
- Test: `crates/dbd-core/src/adapter/mock.rs` (tests mod), `crates/dbd-core/src/design.rs` (tests mod)

- [ ] **Step 1: Write the failing tests**

In `crates/dbd-core/src/adapter/mock.rs`, inside the `#[cfg(test)] mod tests` block (the one starting at line 242), add:

```rust
    #[tokio::test]
    async fn mock_meta_round_trips_scope() {
        let mock = MockAdapter::new();
        mock.set_project_meta("dev", 2, Some("public")).await.unwrap();
        let meta = mock.get_project_meta().await.unwrap().expect("meta");
        assert_eq!(meta.scope.as_deref(), Some("public"));

        // None clears the pin.
        mock.set_project_meta("dev", 3, None).await.unwrap();
        assert_eq!(mock.get_project_meta().await.unwrap().unwrap().scope, None);
    }
```

In `crates/dbd-core/src/design.rs`, inside the `#[cfg(test)] mod tests` block, next to `apply_set_version_writes_meta` (~line 2653), add:

```rust
    #[tokio::test]
    async fn apply_pins_resolved_scope() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();
        let resolved = design.resolve_scope(None, None).unwrap();

        design
            .apply(&mock, None, false, Some(&resolved), |_| {}, |_, _| {}, |_| {})
            .await
            .unwrap();

        let meta = mock.get_project_meta().await.unwrap().expect("apply writes meta");
        assert_eq!(meta.scope.as_deref(), Some(resolved.name.as_str()));
    }
```

- [ ] **Step 2: Run to verify they fail (compile error)**

Run: `cargo test -p dbd-core mock_meta_round_trips_scope apply_pins_resolved_scope 2>&1 | tail -20`
Expected: FAIL — compile errors (`ProjectMeta` has no field `scope`; `set_project_meta` takes 2 args, not 3).

- [ ] **Step 3a: `ProjectMeta` + trait signature** — `crates/dbd-core/src/adapter/mod.rs`

Replace the struct (line 20):

```rust
/// Project-level metadata stored in `_dbd_meta`.
#[derive(Debug, Clone)]
pub struct ProjectMeta {
    pub project: String,
    pub env: String,
    pub version: u32,
    /// The resolved scope this database is pinned to (`None` = unpinned).
    pub scope: Option<String>,
    pub applied_at: Option<String>,
}
```

Replace the trait method (line 239):

```rust
    async fn set_project_meta(&self, env: &str, version: u32, scope: Option<&str>) -> Result<()>;
```

- [ ] **Step 3b: Mock adapter** — `crates/dbd-core/src/adapter/mock.rs`

Update `with_meta` (line 88) to set `scope: None`, and add a `with_scope` seed helper right after it:

```rust
    pub fn with_meta(self, env: &str, version: u32) -> Self {
        *self.meta.lock().unwrap() = Some(ProjectMeta {
            project: "test".to_string(),
            env: env.to_string(),
            version,
            scope: None,
            applied_at: Some("2026-01-01T00:00:00Z".to_string()),
        });
        self
    }

    /// Seed (or overwrite) the pinned scope for scope-guard tests.
    pub fn with_scope(self, scope: &str) -> Self {
        {
            let mut m = self.meta.lock().unwrap();
            match m.as_mut() {
                Some(meta) => meta.scope = Some(scope.to_string()),
                None => {
                    *m = Some(ProjectMeta {
                        project: "test".to_string(),
                        env: "dev".to_string(),
                        version: 0,
                        scope: Some(scope.to_string()),
                        applied_at: Some("2026-01-01T00:00:00Z".to_string()),
                    });
                }
            }
        }
        self
    }
```

Update the `set_project_meta` impl (line 231):

```rust
    async fn set_project_meta(&self, env: &str, version: u32, scope: Option<&str>) -> Result<()> {
        *self.meta.lock().unwrap() = Some(ProjectMeta {
            project: "test".to_string(),
            env: env.to_string(),
            version,
            scope: scope.map(|s| s.to_string()),
            applied_at: Some("2026-01-01T00:00:00Z".to_string()),
        });
        Ok(())
    }
```

Update the existing t3 call (line 303) from `mock.set_project_meta("prod", 5)` to:

```rust
        mock.set_project_meta("prod", 5, None).await.unwrap();
```

- [ ] **Step 3c: Postgres adapter** — `crates/dbd-core/src/adapter/postgres.rs`

`ensure_meta_table` (line 1708) — add `scope` to the CREATE and backfill via `exec_raw` (batch-aware, matching the CREATE and the INSERT):

```rust
    async fn ensure_meta_table(&self) -> Result<()> {
        self.ensure_public_bookkeeping(
            "_dbd_meta",
            "CREATE TABLE IF NOT EXISTS public._dbd_meta ( \
                project     varchar NOT NULL PRIMARY KEY, \
                env         varchar NOT NULL DEFAULT 'dev', \
                version     integer NOT NULL DEFAULT 0, \
                scope       varchar, \
                created_at  timestamptz NOT NULL DEFAULT now(), \
                updated_at  timestamptz NOT NULL DEFAULT now() \
            )",
        )
        .await?;
        // Backfill `scope` on databases whose `_dbd_meta` predates the column.
        self.exec_raw("ALTER TABLE public._dbd_meta ADD COLUMN IF NOT EXISTS scope varchar")
            .await
    }
```

`get_project_meta` (line 1722) — resilient read (falls back to the legacy shape if the `scope` column is missing, so the prod/version guard still sees `env`/`version`):

```rust
    async fn get_project_meta(&self) -> Result<Option<ProjectMeta>> {
        // Resolve `_dbd_meta`'s schema via the catalog (a scoped apply may have
        // left it outside `public`) rather than reading an unqualified name.
        let Some(schema) = self.bookkeeping_schema("_dbd_meta").await? else {
            return Ok(None); // no `_dbd_meta` anywhere yet
        };
        let quoted = schema.replace('"', "\"\"");
        // Preferred read includes `scope`. On a database whose `_dbd_meta`
        // predates the column this SELECT errors; fall back to the legacy shape
        // (scope = None) so env/version — and the prod guard — still work.
        let with_scope = sqlx::query(&format!(
            "SELECT project, env, version, scope, updated_at::text as applied_at FROM \"{quoted}\"._dbd_meta WHERE project = $1"
        ))
        .bind(&self.project)
        .fetch_optional(&self.pool)
        .await;

        match with_scope {
            Ok(Some(row)) => Ok(Some(ProjectMeta {
                project: row.get("project"),
                env: row.get("env"),
                version: row.get::<i32, _>("version") as u32,
                scope: row.try_get("scope").ok(),
                applied_at: row.try_get("applied_at").ok(),
            })),
            Ok(None) => Ok(None),
            Err(_) => {
                let legacy = sqlx::query(&format!(
                    "SELECT project, env, version, updated_at::text as applied_at FROM \"{quoted}\"._dbd_meta WHERE project = $1"
                ))
                .bind(&self.project)
                .fetch_optional(&self.pool)
                .await;
                match legacy {
                    Ok(Some(row)) => Ok(Some(ProjectMeta {
                        project: row.get("project"),
                        env: row.get("env"),
                        version: row.get::<i32, _>("version") as u32,
                        scope: None,
                        applied_at: row.try_get("applied_at").ok(),
                    })),
                    _ => Ok(None),
                }
            }
        }
    }
```

`set_project_meta` (line 1748) — add `scope` param + column:

```rust
    async fn set_project_meta(&self, env: &str, version: u32, scope: Option<&str>) -> Result<()> {
        self.ensure_meta_table().await?;
        let q = sqlx::query(
            "INSERT INTO public._dbd_meta (project, env, version, scope) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (project) DO UPDATE \
             SET env = EXCLUDED.env, version = EXCLUDED.version, scope = EXCLUDED.scope, updated_at = now()"
        )
        .bind(&self.project)
        .bind(env)
        .bind(version as i32)
        .bind(scope);
        let mut guard = self.batch.lock().await;
        match guard.as_mut() {
            Some(tx) => q.execute(&mut **tx).await,
            None => q.execute(&self.pool).await,
        }
        .map_err(|e| DbdError::Config(format!("Set project meta failed: {e}")))?;

        Ok(())
    }
```

- [ ] **Step 3d: SQLite adapter** — `crates/dbd-core/src/adapter/sqlite.rs`

`ensure_meta_table` (line 429) — add `scope` to CREATE + PRAGMA-guarded add-column (SQLite lacks `ADD COLUMN IF NOT EXISTS`):

```rust
    async fn ensure_meta_table(&self) -> Result<()> {
        self.execute_script(
            "CREATE TABLE IF NOT EXISTS _dbd_meta ( \
                project    TEXT NOT NULL PRIMARY KEY, \
                env        TEXT NOT NULL DEFAULT 'dev', \
                version    INTEGER NOT NULL DEFAULT 0, \
                scope      TEXT, \
                created_at TEXT NOT NULL DEFAULT (datetime('now')), \
                updated_at TEXT NOT NULL DEFAULT (datetime('now')) \
            )",
        )
        .await?;
        // SQLite has no `ADD COLUMN IF NOT EXISTS` — add `scope` only when missing.
        let has_scope = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM pragma_table_info('_dbd_meta') WHERE name = 'scope'",
        )
        .fetch_one(&self.pool)
        .await
        .unwrap_or(0)
            > 0;
        if !has_scope {
            self.execute_script("ALTER TABLE _dbd_meta ADD COLUMN scope TEXT").await?;
        }
        Ok(())
    }
```

`get_project_meta` (line 442) — resilient read:

```rust
    async fn get_project_meta(&self) -> Result<Option<ProjectMeta>> {
        let with_scope = sqlx::query(
            "SELECT project, env, version, scope, updated_at AS applied_at FROM _dbd_meta WHERE project = ?1",
        )
        .bind(&self.project)
        .fetch_optional(&self.pool)
        .await;

        match with_scope {
            Ok(Some(row)) => Ok(Some(ProjectMeta {
                project: row.get("project"),
                env: row.get("env"),
                version: row.get::<i64, _>("version") as u32,
                scope: row.try_get("scope").ok(),
                applied_at: row.try_get("applied_at").ok(),
            })),
            Ok(None) => Ok(None),
            Err(_) => {
                let legacy = sqlx::query(
                    "SELECT project, env, version, updated_at AS applied_at FROM _dbd_meta WHERE project = ?1",
                )
                .bind(&self.project)
                .fetch_optional(&self.pool)
                .await;
                match legacy {
                    Ok(Some(row)) => Ok(Some(ProjectMeta {
                        project: row.get("project"),
                        env: row.get("env"),
                        version: row.get::<i64, _>("version") as u32,
                        scope: None,
                        applied_at: row.try_get("applied_at").ok(),
                    })),
                    _ => Ok(None),
                }
            }
        }
    }
```

`set_project_meta` (line 461) — add `scope`:

```rust
    async fn set_project_meta(&self, env: &str, version: u32, scope: Option<&str>) -> Result<()> {
        self.ensure_meta_table().await?;
        sqlx::query(
            "INSERT INTO _dbd_meta (project, env, version, scope) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (project) DO UPDATE \
             SET env = excluded.env, version = excluded.version, scope = excluded.scope, updated_at = datetime('now')",
        )
        .bind(&self.project)
        .bind(env)
        .bind(version as i64)
        .bind(scope)
        .execute(&self.pool)
        .await
        .map_err(|e| DbdError::Config(format!("Set project meta failed: {e}")))?;
        Ok(())
    }
```

Fix the two SQLite test callers: line 810 `a.set_project_meta("dev", 3)` → `a.set_project_meta("dev", 3, None)`; line 1119 `a.set_project_meta("prod", 3)` → `a.set_project_meta("prod", 3, None)`.

- [ ] **Step 3e: Convex adapter** — `crates/dbd-core/src/adapter/convex.rs`

Add `scope` to `ConvexState` (line 330), with `#[serde(default)]` so old state files still parse:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ConvexState {
    project: String,
    env: String,
    version: u32,
    #[serde(default)]
    scope: Option<String>,
    updated_at: String,
    migrations: Vec<MigrationRecord>,
}
```

`get_project_meta` literal (line 731) — add `scope: state.scope,`:

```rust
        Ok(Some(ProjectMeta {
            project: state.project,
            env: state.env,
            version: state.version,
            scope: state.scope,
            applied_at: Some(state.updated_at),
        }))
```

`set_project_meta` (line 739):

```rust
    async fn set_project_meta(&self, env: &str, version: u32, scope: Option<&str>) -> Result<()> {
        let mut state = load_state(&self.output_dir);
        state.project = self.project.clone();
        state.env = env.to_string();
        state.version = version;
        state.scope = scope.map(|s| s.to_string());
        state.updated_at = chrono::Utc::now().to_rfc3339();
        save_state(&self.output_dir, &state)
    }
```

Fix the Convex test caller: line 901 `adapter.set_project_meta("dev", 4)` → `adapter.set_project_meta("dev", 4, None)`.

- [ ] **Step 3f: Pin at the two core write call sites** — `crates/dbd-core/src/design.rs`

`SetVersion` step in `apply` (line 1122):

```rust
                    ExecutionStep::SetVersion(version) => {
                        adapter
                            .set_project_meta(&self.env, *version, scope.map(|s| s.name.as_str()))
                            .await?;
                    }
```

`reconcile` tail (line 1474):

```rust
        adapter.set_project_meta(&self.env, version, scope.map(|s| s.name.as_str())).await?;
```

- [ ] **Step 3g: Fix the embedded-test caller** — `crates/dbd-core/tests/embedded_test.rs`

Line 2305 `.set_project_meta("prod", 8)` → `.set_project_meta("prod", 8, None)`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p dbd-core mock_meta_round_trips_scope apply_pins_resolved_scope 2>&1 | tail -20`
Expected: PASS (both tests).
Then compile the workspace: `cargo build --workspace 2>&1 | tail -5` → no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core
git commit -m "feat(meta): add scope column to _dbd_meta and pin scope on write"
```

(The pre-commit hook runs the full test + clippy suite; it must pass.)

---

## Task 2: `Design::check_scope_guard` helper + unit tests

A pure associated function (no `self`) that decides whether a run's scope is allowed against the pinned scope.

**Files:**
- Modify: `crates/dbd-core/src/design.rs` (add the fn after `check_scope_gaps`, ~line 755)
- Test: `crates/dbd-core/src/design.rs` (tests mod)

- [ ] **Step 1: Write the failing tests**

In the `#[cfg(test)] mod tests` block of `design.rs`, add a small constructor helper and four tests:

```rust
    fn meta_with_scope(scope: Option<&str>) -> crate::adapter::ProjectMeta {
        crate::adapter::ProjectMeta {
            project: "p".to_string(),
            env: "dev".to_string(),
            version: 1,
            scope: scope.map(|s| s.to_string()),
            applied_at: None,
        }
    }

    #[test]
    fn scope_guard_allows_matching_scope() {
        let m = meta_with_scope(Some("public"));
        assert!(Design::check_scope_guard(Some(&m), "public", false).is_ok());
    }

    #[test]
    fn scope_guard_blocks_mismatch() {
        let m = meta_with_scope(Some("public"));
        let err = Design::check_scope_guard(Some(&m), "internal", false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("public") && msg.contains("internal"), "msg was: {msg}");
    }

    #[test]
    fn scope_guard_unpinned_never_blocks() {
        let m = meta_with_scope(None);
        assert!(Design::check_scope_guard(Some(&m), "internal", false).is_ok());
        assert!(Design::check_scope_guard(None, "internal", false).is_ok());
    }

    #[test]
    fn scope_guard_allow_scope_change_bypasses() {
        let m = meta_with_scope(Some("public"));
        assert!(Design::check_scope_guard(Some(&m), "internal", true).is_ok());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p dbd-core scope_guard_ 2>&1 | tail -20`
Expected: FAIL — `no function or associated item named check_scope_guard`.

- [ ] **Step 3: Implement the helper**

In `crates/dbd-core/src/design.rs`, immediately after the `check_scope_gaps` method (which ends ~line 755), add:

```rust
    /// Scope guard: refuse to operate under a scope different from the one this
    /// database was pinned to. `meta` is the stored project meta (`None` on a
    /// fresh database), `requested` is the resolved scope name for this run, and
    /// `allow_scope_change` bypasses the guard (the next successful write re-pins
    /// the DB). A database with no recorded scope (`meta.scope == None`) is
    /// unpinned and never blocks — the current run pins it. Mirrors the prod
    /// guard in [`Design::reset`]; invoked from the CLI write handlers.
    pub fn check_scope_guard(
        meta: Option<&crate::adapter::ProjectMeta>,
        requested: &str,
        allow_scope_change: bool,
    ) -> Result<()> {
        if allow_scope_change {
            return Ok(());
        }
        if let Some(pinned) = meta.and_then(|m| m.scope.as_deref())
            && pinned != requested
        {
            return Err(DbdError::SafetyGuard(format!(
                "scope guard: this database is pinned to scope '{pinned}', but you requested '{requested}'.\n\
                 Applying a different scope would build a divergent schema.\n\
                 → re-run with --scope {pinned}, or pass --allow-scope-change to re-point this database to '{requested}'."
            )));
        }
        Ok(())
    }
```

(`DbdError` and `Result` are already in scope in `design.rs`; `let`-chains are already used by `reset`.)

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p dbd-core scope_guard_ 2>&1 | tail -20`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/design.rs
git commit -m "feat(guard): add Design::check_scope_guard helper"
```

---

## Task 3: CLI `--allow-scope-change` flag + wire the guard into the four handlers

**Files:**
- Modify: `crates/dbd-cli/src/cli.rs` (Apply/Deploy/Reconcile/Reset variants; a parse test; fix `reconcile_flags_parse`)
- Modify: `crates/dbd-cli/src/commands/mod.rs` (dispatch)
- Modify: `crates/dbd-cli/src/commands/schema.rs` (`cmd_apply`)
- Modify: `crates/dbd-cli/src/commands/project.rs` (`cmd_deploy`, `cmd_reconcile`)
- Modify: `crates/dbd-cli/src/commands/migration.rs` (`cmd_reset`)

- [ ] **Step 1: Write the failing flag-parse test**

In `crates/dbd-cli/src/cli.rs`, inside `#[cfg(test)] mod tests`, add:

```rust
    /// `--allow-scope-change` parses on every write subcommand and defaults false.
    #[test]
    fn allow_scope_change_flag_parses() {
        for c in ["apply", "deploy", "reconcile", "reset"] {
            let on = Cli::try_parse_from(["dbd", c, "--allow-scope-change"])
                .unwrap_or_else(|e| panic!("`dbd {c} --allow-scope-change` failed: {e}"));
            let flag = match &on.command {
                Commands::Apply { allow_scope_change, .. } => *allow_scope_change,
                Commands::Deploy { allow_scope_change, .. } => *allow_scope_change,
                Commands::Reconcile { allow_scope_change, .. } => *allow_scope_change,
                Commands::Reset { allow_scope_change, .. } => *allow_scope_change,
                _ => unreachable!(),
            };
            assert!(flag, "{c} --allow-scope-change should be true");

            let off = Cli::try_parse_from(["dbd", c]).unwrap();
            let flag_off = match &off.command {
                Commands::Apply { allow_scope_change, .. } => *allow_scope_change,
                Commands::Deploy { allow_scope_change, .. } => *allow_scope_change,
                Commands::Reconcile { allow_scope_change, .. } => *allow_scope_change,
                Commands::Reset { allow_scope_change, .. } => *allow_scope_change,
                _ => unreachable!(),
            };
            assert!(!flag_off, "{c} allow_scope_change should default false");
        }
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p dbd-cli allow_scope_change_flag_parses 2>&1 | tail -20`
Expected: FAIL — `Apply`/etc. have no field `allow_scope_change`.

- [ ] **Step 3a: Add the flag to the four subcommands** — `crates/dbd-cli/src/cli.rs`

`Apply` (line 70) — add after `with_policies`:

```rust
        /// Allow this database to be re-pointed to a different scope
        /// (bypasses the scope guard).
        #[arg(long)]
        allow_scope_change: bool,
```

`Deploy` (line 134) — add after `clear_cache`:

```rust
        /// Allow this database to be re-pointed to a different scope
        /// (bypasses the scope guard).
        #[arg(long)]
        allow_scope_change: bool,
```

`Reconcile` (line 286) — add after `prune`:

```rust
        /// Allow this database to be re-pointed to a different scope
        /// (bypasses the scope guard).
        #[arg(long)]
        allow_scope_change: bool,
```

`Reset` (line 162) — add after `clean`:

```rust
        /// Allow this database to be re-pointed to a different scope
        /// (bypasses the scope guard).
        #[arg(long)]
        allow_scope_change: bool,
```

Fix the now-exhaustive matches in `reconcile_flags_parse` (they omit `..`): line 432 and line 443 — add `, ..` before the closing brace:

```rust
                &cli.command,
                Commands::Reconcile { dry_run: false, allow_destructive: false, prune: false, .. }
```
```rust
                &cli.command,
                Commands::Reconcile { dry_run: true, allow_destructive: true, prune: true, .. }
```

- [ ] **Step 3b: Thread the flag through dispatch** — `crates/dbd-cli/src/commands/mod.rs`

`Apply` arm (line 47):

```rust
        Commands::Apply { name, dry_run, with_policies, allow_scope_change } => {
            schema::cmd_apply(config, env, project_dir, database_url, name.as_deref(), *dry_run, *with_policies, *allow_scope_change, scope, deps, verbosity).await
        }
```

`Reset` arm (line 59):

```rust
        Commands::Reset { target, dry_run, force, schemas, extensions, clean, allow_scope_change } => {
            let drop_schemas = *schemas || *clean;
            let drop_extensions = *extensions || *clean;
            migration::cmd_reset(config, env, project_dir, database_url, target, *dry_run, *force, drop_schemas, drop_extensions, *allow_scope_change, scope, deps, verbosity).await
        }
```

`Deploy` arm (line 82):

```rust
        Commands::Deploy { dry_run, no_cache, clear_cache, allow_scope_change } => {
            project::cmd_deploy(source, config, env, database_url, *dry_run, *no_cache, *clear_cache, *allow_scope_change, scope, deps, verbosity).await
        }
```

`Reconcile` arm (line 185):

```rust
        Commands::Reconcile { dry_run, allow_destructive, prune, allow_scope_change } => {
            project::cmd_reconcile(config, env, project_dir, database_url, *dry_run, *allow_destructive, *prune, *allow_scope_change, scope, deps, verbosity).await
        }
```

- [ ] **Step 3c: `cmd_apply`** — `crates/dbd-cli/src/commands/schema.rs`

Add the param to the signature (after `with_policies: bool,`, line 306):

```rust
    allow_scope_change: bool,
```

Insert the guard right after `let adapter = get_adapter(config, database_url).await?;` (line 341):

```rust
    // Scope guard: refuse an apply under a different scope than this DB was
    // pinned to (unless the operator opted in to re-point it).
    let meta = adapter.get_project_meta().await?;
    Design::check_scope_guard(meta.as_ref(), &resolved.name, allow_scope_change)?;
```

- [ ] **Step 3d: `cmd_deploy`** — `crates/dbd-cli/src/commands/project.rs`

Add the param to the signature (after `clear_cache: bool,`, line 316):

```rust
    allow_scope_change: bool,
```

Insert the guard right after `let adapter = get_adapter(&config_path, database_url).await?;` (line 369):

```rust
    let meta = adapter.get_project_meta().await?;
    Design::check_scope_guard(meta.as_ref(), &resolved.name, allow_scope_change)?;
```

- [ ] **Step 3e: `cmd_reconcile`** — `crates/dbd-cli/src/commands/project.rs`

Add the param to the signature (after `prune: bool,`, line 460):

```rust
    allow_scope_change: bool,
```

Insert the guard after the `dry_run` early-return block (after line 486, before `output::info(verbosity, "Reconciling schema to design...");`). `adapter` is already created at line 476:

```rust
    let meta = adapter.get_project_meta().await?;
    Design::check_scope_guard(meta.as_ref(), &resolved.name, allow_scope_change)?;
```

- [ ] **Step 3f: `cmd_reset`** — `crates/dbd-cli/src/commands/migration.rs`

Add the param to the signature (after `drop_extensions: bool,`, line 19):

```rust
    allow_scope_change: bool,
```

Insert the guard between adapter creation (line 36) and `design.reset(...)` (line 37). `force || allow_scope_change` bypasses:

```rust
    let adapter = get_adapter(config, database_url).await?;
    let meta = adapter.get_project_meta().await?;
    Design::check_scope_guard(meta.as_ref(), &resolved.name, force || allow_scope_change)?;
    design.reset(&*adapter, target, force, drop_schemas, drop_extensions, Some(&resolved)).await?;
    Ok(())
```

- [ ] **Step 4: Run to verify pass + full workspace green**

Run: `cargo test -p dbd-cli allow_scope_change_flag_parses reconcile_flags_parse 2>&1 | tail -20`
Expected: PASS.
Run: `cargo build --workspace 2>&1 | tail -5`
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-cli
git commit -m "feat(cli): add --allow-scope-change and enforce the scope guard on apply/deploy/reconcile/reset"
```

---

## Task 4: Adapter backward-compat tests (legacy `_dbd_meta` without `scope`)

Proves the resilient read (prod guard survives) and the add-column backfill on a database whose `_dbd_meta` predates the `scope` column.

**Files:**
- Test: `crates/dbd-core/src/adapter/sqlite.rs` (tests mod)
- Test: `crates/dbd-core/tests/embedded_test.rs`

- [ ] **Step 1: SQLite legacy test**

In the SQLite `#[cfg(test)] mod tests` block (near `s6_meta_set_get`, ~line 805), add:

```rust
    #[tokio::test]
    async fn s8_meta_scope_backfills_on_legacy_table() {
        let a = mem().await; // project = "test"
        // Legacy `_dbd_meta` WITHOUT the `scope` column (pre-scope-guard schema).
        a.execute_script(
            "CREATE TABLE _dbd_meta ( \
                project TEXT NOT NULL PRIMARY KEY, \
                env TEXT NOT NULL DEFAULT 'dev', \
                version INTEGER NOT NULL DEFAULT 0, \
                created_at TEXT NOT NULL DEFAULT (datetime('now')), \
                updated_at TEXT NOT NULL DEFAULT (datetime('now')) )",
        )
        .await
        .unwrap();
        a.execute_script("INSERT INTO _dbd_meta (project, env, version) VALUES ('test', 'prod', 2)")
            .await
            .unwrap();

        // Resilient read: prod guard still sees env/version; scope = None.
        let m = a.get_project_meta().await.unwrap().expect("legacy meta reads");
        assert_eq!(m.env, "prod");
        assert_eq!(m.version, 2);
        assert_eq!(m.scope, None);

        // A write backfills the column and pins the scope.
        a.set_project_meta("prod", 3, Some("public")).await.unwrap();
        let m2 = a.get_project_meta().await.unwrap().unwrap();
        assert_eq!(m2.scope.as_deref(), Some("public"));
    }
```

- [ ] **Step 2: Run the SQLite test**

Run: `cargo test -p dbd-core --features sqlite s8_meta_scope_backfills_on_legacy_table 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 3: Postgres legacy test**

In `crates/dbd-core/tests/embedded_test.rs`, add a new test (mirroring the existing `start_pg()`/`connect()` harness used at line 2270):

```rust
#[tokio::test]
async fn legacy_meta_without_scope_reads_and_backfills() {
    let (_pg, url) = start_pg().await;
    let adapter = connect(&url, "legacy_scope_test").await.unwrap();

    // Legacy `public._dbd_meta` WITHOUT the `scope` column, prod row.
    adapter
        .execute_script(
            "CREATE TABLE public._dbd_meta ( \
                project varchar NOT NULL PRIMARY KEY, \
                env varchar NOT NULL DEFAULT 'dev', \
                version integer NOT NULL DEFAULT 0, \
                created_at timestamptz NOT NULL DEFAULT now(), \
                updated_at timestamptz NOT NULL DEFAULT now() ); \
             INSERT INTO public._dbd_meta (project, env, version) \
                VALUES ('legacy_scope_test', 'prod', 4)",
        )
        .await
        .expect("seed legacy public._dbd_meta");

    // Resilient read: prod guard still sees env/version; scope is None.
    let m = adapter.get_project_meta().await.unwrap().expect("legacy meta reads");
    assert_eq!(m.env, "prod");
    assert_eq!(m.version, 4);
    assert_eq!(m.scope, None);

    // A write backfills the `scope` column and pins the scope.
    adapter
        .set_project_meta("prod", 5, Some("public"))
        .await
        .expect("set_project_meta backfills the scope column");
    let m2 = adapter.get_project_meta().await.unwrap().unwrap();
    assert_eq!(m2.scope.as_deref(), Some("public"));
}
```

- [ ] **Step 4: Run the Postgres test**

Run: `cargo test -p dbd-core --features postgres --test embedded_test legacy_meta_without_scope_reads_and_backfills 2>&1 | tail -20`
Expected: PASS (the embedded Postgres harness provisions the DB).

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core
git commit -m "test(meta): cover legacy _dbd_meta scope backfill on sqlite + postgres"
```

---

## Task 5: Document the scope guard

**Files:**
- Modify: the `dbd` skill docs under `docs/skills/dbd/` (the meta / safety-guard section)

- [ ] **Step 1: Locate the meta / guard docs**

Run: `grep -rln "_dbd_meta\|prod guard\|reset is blocked\|safety guard" docs/skills/dbd`
Read the file(s) that describe `_dbd_meta` and the reset prod guard.

- [ ] **Step 2: Add a scope-guard subsection**

In the same section that documents `_dbd_meta` and the prod guard, add prose covering:
- `_dbd_meta` now records the **scope** a database was built with (`scope` column; `NULL` = unpinned).
- `apply`, `deploy`, `reconcile`, and `reset` refuse to run under a different scope than the pinned one, with the message: *"scope guard: this database is pinned to scope 'X', but you requested 'Y'."*
- Override with `--allow-scope-change`, which re-points the database to the new scope on the next successful write. To host multiple modules in one database, define a named scope in `design.yaml` that includes them rather than re-pinning.
- Backward compatible: pre-existing databases are unpinned until their next write pins them.

Match the surrounding doc's heading style and tone. Keep it to a short subsection.

- [ ] **Step 3: Commit**

```bash
git add docs/skills/dbd docs/superpowers/specs/2026-08-10-scope-guard-design.md
git commit -m "docs(dbd): document the scope guard and _dbd_meta.scope"
```

---

## Final verification

- [ ] Run the full gate (same as the pre-commit hook): `cargo test --workspace --quiet && cargo clippy --workspace --all-targets --quiet -- -D warnings` → all green.
- [ ] Manual end-to-end smoke (optional, needs a scratch DB): `dbd apply --scope public` then `dbd apply --scope internal` → the second bails with the scope-guard message; `dbd apply --scope internal --allow-scope-change` succeeds and re-pins.

## Self-Review

- **Spec coverage:** §1 column → Task 1 (Step 3c/3d/3e). §2 `ProjectMeta.scope` → Task 1 (3a). §3 `set_project_meta` + pin → Task 1 (3a–3f). §4 `check_scope_guard` → Task 2. §5 guard call sites (all four CLI handlers) → Task 3 (3c–3f). §6 reset guard-only → Task 3 (3f). T1/T2/T3/T5/T6 → Task 2 unit tests + Task 1 pin test. T4 (forgotten `--scope`) is covered by the same mismatch logic (resolved default ≠ pin) proven in `scope_guard_blocks_mismatch`. T7 reset → Task 3 wiring (guard) + Task 2 logic. T8 round-trip → Task 1 (mock) + Task 4 (sqlite/postgres). T9 reconcile/deploy guarded → Task 3 (3d/3e). Docs → Task 5.
- **Placeholder scan:** none — every step shows exact code or an exact command.
- **Type/signature consistency:** `set_project_meta(&self, env: &str, version: u32, scope: Option<&str>)` is identical across the trait and all four impls and both pin call sites. `check_scope_guard(Option<&ProjectMeta>, &str, bool) -> Result<()>` is called the same way in all four handlers (`force || allow_scope_change` only in `cmd_reset`). `ProjectMeta` field order (`…, version, scope, applied_at`) matches every literal updated in Task 1.
