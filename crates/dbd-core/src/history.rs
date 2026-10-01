//! A project's changelog, read from its snapshots (#29).
//!
//! Each `snapshots/NNN.json` is the schema as it stood when that version was cut,
//! so what a version *changed* is the difference from the one before it. That is
//! what [`crate::diff::diff`] already computes for migrations; this reuses it and
//! [`crate::diff::classify_changes`] rather than re-deriving them, so the changelog
//! and the migrations can never describe the same step two different ways.
//!
//! Three things a naive consecutive diff gets wrong, and this does not:
//!
//! - **A multi-stage change is one version.** A column rename or type change is
//!   cut as `"… (stage 1/2)"` then `"… (stage 2/2)"`, and stage 1 holds a
//!   synthetic `<col>_new` column that never existed in the DDL. The stages are
//!   grouped and diffed end to end, so the reader sees one rename, not an add and
//!   a drop of a column they never wrote.
//! - **A type spelled two ways is not a change.** Both sides go through the same
//!   canonicalisation snapshot creation uses, or `varchar(32)` against
//!   `character varying(32)` would read as a type change on every column.
//! - **Order is stable.** `diff` iterates hash maps; everything here is sorted.
//!
//! The first snapshot is a baseline: it records counts, not every table as
//! "added", which is a long list that says nothing a reader can act on — and
//! which the share link would carry on every diagram.
//!
//! Snapshots hold tables and enums only, so views and routines have no history
//! yet; and a project that has never been released has no snapshots at all.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::diff::{self, ChangeAction, ComplexChange, DiffAction, FieldChange, FieldDetail, FieldType};
use crate::entity::{ColumnDef, EntityType, IdentityKind, IndexDef, TableConstraint};
use crate::error::{DbdError, Result};
use crate::snapshot::{self, Snapshot};

/// One version of the schema: what it changed since the version before it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub version: u32,
    /// The last stage's version, when this version was cut in several stages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through: Option<u32>,
    /// The snapshot's description, without any `(stage k/n)` suffix.
    pub description: String,
    /// When the version was completed: the last stage's timestamp.
    pub timestamp: String,
    /// Present on the first version only: what the schema held when history began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<Baseline>,
    pub changes: Vec<EntityChange>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub tables: usize,
    pub enums: usize,
}

/// A table or enum that was added, removed or modified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityChange {
    pub kind: EntityKind,
    pub schema: String,
    pub name: String,
    pub op: Op,
    /// What changed inside a modified entity. Empty for an added or removed one.
    pub fields: Vec<FieldEdit>,
}

/// A column, constraint, index or enum value inside a modified entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldEdit {
    pub kind: FieldKind,
    pub name: String,
    pub op: Op,
    /// The definition before — for a removal or modification; for a rename, the old name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The definition after — for an addition or modification; for a rename, the new name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// What changed when the definition reads the same before and after (a comment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityKind {
    Table,
    Enum,
}

/// Declared in reading order: a table's columns first, then what constrains them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Column,
    Constraint,
    Index,
    Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Added,
    Removed,
    Modified,
    Renamed,
}

/// Read every snapshot under `project_dir` and describe what each version changed.
///
/// Oldest first. A project with no `snapshots/` has an empty history. A snapshot
/// that cannot be read is an error rather than a skipped version: a history with a
/// silent hole would attribute that version's changes to the next one.
pub fn load(project_dir: &Path) -> Result<Vec<HistoryEntry>> {
    let mut snapshots = Vec::new();
    for info in snapshot::list_snapshots(project_dir) {
        let snap = snapshot::read_snapshot(info.version, project_dir)?.ok_or_else(|| {
            DbdError::Migration(format!("snapshot {} is listed but cannot be read", info.file.display()))
        })?;
        snapshots.push(snapshot::canonical_types(&snap));
    }
    Ok(from_snapshots(&snapshots))
}

/// The history of an ordered run of snapshots, already canonicalised.
fn from_snapshots(snapshots: &[Snapshot]) -> Vec<HistoryEntry> {
    let mut entries = Vec::new();
    let mut i = 0;
    while i < snapshots.len() {
        let end = i + stage_run(&snapshots[i..]);
        let (first, last) = (&snapshots[i], &snapshots[end - 1]);
        let (description, _) = split_stage(&first.description);
        let mut entry = HistoryEntry {
            version: first.version,
            through: (end - i > 1).then_some(last.version),
            description: description.to_string(),
            timestamp: last.timestamp.clone(),
            baseline: None,
            changes: Vec::new(),
        };
        match i.checked_sub(1).map(|prev| &snapshots[prev]) {
            None => {
                entry.baseline = Some(Baseline {
                    tables: last.tables.len(),
                    enums: last.enums.len(),
                })
            }
            Some(before) => entry.changes = changes_between(before, last),
        }
        entries.push(entry);
        i = end;
    }
    entries
}

/// How many snapshots from the start of `run` make up one version: the stages of
/// a multi-stage change, or just the one.
fn stage_run(run: &[Snapshot]) -> usize {
    let (base, stage) = split_stage(&run[0].description);
    let Some((1, total)) = stage else { return 1 };
    let mut len = 1;
    while len < run.len() && len < total {
        match split_stage(&run[len].description) {
            (b, Some((k, t))) if b == base && t == total && k == len + 1 => len += 1,
            _ => break,
        }
    }
    len
}

/// `"rename nick (stage 1/2)"` → `("rename nick", Some((1, 2)))`.
fn split_stage(description: &str) -> (&str, Option<(usize, usize)>) {
    let parsed = description.strip_suffix(')').and_then(|rest| {
        let (base, stage) = rest.rsplit_once(" (stage ")?;
        let (k, n) = stage.split_once('/')?;
        Some((base, (k.parse().ok()?, n.parse().ok()?)))
    });
    match parsed {
        Some((base, stage)) => (base, Some(stage)),
        None => (description, None),
    }
}

/// Everything that changed from `before` to `after`, sorted.
fn changes_between(before: &Snapshot, after: &Snapshot) -> Vec<EntityChange> {
    let diffs = diff::diff(before, after);
    let (simple, complex) = diff::classify_changes(&diffs, before);

    let mut by_entity: Entities = BTreeMap::new();

    for d in &simple {
        let Some(kind) = entity_kind(d.entity_type) else {
            continue;
        };
        let entity = entity_mut(&mut by_entity, &d.entity_name, kind);
        match &d.action {
            DiffAction::Add => entity.op = Op::Added,
            DiffAction::Drop => entity.op = Op::Removed,
            DiffAction::Change(fields) => entity.fields.extend(fields.iter().map(field_edit)),
        }
    }
    for c in &complex {
        match c {
            ComplexChange::ColumnTypeChange {
                table_name,
                column_name,
                old_col,
                new_col,
                ..
            } => {
                entity_mut(&mut by_entity, table_name, EntityKind::Table)
                    .fields
                    .push(FieldEdit {
                        kind: FieldKind::Column,
                        name: column_name.clone(),
                        op: Op::Modified,
                        from: Some(column_signature(old_col)),
                        to: Some(column_signature(new_col)),
                        note: None,
                    });
            }
            ComplexChange::ColumnRename {
                table_name,
                old_name,
                new_name,
                ..
            } => {
                entity_mut(&mut by_entity, table_name, EntityKind::Table)
                    .fields
                    .push(FieldEdit {
                        kind: FieldKind::Column,
                        name: new_name.clone(),
                        op: Op::Renamed,
                        from: Some(old_name.clone()),
                        to: Some(new_name.clone()),
                        note: None,
                    });
            }
            ComplexChange::EnumValueRemoval {
                enum_name,
                removed_values,
                ..
            } => {
                let entity = entity_mut(&mut by_entity, enum_name, EntityKind::Enum);
                entity
                    .fields
                    .extend(removed_values.iter().map(|v| bare(FieldKind::Value, v, Op::Removed)));
            }
        }
    }

    let mut out: Vec<EntityChange> = by_entity.into_values().collect();
    for entity in &mut out {
        entity.fields.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
    }
    out
}

/// Keyed `(schema, name, kind)` so the output comes out sorted for free.
type Entities = BTreeMap<(String, String, EntityKind), EntityChange>;

/// The change for one entity, created as a modification on first touch: a
/// later `Add`/`Drop` overrides the op, and field edits accumulate either way.
fn entity_mut<'a>(by_entity: &'a mut Entities, entity_name: &str, kind: EntityKind) -> &'a mut EntityChange {
    let (schema, name) = split_name(entity_name);
    by_entity
        .entry((schema.clone(), name.clone(), kind))
        .or_insert(EntityChange {
            kind,
            schema,
            name,
            op: Op::Modified,
            fields: Vec::new(),
        })
}

fn entity_kind(t: EntityType) -> Option<EntityKind> {
    match t {
        EntityType::Table => Some(EntityKind::Table),
        EntityType::Enum => Some(EntityKind::Enum),
        _ => None,
    }
}

/// `"app.orders"` → `("app", "orders")`. A snapshot always qualifies its names.
fn split_name(entity_name: &str) -> (String, String) {
    match entity_name.split_once('.') {
        Some((schema, name)) => (schema.to_string(), name.to_string()),
        None => (String::new(), entity_name.to_string()),
    }
}

fn bare(kind: FieldKind, name: &str, op: Op) -> FieldEdit {
    FieldEdit {
        kind,
        name: name.to_string(),
        op,
        from: None,
        to: None,
        note: None,
    }
}

fn field_edit(change: &FieldChange) -> FieldEdit {
    let kind = match change.field_type {
        FieldType::Column => FieldKind::Column,
        FieldType::Constraint => FieldKind::Constraint,
        FieldType::Index => FieldKind::Index,
        FieldType::EnumValue => FieldKind::Value,
    };
    match &change.action {
        ChangeAction::Add(detail) => described(kind, Op::Added, None, Some(detail)),
        ChangeAction::Drop(detail) => described(kind, Op::Removed, Some(detail), None),
        ChangeAction::Alter { old, new } => {
            let mut edit = described(kind, Op::Modified, Some(old), Some(new));
            if edit.from == edit.to {
                edit.note = Some(match (old.as_ref(), new.as_ref()) {
                    (FieldDetail::Column(a), FieldDetail::Column(b)) if a.comment != b.comment => "comment".into(),
                    _ => "definition".into(),
                });
                edit.from = None;
                edit.to = None;
            }
            edit
        }
    }
}

/// A field edit named and described by what the object is.
///
/// A constraint's matching key is synthetic (`fk:customer_id`), so it is named by
/// its definition instead; an index by its name when it has one. Columns carry
/// their definition in `from`/`to`; constraints and enum values are their name.
fn described(kind: FieldKind, op: Op, from: Option<&FieldDetail>, to: Option<&FieldDetail>) -> FieldEdit {
    let detail = to.or(from).expect("an edit has a side");
    let (name, define): (String, fn(&FieldDetail) -> Option<String>) = match detail {
        FieldDetail::Column(c) => (c.name.clone(), |d| match d {
            FieldDetail::Column(c) => Some(column_signature(c)),
            _ => None,
        }),
        FieldDetail::Constraint(tc) => (constraint_name(tc), |_| None),
        FieldDetail::Index(ix) => (ix.name.clone().unwrap_or_else(|| index_definition(ix)), |d| match d {
            FieldDetail::Index(ix) => Some(index_definition(ix)),
            _ => None,
        }),
        FieldDetail::EnumValue(v) => (v.clone(), |_| None),
    };
    FieldEdit {
        kind,
        name,
        op,
        from: from.and_then(define),
        to: to.and_then(define),
        note: None,
    }
}

/// A column as a reader would declare it: `text not null default 'pending'`.
fn column_signature(c: &ColumnDef) -> String {
    let mut s = c.data_type.clone();
    if c.is_pk {
        s.push_str(" primary key");
    } else if !c.nullable {
        s.push_str(" not null");
    }
    if c.is_unique {
        s.push_str(" unique");
    }
    match c.identity {
        Some(IdentityKind::Always) => s.push_str(" generated always as identity"),
        Some(IdentityKind::ByDefault) => s.push_str(" generated by default as identity"),
        None => {}
    }
    if let Some(expr) = &c.generated {
        s.push_str(&format!(" generated always as ({expr}) stored"));
    }
    if let Some(d) = &c.default_value {
        s.push_str(&format!(" default {d}"));
    }
    if let Some(fk) = &c.inline_fk {
        s.push_str(&format!(
            " references {} ({})",
            qualified(&fk.ref_schema, &fk.ref_table),
            fk.ref_columns.join(", ")
        ));
    }
    s
}

fn constraint_name(tc: &TableConstraint) -> String {
    match tc {
        TableConstraint::PrimaryKey { columns, .. } => format!("primary key ({})", columns.join(", ")),
        TableConstraint::Unique { columns, .. } => format!("unique ({})", columns.join(", ")),
        TableConstraint::ForeignKey(fk) => format!(
            "foreign key ({}) → {} ({})",
            fk.columns.join(", "),
            qualified(&fk.ref_schema, &fk.ref_table),
            fk.ref_columns.join(", ")
        ),
        TableConstraint::Check { expression, .. } => format!("check ({expression})"),
    }
}

fn index_definition(ix: &IndexDef) -> String {
    let columns: Vec<&str> = ix.columns.iter().map(|c| c.name.as_str()).collect();
    let mut s = format!("{}({})", if ix.unique { "unique " } else { "" }, columns.join(", "));
    if let Some(t) = &ix.index_type {
        s.push_str(&format!(
            " using {}",
            serde_json::to_value(t)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        ));
    }
    if let Some(p) = &ix.predicate {
        s.push_str(&format!(" where {p}"));
    }
    s
}

fn qualified(schema: &Option<String>, name: &str) -> String {
    match schema {
        Some(s) if !s.is_empty() => format!("{s}.{name}"),
        _ => name.to_string(),
    }
}
