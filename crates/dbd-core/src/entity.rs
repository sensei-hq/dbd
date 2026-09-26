use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// All entity types supported by dbd.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityType {
    Schema,
    Extension,
    Role,
    Sequence,
    Enum,
    Table,
    View,
    MaterializedView,
    Function,
    Procedure,
    /// A T-SQL trigger. Measured at 107 files in one SQL Server corpus (80
    /// `CREATE`, 27 `ALTER`), so reporting one as a `Function` would be a
    /// visible lie rather than a rounding error.
    ///
    /// dbd does not apply triggers — no emitter produces one and no folder
    /// name maps to it — so this exists to let a *reader* say what it found.
    /// `CREATE TYPE` and `CREATE SYNONYM` are deliberately absent for the
    /// opposite reason: 3 files each in the same corpus.
    Trigger,
    External,
    Import,
}

/// Entity types that live under a schema (file path: `ddl/<type>/<schema>/<name>.ddl`)
pub const TYPES_WITH_SCHEMA: &[EntityType] = &[
    EntityType::Sequence,
    EntityType::Enum,
    EntityType::Table,
    EntityType::View,
    EntityType::MaterializedView,
    EntityType::Function,
    EntityType::Procedure,
];

/// Entity types without schema qualification (file path: `ddl/<type>/<name>.ddl`)
pub const TYPES_WITHOUT_SCHEMA: &[EntityType] = &[EntityType::Role, EntityType::Schema, EntityType::Extension];

impl EntityType {
    /// Parse a type string from a folder name.
    pub fn from_folder_name(name: &str) -> Option<Self> {
        match name {
            "table" | "tables" => Some(Self::Table),
            "view" | "views" => Some(Self::View),
            "materialized_view" | "materialized_views" | "matview" | "matviews" => Some(Self::MaterializedView),
            "function" | "functions" => Some(Self::Function),
            "procedure" | "procedures" => Some(Self::Procedure),
            "enum" | "enums" => Some(Self::Enum),
            "role" | "roles" => Some(Self::Role),
            "sequence" | "sequences" => Some(Self::Sequence),
            _ => None,
        }
    }

    /// Whether this type requires schema qualification.
    pub fn has_schema(&self) -> bool {
        TYPES_WITH_SCHEMA.contains(self)
    }

    /// Lowercase tag for display, e.g. `EntityType::Table` → `"table"`.
    pub fn tag(&self) -> String {
        format!("{self:?}").to_lowercase()
    }

    /// On-disk DDL folder name for this type (e.g. `materialized_view`).
    /// Differs from `tag()` only where the lowercased variant name is not a
    /// readable folder (currently just `MaterializedView`).
    pub fn folder_name(&self) -> String {
        match self {
            EntityType::MaterializedView => "materialized_view".to_string(),
            other => other.tag(),
        }
    }

    /// Apply-order rank, used only to break ties between entities the
    /// dependency graph leaves unordered (see `dependency::sort_by_dependencies`).
    ///
    /// Dependencies always win: a view that calls a function is applied after
    /// it even though View outranks Function, and a function whose body reads a
    /// view is applied after that view. The rank supplies the order for the
    /// relationships Postgres needs but that no `refers` edge records — a table
    /// does not "refer to" its schema, a `DEFAULT nextval('s')` column does not
    /// refer to `s`, and a table owned by a role does not refer to the role.
    pub fn apply_rank(&self) -> u8 {
        match self {
            EntityType::Schema => 0,
            EntityType::Extension => 1,
            EntityType::Role => 2,
            EntityType::Sequence => 3,
            EntityType::Enum => 4,
            EntityType::Table => 5,
            EntityType::View => 6,
            EntityType::MaterializedView => 7,
            EntityType::Function | EntityType::Procedure => 8,
            // A trigger fires on a table and calls a routine, so it is applied
            // after both. dbd does not apply one today — this rank exists so
            // the sort is total rather than because anything sorts by it.
            EntityType::Trigger => 9,
            EntityType::External => 10,
            // Anything else sorts with tables, matching the historical
            // catch-all bucket.
            EntityType::Import => 5,
        }
    }
}

/// Where the schema in a qualified name came from.
///
/// The PostgreSQL reader qualifies a bare `REFERENCES parent` with the first
/// entry on the entity's `search_path`, so the result reads exactly like a
/// source that wrote `app.parent`. Without this, a guess and a statement are
/// the same string.
///
/// That matters to two different callers:
///
/// - **A per-file consumer**, which cannot run
///   [`resolve_references`](crate::references::resolve_references) — that needs
///   the whole entity set — and so would otherwise record a guessed schema as a
///   confident edge.
/// - **The resolver itself**, which used to infer this from the value: "the
///   schema equals `search_path[0]`, so the parser must have supplied it". A
///   source that deliberately writes `app.parent` under `search_path = app`
///   matches that test, and could have its explicit qualification re-pointed.
///
/// Deliberately **not** part of a foreign key's identity or its serialized
/// form — see [`ForeignKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaSource {
    /// The source wrote the schema, or there is no schema to doubt — a T-SQL
    /// name left unqualified is reported unqualified rather than guessed at.
    ///
    /// The default, so a value built by hand or loaded from an older snapshot
    /// does not claim to be a guess.
    #[default]
    Stated,
    /// dbd supplied it from `search_path[0]` because the source wrote a bare
    /// name. **It can be wrong**: the real target may live in another schema on
    /// the path.
    Inferred,
    /// Inferred, then checked against the full entity set and found to name a
    /// real target — either the guess held, or the resolver re-pointed it.
    Resolved,
}

impl SchemaSource {
    /// Whether the schema is dbd's guess rather than the source's word.
    ///
    /// The question a caller usually has. `Resolved` answers `false`: it began
    /// as a guess but has been checked against every entity in the scan.
    pub fn is_guess(self) -> bool {
        self == Self::Inferred
    }

    fn is_stated(&self) -> bool {
        *self == Self::Stated
    }

    /// How far a caller can trust the schema — lower is better.
    ///
    /// Used to pick a winner when the same name is reached twice in one body,
    /// once written and once bare. An explicit ranking rather than a derived
    /// `Ord`, because the declaration order of the variants is about reading
    /// them, not about which one wins.
    pub(crate) fn confidence_rank(self) -> u8 {
        match self {
            Self::Stated => 0,
            Self::Resolved => 1,
            Self::Inferred => 2,
        }
    }
}

/// One entry on a schema path.
///
/// Almost always a named schema. The exception is Postgres's `"$user"`, which
/// is not a schema name but a placeholder for the connecting role's own schema
/// — so it resolves differently per connection and cannot be known while
/// parsing a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathEntry {
    /// A schema, named.
    Schema(String),
    /// Postgres `"$user"` — the connecting role's own schema.
    ///
    /// Recorded rather than dropped, because a caller that *has* a connection
    /// can resolve it, and its **position** matters even to one that cannot:
    /// `"$user", public` means a role-owned table shadows the `public` one.
    /// Never used to qualify a name — doing so produced references to a schema
    /// called `$user`, which cannot exist.
    CurrentUser,
}

/// Where unqualified names in a file resolve, in order.
///
/// Each dialect states this differently: PostgreSQL with `SET search_path TO
/// a, b`, T-SQL and MySQL with `USE db` (which names a database — dbd's
/// [`Entity::catalog`] — and leaves the schema to the connection).
///
/// # `stated` is the load-bearing part
///
/// A file that says nothing is not the same as one that says `public`, and
/// dbd used to report both as `["public"]`. Postgres's actual default is
/// `"$user", public`: given a schema named after the connecting role, a bare
/// `lookup` resolves to `<role>.lookup`. Which applies depends on the role and
/// the database — `ALTER ROLE … SET search_path` and `ALTER DATABASE … SET
/// search_path` both move it — so it is not knowable from the file.
///
/// `stated: false` says "dbd supplied this, the session decides". A consumer
/// that wants to be careful can check it; dbd's own resolver still falls back
/// to `public`, because every existing project's apply path depends on that.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaPath {
    /// The entries, in resolution order. Empty for a dialect that has no such
    /// concept.
    pub entries: Vec<PathEntry>,
    /// Whose answer this is. See [`PathSource`].
    pub source: PathSource,
}

/// Who established a [`SchemaPath`].
///
/// Three possible authors, and the difference is actionable: a caller can
/// trust a file's own statement, should know when it is instead reading the
/// project's blanket default, and must not treat the session default as a fact
/// about anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathSource {
    /// The file itself — `SET search_path TO …`, the convention every dbd DDL
    /// file is expected to open with.
    File,
    /// `source.search_path` in design.yaml. The file said nothing, so the
    /// project's blanket answer applies. **Never silent** — the load reports
    /// every file it had to do this for.
    Project,
    /// Nobody. The file states none and the project configures none, so
    /// whatever the connection happens to have applies: `"$user", public`,
    /// unless a role or database setting moved it. dbd cannot know which.
    #[default]
    SessionDefault,
}

impl SchemaPath {
    /// A path the file itself stated.
    pub fn from_file(entries: Vec<PathEntry>) -> Self {
        Self {
            entries,
            source: PathSource::File,
        }
    }

    /// A path `source.search_path` supplied, for a file that stated none.
    pub fn from_project(entries: Vec<PathEntry>) -> Self {
        Self {
            entries,
            source: PathSource::Project,
        }
    }

    /// The default Postgres applies when nothing else does: the connecting
    /// role's own schema, then `public`.
    pub fn postgres_default() -> Self {
        Self {
            entries: vec![
                PathEntry::CurrentUser,
                PathEntry::Schema(crate::reconcile::DEFAULT_SCHEMA.to_string()),
            ],
            source: PathSource::SessionDefault,
        }
    }

    /// Whether the file itself established this.
    pub fn stated(&self) -> bool {
        self.source == PathSource::File
    }

    /// The schemas on the path, in order, skipping any placeholder.
    ///
    /// What a caller resolving a bare name walks. [`PathEntry::CurrentUser`] is
    /// omitted rather than rendered, so nobody qualifies a name with `$user` by
    /// accident.
    pub fn schemas(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().filter_map(|e| match e {
            PathEntry::Schema(s) => Some(s.as_str()),
            PathEntry::CurrentUser => None,
        })
    }

    /// The first named schema — what dbd qualifies a bare name against.
    pub fn default_schema(&self) -> Option<&str> {
        self.schemas().next()
    }

    /// Whether any entry cannot be resolved without a connection.
    ///
    /// True when the path contains `"$user"`. A caller resolving names offline
    /// should treat a miss as "cannot say" rather than "does not exist".
    pub fn needs_a_connection(&self) -> bool {
        self.entries.contains(&PathEntry::CurrentUser)
    }
}

/// What an entity does to something it refers to.
///
/// Replaces `Reference::ref_type`, an `Option<String>` that carried `None` for
/// reads and writes alike, `Some("function")` for calls, and `Some("table")`
/// at two sites nothing ever read — so it distinguished only "is this a call",
/// and the read/write split lived in two other fields entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    /// `FROM`, `JOIN`, or a foreign key's target. A **hard** dependency: the
    /// target must exist before this entity is applied.
    Reads,
    /// `INSERT`/`UPDATE`/`MERGE`, or the object an `ALTER`/`DROP` names. Also
    /// hard.
    Writes,
    /// `EXEC`, or a schema-qualified call in an expression.
    ///
    /// **Soft**: a body is full of built-in and aggregate calls (`now()`,
    /// `coalesce()`) that look exactly like a call to a project-managed
    /// function, and only the resolver knows which is which. One that does not
    /// resolve is dropped silently rather than warned about.
    Calls,
    /// A role granted to another role.
    ///
    /// Hard, like a read, but deliberately not one: a caller walking data flow
    /// through `reads()` must not find role memberships in it.
    Member,
}

impl RefKind {
    /// Whether an unresolved reference of this kind is dropped rather than
    /// warned about. See [`Self::Calls`].
    pub fn is_soft(self) -> bool {
        self == Self::Calls
    }
}

/// One reference an entity makes.
///
/// Three facts on one row — *what*, *how*, and *how far the schema can be
/// trusted*. They used to be spread across four parallel fields, joinable only
/// by name, and the name is not a key: a routine that both reads and writes
/// one table produced two indistinguishable entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ref {
    /// The target, schema-qualified where a schema is known. See
    /// [`Self::schema_source`] for whether that schema is the source's word.
    pub name: String,
    /// What this entity does to it.
    pub kind: RefKind,
    /// Where the schema in [`Self::name`] came from.
    #[serde(default, skip_serializing_if = "SchemaSource::is_stated")]
    pub schema_source: SchemaSource,
    /// Set by [`resolve_references`] when it could not match this to any known
    /// entity.
    ///
    /// **Marked, not deleted.** The two fields this replaced disagreed on
    /// purpose: `refers` held only what resolved, so a dependency graph never
    /// waited on something that does not exist, while `reads`/`writes` kept
    /// the file's own account, which is what the import plan matches a staging
    /// table against. Dropping the row would lose the second; keeping it
    /// unmarked would break the first. So [`Entity::refers`] skips these and
    /// [`Entity::reads`]/[`Entity::writes`] do not.
    ///
    /// [`resolve_references`]: crate::references::resolve_references
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unresolved: bool,
}

impl Ref {
    /// A reference whose schema the source wrote (or that has none).
    pub fn stated(name: impl Into<String>, kind: RefKind) -> Self {
        Self {
            name: name.into(),
            kind,
            schema_source: SchemaSource::Stated,
            unresolved: false,
        }
    }
}

/// Foreign key constraint with full detail.
///
/// [`Self::ref_schema_source`] is metadata about how dbd learned the schema,
/// not part of what the constraint *is*. Two consequences are load-bearing and
/// both are enforced by tests:
///
/// - It is **excluded from [`PartialEq`]** (hand-written below). A parsed FK
///   whose schema was inferred and an introspected one that states it are the
///   same constraint; if provenance counted, every such pair would read as
///   drift on every run and `reconcile` would never converge.
/// - It is **not serialized** when `Stated`, and never deserialized as
///   anything else by default. A snapshot records the schema, not dbd's
///   epistemics, and writing it would make existing snapshots differ the next
///   time they were generated.
#[derive(Debug, Clone, Default, Eq, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: Option<String>,
    pub columns: Vec<String>,
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: Option<FkAction>,
    pub on_update: Option<FkAction>,
    /// Where [`Self::ref_schema`] came from. See [`SchemaSource`], and the note
    /// on this struct for why it is outside `PartialEq`.
    ///
    /// `skip`, not `skip_serializing_if`: a foreign key reaches a **snapshot**
    /// through [`ColumnDef::inline_fk`] and [`TableConstraint::ForeignKey`], and
    /// a snapshot records what the schema is. Emitting provenance would rewrite
    /// every existing snapshot the next time one was generated — and after
    /// `resolve_references` has run, most previously-bare keys are `Resolved`,
    /// so it would be nearly all of them. Reading one back gives `Stated`,
    /// which is the honest reading: a snapshot states its schemas.
    ///
    /// [`Ref::schema_source`] is *not* skipped, because a [`Ref`] goes to a
    /// consumer rather than into a durable artifact, and that consumer is the
    /// whole reason this exists.
    #[serde(skip)]
    pub ref_schema_source: SchemaSource,
}

impl PartialEq for ForeignKey {
    /// Every field except [`Self::ref_schema_source`].
    ///
    /// Written out by destructuring rather than compared field by field, so a
    /// field added later is a compile error here and has to be decided about
    /// instead of silently left out of equality.
    fn eq(&self, other: &Self) -> bool {
        let Self {
            name,
            columns,
            ref_schema,
            ref_table,
            ref_columns,
            on_delete,
            on_update,
            ref_schema_source: _,
        } = self;
        *name == other.name
            && *columns == other.columns
            && *ref_schema == other.ref_schema
            && *ref_table == other.ref_table
            && *ref_columns == other.ref_columns
            && *on_delete == other.on_delete
            && *on_update == other.on_update
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FkAction {
    Cascade,
    Restrict,
    SetNull,
    SetDefault,
    NoAction,
}

impl FkAction {
    /// DBML FK action keyword for this action (used in `delete:`/`update:` settings).
    pub fn as_dbml(&self) -> &'static str {
        match self {
            FkAction::Cascade => "cascade",
            FkAction::Restrict => "restrict",
            FkAction::SetNull => "set null",
            FkAction::SetDefault => "set default",
            FkAction::NoAction => "no action",
        }
    }

    /// Map a DBML FK action keyword to an [`FkAction`]. `no action` → `Some(FkAction::NoAction)`
    /// (matches the exporter, which emits the keyword; the round-trip keeps `NoAction`
    /// distinguishable from "no FK action specified").
    pub fn from_dbml(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cascade" => Some(FkAction::Cascade),
            "restrict" => Some(FkAction::Restrict),
            "set null" => Some(FkAction::SetNull),
            "set default" => Some(FkAction::SetDefault),
            "no action" => Some(FkAction::NoAction),
            _ => None,
        }
    }
}

/// Table-level constraint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TableConstraint {
    PrimaryKey {
        name: Option<String>,
        columns: Vec<String>,
    },
    Unique {
        name: Option<String>,
        columns: Vec<String>,
        /// `UNIQUE NULLS NOT DISTINCT (…)` — NULLs collide instead of duplicating,
        /// so it changes what the constraint enforces and two UNIQUEs that differ
        /// only here are not the same constraint. Mirrors [`IndexDef::nulls_not_distinct`]
        /// for the inline-constraint spelling.
        ///
        /// `#[serde(default)]` so snapshots written before this field existed still
        /// deserialize as the `NULLS DISTINCT` default.
        #[serde(default)]
        nulls_not_distinct: bool,
    },
    ForeignKey(ForeignKey),
    Check {
        name: Option<String>,
        expression: String,
    },
}

/// How an identity column generates its value (`GENERATED … AS IDENTITY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind {
    /// `GENERATED ALWAYS AS IDENTITY` (`pg_attribute.attidentity = 'a'`).
    Always,
    /// `GENERATED BY DEFAULT AS IDENTITY` (`pg_attribute.attidentity = 'd'`).
    ByDefault,
}

/// Parsed column definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default_value: Option<String>,
    pub is_pk: bool,
    pub is_unique: bool,
    /// Identity generation, when the column is `GENERATED … AS IDENTITY`.
    /// `None` for non-identity columns. Replaces the former `is_identity: bool`.
    /// `#[serde(default)]` so snapshots written before this field existed (which
    /// carried `is_identity`) still deserialize — the obsolete field is ignored.
    #[serde(default)]
    pub identity: Option<IdentityKind>,
    /// The expression of a `GENERATED ALWAYS AS (<expr>) STORED` computed column
    /// (`pg_attribute.attgenerated = 's'`), or `None`.
    ///
    /// Distinct from [`Self::identity`], which is the sequence-backed
    /// `GENERATED … AS IDENTITY`, and it must stay distinct from
    /// [`Self::default_value`]: Postgres exposes the generation expression
    /// through `pg_attrdef`, exactly where an ordinary `DEFAULT` lives. Reading
    /// it as a default made reconcile plan
    /// `ALTER COLUMN … DROP DEFAULT`, which Postgres refuses with
    /// *"column … is a generated column"* — aborting every reconcile on any
    /// project containing one (issue #16).
    ///
    /// `#[serde(default)]` so snapshots written before this field existed still
    /// deserialize.
    #[serde(default)]
    pub generated: Option<String>,
    pub comment: Option<String>,
    pub inline_fk: Option<ForeignKey>,
}

/// Index definition.
///
/// Models everything about a Postgres index that distinguishes it from another
/// index on the same columns, because reconcile compares an authored index
/// against an introspected one: anything this struct cannot hold is invisible to
/// that comparison and shows up as permanent, never-converging drift.
///
/// Every field past `index_type` is `#[serde(default)]` so snapshots written
/// before it existed still deserialize.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IndexDef {
    pub name: Option<String>,
    pub columns: Vec<IndexColumn>,
    pub unique: bool,
    pub index_type: Option<IndexType>,
    /// `WHERE` predicate of a partial index, canonicalized via
    /// [`crate::sql_expr::canonicalize_predicate`] so the authored spelling and
    /// `pg_get_expr`'s analyzed one compare equal.
    #[serde(default)]
    pub predicate: Option<String>,
    /// Non-key payload columns: `INCLUDE (a, b)`.
    #[serde(default)]
    pub include: Vec<String>,
    /// `NULLS NOT DISTINCT` — makes NULLs collide instead of duplicate, so it
    /// changes what a UNIQUE index actually enforces.
    #[serde(default)]
    pub nulls_not_distinct: bool,
    /// Access-method storage parameters: `WITH (m = 16, ef_construction = 64)`.
    /// Keyed and ordered by name so an authored and a `reloptions` ordering
    /// compare equal.
    #[serde(default)]
    pub with_options: std::collections::BTreeMap<String, String>,
}

/// One entry in an index's key list: a column, or an expression over columns.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IndexColumn {
    /// A column name, or the expression text when [`Self::is_expression`].
    pub name: String,
    /// Whether `name` holds an expression (`(context ->> 'module')`, `lower(x)`)
    /// rather than a column name. Emitters must not quote an expression as an
    /// identifier — that produces `column "(context ->> 'module')" does not exist`.
    #[serde(default)]
    pub is_expression: bool,
    pub order: Option<SortOrder>,
    /// `NULLS FIRST` (`Some(true)`) / `NULLS LAST` (`Some(false)`); `None` for
    /// the access method's default.
    #[serde(default)]
    pub nulls_first: Option<bool>,
    /// Operator class, e.g. `vector_cosine_ops` — picks which operators the
    /// index can answer, so two indexes differing only here are not the same.
    #[serde(default)]
    pub opclass: Option<String>,
}

/// An index access method. Postgres's set is open-ended (extensions add `hnsw`,
/// `ivfflat`, `bloom`, …), so unrecognized methods round-trip through
/// [`IndexType::Other`] rather than collapsing to the btree default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexType {
    Btree,
    Hash,
    Gin,
    Gist,
    Brin,
    /// SP-GiST. Serializes as `spgist` (Postgres's `pg_am.amname`).
    #[serde(rename = "spgist")]
    SpGist,
    /// Any other access method, held as its `pg_am.amname`.
    Other(String),
}

impl IndexType {
    /// The `pg_am.amname` for this access method — what `USING <method>` takes.
    pub fn amname(&self) -> &str {
        match self {
            IndexType::Btree => "btree",
            IndexType::Hash => "hash",
            IndexType::Gin => "gin",
            IndexType::Gist => "gist",
            IndexType::Brin => "brin",
            IndexType::SpGist => "spgist",
            IndexType::Other(name) => name,
        }
    }

    /// Parse a `pg_am.amname`, mapping the methods dbd names explicitly and
    /// keeping anything else verbatim.
    pub fn from_amname(amname: &str) -> Self {
        match amname.to_lowercase().as_str() {
            "btree" => IndexType::Btree,
            "hash" => IndexType::Hash,
            "gin" => IndexType::Gin,
            "gist" => IndexType::Gist,
            "brin" => IndexType::Brin,
            "spgist" => IndexType::SpGist,
            other => IndexType::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    Asc,
    Desc,
}

/// Table and column comments.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TableComments {
    pub table: Option<String>,
    pub columns: HashMap<String, String>,
}

/// Full parsed table structure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableDef {
    pub columns: Vec<ColumnDef>,
    pub constraints: Vec<TableConstraint>,
    pub indexes: Vec<IndexDef>,
    pub comments: TableComments,
}

/// Enum variant with optional note.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumValue {
    pub name: String,
    pub note: Option<String>,
}

/// The central data structure. All DDL objects flow through this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub entity_type: EntityType,
    pub name: String,
    pub schema: Option<String>,
    /// The database this entity lives in, when that is part of its identity.
    ///
    /// `None` for PostgreSQL, always: cross-database references are not
    /// possible on one connection, so every entity a scan sees belongs to the
    /// same database and naming it would add a level that distinguishes
    /// nothing.
    ///
    /// `Some` for the dialects where it does distinguish something.
    /// `OtherDb.dbo.Users` is an ordinary reference in a SQL Server codebase,
    /// and MySQL's `db.users` puts the *database* where this model expects a
    /// schema. Without this level, `dbo.Users` in two databases is one entity
    /// and a scan across a multi-database repository merges them silently.
    ///
    /// Deliberately **not** folded into [`Self::name`], which stays
    /// `schema.name` — every existing caller reads it that way.
    /// [`Self::qualified_key`] is the composite, and resolution uses that.
    ///
    /// `#[serde(default, skip_serializing_if)]` so snapshots written before
    /// this field existed still load, and a catalog-less entity does not start
    /// writing it — otherwise every snapshot in every project churns on the
    /// next write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    pub file: Option<PathBuf>,
    pub format: Option<String>,
    /// Every reference this entity makes — see [`Ref`].
    ///
    /// One list rather than the four parallel ones it replaces, because the
    /// three facts a caller needs (name, direction, schema provenance) belong
    /// on the same row. [`Self::reads`], [`Self::writes`], [`Self::calls`] and
    /// [`Self::refers`] are views over it.
    #[serde(default)]
    pub refs: Vec<Ref>,
    /// Where unqualified names in this entity's file resolve — see
    /// [`SchemaPath`]. Every entity carries it, for every dialect that has the
    /// concept; a consumer resolving a bare reference needs it and cannot
    /// recover it from the name.
    ///
    /// `#[serde(default)]` because this replaced a `search_paths: Vec<String>`
    /// that was required. Anything serialized before now carries the old key,
    /// which serde ignores — without the default, every such document would
    /// fail to load with `missing field`. An absent path deserializes as
    /// unstated and empty, which is the truth: that document never recorded
    /// one.
    #[serde(default)]
    pub schema_path: SchemaPath,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// The entity's own DDL text, verbatim, for the types whose `CREATE` is
    /// reconstructed from a body rather than composed from a structured model:
    /// a view's or matview's `SELECT`, a sequence's whole `CREATE`, and one
    /// string per routine overload.
    ///
    /// Separate from [`Self::refs`] because it used to share a field with
    /// them. `writes` held table names when a parser filled it and body SQL
    /// when the introspector did, and `emit_view`/`emit_sequence`/
    /// `emit_routine` read `writes[0]` back out as the body. Nothing mixed the
    /// two in practice — `emit_entity` only ever sees introspected entities,
    /// plus parsed matviews whose parser followed the same convention — but
    /// one field meaning two things is a trap regardless.
    ///
    /// Distinct from [`Self::raw_ddl`], which bypasses the emitter entirely:
    /// this is a *fragment* the emitter wraps in the right `CREATE`.
    #[serde(default)]
    pub body: Vec<String>,
    pub table_def: Option<TableDef>,
    pub enum_values: Vec<EnumValue>,
    /// Verbatim DDL to emit as-is, bypassing the structured emitter. Used by sources
    /// that already hold the exact `CREATE …` text (e.g. SQLite's `sqlite_master.sql`).
    /// `None` for the Postgres/DBML paths, which reconstruct DDL from the structured model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_ddl: Option<String>,
}

impl Entity {
    /// The references of one kind.
    pub fn refs_of(&self, kind: RefKind) -> impl Iterator<Item = &Ref> {
        self.refs.iter().filter(move |r| r.kind == kind)
    }

    /// Tables and views this entity reads.
    pub fn reads(&self) -> impl Iterator<Item = &Ref> {
        self.refs_of(RefKind::Reads)
    }

    /// Tables this entity writes.
    pub fn writes(&self) -> impl Iterator<Item = &Ref> {
        self.refs_of(RefKind::Writes)
    }

    /// Routines this entity calls. Soft — see [`RefKind::Calls`].
    pub fn calls(&self) -> impl Iterator<Item = &Ref> {
        self.refs_of(RefKind::Calls)
    }

    /// Every name this entity refers to, deduplicated, in first-seen order.
    ///
    /// What a dependency graph wants, so it **omits** anything
    /// [`Ref::unresolved`] — waiting on an entity that does not exist would
    /// deadlock the topological sort. [`Self::reads`] and [`Self::writes`] do
    /// include them, because they are the file's own account.
    ///
    /// Deduplicated because one table can be both read and written, which is
    /// two [`Ref`]s but one edge — the old `refers: Vec<String>` listed such a
    /// name twice.
    pub fn refers(&self) -> impl Iterator<Item = &str> {
        let mut seen = std::collections::HashSet::new();
        self.refs
            .iter()
            .filter(|r| !r.unresolved)
            .map(|r| r.name.as_str())
            .filter(move |n| seen.insert(*n))
    }

    /// Whether this entity refers to `name`, in any way.
    pub fn refers_to(&self, name: &str) -> bool {
        self.refs.iter().any(|r| r.name == name)
    }

    /// Record a reference, ignoring one already recorded with the same name
    /// and kind.
    pub fn push_ref(&mut self, r: Ref) {
        if !self.refs.iter().any(|x| x.name == r.name && x.kind == r.kind) {
            self.refs.push(r);
        }
    }

    /// Replace every reference of the given kinds with `refs`.
    ///
    /// For a producer that recomputes one direction at a time; the kinds it
    /// does not name are left alone.
    pub fn set_refs_of(&mut self, kinds: &[RefKind], refs: Vec<Ref>) {
        self.refs.retain(|r| !kinds.contains(&r.kind));
        for r in refs {
            self.push_ref(r);
        }
    }

    /// Create a new empty entity with the given type and name.
    pub fn new(entity_type: EntityType, name: &str) -> Self {
        let (schema, _) = split_qualified_name(name);
        Self {
            entity_type,
            name: name.to_string(),
            schema,
            catalog: None,
            file: None,
            format: None,
            refs: Vec::new(),
            schema_path: SchemaPath::default(),
            errors: Vec::new(),
            warnings: Vec::new(),
            body: Vec::new(),
            table_def: None,
            enum_values: Vec::new(),
            raw_ddl: None,
        }
    }

    /// Create an entity from a DDL file path.
    ///
    /// Path format: `ddl/<type>/<schema>/<name>.ddl` (schema-scoped)
    ///              `ddl/<type>/<name>.ddl` (non-schema types like role)
    pub fn from_file(path: &Path) -> Self {
        let parts: Vec<&str> = path.components().filter_map(|c| c.as_os_str().to_str()).collect();

        // Find the "ddl" component and use everything after it.
        // This supports both relative paths ("ddl/table/...") and
        // absolute paths ("/path/to/fixtures/ddl/table/...").
        let ddl_pos = parts.iter().rposition(|&p| p == "ddl");
        let parts = match ddl_pos {
            Some(pos) => &parts[pos + 1..],
            None => &parts,
        };

        // An unrecognized folder name (typo, unsupported kind) is recorded as an
        // error on the entity rather than silently classified as a Table.
        let folder = parts.first().copied().unwrap_or("");
        let recognized_type = EntityType::from_folder_name(folder);
        let entity_type = recognized_type.unwrap_or(EntityType::Table);

        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("ddl");

        let (name, schema) = if entity_type.has_schema() && parts.len() >= 3 {
            let schema = parts[1].to_string();
            let qualified = format!("{}.{}", schema, stem);
            (qualified, Some(schema))
        } else {
            (stem.to_string(), None)
        };

        let mut entity = Self::new(entity_type, &name);
        entity.schema = schema;
        entity.file = Some(path.to_path_buf());
        entity.format = Some(ext.to_string());
        if recognized_type.is_none() {
            entity.errors.push(format!(
                "unrecognized DDL folder '{folder}' (expected table/view/materialized_view/function/procedure/enum/role/sequence)"
            ));
        }
        entity
    }

    /// Create a schema entity.
    pub fn schema(name: &str) -> Self {
        Self::new(EntityType::Schema, name)
    }

    /// Create an external entity (FK stub).
    pub fn external(name: &str) -> Self {
        Self::new(EntityType::External, name)
    }

    /// Create an import entity from a data file path.
    ///
    /// Path format: `import/<schema>/<name>.<ext>` or `import/<env>/<schema>/<name>.<ext>`
    /// Returns the entity with the file path set, format derived from extension.
    pub fn from_import_file(path: &Path) -> Self {
        let parts: Vec<&str> = path.components().filter_map(|c| c.as_os_str().to_str()).collect();

        // Find "import" in the path and work from there
        let import_pos = parts.iter().rposition(|&p| p == "import");
        let after_import = match import_pos {
            Some(pos) => &parts[pos + 1..],
            None => &parts,
        };

        // Detect env prefix: import/dev/staging/file.csv → env=dev, schema=staging
        // vs import/staging/file.csv → env=None, schema=staging
        let (_env, schema_and_rest) =
            if after_import.len() >= 3 && (after_import[0] == "dev" || after_import[0] == "prod") {
                (Some(after_import[0].to_string()), &after_import[1..])
            } else {
                (None, after_import)
            };

        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("csv");

        let (name, schema) = if schema_and_rest.len() >= 2 {
            let schema = schema_and_rest[0].to_string();
            (format!("{schema}.{stem}"), Some(schema))
        } else {
            (stem.to_string(), None)
        };

        let mut entity = Self::new(EntityType::Import, &name);
        entity.schema = schema;
        entity.file = Some(path.to_path_buf());
        entity.format = Some(ext.to_string());
        entity
    }

    /// The key resolution matches on: `catalog.schema.name`, or `schema.name`
    /// when there is no catalog.
    ///
    /// Byte-identical to [`Self::name`] for every catalog-less entity, which is
    /// every entity in every PostgreSQL project — so introducing the level is
    /// invisible to them.
    pub fn qualified_key(&self) -> String {
        match &self.catalog {
            Some(catalog) => format!("{catalog}.{}", self.name),
            None => self.name.clone(),
        }
    }

    /// Whether this entity has validation errors.
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

/// Split "schema.name" into (Some("schema"), "name"), or (None, "name").
pub fn split_qualified_name(name: &str) -> (Option<String>, String) {
    match name.split_once('.') {
        Some((schema, entity)) => (Some(schema.to_string()), entity.to_string()),
        None => (None, name.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn entity_type_from_folder_name() {
        assert_eq!(EntityType::from_folder_name("table"), Some(EntityType::Table));
        assert_eq!(EntityType::from_folder_name("view"), Some(EntityType::View));
        assert_eq!(EntityType::from_folder_name("function"), Some(EntityType::Function));
        assert_eq!(EntityType::from_folder_name("procedure"), Some(EntityType::Procedure));
        assert_eq!(EntityType::from_folder_name("enum"), Some(EntityType::Enum));
        assert_eq!(EntityType::from_folder_name("role"), Some(EntityType::Role));
        assert_eq!(EntityType::from_folder_name("sequence"), Some(EntityType::Sequence));
        assert_eq!(EntityType::from_folder_name("sequences"), Some(EntityType::Sequence));
        assert_eq!(EntityType::from_folder_name("unknown"), None);
    }

    #[test]
    fn entity_type_has_schema() {
        assert!(EntityType::Table.has_schema());
        assert!(EntityType::View.has_schema());
        assert!(EntityType::Enum.has_schema());
        assert!(EntityType::Sequence.has_schema());
        assert_eq!(EntityType::Sequence.tag(), "sequence");
        assert!(!EntityType::Role.has_schema());
        assert!(!EntityType::Schema.has_schema());
    }

    #[test]
    fn entity_from_table_file() {
        let entity = Entity::from_file(Path::new("ddl/table/config/lookups.ddl"));
        assert_eq!(entity.entity_type, EntityType::Table);
        assert_eq!(entity.name, "config.lookups");
        assert_eq!(entity.schema, Some("config".to_string()));
        assert_eq!(entity.file, Some(PathBuf::from("ddl/table/config/lookups.ddl")));
        assert_eq!(entity.format, Some("ddl".to_string()));
    }

    #[test]
    fn entity_from_view_file() {
        let entity = Entity::from_file(Path::new("ddl/view/config/genders.ddl"));
        assert_eq!(entity.entity_type, EntityType::View);
        assert_eq!(entity.name, "config.genders");
        assert_eq!(entity.schema, Some("config".to_string()));
    }

    #[test]
    fn entity_from_procedure_file() {
        let entity = Entity::from_file(Path::new("ddl/procedure/staging/import_lookups.ddl"));
        assert_eq!(entity.entity_type, EntityType::Procedure);
        assert_eq!(entity.name, "staging.import_lookups");
        assert_eq!(entity.schema, Some("staging".to_string()));
    }

    #[test]
    fn entity_from_enum_file() {
        let entity = Entity::from_file(Path::new("ddl/enum/config/status.sql"));
        assert_eq!(entity.entity_type, EntityType::Enum);
        assert_eq!(entity.name, "config.status");
        assert_eq!(entity.schema, Some("config".to_string()));
        assert_eq!(entity.format, Some("sql".to_string()));
    }

    #[test]
    fn entity_from_role_file() {
        let entity = Entity::from_file(Path::new("ddl/role/admin.ddl"));
        assert_eq!(entity.entity_type, EntityType::Role);
        assert_eq!(entity.name, "admin");
        assert_eq!(entity.schema, None);
    }

    #[test]
    fn entity_new_with_qualified_name() {
        let entity = Entity::new(EntityType::Table, "config.lookups");
        assert_eq!(entity.name, "config.lookups");
        assert_eq!(entity.schema, Some("config".to_string()));
    }

    #[test]
    fn entity_new_with_unqualified_name() {
        let entity = Entity::new(EntityType::Role, "admin");
        assert_eq!(entity.name, "admin");
        assert_eq!(entity.schema, None);
    }

    #[test]
    fn entity_schema_constructor() {
        let entity = Entity::schema("config");
        assert_eq!(entity.entity_type, EntityType::Schema);
        assert_eq!(entity.name, "config");
    }

    #[test]
    fn split_qualified_name_with_schema() {
        let (schema, name) = split_qualified_name("config.lookups");
        assert_eq!(schema, Some("config".to_string()));
        assert_eq!(name, "lookups");
    }

    #[test]
    fn split_qualified_name_without_schema() {
        let (schema, name) = split_qualified_name("admin");
        assert_eq!(schema, None);
        assert_eq!(name, "admin");
    }

    #[test]
    fn entity_has_errors() {
        let mut entity = Entity::new(EntityType::Table, "test");
        assert!(!entity.has_errors());
        entity.errors.push("missing file".to_string());
        assert!(entity.has_errors());
    }

    #[test]
    fn fk_action_serializes() {
        let fk = ForeignKey {
            on_delete: Some(FkAction::Cascade),
            on_update: Some(FkAction::NoAction),
            ..Default::default()
        };
        let json = serde_json::to_string(&fk).unwrap();
        assert!(json.contains("cascade"));
        assert!(json.contains("no_action"));
    }

    // ── EX1: External entity constructor ─────────────────

    #[test]
    fn ex1_external_entity_constructor() {
        let entity = Entity::external("pg_catalog.pg_type");
        assert_eq!(entity.entity_type, EntityType::External);
        assert_eq!(entity.name, "pg_catalog.pg_type");
        assert_eq!(entity.schema, Some("pg_catalog".to_string()));
    }

    // ── IF1: Import entity from CSV file ─────────────────

    #[test]
    fn if1_import_entity_from_csv_file() {
        let entity = Entity::from_import_file(Path::new("import/staging/lookups.csv"));
        assert_eq!(entity.entity_type, EntityType::Import);
        assert_eq!(entity.name, "staging.lookups");
        assert_eq!(entity.schema, Some("staging".to_string()));
        assert_eq!(entity.format, Some("csv".to_string()));
    }

    // ── IF2: Import entity from TSV file ─────────────────

    #[test]
    fn if2_import_entity_from_tsv_file() {
        let entity = Entity::from_import_file(Path::new("import/staging/data.tsv"));
        assert_eq!(entity.entity_type, EntityType::Import);
        assert_eq!(entity.name, "staging.data");
        assert_eq!(entity.format, Some("tsv".to_string()));
    }

    #[test]
    fn entity_type_from_folder_name_matview() {
        assert_eq!(
            EntityType::from_folder_name("materialized_view"),
            Some(EntityType::MaterializedView)
        );
        assert_eq!(
            EntityType::from_folder_name("materialized_views"),
            Some(EntityType::MaterializedView)
        );
        assert_eq!(
            EntityType::from_folder_name("matview"),
            Some(EntityType::MaterializedView)
        );
        assert_eq!(
            EntityType::from_folder_name("matviews"),
            Some(EntityType::MaterializedView)
        );
    }

    #[test]
    fn matview_has_schema_and_folder_name() {
        assert!(EntityType::MaterializedView.has_schema());
        assert_eq!(EntityType::MaterializedView.folder_name(), "materialized_view");
        assert_eq!(EntityType::Table.folder_name(), "table");
    }

    #[test]
    fn entity_from_matview_file() {
        let e = Entity::from_file(Path::new("ddl/materialized_view/analytics/daily_sales.ddl"));
        assert_eq!(e.entity_type, EntityType::MaterializedView);
        assert_eq!(e.name, "analytics.daily_sales");
        assert_eq!(e.schema, Some("analytics".to_string()));
    }
}
