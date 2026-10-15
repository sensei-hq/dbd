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
        warnings.extend(unnamed_constraint_drop_warnings(&d.entity_name, changes));
    }

    warnings
}

/// Warn about a constraint that must go but cannot be dropped, because neither
/// side of the diff knows a name for it.
///
/// `DROP CONSTRAINT` takes a name and nothing else. Rather than emit a statement
/// that cannot run — the defect that produced
/// `DROP CONSTRAINT pk:tenant_id,metric_id` — [`generate_field_sql`] writes a
/// comment, and this surfaces the same fact where a caller will actually see it.
fn unnamed_constraint_drop_warnings(entity_name: &str, changes: &[FieldChange]) -> Vec<String> {
    let mut warnings = Vec::new();
    for change in changes {
        if change.field_type != FieldType::Constraint {
            continue;
        }
        let ChangeAction::Drop(ref detail) = change.action else {
            continue;
        };
        let FieldDetail::Constraint(ref con) = **detail else {
            continue;
        };
        if constraint_name(con).is_none() {
            warnings.push(format!(
                "{}: {} must be dropped but has no constraint name — drop it manually",
                entity_name,
                constraint_description(con)
            ));
        }
    }
    warnings
}

/// Say what a column type change does: it is made in place.
fn type_change_warnings(entity_name: &str, changes: &[FieldChange]) -> Vec<String> {
    let mut warnings = Vec::new();
    for change in changes {
        if change.field_type == FieldType::Column
            && let ChangeAction::Alter { ref old, ref new } = change.action
            && let (FieldDetail::Column(old_col), FieldDetail::Column(new_col)) = (old.as_ref(), new.as_ref())
            && old_col.data_type != new_col.data_type
        {
            warnings.push(format!(
                "{}.{}: type change {} -> {} — the column is altered in place: Postgres converts every \
                 value, and refuses one the new type cannot hold",
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
        .filter(|c| c.field_type == FieldType::Column && matches!(c.action, ChangeAction::Drop(_)))
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
                    "{}: column '{}' dropped and '{}' added — the drop loses '{}'s values; \
                     if this is a rename, rename it in place (ALTER TABLE … RENAME COLUMN) instead",
                    entity_name, drop_col.field_name, add_col.field_name, drop_col.field_name,
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
        if change.field_type == FieldType::EnumValue && matches!(change.action, ChangeAction::Drop(_)) {
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
                // The name may be one a live catalog holds and the design does
                // not: written as a name, never pasted in as SQL.
                EntityType::Table => {
                    lines.push(format!("DROP TABLE {} CASCADE;", name_if_needed(&d.entity_name)));
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

/// A constraint's real SQL identifier, or `None` when it has none.
///
/// Only a name can appear in `DROP CONSTRAINT`. Postgres auto-names every
/// constraint it creates, so a constraint read back from a live database always
/// has one; a constraint parsed from a design usually does not.
fn constraint_name(con: &TableConstraint) -> Option<&str> {
    match con {
        TableConstraint::PrimaryKey { name, .. } | TableConstraint::Unique { name, .. } => name.as_deref(),
        TableConstraint::ForeignKey(fk) => fk.name.as_deref(),
        TableConstraint::Check { name, .. } | TableConstraint::Exclude { name, .. } => name.as_deref(),
    }
}

/// The comment a migration carries for a constraint it must drop but cannot
/// name: neither side of the diff knows the name Postgres gave it.
pub(crate) fn unnamed_drop_comment(entity_name: &str, con: &TableConstraint) -> String {
    format!(
        "-- {}: cannot drop {} — the constraint has no name; drop it manually",
        entity_name,
        constraint_description(con)
    )
}

/// On PostgreSQL, a `DO` block dropping a primary key, unique key or foreign
/// key the design left unnamed (#65), by the name the catalog has for it: the
/// one primary key of `entity_name`; the unique key or foreign key on exactly
/// those columns, in that order — a foreign key to that table. Gone already,
/// it drops nothing, as `DROP CONSTRAINT IF EXISTS` would. `None` for a CHECK
/// or EXCLUDE, which only their expression identifies.
pub(crate) fn catalog_drop_sql(entity_name: &str, con: &TableConstraint) -> Option<String> {
    use crate::sql_quote::literal;
    let columns_are = |columns: &[String]| {
        let list = columns.iter().map(|c| literal(c)).collect::<Vec<_>>().join(", ");
        format!(
            " AND ARRAY(SELECT a.attname::text FROM unnest(k.conkey) WITH ORDINALITY AS u(attnum, i) \
             JOIN pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = u.attnum ORDER BY u.i) \
             = ARRAY[{list}]::text[]"
        )
    };
    let (contype, condition) = match con {
        TableConstraint::PrimaryKey { .. } => ("p", String::new()),
        TableConstraint::Unique { columns, .. } => ("u", columns_are(columns)),
        TableConstraint::ForeignKey(fk) => {
            let (schema, _) = crate::entity::split_qualified_name(entity_name);
            let target = match fk.ref_schema.as_deref().or(schema.as_deref()) {
                Some(schema) => format!("{schema}.{}", fk.ref_table),
                None => fk.ref_table.clone(),
            };
            // The columns it references too: two keys from the same columns to
            // one table are told apart by them.
            let references = if fk.ref_columns.is_empty() {
                String::new()
            } else {
                let list = fk.ref_columns.iter().map(|c| literal(c)).collect::<Vec<_>>().join(", ");
                format!(
                    " AND ARRAY(SELECT a.attname::text FROM unnest(k.confkey) WITH ORDINALITY AS u(attnum, i) \
                     JOIN pg_attribute a ON a.attrelid = k.confrelid AND a.attnum = u.attnum ORDER BY u.i) \
                     = ARRAY[{list}]::text[]"
                )
            };
            (
                "f",
                format!(
                    "{} AND k.confrelid = to_regclass({}){references}",
                    columns_are(&fk.columns),
                    literal(&target)
                ),
            )
        }
        TableConstraint::Check { .. } | TableConstraint::Exclude { .. } => return None,
    };
    let table = literal(entity_name);
    Some(format!(
        "-- {}: drop {} by the name the catalog has for it\n\
         DO $dbd$\n\
         DECLARE c name;\n\
         BEGIN\n\
         \x20 SELECT k.conname INTO c FROM pg_constraint k\n\
         \x20 WHERE k.conrelid = to_regclass({table}) AND k.contype = '{contype}'{condition}\n\
         \x20 LIMIT 1;\n\
         \x20 IF c IS NOT NULL THEN\n\
         \x20   EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', {table}, c);\n\
         \x20 END IF;\n\
         END $dbd$;",
        entity_name,
        constraint_description(con)
    ))
}

/// A `schema.name` entity name as SQL: each part bare when it reads back as
/// itself, quoted — its quotes doubled — when it does not.
fn name_if_needed(entity_name: &str) -> String {
    use crate::sql_quote::ident_if_needed;
    match crate::entity::split_qualified_name(entity_name) {
        (Some(schema), name) => format!("{}.{}", ident_if_needed(&schema), ident_if_needed(&name)),
        (None, name) => ident_if_needed(&name),
    }
}

/// The `CONSTRAINT <name> ` prefix for an `ALTER TABLE … ADD …`, or an empty
/// string when the constraint is unnamed so Postgres auto-names it.
fn constraint_clause(name: Option<&str>) -> String {
    name.map(|n| format!("CONSTRAINT {n} ")).unwrap_or_default()
}

/// A human-readable description of an unnamed constraint, for the comment and
/// warning that stand in for DDL that cannot be written.
pub(crate) fn constraint_description(con: &TableConstraint) -> String {
    match con {
        TableConstraint::PrimaryKey { columns, .. } => format!("PRIMARY KEY ({})", columns.join(", ")),
        TableConstraint::Unique {
            columns,
            nulls_not_distinct,
            ..
        } => format!(
            "UNIQUE{} ({})",
            if *nulls_not_distinct { " NULLS NOT DISTINCT" } else { "" },
            columns.join(", ")
        ),
        TableConstraint::ForeignKey(fk) => format!("FOREIGN KEY ({})", fk.columns.join(", ")),
        TableConstraint::Check { expression, .. } => format!("CHECK ({expression})"),
        TableConstraint::Exclude { definition, .. } => format!("EXCLUDE {definition}"),
    }
}

/// Generate SQL for a single field-level change.
fn generate_field_sql(entity_name: &str, _entity_type: &EntityType, change: &FieldChange, lines: &mut Vec<String>) {
    match (&change.field_type, &change.action) {
        // ── Column ──────────────────────────────────────
        (FieldType::Column, ChangeAction::Add(detail)) => {
            if let FieldDetail::Column(col) = detail.as_ref() {
                lines.push(column_add_sql(entity_name, col));
            }
        }
        (FieldType::Column, ChangeAction::Drop(_)) => {
            lines.push(format!(
                "ALTER TABLE {} DROP COLUMN {};",
                entity_name,
                crate::sql_quote::ident_if_needed(&change.field_name)
            ));
        }
        (FieldType::Column, ChangeAction::Alter { old, new }) => {
            if let (FieldDetail::Column(old_col), FieldDetail::Column(new_col)) = (old.as_ref(), new.as_ref()) {
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
        (FieldType::Constraint, ChangeAction::Drop(detail)) => {
            let FieldDetail::Constraint(con) = detail.as_ref() else {
                return;
            };
            // The real name off the dropped constraint, never `change.field_name`
            // — that is a matching key and is synthetic for anything the design
            // left unnamed (`pk:tenant_id,metric_id`).
            match constraint_name(con) {
                // `IF EXISTS` because a constraint can already be gone by the time
                // this runs: dropping a column earlier in the same batch takes the
                // PRIMARY KEY covering it along with it.
                Some(name) => lines.push(format!(
                    "ALTER TABLE {} DROP CONSTRAINT IF EXISTS {};",
                    entity_name,
                    crate::sql_quote::ident(name)
                )),
                None => lines.push(unnamed_drop_comment(entity_name, con)),
            }
        }

        // ── Index ───────────────────────────────────────
        (FieldType::Index, ChangeAction::Add(detail)) => {
            if let FieldDetail::Index(idx) = detail.as_ref() {
                lines.push(index_add_sql(entity_name, idx));
            }
        }
        (FieldType::Index, ChangeAction::Drop(detail)) => {
            let FieldDetail::Index(idx) = detail.as_ref() else {
                return;
            };
            // In the schema of the table it is on, as reconcile drops it: a
            // migration runs on its file's search path, which that schema may
            // not be on, and `IF EXISTS` would make the miss a no-op (#73).
            let schema = match crate::entity::split_qualified_name(entity_name) {
                (Some(schema), _) => format!("{}.", crate::sql_quote::ident(&schema)),
                (None, _) => String::new(),
            };
            match &idx.name {
                Some(name) => lines.push(format!(
                    "DROP INDEX IF EXISTS {schema}{};",
                    crate::sql_quote::ident(name)
                )),
                None => lines.push(format!(
                    "-- {}: cannot drop the index on ({}) — it has no name; drop it manually",
                    entity_name,
                    idx.columns
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }

        // ── EnumValue ───────────────────────────────────
        (FieldType::EnumValue, ChangeAction::Add(detail)) => {
            if let FieldDetail::EnumValue(val) = detail.as_ref() {
                lines.push(format!("ALTER TYPE {} ADD VALUE '{}';", entity_name, val));
            }
        }
        (FieldType::EnumValue, ChangeAction::Drop(_)) => {
            // No SQL for enum value drop — warning only
        }

        // Catch-all for any unexpected combinations
        _ => {}
    }
}

/// `ALTER TABLE … ADD COLUMN …` for a newly added column.
fn column_add_sql(entity_name: &str, col: &crate::entity::ColumnDef) -> String {
    let mut stmt = format!("ALTER TABLE {} ADD COLUMN {} {}", entity_name, col.name, col.data_type);
    if let Some(collation) = &col.collation {
        stmt.push_str(&format!(" COLLATE {collation}"));
    }
    if !col.nullable {
        stmt.push_str(" NOT NULL");
    }
    if let Some(ref default) = col.default_value {
        stmt.push_str(&format!(" DEFAULT {default}"));
    }
    stmt.push(';');
    stmt
}

/// Push the statements for a column where either side is a `GENERATED ALWAYS AS
/// (…) STORED` computed column.
///
/// Three shapes, each with the only verb Postgres accepts for it:
///
/// | old → new | statement |
/// |---|---|
/// | generated → generated (same expr) | nothing — or its type, in place |
/// | generated → generated (changed) | `ALTER COLUMN … SET EXPRESSION AS (…)`, after any type |
/// | generated → plain | `ALTER COLUMN … DROP EXPRESSION` |
/// | plain → generated | `DROP COLUMN` + `ADD COLUMN … GENERATED …` |
///
/// `SET EXPRESSION AS` is PG17+, which is dbd's declared minimum (the enum-value
/// rename path already depends on the same floor).
///
/// The rebuild in the last row is the only DDL Postgres offers — it rejects
/// `SET EXPRESSION` on a column that is not already generated. It is safe for
/// precisely this shape: the rebuilt column is computed, so Postgres recomputes
/// every value from the surviving columns rather than losing them.
fn push_generated_alter_sql(
    entity_name: &str,
    old_col: &crate::entity::ColumnDef,
    new_col: &crate::entity::ColumnDef,
    lines: &mut Vec<String>,
) {
    match (&old_col.generated, &new_col.generated) {
        (Some(old_expr), Some(new_expr)) => {
            // The type first, in place: Postgres recomputes the column at it.
            if old_col.data_type != new_col.data_type {
                lines.push(format!(
                    "ALTER TABLE {} ALTER COLUMN {} TYPE {};",
                    entity_name, new_col.name, new_col.data_type
                ));
            }
            if old_expr != new_expr {
                lines.push(format!(
                    "ALTER TABLE {} ALTER COLUMN {} SET EXPRESSION AS ({new_expr});",
                    entity_name, new_col.name
                ));
            }
        }
        (Some(_), None) => lines.push(format!(
            "ALTER TABLE {} ALTER COLUMN {} DROP EXPRESSION;",
            entity_name, new_col.name
        )),
        (None, Some(expr)) => {
            lines.push(format!("ALTER TABLE {} DROP COLUMN {};", entity_name, new_col.name));
            lines.push(format!(
                "ALTER TABLE {} ADD COLUMN {} {} GENERATED ALWAYS AS ({expr}) STORED;",
                entity_name, new_col.name, new_col.data_type
            ));
        }
        (None, None) => unreachable!("caller checks at least one side is generated"),
    }
}

/// Whether a type change is character data into a bounded character type —
/// `text` → `varchar(3)` — which an explicit cast would cut to fit, and an
/// assignment refuses as `value too long`.
fn narrows_by_assignment(old_type: &str, new_type: &str) -> bool {
    let base = |t: &str| t.trim().to_lowercase();
    let characters = |t: &str| {
        let t = base(t);
        let name = t.split('(').next().unwrap_or(&t).trim().to_string();
        matches!(
            name.as_str(),
            "text" | "varchar" | "character varying" | "char" | "character" | "bpchar" | "name" | "citext"
        )
    };
    let bounded = base(new_type).contains('(') && !base(new_type).ends_with("[]");
    characters(old_type) && characters(new_type) && bounded
}

/// Push the `ALTER TABLE … ALTER COLUMN …` statements for a column whose type,
/// nullability, or default value changed.
fn push_column_alter_sql(
    entity_name: &str,
    old_col: &crate::entity::ColumnDef,
    new_col: &crate::entity::ColumnDef,
    lines: &mut Vec<String>,
) {
    // A `GENERATED ALWAYS AS (…) STORED` column is settled on its own terms and
    // then left alone: its value comes from the expression, so every DEFAULT verb
    // below is one Postgres refuses outright ("column … is a generated column").
    // Postgres stores the expression in `pg_attrdef` — the same catalog a real
    // DEFAULT uses — which is how it came to be read as one (issue #16).
    if old_col.generated.is_some() || new_col.generated.is_some() {
        push_generated_alter_sql(entity_name, old_col, new_col, lines);
        if new_col.generated.is_some() || old_col.generated.is_none() {
            return;
        }
        // Turned plain: the rest of it settles as any column's does.
    }

    let type_changed = old_col.data_type != new_col.data_type;
    // A collation is changed with the type it qualifies — `ALTER COLUMN … TYPE
    // t COLLATE c` — and going back to the type's own is `COLLATE "default"`.
    let collation_changed = old_col.collation != new_col.collation;
    let collate = match &new_col.collation {
        Some(collation) => format!(" COLLATE {collation}"),
        None if collation_changed => " COLLATE \"default\"".to_string(),
        None => String::new(),
    };

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

    if type_changed && narrows_by_assignment(&old_col.data_type, &new_col.data_type) {
        // Character data into a bounded character type: by assignment, where
        // Postgres refuses a value too long. An explicit cast (`USING c::t`)
        // cuts it to fit without a word.
        lines.push(format!(
            "ALTER TABLE {} ALTER COLUMN {} TYPE {}{collate};",
            entity_name, new_col.name, new_col.data_type
        ));
    } else if type_changed {
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
            "ALTER TABLE {} ALTER COLUMN {} TYPE {}{collate} USING {}::{};",
            entity_name, new_col.name, new_col.data_type, new_col.name, new_col.data_type
        ));
    } else if collation_changed {
        lines.push(format!(
            "ALTER TABLE {} ALTER COLUMN {} TYPE {}{collate};",
            entity_name, new_col.name, new_col.data_type
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
                entity_name,
                new_col.name,
                esc(c)
            )),
            None => lines.push(format!("COMMENT ON COLUMN {}.{} IS NULL;", entity_name, new_col.name)),
        }
    }
    if old_col.identity != new_col.identity {
        match (&old_col.identity, &new_col.identity) {
            (None, Some(kind)) => lines.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} ADD GENERATED {} AS IDENTITY;",
                entity_name,
                new_col.name,
                identity_kind_sql(kind)
            )),
            (Some(_), None) => lines.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} DROP IDENTITY;",
                entity_name, new_col.name
            )),
            (Some(_), Some(kind)) => lines.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} SET GENERATED {};",
                entity_name,
                new_col.name,
                identity_kind_sql(kind)
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

/// Generate ADD CONSTRAINT SQL for a table constraint.
fn constraint_add_sql(entity_name: &str, con: &TableConstraint) -> String {
    match con {
        // An unnamed PK/UNIQUE (the design's inline `primary key` / `unique`) is
        // emitted without a `CONSTRAINT` clause so Postgres auto-names it, exactly
        // as the FK arm below does. Naming it literally "unnamed" was worse than
        // ugly: a PK's backing index takes the constraint name and index names are
        // schema-scoped, so the second table in a schema to converge this way
        // failed with `relation "unnamed" already exists`.
        TableConstraint::PrimaryKey { name, columns } => {
            format!(
                "ALTER TABLE {} ADD {}PRIMARY KEY ({});",
                entity_name,
                constraint_clause(name.as_deref()),
                columns.join(", ")
            )
        }
        // `NULLS NOT DISTINCT` sits between the keyword and the column list, and is
        // the difference between NULL rows colliding and duplicating freely.
        // Omitting it here applied a constraint weaker than the design declared —
        // silently, because `diff` then compared the two as equal (issue #12).
        TableConstraint::Unique {
            name,
            columns,
            nulls_not_distinct,
        } => {
            format!(
                "ALTER TABLE {} ADD {}UNIQUE{} ({});",
                entity_name,
                constraint_clause(name.as_deref()),
                if *nulls_not_distinct { " NULLS NOT DISTINCT" } else { "" },
                columns.join(", ")
            )
        }
        TableConstraint::ForeignKey(fk) => {
            let ref_schema = fk.ref_schema.as_deref().map(|s| format!("{}.", s)).unwrap_or_default();
            // An unnamed FK (e.g. the design's inline `references …`) is emitted
            // without a `CONSTRAINT <name>` clause so Postgres auto-names it —
            // rather than literally naming the constraint "unnamed".
            let con_clause = constraint_clause(fk.name.as_deref());
            // A key naming no columns references the primary key: `()` is no
            // column list, but a syntax error (#65).
            let ref_columns = if fk.ref_columns.is_empty() {
                String::new()
            } else {
                format!("({})", fk.ref_columns.join(", "))
            };
            let mut sql = format!(
                "ALTER TABLE {} ADD {}FOREIGN KEY ({}) REFERENCES {}{}{}",
                entity_name,
                con_clause,
                fk.columns.join(", "),
                ref_schema,
                fk.ref_table,
                ref_columns
            );
            crate::emit::push_fk_clauses(&mut sql, fk);
            sql.push(';');
            sql
        }
        TableConstraint::Check { name, expression } => {
            // An unnamed CHECK (the design's inline `check (…)`) is emitted
            // without a `CONSTRAINT <name>` clause so Postgres auto-names it —
            // rather than literally naming the constraint "unnamed", which is
            // what a shared name would then collide on. Mirrors the FK arm.
            let con_clause = name.as_deref().map(|n| format!("CONSTRAINT {n} ")).unwrap_or_default();
            format!("ALTER TABLE {entity_name} ADD {con_clause}CHECK ({expression});")
        }
        // Unnamed, Postgres names it `<table>_<columns>_excl`, as it names a
        // CHECK — mirrored for the same reason.
        TableConstraint::Exclude { name, definition } => {
            format!(
                "ALTER TABLE {entity_name} ADD {}EXCLUDE {definition};",
                constraint_clause(name.as_deref())
            )
        }
    }
}
