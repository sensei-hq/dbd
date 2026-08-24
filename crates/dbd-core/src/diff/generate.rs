use crate::entity::{EntityType, TableConstraint};

use super::types::*;

/// Analyze diffs for risky changes that may need to be split across two migrations.
///
/// Returns warnings for:
/// - Column type changes (may need data correction)
/// - Possible renames (column dropped + column added with same type)
/// - Enum value drops (data may reference removed values)
pub fn migration_warnings(diffs: &[MigrationDiff]) -> Vec<String> {
    let mut warnings = Vec::new();

    for d in diffs {
        let DiffAction::Change(ref changes) = d.action else {
            if matches!(d.action, DiffAction::Drop) && d.entity_type == EntityType::Enum {
                warnings.push(format!(
                    "Enum '{}' dropped — ensure no columns reference this type before applying",
                    d.entity_name
                ));
            }
            continue;
        };

        warnings.extend(type_change_warnings(&d.entity_name, changes));
        warnings.extend(rename_warnings(&d.entity_name, changes));
        warnings.extend(enum_value_drop_warnings(&d.entity_name, changes));
    }

    warnings
}

/// Warn about column type changes (suggest a two-snapshot split).
fn type_change_warnings(entity_name: &str, changes: &[FieldChange]) -> Vec<String> {
    let mut warnings = Vec::new();
    for change in changes {
        if change.field_type == FieldType::Column
            && let ChangeAction::Alter { ref old, ref new } = change.action
            && let (FieldDetail::Column(old_col), FieldDetail::Column(new_col)) =
                (old.as_ref(), new.as_ref())
            && old_col.data_type != new_col.data_type
        {
            warnings.push(format!(
                "{}.{}: type change {} -> {} — consider splitting across two snapshots \
                 (v(N): add new column + data correction, v(N+1): drop old column)",
                entity_name, change.field_name, old_col.data_type, new_col.data_type
            ));
        }
    }
    warnings
}

/// Warn about a column dropped + column added in the same table (possible rename).
fn rename_warnings(entity_name: &str, changes: &[FieldChange]) -> Vec<String> {
    let dropped: Vec<&FieldChange> = changes
        .iter()
        .filter(|c| c.field_type == FieldType::Column && matches!(c.action, ChangeAction::Drop))
        .collect();
    let added: Vec<&FieldChange> = changes
        .iter()
        .filter(|c| c.field_type == FieldType::Column && matches!(c.action, ChangeAction::Add(_)))
        .collect();

    let mut warnings = Vec::new();
    for drop_col in &dropped {
        for add_col in &added {
            if let ChangeAction::Add(ref detail) = add_col.action
                && matches!(**detail, FieldDetail::Column(_))
            {
                warnings.push(format!(
                    "{}: column '{}' dropped and '{}' added — if this is a rename, \
                     consider splitting: v(N): add '{}' + UPDATE, v(N+1): drop '{}'",
                    entity_name,
                    drop_col.field_name,
                    add_col.field_name,
                    add_col.field_name,
                    drop_col.field_name,
                ));
            }
        }
    }
    warnings
}

/// Warn about dropped enum values (rows may still reference them).
fn enum_value_drop_warnings(entity_name: &str, changes: &[FieldChange]) -> Vec<String> {
    let mut warnings = Vec::new();
    for change in changes {
        if change.field_type == FieldType::EnumValue && matches!(change.action, ChangeAction::Drop) {
            warnings.push(format!(
                "{}: enum value '{}' dropped — ensure no rows reference this value",
                entity_name, change.field_name
            ));
        }
    }
    warnings
}

/// Generate PostgreSQL migration SQL from a list of diffs.
pub fn generate_migration_sql(diffs: &[MigrationDiff]) -> String {
    let mut lines: Vec<String> = Vec::new();

    for d in diffs {
        match &d.action {
            DiffAction::Add => {
                // New entities use regular apply — no SQL emitted
            }
            DiffAction::Drop => match d.entity_type {
                EntityType::Table => {
                    lines.push(format!("DROP TABLE {} CASCADE;", d.entity_name));
                }
                EntityType::Enum => {
                    lines.push(format!(
                        "-- WARNING: manual migration required for dropped enum {}",
                        d.entity_name
                    ));
                }
                _ => {}
            },
            DiffAction::Change(changes) => {
                for change in changes {
                    generate_field_sql(&d.entity_name, &d.entity_type, change, &mut lines);
                }
            }
        }
    }

    lines.join("\n")
}

/// Generate SQL for a single field-level change.
fn generate_field_sql(
    entity_name: &str,
    _entity_type: &EntityType,
    change: &FieldChange,
    lines: &mut Vec<String>,
) {
    match (&change.field_type, &change.action) {
        // ── Column ──────────────────────────────────────
        (FieldType::Column, ChangeAction::Add(detail)) => {
            if let FieldDetail::Column(col) = detail.as_ref() {
                lines.push(column_add_sql(entity_name, col));
            }
        }
        (FieldType::Column, ChangeAction::Drop) => {
            lines.push(format!(
                "ALTER TABLE {} DROP COLUMN {};",
                entity_name, change.field_name
            ));
        }
        (FieldType::Column, ChangeAction::Alter { old, new }) => {
            if let (FieldDetail::Column(old_col), FieldDetail::Column(new_col)) =
                (old.as_ref(), new.as_ref())
            {
                push_column_alter_sql(entity_name, old_col, new_col, lines);
            }
        }

        // ── Constraint ──────────────────────────────────
        (FieldType::Constraint, ChangeAction::Add(detail)) => {
            if let FieldDetail::Constraint(con) = detail.as_ref() {
                let sql = constraint_add_sql(entity_name, con);
                lines.push(sql);
            }
        }
        (FieldType::Constraint, ChangeAction::Drop) => {
            lines.push(format!(
                "ALTER TABLE {} DROP CONSTRAINT {};",
                entity_name, change.field_name
            ));
        }

        // ── Index ───────────────────────────────────────
        (FieldType::Index, ChangeAction::Add(detail)) => {
            if let FieldDetail::Index(idx) = detail.as_ref() {
                lines.push(index_add_sql(entity_name, idx));
            }
        }
        (FieldType::Index, ChangeAction::Drop) => {
            lines.push(format!("DROP INDEX {};", change.field_name));
        }

        // ── EnumValue ───────────────────────────────────
        (FieldType::EnumValue, ChangeAction::Add(detail)) => {
            if let FieldDetail::EnumValue(val) = detail.as_ref() {
                lines.push(format!(
                    "ALTER TYPE {} ADD VALUE '{}';",
                    entity_name, val
                ));
            }
        }
        (FieldType::EnumValue, ChangeAction::Drop) => {
            // No SQL for enum value drop — warning only
        }

        // Catch-all for any unexpected combinations
        _ => {}
    }
}

/// `ALTER TABLE … ADD COLUMN …` for a newly added column.
fn column_add_sql(entity_name: &str, col: &crate::entity::ColumnDef) -> String {
    let mut stmt = format!(
        "ALTER TABLE {} ADD COLUMN {} {}",
        entity_name, col.name, col.data_type
    );
    if !col.nullable {
        stmt.push_str(" NOT NULL");
    }
    if let Some(ref default) = col.default_value {
        stmt.push_str(&format!(" DEFAULT {default}"));
    }
    stmt.push(';');
    stmt
}

/// Push the `ALTER TABLE … ALTER COLUMN …` statements for a column whose type,
/// nullability, or default value changed.
fn push_column_alter_sql(
    entity_name: &str,
    old_col: &crate::entity::ColumnDef,
    new_col: &crate::entity::ColumnDef,
    lines: &mut Vec<String>,
) {
    let type_changed = old_col.data_type != new_col.data_type;

    // A default the new type can't absorb blocks the type change itself —
    // `default for column "c" cannot be cast automatically to type status_t` —
    // and that fires even with a `USING` clause, because Postgres re-casts the
    // default separately. Stash the default before the alter and restore it
    // after. Nothing used to be emitted at all when the default was equal on
    // both sides, so `text default 'active'` → enum could never converge.
    let stash_default = type_changed && old_col.default_value.is_some();
    if stash_default {
        lines.push(format!(
            "ALTER TABLE {} ALTER COLUMN {} DROP DEFAULT;",
            entity_name, new_col.name
        ));
    }

    if type_changed {
        // Always spell the cast out. Postgres accepts the bare
        // `ALTER COLUMN … TYPE t` only where an assignment cast exists, so
        // `text → integer`, `text → <enum>`, `text → uuid`, `text → jsonb` and
        // `integer → boolean` all failed with "cannot be cast automatically" —
        // aborting reconcile after earlier passes had already committed, which
        // left the database half-converged and every re-run failing the same
        // way. `USING col::t` is a superset of the bare form (it still covers
        // int → bigint), so it is unconditional rather than gated on a
        // cast-safety guess that can drift from Postgres's own cast catalog.
        lines.push(format!(
            "ALTER TABLE {} ALTER COLUMN {} TYPE {} USING {}::{};",
            entity_name, new_col.name, new_col.data_type, new_col.name, new_col.data_type
        ));
    }
    if old_col.nullable != new_col.nullable {
        if new_col.nullable {
            lines.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} DROP NOT NULL;",
                entity_name, new_col.name
            ));
        } else {
            lines.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} SET NOT NULL;",
                entity_name, new_col.name
            ));
        }
    }
    // Settle the default last, once the column is already the new type. A
    // stashed default is restored even when it did not change; a default that
    // was stashed and is now gone needs no second `DROP DEFAULT`.
    match &new_col.default_value {
        Some(val) if stash_default || old_col.default_value != new_col.default_value => {
            lines.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} SET DEFAULT {};",
                entity_name, new_col.name, val
            ));
        }
        None if !stash_default && old_col.default_value.is_some() => {
            lines.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} DROP DEFAULT;",
                entity_name, new_col.name
            ));
        }
        _ => {}
    }
    if old_col.comment != new_col.comment {
        match &new_col.comment {
            Some(c) => lines.push(format!(
                "COMMENT ON COLUMN {}.{} IS '{}';",
                entity_name, new_col.name, esc(c)
            )),
            None => lines.push(format!(
                "COMMENT ON COLUMN {}.{} IS NULL;",
                entity_name, new_col.name
            )),
        }
    }
    if old_col.identity != new_col.identity {
        match (&old_col.identity, &new_col.identity) {
            (None, Some(kind)) => lines.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} ADD GENERATED {} AS IDENTITY;",
                entity_name, new_col.name, identity_kind_sql(kind)
            )),
            (Some(_), None) => lines.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} DROP IDENTITY;",
                entity_name, new_col.name
            )),
            (Some(_), Some(kind)) => lines.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} SET GENERATED {};",
                entity_name, new_col.name, identity_kind_sql(kind)
            )),
            (None, None) => {} // unreachable: guarded by the inequality above
        }
    }
    // PK / unique / inline-FK are also modelled as table constraints, whose real
    // ADD/DROP DDL the constraint diff emits. Surface the column-flag change as an
    // advisory comment so it is never a silent blank alter, without risking DDL
    // that duplicates or conflicts with the constraint diff.
    if old_col.is_pk != new_col.is_pk {
        lines.push(format!(
            "-- {}.{}: primary-key flag changed ({} -> {}); manage as a table PRIMARY KEY constraint",
            entity_name, new_col.name, old_col.is_pk, new_col.is_pk
        ));
    }
    if old_col.is_unique != new_col.is_unique {
        lines.push(format!(
            "-- {}.{}: unique flag changed ({} -> {}); manage as a table UNIQUE constraint",
            entity_name, new_col.name, old_col.is_unique, new_col.is_unique
        ));
    }
    if old_col.inline_fk != new_col.inline_fk {
        let verb = match (&old_col.inline_fk, &new_col.inline_fk) {
            (None, Some(_)) => "added",
            (Some(_), None) => "dropped",
            _ => "changed",
        };
        lines.push(format!(
            "-- {}.{}: inline foreign key {}; manage as a table FOREIGN KEY constraint",
            entity_name, new_col.name, verb
        ));
    }
}

/// `CREATE [UNIQUE] INDEX …` for a newly added index.
///
/// Delegates to [`crate::emit::emit_index_sql`] so the migration SQL carries the
/// same clauses (access method, operator class, `WHERE`, `WITH`, …) the initial
/// apply would. Rendering a reduced form here is what made a partial or `hnsw`
/// index diff forever: the statement created something other than what the design
/// declared, so the next diff reported the same change again.
fn index_add_sql(entity_name: &str, idx: &crate::entity::IndexDef) -> String {
    // `entity_name` is the already-qualified `schema.table`, and the fallback
    // index name only needs the bare table part.
    let table_name = entity_name.rsplit('.').next().unwrap_or(entity_name);
    crate::emit::emit_index_sql(idx, entity_name, table_name, false)
}

/// Escape single quotes for a SQL string literal (doubling), matching `emit.rs`.
fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

/// The `GENERATED { ALWAYS | BY DEFAULT }` keyword for an identity column,
/// shared by the `ADD … AS IDENTITY` and `SET GENERATED …` alter forms.
fn identity_kind_sql(kind: &crate::entity::IdentityKind) -> &'static str {
    use crate::entity::IdentityKind;
    match kind {
        IdentityKind::Always => "ALWAYS",
        IdentityKind::ByDefault => "BY DEFAULT",
    }
}

/// Convert an FkAction to its SQL keyword.
fn fk_action_to_sql(action: &crate::entity::FkAction) -> &'static str {
    use crate::entity::FkAction;
    match action {
        FkAction::Cascade => "CASCADE",
        FkAction::Restrict => "RESTRICT",
        FkAction::SetNull => "SET NULL",
        FkAction::SetDefault => "SET DEFAULT",
        FkAction::NoAction => "NO ACTION",
    }
}

/// Generate ADD CONSTRAINT SQL for a table constraint.
fn constraint_add_sql(entity_name: &str, con: &TableConstraint) -> String {
    match con {
        TableConstraint::PrimaryKey { name, columns } => {
            let con_name = name.as_deref().unwrap_or("unnamed");
            format!(
                "ALTER TABLE {} ADD CONSTRAINT {} PRIMARY KEY ({});",
                entity_name,
                con_name,
                columns.join(", ")
            )
        }
        TableConstraint::Unique { name, columns } => {
            let con_name = name.as_deref().unwrap_or("unnamed");
            format!(
                "ALTER TABLE {} ADD CONSTRAINT {} UNIQUE ({});",
                entity_name,
                con_name,
                columns.join(", ")
            )
        }
        TableConstraint::ForeignKey(fk) => {
            let ref_schema = fk
                .ref_schema
                .as_deref()
                .map(|s| format!("{}.", s))
                .unwrap_or_default();
            // An unnamed FK (e.g. the design's inline `references …`) is emitted
            // without a `CONSTRAINT <name>` clause so Postgres auto-names it —
            // rather than literally naming the constraint "unnamed".
            let con_clause = fk
                .name
                .as_deref()
                .map(|n| format!("CONSTRAINT {n} "))
                .unwrap_or_default();
            let mut sql = format!(
                "ALTER TABLE {} ADD {}FOREIGN KEY ({}) REFERENCES {}{}({})",
                entity_name,
                con_clause,
                fk.columns.join(", "),
                ref_schema,
                fk.ref_table,
                fk.ref_columns.join(", ")
            );
            if let Some(ref action) = fk.on_delete {
                sql.push_str(&format!(" ON DELETE {}", fk_action_to_sql(action)));
            }
            if let Some(ref action) = fk.on_update {
                sql.push_str(&format!(" ON UPDATE {}", fk_action_to_sql(action)));
            }
            sql.push(';');
            sql
        }
        TableConstraint::Check { name, expression } => {
            // An unnamed CHECK (the design's inline `check (…)`) is emitted
            // without a `CONSTRAINT <name>` clause so Postgres auto-names it —
            // rather than literally naming the constraint "unnamed", which is
            // what a shared name would then collide on. Mirrors the FK arm.
            let con_clause = name
                .as_deref()
                .map(|n| format!("CONSTRAINT {n} "))
                .unwrap_or_default();
            format!("ALTER TABLE {entity_name} ADD {con_clause}CHECK ({expression});")
        }
    }
}
