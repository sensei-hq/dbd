use std::collections::{HashMap, HashSet};
use std::path::Path;

use async_trait::async_trait;

use crate::entity::{Entity, EntityType};
use crate::error::Result;

/// Classification result for a reference name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceClass {
    /// Built-in function, type, or operator (pg_catalog, SQL standard)
    Internal,
    /// From a specific database extension
    Extension(String),
    /// Not recognized — treated as a project entity dependency
    UserDefined,
}

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

/// One thing an object in the database needs from another: what Postgres
/// records in `pg_depend` — a column's type, a default's function, a view's
/// table, an index's operator class — read at the level of whole objects.
///
/// Reverse-engineering a few schemas uses it to find what the captured
/// objects need from outside them: the extensions to declare, and the types,
/// functions and tables to name, so a scoped capture says what a new database
/// must already have. A routine's body is not in `pg_depend` (PL/pgSQL is
/// checked when it runs), so what one calls is not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    /// The schema of the object that depends.
    pub dependent_schema: String,
    /// The object that depends, qualified — `app.t`, `util.prio()`.
    pub dependent: String,
    /// What kind of object it needs, as Postgres names it — `type`,
    /// `function`, `table`, `sequence`.
    pub referenced_kind: String,
    /// The schema of the object it needs, when it has one.
    pub referenced_schema: Option<String>,
    /// The object it needs, qualified — `ref.mood`, `core.accounts`; a
    /// column or index is reported as its table.
    pub referenced: String,
    /// The extension that owns the object it needs, if one does, and the
    /// schema that extension installed into.
    pub extension: Option<(String, String)>,
}

/// Catalog data loaded from the database for reference classification.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CatalogData {
    /// Qualified function names: "pg_catalog.array_agg", "extensions.st_distance"
    pub functions: HashSet<String>,
    /// Qualified type names: "pg_catalog.int4", "public.geometry"
    pub types: HashSet<String>,
    /// Bare name -> extension name: "st_distance" -> "postgis"
    pub extension_objects: HashMap<String, String>,
    /// Schemas owned by extensions
    pub extension_schemas: HashSet<String>,
}

/// The database adapter trait — each target implements this.
///
/// Adapters handle:
/// - Connection lifecycle
/// - DDL execution (apply entities)
/// - Data operations (import/export via COPY or equivalent)
/// - Catalog queries (reference classification, entity resolution)
/// - Migration tracking (_dbd_migrations + _dbd_meta)
#[async_trait]
pub trait DatabaseAdapter: Send + Sync {
    // ── Lifecycle ───────────────────────────────────────

    async fn connect(&mut self) -> Result<()>;
    async fn disconnect(&mut self) -> Result<()>;
    async fn test_connection(&self) -> Result<bool>;

    // ── DDL execution ──────────────────────────────────

    async fn execute_script(&self, sql: &str) -> Result<()>;

    async fn apply_entity(&self, entity: &Entity) -> Result<()>;

    async fn apply_entities(&self, entities: &[Entity]) -> Result<()> {
        for entity in entities {
            self.apply_entity(entity).await?;
        }
        Ok(())
    }

    /// Return true if the adapter wants all entities at once (e.g., Convex).
    fn prefers_batch_apply(&self) -> bool {
        false
    }

    /// Whether this target has a SQL grant model (`GRANT` / `ALTER DEFAULT
    /// PRIVILEGES`). Postgres/Supabase only — SQLite and Convex have no grant
    /// concept, so the apply path skips grant emission for them rather than
    /// feeding them Postgres-only DDL. Default `false`.
    fn supports_schema_grants(&self) -> bool {
        false
    }

    // ── Batch transaction (atomic apply) ───────────────
    //
    // When an adapter's backend has transactional DDL, `Design::apply` wraps the
    // whole execution plan in one transaction: begin → run every step → commit,
    // or rollback on the first failure so an interrupted upgrade leaves the schema
    // at its prior version rather than half-applied. Adapters opt in by returning
    // `true` here and routing their DDL/meta writes through the open transaction
    // between `begin_batch` and `commit_batch`/`rollback_batch`.

    /// True if an entire apply batch can run inside one transaction (the backend
    /// has transactional DDL). Default `false` — apply runs step-by-step as today.
    fn supports_transactional_apply(&self) -> bool {
        false
    }

    /// Open a batch transaction. Until `commit_batch`/`rollback_batch`, DDL and
    /// meta writes route through it. Default is a no-op.
    async fn begin_batch(&self) -> Result<()> {
        Ok(())
    }

    /// Commit the open batch transaction. Default is a no-op.
    async fn commit_batch(&self) -> Result<()> {
        Ok(())
    }

    /// Roll back the open batch transaction. Default is a no-op.
    async fn rollback_batch(&self) -> Result<()> {
        Ok(())
    }

    // ── Materialized-view refresh scheduling ────────────

    /// Live materialized-view drift state: `"schema.name"` → the drift stamp
    /// recorded for it (see [`Self::stamp_matview`]), or else the object's raw
    /// comment text, where a dbd before 0.25 kept the stamp (`None` when there
    /// is neither). An ABSENT key
    /// means the matview does not exist. Deliberately unparsed here — this
    /// trait is public crate API, while `reconcile::Sentinel` (the
    /// parsed `dbd:hash` stamp) is an internal reconcile detail; callers run it
    /// through `reconcile::parse_dbd_hash` to decide create / skip /
    /// restamp / recreate per matview. Default: an empty map (targets that
    /// don't expose materialized views).
    async fn matview_states(&self) -> Result<std::collections::HashMap<String, Option<String>>> {
        Ok(std::collections::HashMap::new())
    }

    /// The deployed materialized views, each with its definition as the
    /// database prints it (`body[0]`) and its indexes — what `introspect`
    /// returns for them, without the rest. Default: none (targets without
    /// materialized views).
    async fn live_matviews(&self) -> Result<Vec<Entity>> {
        Ok(Vec::new())
    }

    /// Record `stamp` — a `dbd:hash` sentinel — as the drift stamp of
    /// materialized view `qualified` (`schema.name`). Kept apart from the view's
    /// own comment, which belongs to its author. Default: no-op (targets without
    /// materialized views).
    async fn stamp_matview(&self, _qualified: &str, _stamp: &str) -> Result<()> {
        Ok(())
    }

    /// Forget the drift stamps of the materialized views `qualified`
    /// (`schema.name`), which the caller has just dropped. A stamp that outlives
    /// its view describes nothing, and would vouch for a later view of the same
    /// name that dbd never built. Default: no-op (targets without materialized
    /// views).
    async fn forget_matview_stamps(&self, _qualified: &[String]) -> Result<()> {
        Ok(())
    }

    /// Sync pg_cron refresh jobs for the given (qualified_name, ResolvedMatview)
    /// set. Default: no-op (targets without pg_cron support).
    async fn sync_refresh_jobs(&self, _jobs: &[(String, crate::config::ResolvedMatview)]) -> Result<()> {
        Ok(())
    }

    /// Refresh one matview now. Default: unsupported.
    async fn refresh_matview(&self, _qualified: &str, _concurrently: bool) -> Result<()> {
        Err(crate::error::DbdError::Config(
            "REFRESH MATERIALIZED VIEW is not supported by this target".into(),
        ))
    }

    // ── Data operations ────────────────────────────────

    /// Load a data file into a table. `null_value` is the sentinel string that
    /// maps to SQL NULL (empty string = the default, meaning an empty cell is NULL).
    async fn import_data(&self, entity: &Entity, null_value: &str, dry_run: bool) -> Result<()>;

    /// Empty `table` (a `schema.name`) before an import reloads it — the
    /// `import.options.truncate` step.
    ///
    /// Routed through the adapter because the statement is not portable: the
    /// default is SQL's `TRUNCATE TABLE`, which Postgres runs, but SQLite has no
    /// `TRUNCATE` and Convex runs no SQL at all. Sending the Postgres statement
    /// to every target made the default import fail on both.
    async fn truncate_table(&self, table: &str) -> Result<()> {
        self.execute_script(&format!("TRUNCATE TABLE {}", crate::sql_quote::qualified(table)))
            .await
    }

    /// Export a table's data to a file.
    ///
    /// `out_dir`:
    /// - `Some(dir)` → write `dir/<name>.<format>` (the directory is created if
    ///   needed); `<name>` is the bare table name (any `schema.` prefix stripped).
    /// - `None` → the folder convention: `export/<schema>/<name>.<format>`.
    async fn export_data(&self, entity: &Entity, out_dir: Option<&Path>) -> Result<()>;

    async fn batch_export(&self, entities: &[Entity]) -> Result<()> {
        for entity in entities {
            self.export_data(entity, None).await?;
        }
        Ok(())
    }

    // ── Catalog — adapter-owned knowledge ──────────────

    /// Load the catalog of built-in functions, types, and extension objects.
    /// Called once after connect. Results cached for the session.
    async fn load_catalog(&mut self) -> Result<()>;

    /// Classify a reference as internal, extension, or user-defined.
    fn classify_reference(&self, name: &str, installed_extensions: &[String]) -> ReferenceClass;

    /// Check if an entity exists in the database catalog.
    async fn resolve_entity(&self, name: &str) -> Result<Option<String>>;

    /// List all user-defined entities (tables, views, enum types) in the
    /// database, returned as schema-qualified names (e.g. `auth.users`).
    /// System catalogs (pg_catalog, information_schema, pg_toast) are excluded.
    ///
    /// Used by `inspect --from-db` to build a project-local reference cache
    /// for offline lookups.
    async fn list_entities(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Which of `objects` — each a kind and a name: `schema.name` for a
    /// schema-qualified kind, the bare name of a role, schema or extension —
    /// exist now.
    ///
    /// `reset` asks before and after it drops, so it reports what it dropped
    /// rather than what it ran: every drop is `IF EXISTS`. Default: tables and
    /// views, from [`Self::list_entities`] — all a target without schemas,
    /// roles or extensions (SQLite) has.
    async fn existing_objects(&self, objects: &[(EntityType, String)]) -> Result<HashSet<(EntityType, String)>> {
        let live: HashSet<String> = self.list_entities().await?.into_iter().collect();
        Ok(objects
            .iter()
            .filter(|(kind, name)| {
                let (_, bare) = crate::entity::split_qualified_name(name);
                matches!(kind, EntityType::Table | EntityType::View) && (live.contains(name) || live.contains(&bare))
            })
            .cloned()
            .collect())
    }

    /// The other databases on this server where each of `roles` holds
    /// anything — a privilege, a default privilege, an object, or the database
    /// itself — as role → those databases' names, sorted. A role missing from
    /// the map holds nothing elsewhere.
    ///
    /// Roles are server-wide, so a role this design declares is shared by
    /// every database on the server built from it. `reset` keeps one another
    /// database holds rather than break that database. Default: none (targets
    /// without server-wide roles).
    async fn roles_held_elsewhere(&self, _roles: &[String]) -> Result<HashMap<String, Vec<String>>> {
        Ok(HashMap::new())
    }

    /// Introspect the live database and return a complete `Vec<Entity>` covering
    /// schemas, extensions, enums, tables (with columns, constraints, indexes,
    /// and comments), and views — in that order.
    ///
    /// The default implementation returns an unsupported error. Adapters that
    /// support full introspection (currently only `PostgresAdapter`) override this.
    async fn introspect(&self) -> Result<Vec<Entity>> {
        Err(crate::error::DbdError::Config(
            "introspect() is not supported for this adapter".to_string(),
        ))
    }

    /// What objects in the database need from other objects, at the level of
    /// whole objects and across schemas or from extensions — see
    /// [`Dependency`]. Default: nothing known (a source with no catalog of
    /// dependencies); Postgres overrides.
    async fn dependencies(&self) -> Result<Vec<Dependency>> {
        Ok(Vec::new())
    }

    /// Reverse-engineering safety: `Some(version)` if this DB is dbd-managed (a
    /// `_dbd_meta` table exists in ANY schema), reading the applied version for
    /// `self.project` — `0` if the table exists but has no matching row. `None`
    /// for a foreign DB (no `_dbd_meta`). Default returns `Ok(None)`; Postgres
    /// overrides. Robust to `_dbd_meta` living outside the search_path.
    ///
    /// Note: `_dbd_meta` has `PRIMARY KEY (project)` — exactly one row per
    /// project. `env` records the *last-applied* environment and is not part of
    /// the key, so this read is keyed on `project` only (env-agnostic).
    async fn reverse_managed_version(&self) -> Result<Option<u32>> {
        Ok(None)
    }

    /// Reverse-engineer cluster-global roles (name + role memberships only — no
    /// attributes, never passwords). Returns one `Entity { entity_type: Role }`
    /// per kept role, with `refers` = the granted roles that are also kept (so
    /// the emitted set is self-contained). Platform/managed roles (superusers,
    /// `pg_*`/cloud-provider/Supabase roles, and the named denylist) are excluded
    /// — see [`crate::reverse::role_is_managed`].
    ///
    /// This is NOT part of `introspect()`: roles are cluster-global and managed
    /// clusters are dominated by platform roles, so the CLI calls this separately
    /// and appends the results only when `--roles` is set. Default returns
    /// `Ok(vec![])`; Postgres overrides.
    async fn introspect_roles(&self) -> Result<Vec<Entity>> {
        Ok(Vec::new())
    }

    // ── Migration tracking ─────────────────────────────

    /// Ensure bookkeeping storage exists and is at the current layout, healing any
    /// legacy/mislocated bookkeeping in place. Idempotent; safe to call at the start
    /// of every ownership operation (apply/deploy/reconcile/reset/migrate). Runs
    /// before any version/guard read so those reads see consistent state.
    async fn heal_bookkeeping(&self) -> Result<()>;
    async fn get_db_version(&self) -> Result<u32>;
    async fn apply_migration(&self, version: u32, sql: &str, description: &str, checksum: &str) -> Result<()>;
    async fn clear_project_migrations(&self) -> Result<()>;
    /// Forget the database's record of this project — its env, version and
    /// scope pin — as a full `reset` leaves nothing of the project behind.
    async fn forget_project_meta(&self) -> Result<()>;

    // ── Internal dbd procedures ────────────────────────

    /// Ensure the internal `dbd.import_jsonb_to_table` procedure exists.
    /// An adapter whose JSON import needs it calls this from `import_data`
    /// itself, so no caller has to know the precondition. The procedure is
    /// embedded in the dbd binary — users do not own or manage it.
    async fn ensure_import_procedure(&self) -> Result<()>;

    // ── Meta tracking (environment, safety guards) ─────

    async fn get_project_meta(&self) -> Result<Option<ProjectMeta>>;
    async fn set_project_meta(&self, env: &str, version: u32, scope: Option<&str>) -> Result<()>;
}

/// The records of a JSON data file, each as its own JSON text.
///
/// `jsonl` is one record per line. A `json` file is read in either shape,
/// decided by its first non-blank character: a JSON array of records — what
/// `.json` means to most tools, Convex's importer among them — or JSON lines,
/// which is how dbd has always read it. A JSON-lines record is an object, so a
/// file of them never starts with `[`.
///
/// One reader for every SQL adapter, so a file that loads on one target loads
/// on the others. Records come back as raw text rather than parsed values:
/// Postgres parses each as `jsonb` itself, keeping numeric precision that a
/// trip through `serde_json`'s `f64` would lose.
pub(crate) fn json_records(data: &str, format: &str, file: &Path) -> Result<Vec<String>> {
    if format == "json" && data.trim_start().starts_with('[') {
        let records: Vec<&serde_json::value::RawValue> = serde_json::from_str(data).map_err(|e| {
            crate::error::DbdError::Config(format!("{}: not a JSON array of records: {e}", file.display()))
        })?;
        return Ok(records.iter().map(|r| r.get().to_string()).collect());
    }
    Ok(data
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

/// One field of a CSV or TSV record.
pub(crate) struct Field {
    pub value: String,
    /// Whether the field was quoted. Quoting is how CSV tells an empty string
    /// (`""`) from NULL (an empty field): Postgres's COPY writes them so, and
    /// never reads a quoted field as NULL.
    pub quoted: bool,
}

/// A CSV or TSV data file, read a record at a time.
///
/// RFC 4180, as Postgres's `COPY … (FORMAT csv)` reads it: a quoted field may
/// hold the delimiter, a line break and a doubled quote; nothing is trimmed;
/// a TSV is the same format with a tab between fields (`DELIMITER E'\t'`).
/// A blank line is skipped, as SQLite's import always did (Postgres's COPY
/// refuses one in a table of more than one column). csv-core does the parsing; this adds what it does not
/// report — whether a field was quoted, and a quote left open at the end of
/// the file, which csv-core would read as one value holding every row after
/// it. One reader for every SQL adapter, so a file reads the same on each.
pub(crate) struct Delimited<'a> {
    data: &'a str,
    file: &'a Path,
    reader: csv_core::Reader,
    /// Offset of the next byte csv-core has not read.
    at: usize,
    /// Offset of the first byte of the record last read.
    record_at: usize,
}

impl<'a> Delimited<'a> {
    pub(crate) fn new(data: &'a str, tab: bool, file: &'a Path) -> Self {
        // A byte-order mark (spreadsheet exports start with one) is not part
        // of the first field — skipped here, not by csv-core, so the first
        // byte of every field is what decides whether it was quoted.
        let at = if data.starts_with('\u{feff}') {
            '\u{feff}'.len_utf8()
        } else {
            0
        };
        Self {
            data,
            file,
            reader: csv_core::ReaderBuilder::new()
                .delimiter(if tab { b'\t' } else { b',' })
                .build(),
            at,
            record_at: at,
        }
    }

    /// The next record, or `None` at the end of the file.
    pub(crate) fn next_record(&mut self) -> Result<Option<Vec<Field>>> {
        let bytes = self.data.as_bytes();
        let mut fields = Vec::new();
        let mut value = Vec::new();
        // The field's first byte. csv-core consumes the blank lines before a
        // record, and the `\n` of a CRLF, as part of the field that follows.
        let mut start: Option<usize> = None;
        let mut out = [0u8; 8192];
        loop {
            let input = &bytes[self.at..];
            let (result, nin, nout) = self.reader.read_field(input, &mut out);
            if start.is_none() {
                start = input[..nin]
                    .iter()
                    .position(|b| !matches!(b, b'\r' | b'\n'))
                    .map(|i| self.at + i);
                if let Some(first) = start
                    && fields.is_empty()
                {
                    self.record_at = first;
                }
            }
            self.at += nin;
            value.extend_from_slice(&out[..nout]);
            match result {
                csv_core::ReadFieldResult::InputEmpty | csv_core::ReadFieldResult::OutputFull => continue,
                csv_core::ReadFieldResult::End => return Ok(None),
                csv_core::ReadFieldResult::Field { record_end } => {
                    let quoted = start.is_some_and(|s| bytes[s] == b'"');
                    if quoted && self.at == bytes.len() {
                        self.refuse_an_open_quote(start.unwrap_or(self.at))?;
                    }
                    let value = String::from_utf8(std::mem::take(&mut value)).map_err(|e| {
                        crate::error::DbdError::Config(format!("{}: not UTF-8: {e}", self.file.display()))
                    })?;
                    fields.push(Field { value, quoted });
                    start = None;
                    if record_end {
                        return Ok(Some(fields));
                    }
                }
            }
        }
    }

    /// Refuse a quoted field that ran to the end of the file unclosed. Its
    /// quotes are the opening one, doubled pairs, and the closing one — an
    /// odd count means the closing one never came.
    fn refuse_an_open_quote(&self, start: usize) -> Result<()> {
        let quotes = self.data.as_bytes()[start..].iter().filter(|b| **b == b'"').count();
        if quotes % 2 == 0 {
            return Ok(());
        }
        Err(crate::error::DbdError::Config(format!(
            "{}: the quote opened on line {} is never closed — every row after it would load as part \
             of one value",
            self.file.display(),
            self.line_of(start)
        )))
    }

    /// The data file, for messages about it.
    pub(crate) fn file(&self) -> &Path {
        self.file
    }

    /// The line the record last read starts on, counting from 1.
    pub(crate) fn line(&self) -> usize {
        self.line_of(self.record_at)
    }

    fn line_of(&self, offset: usize) -> usize {
        1 + self.data.as_bytes()[..offset].iter().filter(|b| **b == b'\n').count()
    }

    /// Offset into the data where the next record begins — the rows still to
    /// read, as written.
    pub(crate) fn offset(&self) -> usize {
        // csv-core ends a CRLF record at its `\r` and leaves the `\n` to the
        // next read. Rows handed on from there start with a `\n` that COPY
        // reads as a row of its own.
        if self.data[..self.at].ends_with('\r') && self.data[self.at..].starts_with('\n') {
            self.at + 1
        } else {
            self.at
        }
    }
}

/// A CSV or TSV data file's header: the column names it loads, and the byte
/// offset where its rows begin. `None` for a file with no header — no columns,
/// nothing to load. Read by [`Delimited`], so a header may quote a name.
pub(crate) fn delimited_header(data: &str, tab: bool, file: &Path) -> Result<Option<(Vec<String>, usize)>> {
    let mut reader = Delimited::new(data, tab, file);
    Ok(reader
        .next_record()?
        .map(|header| (header.into_iter().map(|f| f.value).collect(), reader.offset())))
}

/// The columns of `table` a data file's header names, in the header's order —
/// the column list its rows load into.
///
/// A name matches a column as Postgres resolves it: exactly, as a quoted
/// identifier, or folded to lower case, as an unquoted one — so `Name` loads
/// `name`. A name `columns` lacks is refused, as is a column named twice:
/// loading the file anyway, by position or without that column, puts values
/// where the file did not say. One reader for every SQL adapter, so a file
/// loads into the same columns on each.
pub(crate) fn header_columns(header: &[String], columns: &[String], table: &str, file: &Path) -> Result<Vec<String>> {
    let mut matched: Vec<String> = Vec::with_capacity(header.len());
    let mut unknown: Vec<String> = Vec::new();
    for name in header {
        let folded = name.to_ascii_lowercase();
        match columns
            .iter()
            .find(|c| *c == name)
            .or_else(|| columns.iter().find(|c| **c == folded))
        {
            Some(column) if matched.contains(column) => {
                return Err(crate::error::DbdError::Config(format!(
                    "{}: the header names column {column} of {table} twice",
                    file.display()
                )));
            }
            Some(column) => matched.push(column.clone()),
            None => unknown.push(format!("'{name}'")),
        }
    }
    if unknown.is_empty() {
        return Ok(matched);
    }
    Err(crate::error::DbdError::Config(format!(
        "{}: the header names {} but {table} has no such column — its columns are {}. A data file's \
         header names the columns its values load into",
        file.display(),
        unknown.join(", "),
        columns.join(", ")
    )))
}

/// The keys `records` hold, each once — refusing a JSON record holding a key
/// `table` has no column for, naming each such key, the first record holding
/// it, and the file — as [`header_columns`] refuses a CSV header naming one.
///
/// A key is a column name as written: the load matches keys to columns
/// exactly. Postgres's JSON load moved only the keys matching a column and
/// dropped the rest without a word, so a misspelled key left its column empty
/// while the import reported success. A record that is not a JSON object is
/// refused here too, before anything is staged.
pub(crate) fn check_json_keys(records: &[String], columns: &[String], table: &str, file: &Path) -> Result<Vec<String>> {
    let mut present: Vec<String> = Vec::new();
    let mut unknown: Vec<(String, usize)> = Vec::new();
    for (at, record) in records.iter().enumerate() {
        let keys: std::collections::BTreeMap<String, serde::de::IgnoredAny> =
            serde_json::from_str(record).map_err(|e| {
                crate::error::DbdError::Config(format!(
                    "{}: record {} is not a JSON object: {e}",
                    file.display(),
                    at + 1
                ))
            })?;
        for key in keys.into_keys() {
            if !columns.contains(&key) {
                if !unknown.iter().any(|(k, _)| *k == key) {
                    unknown.push((key, at + 1));
                }
            } else if !present.contains(&key) {
                present.push(key);
            }
        }
    }
    if unknown.is_empty() {
        return Ok(present);
    }
    let named: Vec<String> = unknown
        .iter()
        .map(|(key, record)| format!("'{key}' (record {record})"))
        .collect();
    Err(crate::error::DbdError::Config(format!(
        "{}: the key {} names no column of {table} — its columns are {}. A JSON record's keys name the \
         columns its values load into",
        file.display(),
        named.join(", "),
        columns.join(", ")
    )))
}

/// Refuse a data file that sets a generated column of `table` — any of
/// `loaded` (the columns its header or keys name) that is in `generated`.
///
/// A generated column's value is computed from the row, so a file cannot set
/// one. COPY refused it with "column is a generated column" and the JSON load
/// dropped it without a word; both now say which column, and that the file
/// should leave it out — as `dbd export` does.
pub(crate) fn refuse_generated(loaded: &[String], generated: &[String], table: &str, file: &Path) -> Result<()> {
    let set: Vec<&str> = loaded
        .iter()
        .filter(|c| generated.contains(c))
        .map(String::as_str)
        .collect();
    if set.is_empty() {
        return Ok(());
    }
    Err(crate::error::DbdError::Config(format!(
        "{}: {} of {table} {} generated — computed from the row, so a data file cannot set it. Leave it \
         out of the file; dbd export writes no generated column",
        file.display(),
        set.join(", "),
        if set.len() == 1 { "is" } else { "are" }
    )))
}

pub mod convex;
pub mod mock;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(test)]
mod tests {
    use super::*;

    // ── Reading a CSV/TSV header ─────────────────────────────────────────────

    fn header_of(data: &str) -> Option<(Vec<String>, String)> {
        delimited_header(data, false, Path::new("t.csv"))
            .unwrap()
            .map(|(names, at)| (names, data[at..].to_string()))
    }

    /// The rows Postgres is handed start exactly after the header, whatever
    /// ends its line — a CRLF header must not leave a `\n` row behind.
    #[test]
    fn the_rows_begin_right_after_the_header_line() {
        let (names, rows) = header_of("a,b\r\n1,2\r\n").unwrap();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(rows, "1,2\r\n");
        let (_, rows) = header_of("a,b").unwrap();
        assert_eq!(rows, "", "a header-only file has no rows");
    }

    /// A quoted header name may hold the delimiter, and a doubled quote is one.
    #[test]
    fn a_header_name_is_read_as_csv() {
        let (names, _) = header_of("\"a,b\",\"say \"\"hi\"\"\"\n1,2\n").unwrap();
        assert_eq!(names, ["a,b", "say \"hi\""]);
    }

    /// Spreadsheet exports start with a byte-order mark; it is not part of the
    /// first column's name.
    #[test]
    fn a_byte_order_mark_is_not_part_of_the_first_name() {
        let (names, rows) = header_of("\u{feff}a,b\n1,2\n").unwrap();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(rows, "1,2\n");
    }

    #[test]
    fn an_empty_file_has_no_header() {
        assert!(header_of("").is_none());
    }

    #[test]
    fn a_tsv_header_is_split_on_tabs() {
        let (names, _) = delimited_header("a b\tc\n", true, Path::new("t.tsv")).unwrap().unwrap();
        assert_eq!(names, ["a b", "c"]);
    }

    /// A value longer than the reader's buffer arrives in pieces; it is still
    /// one value, still quoted, and the record after it starts where it should.
    #[test]
    fn a_value_longer_than_the_read_buffer_is_one_field() {
        let long = format!("{},{}", "x".repeat(20_000), "y".repeat(20_000));
        let data = format!("v,w\n\"{long}\",b\nc,d\n");
        let mut reader = Delimited::new(&data, false, Path::new("t.csv"));
        reader.next_record().unwrap().unwrap();
        let row = reader.next_record().unwrap().unwrap();
        assert_eq!(row[0].value, long);
        assert!(row[0].quoted && !row[1].quoted);
        let next = reader.next_record().unwrap().unwrap();
        assert_eq!((next[0].value.as_str(), reader.line()), ("c", 3));
        assert!(reader.next_record().unwrap().is_none());
    }

    // ── Matching a header to the table's columns ─────────────────────────────

    fn columns(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_header_maps_to_columns_in_its_own_order() {
        let got = header_columns(
            &columns(&["name", "code"]),
            &columns(&["code", "name"]),
            "t",
            Path::new("f"),
        );
        assert_eq!(got.unwrap(), ["name", "code"]);
    }

    /// `Name` resolves to `name`, as an unquoted identifier would; an exact
    /// match wins over the folded one, as a quoted identifier would.
    #[test]
    fn a_header_name_matches_as_postgres_resolves_it() {
        let got = header_columns(&columns(&["Name"]), &columns(&["name"]), "t", Path::new("f"));
        assert_eq!(got.unwrap(), ["name"]);
        let got = header_columns(&columns(&["Name"]), &columns(&["name", "Name"]), "t", Path::new("f"));
        assert_eq!(got.unwrap(), ["Name"]);
        // Folding is one way: a lower-case header cannot name a mixed-case column.
        let got = header_columns(&columns(&["name"]), &columns(&["Name"]), "t", Path::new("f"));
        assert!(got.is_err());
    }

    #[test]
    fn a_column_named_twice_is_refused() {
        let err = header_columns(&columns(&["code", "CODE"]), &columns(&["code"]), "t", Path::new("f"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("code") && err.contains("twice"), "{err}");
    }

    /// Every unknown name at once, quoted so a stray space shows.
    #[test]
    fn every_unknown_header_name_is_named() {
        let err = header_columns(
            &columns(&["code", " name", "iso"]),
            &columns(&["code", "name"]),
            "staging.c",
            Path::new("f"),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("' name'") && err.contains("'iso'") && err.contains("staging.c"),
            "{err}"
        );
    }

    #[test]
    fn reference_class_equality() {
        assert_eq!(ReferenceClass::Internal, ReferenceClass::Internal);
        assert_eq!(
            ReferenceClass::Extension("postgis".into()),
            ReferenceClass::Extension("postgis".into())
        );
        assert_ne!(ReferenceClass::Internal, ReferenceClass::UserDefined);
    }

    #[test]
    fn c1_pg_catalog_function_is_internal() {
        let mut catalog = CatalogData::default();
        catalog.functions.insert("pg_catalog.array_agg".to_string());
        assert!(catalog.functions.contains("pg_catalog.array_agg"));
    }

    #[test]
    fn c4_sql_noise_keywords_present() {
        // SQL noise keywords are checked via PostgresAdapter::is_sql_noise,
        // but we verify common noise words are recognized patterns.
        let noise_words = ["varchar", "integer", "now", "coalesce", "count"];
        for word in noise_words {
            assert!(
                [
                    "varchar",
                    "int",
                    "integer",
                    "bigint",
                    "smallint",
                    "numeric",
                    "decimal",
                    "boolean",
                    "text",
                    "date",
                    "timestamp",
                    "timestamptz",
                    "uuid",
                    "jsonb",
                    "json",
                    "bytea",
                    "float",
                    "double",
                    "real",
                    "serial",
                    "bigserial",
                    "btree",
                    "hash",
                    "gin",
                    "gist",
                    "brin",
                    "now",
                    "coalesce",
                    "nullif",
                    "greatest",
                    "least",
                    "extract",
                    "count",
                    "sum",
                    "avg",
                    "min",
                    "max",
                    "string_agg",
                    "row_number",
                    "rank",
                    "dense_rank",
                    "lead",
                    "lag",
                    "upper",
                    "lower",
                    "trim",
                    "length",
                    "replace",
                    "substring",
                    "cast",
                    "exists",
                    "between",
                    "like",
                    "in",
                    "not",
                    "and",
                    "or",
                    "true",
                    "false",
                    "null",
                    "default",
                    "current_user",
                    "localtime",
                    "localtimestamp",
                    "random",
                    "floor",
                    "ceil",
                    "abs",
                    "round",
                    "enum",
                    "record",
                    "void",
                    "trigger",
                    "event_trigger",
                ]
                .contains(&word),
                "{word} should be SQL noise"
            );
        }
    }

    #[test]
    fn c10_catalog_data_default_is_empty() {
        let catalog = CatalogData::default();
        assert!(catalog.functions.is_empty());
        assert!(catalog.types.is_empty());
        assert!(catalog.extension_objects.is_empty());
        assert!(catalog.extension_schemas.is_empty());
    }

    #[test]
    fn c11_catalog_data_serialization_roundtrip() {
        let mut catalog = CatalogData::default();
        catalog.functions.insert("pg_catalog.array_agg".to_string());
        catalog
            .extension_objects
            .insert("st_distance".to_string(), "postgis".to_string());
        let json = serde_json::to_string(&catalog).unwrap();
        let deserialized: CatalogData = serde_json::from_str(&json).unwrap();
        assert!(deserialized.functions.contains("pg_catalog.array_agg"));
        assert_eq!(
            deserialized.extension_objects.get("st_distance"),
            Some(&"postgis".to_string())
        );
    }
}
