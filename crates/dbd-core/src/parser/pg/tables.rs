//! Table DDL, parsed with libpg_query.
//!
//! # This is the one type that must refuse rather than degrade
//!
//! `reconcile::raw_snapshot_from_entities` keeps only entities whose
//! `table_def.is_some()`. A table dbd cannot structurally read is therefore
//! absent from the *desired* snapshot, the live table reads as an **orphan**,
//! and `dbd reconcile --prune` DROPs it. Every other native parser in this
//! module may degrade to a partial result; this one may not. So every
//! extraction step returns [`Extract`], and the first thing that cannot be
//! represented aborts the whole table into `entity.errors` with no
//! `table_def` — which makes `ensure_fully_parsed` refuse the run.
//!
//! # Why text comes back through Postgres's own deparser
//!
//! `TableDef` stores types, defaults and CHECK bodies as *text*, and the raw
//! parse tree holds neither: Postgres rewrites SQL-standard type names into its
//! internals (`int` → `pg_catalog.int4`) and keeps expressions as trees. Node
//! locations do not delimit either one, and `Node::deparse` only reconstructs
//! whole statements. So each fragment is planted into a template statement —
//! parsed, so every unrelated field is a value the C deparser accepts — and the
//! fixed frame is stripped off the result. That yields Postgres's own spelling
//! (`varchar(30)`, `timestamp with time zone`, `s IN ('a', 'b')`) rather than a
//! hand-rolled rendering that would have to re-derive the alias table, array
//! bounds and type-modifier syntax.

use std::collections::BTreeMap;

use pg_query::NodeEnum;
use pg_query::protobuf;

use crate::entity::{
    ColumnDef, Entity, FkAction, ForeignKey, IdentityKind, IndexColumn, IndexDef, IndexType,
    REF_TYPE_FUNCTION, Reference, SortOrder, TableComments, TableConstraint, TableDef,
};
use crate::error::Result;

use super::common;

/// An extraction step's result: the value, or the reason the table is unreadable.
///
/// A `String` rather than [`crate::error::DbdError`] because these are not
/// failures of dbd — they are "this table says something `TableDef` cannot
/// hold", which belongs on the entity as an error the user can act on.
type Extract<T> = std::result::Result<T, String>;

/// Parse a table DDL file.
pub(crate) fn parse_table(mut entity: Entity, sql: &str) -> Result<Entity> {
    // Set before any early return, like every other native parser here: an
    // errored entity must still carry the sqlparser path's `["public"]`
    // default, since references are qualified against it.
    entity.search_paths = common::extract_search_paths_via_pg_query(sql);

    let parsed = match pg_query::parse(sql) {
        Ok(p) => p,
        Err(e) => {
            entity.errors.push(format!("Parse error: {e}"));
            return Ok(entity);
        }
    };

    let default_schema = entity
        .search_paths
        .first()
        .cloned()
        .unwrap_or_else(|| "public".to_string());

    match extract(&parsed, &default_schema) {
        Ok((table_def, references)) => {
            entity.refers = references.iter().map(|r| r.name.clone()).collect();
            entity.references = references;
            entity.table_def = Some(table_def);
        }
        // No `table_def` on this path — see the module note on `--prune`.
        Err(why) => entity.errors.push(why),
    }

    Ok(entity)
}

/// The whole file: its `CREATE TABLE`s, plus the `CREATE INDEX` and `COMMENT ON`
/// statements that ship alongside them (Postgres has no way to nest either one
/// inside `CREATE TABLE`, so they arrive as siblings).
fn extract(
    parsed: &pg_query::ParseResult,
    default_schema: &str,
) -> Extract<(TableDef, Vec<Reference>)> {
    let mut columns: Vec<ColumnDef> = Vec::new();
    let mut constraints: Vec<TableConstraint> = Vec::new();
    let mut indexes: Vec<IndexDef> = Vec::new();
    let mut comments = TableComments::default();
    let mut references: Vec<Reference> = Vec::new();
    let mut functions: Vec<String> = Vec::new();
    let mut declares_a_table = false;

    for stmt in &parsed.protobuf.stmts {
        match stmt.stmt.as_ref().and_then(|s| s.node.as_ref()) {
            Some(NodeEnum::CreateStmt(create)) => {
                declares_a_table = true;
                process_create_table(
                    create,
                    default_schema,
                    &mut columns,
                    &mut constraints,
                    &mut references,
                    &mut functions,
                )?;
            }
            Some(NodeEnum::IndexStmt(ix)) => {
                indexes.push(extract_index(ix, default_schema, &mut functions)?);
            }
            Some(NodeEnum::CommentStmt(c)) => record_comment(c, &mut comments),
            _ => {}
        }
    }

    if !declares_a_table {
        return Err("this table file declares no `CREATE TABLE`".to_string());
    }

    // Column comments are addressed by name, so they can only be attached once
    // every `CREATE TABLE` in the file has been read.
    for col in &mut columns {
        if let Some(comment) = comments.columns.get(&col.name) {
            col.comment = Some(comment.clone());
        }
    }

    references.extend(functions.into_iter().map(|name| Reference {
        name,
        ref_type: Some(REF_TYPE_FUNCTION.to_string()),
    }));

    Ok((
        TableDef { columns, constraints, indexes, comments },
        references,
    ))
}

/// One `CREATE TABLE`'s columns, then its table-level constraints.
///
/// Two passes over the same list rather than one: a table-level `PRIMARY KEY`
/// flags its member columns, which needs every column already collected, and
/// the resulting constraint order (inline CHECKs in column order, then
/// table-level constraints in theirs) is what the snapshot records.
fn process_create_table(
    create: &protobuf::CreateStmt,
    default_schema: &str,
    columns: &mut Vec<ColumnDef>,
    constraints: &mut Vec<TableConstraint>,
    references: &mut Vec<Reference>,
    functions: &mut Vec<String>,
) -> Extract<()> {
    // `PARTITION OF p`, `INHERITS (p)` and `OF a_type` all declare a table whose
    // columns live somewhere else, so `table_elts` is empty and every column is
    // invisible here. Accepting that would put a *zero-column* table in the
    // desired snapshot, and reconcile would read every live column as one to
    // DROP — a worse outcome than the orphaning the module note describes.
    if !create.inh_relations.is_empty() || create.of_typename.is_some() {
        return Err(
            "this table takes its columns from another table or a composite type \
             (PARTITION OF / INHERITS / OF), which dbd cannot see"
                .to_string(),
        );
    }

    for elt in &create.table_elts {
        match elt.node.as_ref() {
            Some(NodeEnum::ColumnDef(col_def)) => {
                let (col, checks) =
                    extract_column(col_def, default_schema, references, functions)?;
                columns.push(col);
                // Postgres promotes a column-level CHECK to a table constraint,
                // and introspection reports it as one. Dropping it would make
                // every inline CHECK read as a live-only constraint reconcile
                // offers to delete.
                constraints.extend(checks);
            }
            Some(NodeEnum::Constraint(_)) => {}
            // The only other element Postgres's grammar allows here is
            // `LIKE other_table`, which copies a shape `TableDef` never sees —
            // so the columns it would contribute are simply missing, exactly
            // the silent partial this parser must not produce.
            _ => {
                return Err(
                    "this table uses a LIKE clause, whose columns dbd cannot see".to_string()
                );
            }
        }
    }

    for elt in &create.table_elts {
        let Some(NodeEnum::Constraint(constraint)) = elt.node.as_ref() else {
            continue;
        };
        let tc = extract_table_constraint(constraint, default_schema, references, functions)?;
        if let TableConstraint::PrimaryKey { columns: pk_cols, .. } = &tc {
            mark_pk_columns(columns, pk_cols);
        }
        constraints.push(tc);
    }

    Ok(())
}

/// Flag the named columns as primary-key members (implicitly `NOT NULL`).
///
/// A composite `PRIMARY KEY (a, b)` therefore lands twice — as a table-level
/// constraint *and* as `is_pk` on each member — which is the exact shape
/// `reconcile`'s `lift_pk_unique_keep_others` and `pk_unique_col_sets` read.
fn mark_pk_columns(columns: &mut [ColumnDef], pk_cols: &[String]) {
    for col in columns {
        if pk_cols.contains(&col.name) {
            col.is_pk = true;
            col.nullable = false;
        }
    }
}

/// One column, plus any column-level `CHECK`s, which belong to the table.
fn extract_column(
    col_def: &protobuf::ColumnDef,
    default_schema: &str,
    references: &mut Vec<Reference>,
    functions: &mut Vec<String>,
) -> Extract<(ColumnDef, Vec<TableConstraint>)> {
    let name = col_def.colname.clone();
    let type_name = col_def
        .type_name
        .as_ref()
        .ok_or_else(|| format!("column {name:?} has no type"))?;
    let data_type = type_text(type_name)
        .ok_or_else(|| format!("column {name:?}: dbd could not render its type back to SQL"))?;

    let mut nullable = true;
    let mut is_pk = false;
    let mut is_unique = false;
    let mut identity = None;
    let mut default_value = None;
    let mut inline_fk = None;
    let mut checks = Vec::new();

    for node in &col_def.constraints {
        let Some(NodeEnum::Constraint(c)) = node.node.as_ref() else {
            return Err(format!("column {name:?} carries an unreadable constraint"));
        };
        use protobuf::ConstrType::*;
        match c.contype() {
            ConstrPrimary => {
                is_pk = true;
                nullable = false;
            }
            ConstrUnique => is_unique = true,
            ConstrNotnull => nullable = false,
            ConstrNull => nullable = true,
            ConstrDefault => {
                let rendered = constraint_expr(c, &name, "DEFAULT")?;
                collect_function_refs(&rendered, default_schema, functions);
                default_value = Some(rendered);
            }
            ConstrCheck => {
                let rendered = constraint_expr(c, &name, "CHECK")?;
                collect_function_refs(&rendered, default_schema, functions);
                checks.push(TableConstraint::Check {
                    name: constraint_name(c),
                    expression: rendered,
                });
            }
            ConstrForeign => {
                let fk = extract_foreign_key(c, vec![name.clone()], default_schema, references)?;
                inline_fk = Some(ForeignKey {
                    // The incumbent parks a `CONSTRAINT x REFERENCES …` name on
                    // the column option rather than the key, so an inline FK has
                    // never carried one; reconcile matches inline FKs by shape.
                    name: None,
                    // `REFERENCES parent` with no column list targets the
                    // parent's primary key, which this parser cannot see —
                    // `id` is the incumbent's assumption and what the live
                    // comparison is calibrated against.
                    ref_columns: if fk.ref_columns.is_empty() {
                        vec!["id".to_string()]
                    } else {
                        fk.ref_columns
                    },
                    ..fk
                });
            }
            // `GENERATED { ALWAYS | BY DEFAULT } AS IDENTITY` — sequence-backed,
            // and implicitly NOT NULL.
            ConstrIdentity => {
                identity = Some(match c.generated_when.as_str() {
                    "d" => IdentityKind::ByDefault,
                    _ => IdentityKind::Always,
                });
                nullable = false;
            }
            // `GENERATED ALWAYS AS (expr) STORED` — a computed column. The
            // expression has nowhere to live on `ColumnDef`, but a function it
            // calls must exist before the table is created.
            ConstrGenerated => {
                let rendered = constraint_expr(c, &name, "GENERATED")?;
                collect_function_refs(&rendered, default_schema, functions);
            }
            // Deferrability modifiers carry no payload of their own.
            ConstrAttrDeferrable | ConstrAttrNotDeferrable | ConstrAttrDeferred
            | ConstrAttrImmediate => {}
            other => {
                return Err(format!(
                    "column {name:?} has a {other:?} constraint dbd cannot represent"
                ));
            }
        }
    }

    // SERIAL is sugar for an integer plus an owned sequence; it is NOT an
    // IDENTITY column, so it leaves `identity` alone.
    if data_type.to_uppercase().contains("SERIAL") {
        is_pk = true;
        nullable = false;
    }

    let column = ColumnDef {
        name,
        data_type,
        nullable,
        default_value,
        is_pk,
        is_unique,
        identity,
        comment: None,
        inline_fk,
    };
    Ok((column, checks))
}

/// One table-level constraint.
fn extract_table_constraint(
    c: &protobuf::Constraint,
    default_schema: &str,
    references: &mut Vec<Reference>,
    functions: &mut Vec<String>,
) -> Extract<TableConstraint> {
    use protobuf::ConstrType::*;
    match c.contype() {
        ConstrPrimary => Ok(TableConstraint::PrimaryKey {
            name: constraint_name(c),
            columns: string_list(&c.keys),
        }),
        ConstrUnique => Ok(TableConstraint::Unique {
            name: constraint_name(c),
            columns: string_list(&c.keys),
        }),
        ConstrCheck => {
            let label = constraint_name(c).unwrap_or_else(|| "<unnamed>".to_string());
            let rendered = constraint_expr(c, &label, "CHECK")?;
            collect_function_refs(&rendered, default_schema, functions);
            Ok(TableConstraint::Check {
                name: constraint_name(c),
                expression: rendered,
            })
        }
        ConstrForeign => {
            let fk = extract_foreign_key(c, string_list(&c.fk_attrs), default_schema, references)?;
            Ok(TableConstraint::ForeignKey(ForeignKey {
                name: constraint_name(c),
                ..fk
            }))
        }
        other => Err(format!(
            "this table has a {other:?} constraint dbd cannot represent"
        )),
    }
}

/// The target half of a foreign key, and the hard reference it implies.
///
/// `name` is left empty for the caller to fill: a table-level key takes the
/// constraint's, an inline one has never carried one.
fn extract_foreign_key(
    c: &protobuf::Constraint,
    columns: Vec<String>,
    default_schema: &str,
    references: &mut Vec<Reference>,
) -> Extract<ForeignKey> {
    let target = c
        .pktable
        .as_ref()
        .ok_or_else(|| "a foreign key names no target table".to_string())?;
    let ref_schema = if target.schemaname.is_empty() {
        default_schema.to_string()
    } else {
        target.schemaname.clone()
    };

    references.push(Reference {
        name: format!("{ref_schema}.{}", target.relname),
        ref_type: Some("table".to_string()),
    });

    Ok(ForeignKey {
        name: None,
        columns,
        ref_schema: Some(ref_schema),
        ref_table: target.relname.clone(),
        ref_columns: string_list(&c.pk_attrs),
        on_delete: fk_action(&c.fk_del_action),
        on_update: fk_action(&c.fk_upd_action),
    })
}

/// Postgres's referential-action code.
///
/// `a` (NO ACTION) maps to `None`, not `FkAction::NoAction`: an omitted clause
/// parses to the same code as an explicit `ON DELETE NO ACTION`, so the two are
/// indistinguishable here, and reconcile's `normalize_fk` already treats them as
/// equal. Reporting the omitted — far more common — form is the honest default.
fn fk_action(code: &str) -> Option<FkAction> {
    match code {
        "c" => Some(FkAction::Cascade),
        "r" => Some(FkAction::Restrict),
        "n" => Some(FkAction::SetNull),
        "d" => Some(FkAction::SetDefault),
        _ => None,
    }
}

/// A constraint's name, or `None` when Postgres will generate one.
fn constraint_name(c: &protobuf::Constraint) -> Option<String> {
    (!c.conname.is_empty()).then(|| c.conname.clone())
}

/// The rendered text of a constraint's expression, or why it could not be read.
fn constraint_expr(c: &protobuf::Constraint, owner: &str, clause: &str) -> Extract<String> {
    let raw = c
        .raw_expr
        .as_ref()
        .ok_or_else(|| format!("{owner}: its {clause} clause carries no expression"))?;
    expr_text(raw)
        .ok_or_else(|| format!("{owner}: dbd could not render its {clause} expression back to SQL"))
}

/// Record a `COMMENT ON TABLE`/`COLUMN`. Comments on anything else in a table
/// file (an index, a constraint) have nowhere to live on `TableDef`, and are
/// re-emitted from the object they belong to, so they are not errors.
fn record_comment(stmt: &protobuf::CommentStmt, comments: &mut TableComments) {
    let parts = stmt
        .object
        .as_ref()
        .map(|o| match o.node.as_ref() {
            Some(NodeEnum::List(list)) => string_list(&list.items),
            _ => Vec::new(),
        })
        .unwrap_or_default();

    match stmt.objtype() {
        protobuf::ObjectType::ObjectTable => {
            comments.table = Some(stmt.comment.clone());
        }
        protobuf::ObjectType::ObjectColumn => {
            // `[schema, table, column]` or `[table, column]` — the column is last.
            if let Some(column) = parts.last() {
                comments
                    .columns
                    .insert(column.clone(), stmt.comment.clone());
            }
        }
        _ => {}
    }
}

/// One `CREATE INDEX`.
///
/// Everything the statement says is captured, because whatever is dropped here
/// reads as drift against the introspected index forever.
///
/// `pub(super)` because a materialized view's trailing indexes are read exactly
/// the same way; that module used to carry a reduced copy that skipped opclass,
/// predicate, `INCLUDE` and storage parameters.
pub(super) fn extract_index(
    ix: &protobuf::IndexStmt,
    default_schema: &str,
    functions: &mut Vec<String>,
) -> Extract<IndexDef> {
    let label = if ix.idxname.is_empty() { "<unnamed>" } else { &ix.idxname };

    let mut columns = Vec::with_capacity(ix.index_params.len());
    for param in &ix.index_params {
        let Some(NodeEnum::IndexElem(elem)) = param.node.as_ref() else {
            return Err(format!("index {label:?} has an unreadable key"));
        };
        // An empty `name` means the key is an expression, held in `expr`.
        let (name, is_expression) = if elem.name.is_empty() {
            let expr = elem
                .expr
                .as_ref()
                .ok_or_else(|| format!("index {label:?} has a key that is neither column nor expression"))?;
            let rendered = expr_text(expr).ok_or_else(|| {
                format!("index {label:?}: dbd could not render an expression key back to SQL")
            })?;
            collect_function_refs(&rendered, default_schema, functions);
            (parenthesize_key(expr, rendered), true)
        } else {
            (elem.name.clone(), false)
        };
        columns.push(IndexColumn {
            name,
            is_expression,
            order: sort_order(elem.ordering),
            nulls_first: nulls_first(elem.nulls_ordering),
            opclass: (!elem.opclass.is_empty()).then(|| string_list(&elem.opclass).join(".")),
        });
    }

    let mut include = Vec::with_capacity(ix.index_including_params.len());
    for param in &ix.index_including_params {
        match param.node.as_ref() {
            Some(NodeEnum::IndexElem(elem)) if !elem.name.is_empty() => {
                include.push(elem.name.clone());
            }
            // Postgres's grammar allows only a plain column name here.
            _ => return Err(format!("index {label:?} has an unreadable INCLUDE column")),
        }
    }

    let mut with_options = BTreeMap::new();
    for option in &ix.options {
        let Some(NodeEnum::DefElem(def)) = option.node.as_ref() else {
            return Err(format!("index {label:?} has an unreadable storage parameter"));
        };
        let value = def
            .arg
            .as_ref()
            .and_then(|a| scalar_text(a))
            .ok_or_else(|| {
                format!("index {label:?}: storage parameter {:?} has an unreadable value", def.defname)
            })?;
        with_options.insert(def.defname.to_lowercase(), value);
    }

    let predicate = match ix.where_clause.as_ref() {
        Some(w) => {
            let raw = expr_text(w).ok_or_else(|| {
                format!("index {label:?}: dbd could not render its WHERE predicate back to SQL")
            })?;
            // Canonicalized so an authored `where status = 'active'` matches the
            // `status = 'active'::app.status_t` Postgres reports back.
            Some(crate::sql_expr::canonicalize_predicate(&raw).unwrap_or(raw))
        }
        None => None,
    };

    // libpg_query fills in `btree` whether or not `USING` was written, so an
    // explicit `USING btree` is indistinguishable from the default — and the
    // default is what `None` means to the emitter, which omits the clause.
    let access_method = ix.access_method.to_lowercase();
    let index_type = (!access_method.is_empty() && access_method != "btree")
        .then(|| IndexType::from_amname(&access_method));

    Ok(IndexDef {
        name: (!ix.idxname.is_empty()).then(|| ix.idxname.clone()),
        columns,
        unique: ix.unique,
        index_type,
        predicate,
        include,
        nulls_not_distinct: ix.nulls_not_distinct,
        with_options,
    })
}

/// Postgres's grammar requires an index key that is not a bare function call to
/// be parenthesized, and `IndexColumn::name` is emitted as written — so the
/// parens have to be part of it or the generated `CREATE INDEX` is a syntax
/// error. A function call already reads as one and is left alone.
fn parenthesize_key(expr: &protobuf::Node, rendered: String) -> String {
    match expr.node.as_ref() {
        Some(NodeEnum::FuncCall(_)) => rendered,
        _ => format!("({rendered})"),
    }
}

/// `SortByDir` → [`SortOrder`]; `SortbyDefault` (no explicit `ASC`/`DESC`) is
/// `None`, which is what the emitter reads as "leave the clause off".
fn sort_order(ordering: i32) -> Option<SortOrder> {
    use protobuf::SortByDir;
    match ordering {
        x if x == SortByDir::SortbyAsc as i32 => Some(SortOrder::Asc),
        x if x == SortByDir::SortbyDesc as i32 => Some(SortOrder::Desc),
        _ => None,
    }
}

/// `SortByNulls` → the tri-state `nulls_first`; the access method's default
/// (no explicit `NULLS FIRST`/`NULLS LAST`) is `None`.
fn nulls_first(nulls_ordering: i32) -> Option<bool> {
    use protobuf::SortByNulls;
    match nulls_ordering {
        x if x == SortByNulls::SortbyNullsFirst as i32 => Some(true),
        x if x == SortByNulls::SortbyNullsLast as i32 => Some(false),
        _ => None,
    }
}

/// The `String` values of a node list — key lists, comment object paths and
/// operator-class names all arrive in this shape.
fn string_list(nodes: &[protobuf::Node]) -> Vec<String> {
    nodes
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(NodeEnum::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// A storage parameter's value, stored and re-emitted bare to match
/// `reloptions`.
///
/// `WITH (fillfactor = 70)` arrives as `Integer`, and a quoted value as
/// `String` — but a *bare* one (`deduplicate_items = off`) comes through
/// Postgres's `def_arg` production as a `TypeName`, not a string, so it needs
/// the type renderer to get its text back.
fn scalar_text(node: &protobuf::Node) -> Option<String> {
    match node.node.as_ref()? {
        NodeEnum::Integer(i) => Some(i.ival.to_string()),
        NodeEnum::Float(f) => Some(f.fval.clone()),
        NodeEnum::Boolean(b) => Some(b.boolval.to_string()),
        NodeEnum::String(s) => Some(s.sval.clone()),
        NodeEnum::TypeName(t) => type_text(t),
        _ => None,
    }
}

/// The functions an expression calls, appended to `functions` as *soft*
/// references (see [`REF_TYPE_FUNCTION`]): `default now()` is indistinguishable
/// here from a call to a project-managed function, so the resolver keeps the
/// ones naming a known entity and drops the rest without warning.
///
/// Takes the rendered text rather than the node because `call_functions()` is a
/// whole-`ParseResult` query — and it reports nothing for a `CreateStmt`, so the
/// expression has to be re-parsed on its own. Sorted before appending:
/// `call_functions()` is `HashSet`-derived, so its order is Rust's randomized
/// per-process hash order rather than source order (the nondeterminism the
/// parity gate already caught twice in `common`).
fn collect_function_refs(rendered: &str, default_schema: &str, functions: &mut Vec<String>) {
    let Ok(parsed) = pg_query::parse(&format!("SELECT {rendered}")) else {
        return;
    };
    let mut names: Vec<String> = parsed
        .call_functions()
        .into_iter()
        .filter_map(|name| common::qualify_name_str(&name, default_schema))
        .collect();
    names.sort();
    for name in names {
        common::push_unique(functions, name);
    }
}

// ── Rendering fragments back to SQL through Postgres's deparser ──────────────

/// A name no real schema object would collide with, used as the frame around a
/// fragment being deparsed so the frame can be stripped back off exactly.
const FRAME: &str = "dbd_deparse_frame";

/// A type name, in Postgres's own spelling.
///
/// The template is parsed rather than constructed: `pg_query`'s deparser is
/// Postgres's C code and aborts the process on an out-of-range enum, so every
/// field the fragment does not own must hold a value a real parse produced.
fn type_text(type_name: &protobuf::TypeName) -> Option<String> {
    let mut parsed = pg_query::parse(&format!("CREATE TABLE {FRAME} ({FRAME} int)")).ok()?;
    let Some(NodeEnum::CreateStmt(create)) = parsed.protobuf.stmts.first_mut()?.stmt.as_mut()?.node.as_mut()
    else {
        return None;
    };
    let Some(NodeEnum::ColumnDef(col)) = create.table_elts.first_mut()?.node.as_mut() else {
        return None;
    };
    col.type_name = Some(type_name.clone());

    let rendered = pg_query::deparse(&parsed.protobuf).ok()?;
    let rendered = rendered
        .strip_prefix(&format!("CREATE TABLE {FRAME} ({FRAME} "))?
        .strip_suffix(')')?;
    Some(without_typmod_spaces(rendered))
}

/// Postgres's deparser writes a type-modifier list as `numeric(10, 2)`, but
/// `format_type` — what introspection reports, and what the alias table in
/// `reconcile::canonical_type` normalizes toward — writes `numeric(10,2)`. Left
/// alone the space reads as drift on every parameterized column, forever.
fn without_typmod_spaces(rendered: &str) -> String {
    match rendered.split_once('(') {
        Some((base, args)) => format!("{base}({}", args.replace(", ", ",")),
        None => rendered.to_string(),
    }
}

/// An expression, in Postgres's own spelling.
///
/// Rendered in select-list position: it is the one context that accepts any
/// expression type and adds no clause of its own, so the frame is a fixed
/// `SELECT ` prefix. See [`type_text`] on why the template is parsed.
fn expr_text(expr: &protobuf::Node) -> Option<String> {
    let mut parsed = pg_query::parse("SELECT NULL").ok()?;
    let Some(NodeEnum::SelectStmt(select)) = parsed.protobuf.stmts.first_mut()?.stmt.as_mut()?.node.as_mut()
    else {
        return None;
    };
    let Some(NodeEnum::ResTarget(target)) = select.target_list.first_mut()?.node.as_mut() else {
        return None;
    };
    target.val = Some(Box::new(expr.clone()));

    pg_query::deparse(&parsed.protobuf)
        .ok()?
        .strip_prefix("SELECT ")
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{EntityType, FkAction, IdentityKind, IndexType, SortOrder, TableConstraint};

    fn parse(sql: &str) -> Entity {
        parse_table(Entity::new(EntityType::Table, "app.t"), sql).unwrap()
    }

    fn def(sql: &str) -> TableDef {
        let e = parse(sql);
        assert!(e.errors.is_empty(), "unexpected errors: {:?}", e.errors);
        e.table_def.expect("a readable table must carry a table_def")
    }

    // ── 1. Columns: name, type, nullability, default, identity ──────────────

    #[test]
    fn columns_keep_their_name_and_declared_order() {
        let d = def("create table t (id uuid, name text, qty int);");
        let names: Vec<&str> = d.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "name", "qty"]);
    }

    /// Postgres rewrites SQL-standard type names into its internal ones at parse
    /// time (`int` → `int4`), so the type has to come back through its own
    /// deparser rather than off the raw name list — otherwise every such column
    /// would be spelled `pg_catalog.int4` in emitted DDL and DBML.
    #[test]
    fn types_come_back_in_their_sql_standard_spelling() {
        let d = def(
            "create table t (a int, b varchar(30), c char(2), d numeric(10,2), e boolean,\
             f timestamp with time zone, g text[], h uuid, i app.status_t, j serial);",
        );
        let types: Vec<&str> = d.columns.iter().map(|c| c.data_type.as_str()).collect();
        assert_eq!(
            types,
            vec![
                "int",
                "varchar(30)",
                "char(2)",
                "numeric(10,2)",
                "boolean",
                "timestamp with time zone",
                "text[]",
                "uuid",
                "app.status_t",
                "serial",
            ]
        );
    }

    /// `format_type` — what introspection reports — spells a type modifier list
    /// without spaces, so a deparsed `numeric(10, 2)` would read as permanent
    /// drift against the very same column in the database.
    #[test]
    fn type_modifiers_carry_no_space_after_the_comma() {
        let d = def("create table t (a numeric(10,2), b numeric(12, 4));");
        assert_eq!(d.columns[0].data_type, "numeric(10,2)");
        assert_eq!(d.columns[1].data_type, "numeric(12,4)");
    }

    #[test]
    fn nullability_defaults_to_true_and_not_null_clears_it() {
        let d = def("create table t (a int, b int not null, c int null);");
        assert!(d.columns[0].nullable);
        assert!(!d.columns[1].nullable);
        assert!(d.columns[2].nullable);
    }

    #[test]
    fn defaults_are_captured_in_postgres_own_spelling() {
        let d = def(
            "create table t (a int default 0, b text default 'x', c uuid default gen_random_uuid(),\
             d boolean default true, e text);",
        );
        assert_eq!(d.columns[0].default_value.as_deref(), Some("0"));
        assert_eq!(d.columns[1].default_value.as_deref(), Some("'x'"));
        assert_eq!(d.columns[2].default_value.as_deref(), Some("gen_random_uuid()"));
        assert_eq!(d.columns[3].default_value.as_deref(), Some("true"));
        assert_eq!(d.columns[4].default_value, None);
    }

    #[test]
    fn identity_columns_record_which_form_and_are_not_nullable() {
        let d = def(
            "create table t (a int generated always as identity,\
             b bigint generated by default as identity, c int);",
        );
        assert_eq!(d.columns[0].identity, Some(IdentityKind::Always));
        assert!(!d.columns[0].nullable);
        assert_eq!(d.columns[1].identity, Some(IdentityKind::ByDefault));
        assert_eq!(d.columns[2].identity, None);
    }

    /// SERIAL is sugar for an integer plus an owned sequence, not an IDENTITY
    /// column — so it must not set `identity`. The `is_pk` it does set mirrors
    /// the sqlparser incumbent, which reconcile's snapshot shape depends on.
    #[test]
    fn serial_is_not_an_identity_column() {
        let d = def("create table t (a serial, b bigserial);");
        assert_eq!(d.columns[0].identity, None);
        assert!(!d.columns[0].nullable);
        assert!(d.columns[0].is_pk);
        assert!(d.columns[1].is_pk);
    }

    /// `GENERATED ALWAYS AS (expr) STORED` is a computed column, not an identity
    /// one — but a function it calls has to exist before the table is created.
    #[test]
    fn a_stored_generated_column_is_not_identity_but_keeps_its_function_ref() {
        let e = parse(
            "set search_path to app;\n\
             create table t (a int, b int generated always as (app.doubled(a)) stored);",
        );
        let d = e.table_def.as_ref().unwrap();
        assert_eq!(d.columns[1].identity, None);
        assert!(e.refers.contains(&"app.doubled".to_string()), "got {:?}", e.refers);
    }

    // ── 2. Inline column constraints: PK, unique, FK, CHECK ─────────────────

    #[test]
    fn an_inline_primary_key_flags_the_column_and_clears_nullability() {
        let d = def("create table t (id uuid primary key, other int);");
        assert!(d.columns[0].is_pk);
        assert!(!d.columns[0].nullable);
        assert!(!d.columns[1].is_pk);
    }

    #[test]
    fn an_inline_unique_flags_the_column() {
        let d = def("create table t (code text unique, other int);");
        assert!(d.columns[0].is_unique);
        assert!(!d.columns[1].is_unique);
    }

    #[test]
    fn an_inline_fk_records_its_target_and_actions() {
        let e = parse(
            "set search_path to app;\n\
             create table t (pid uuid references parent (id) on delete cascade on update set null);",
        );
        let fk = e.table_def.as_ref().unwrap().columns[0]
            .inline_fk
            .as_ref()
            .expect("inline FK");
        assert_eq!(fk.columns, vec!["pid"]);
        assert_eq!(fk.ref_schema.as_deref(), Some("app"));
        assert_eq!(fk.ref_table, "parent");
        assert_eq!(fk.ref_columns, vec!["id"]);
        assert_eq!(fk.on_delete, Some(FkAction::Cascade));
        assert_eq!(fk.on_update, Some(FkAction::SetNull));
        assert!(e.refers.contains(&"app.parent".to_string()), "got {:?}", e.refers);
    }

    /// An explicit schema on the target wins over the file's search path.
    #[test]
    fn an_inline_fk_keeps_an_explicit_target_schema() {
        let e = parse(
            "set search_path to app;\ncreate table t (pid uuid references other.parent (id));",
        );
        let fk = e.table_def.as_ref().unwrap().columns[0].inline_fk.as_ref().unwrap();
        assert_eq!(fk.ref_schema.as_deref(), Some("other"));
        assert!(e.refers.contains(&"other.parent".to_string()), "got {:?}", e.refers);
    }

    /// `references parent` with no column list targets the parent's primary key;
    /// the incumbent records `id`, and reconcile's FK shape compares against it.
    #[test]
    fn an_inline_fk_with_no_column_list_assumes_id() {
        let e = parse("set search_path to app;\ncreate table t (pid uuid references parent);");
        let fk = e.table_def.as_ref().unwrap().columns[0].inline_fk.as_ref().unwrap();
        assert_eq!(fk.ref_columns, vec!["id"]);
    }

    /// Postgres promotes a column-level CHECK to a table constraint and
    /// introspection reports it as one — dropping it here would make every
    /// inline CHECK read as a live-only constraint reconcile offers to delete.
    #[test]
    fn a_column_level_check_becomes_a_table_constraint() {
        let d = def(
            "create table t (\
               singleton boolean primary key default true check (singleton),\
               source text not null check (source in ('mcp', 'builtin')),\
               qty int constraint t_qty_ck check (qty > 0));",
        );
        let checks: Vec<(Option<String>, String)> = d
            .constraints
            .iter()
            .filter_map(|c| match c {
                TableConstraint::Check { name, expression } => {
                    Some((name.clone(), expression.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(checks.len(), 3, "got {checks:?}");
        assert!(checks.iter().any(|(n, e)| n.is_none() && e == "singleton"));
        assert!(checks.iter().any(|(_, e)| e == "source IN ('mcp', 'builtin')"));
        assert!(checks.iter().any(|(n, _)| n.as_deref() == Some("t_qty_ck")));
    }

    #[test]
    fn a_default_calling_a_function_records_a_soft_reference() {
        let e = parse(
            "set search_path to app;\ncreate table t (id uuid default app.new_id());",
        );
        let r = e
            .references
            .iter()
            .find(|r| r.name == "app.new_id")
            .expect("function reference");
        assert_eq!(r.ref_type.as_deref(), Some(crate::entity::REF_TYPE_FUNCTION));
    }

    // ── 3. Table-level constraints ──────────────────────────────────────────

    /// A composite key must produce BOTH the table-level constraint AND `is_pk`
    /// on each member column: `lift_pk_unique_keep_others` and
    /// `pk_unique_col_sets` in `reconcile` read exactly that shape.
    #[test]
    fn a_composite_primary_key_is_both_a_constraint_and_a_per_column_flag() {
        let d = def(
            "create table t (a uuid not null, b uuid not null, c int, primary key (a, b));",
        );
        assert!(d.constraints.iter().any(|c| matches!(
            c,
            TableConstraint::PrimaryKey { name, columns }
                if name.is_none() && columns == &["a".to_string(), "b".to_string()]
        )));
        assert!(d.columns[0].is_pk && !d.columns[0].nullable);
        assert!(d.columns[1].is_pk && !d.columns[1].nullable);
        assert!(!d.columns[2].is_pk);
    }

    #[test]
    fn named_table_constraints_keep_their_names() {
        let d = def(
            "set search_path to app;\n\
             create table t (id uuid, code text, qty int, pid uuid,\
               constraint t_pk primary key (id),\
               constraint t_code_uk unique (code),\
               constraint t_qty_ck check (qty > 0),\
               constraint t_pid_fk foreign key (pid) references parent (id) on delete cascade);",
        );
        assert!(d.constraints.iter().any(|c| matches!(
            c, TableConstraint::PrimaryKey { name: Some(n), .. } if n == "t_pk"
        )));
        assert!(d.constraints.iter().any(|c| matches!(
            c, TableConstraint::Unique { name: Some(n), .. } if n == "t_code_uk"
        )));
        assert!(d.constraints.iter().any(|c| matches!(
            c, TableConstraint::Check { name: Some(n), .. } if n == "t_qty_ck"
        )));
        let fk = d
            .constraints
            .iter()
            .find_map(|c| match c {
                TableConstraint::ForeignKey(fk) => Some(fk),
                _ => None,
            })
            .expect("FK constraint");
        assert_eq!(fk.name.as_deref(), Some("t_pid_fk"));
        assert_eq!(fk.columns, vec!["pid"]);
        assert_eq!(fk.ref_table, "parent");
        assert_eq!(fk.on_delete, Some(FkAction::Cascade));
    }

    /// An omitted `ON UPDATE` and an explicit `ON UPDATE NO ACTION` parse to the
    /// same Postgres action code, so both are reported as "unspecified" — the
    /// spelling the far more common omitted form has, and the one reconcile's
    /// `normalize_fk` already treats as equal to `NO ACTION`.
    #[test]
    fn an_unspecified_referential_action_is_none() {
        let d = def("create table t (pid uuid, foreign key (pid) references parent (id));");
        let fk = d
            .constraints
            .iter()
            .find_map(|c| match c {
                TableConstraint::ForeignKey(fk) => Some(fk),
                _ => None,
            })
            .unwrap();
        assert_eq!(fk.on_delete, None);
        assert_eq!(fk.on_update, None);
    }

    #[test]
    fn unnamed_table_constraints_have_no_name() {
        let d = def("create table t (a int, b int, unique (a), check (a < b));");
        assert!(d.constraints.iter().any(|c| matches!(
            c, TableConstraint::Unique { name: None, columns } if columns == &["a".to_string()]
        )));
        assert!(d.constraints.iter().any(|c| matches!(
            c, TableConstraint::Check { name: None, expression } if expression == "a < b"
        )));
    }

    /// Inline CHECKs come first, in column order, then the table-level
    /// constraints in theirs — the order the incumbent produces and that the
    /// snapshot therefore records.
    #[test]
    fn inline_checks_precede_table_level_constraints() {
        let d = def(
            "create table t (a int check (a > 0), b int, constraint t_b_ck check (b > 0));",
        );
        let names: Vec<Option<&str>> = d
            .constraints
            .iter()
            .map(|c| match c {
                TableConstraint::Check { name, .. } => name.as_deref(),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec![None, Some("t_b_ck")]);
    }

    // ── 4. Comments ─────────────────────────────────────────────────────────

    #[test]
    fn table_and_column_comments_are_captured() {
        let d = def(
            "create table t (id int, name text);\n\
             comment on table t is 'the table';\n\
             comment on column t.id is 'the id';\n\
             comment on column app.t.name is 'the name';",
        );
        assert_eq!(d.comments.table.as_deref(), Some("the table"));
        assert_eq!(d.comments.columns.get("id").map(String::as_str), Some("the id"));
        assert_eq!(d.columns[0].comment.as_deref(), Some("the id"));
        assert_eq!(d.columns[1].comment.as_deref(), Some("the name"));
    }

    // ── 5. Indexes ──────────────────────────────────────────────────────────

    #[test]
    fn a_plain_index_records_its_name_columns_and_uniqueness() {
        let d = def("create table t (a int);\ncreate unique index t_a_uk on t (a);");
        assert_eq!(d.indexes.len(), 1);
        let ix = &d.indexes[0];
        assert_eq!(ix.name.as_deref(), Some("t_a_uk"));
        assert!(ix.unique);
        assert_eq!(ix.columns[0].name, "a");
        assert!(!ix.columns[0].is_expression);
        // No explicit `USING`, so no access method is recorded (btree default).
        assert_eq!(ix.index_type, None);
    }

    #[test]
    fn an_explicit_access_method_and_opclass_survive() {
        let d = def(
            "create table t (tags text[], name text);\n\
             create index t_tags on t using gin (tags array_ops);\n\
             create index t_hnsw on t using hnsw (name);",
        );
        assert_eq!(d.indexes[0].index_type, Some(IndexType::Gin));
        assert_eq!(d.indexes[0].columns[0].opclass.as_deref(), Some("array_ops"));
        assert_eq!(d.indexes[1].index_type, Some(IndexType::Other("hnsw".to_string())));
    }

    #[test]
    fn sort_order_and_nulls_ordering_are_captured() {
        let d = def(
            "create table t (a int, b int);\n\
             create index t_ix on t (a desc nulls last, b asc nulls first);",
        );
        let cols = &d.indexes[0].columns;
        assert_eq!(cols[0].order, Some(SortOrder::Desc));
        assert_eq!(cols[0].nulls_first, Some(false));
        assert_eq!(cols[1].order, Some(SortOrder::Asc));
        assert_eq!(cols[1].nulls_first, Some(true));
    }

    /// An expression key must be flagged as one, and a non-function expression
    /// must keep the parentheses Postgres's own grammar requires around it —
    /// without them the emitted DDL is a syntax error.
    #[test]
    fn expression_keys_are_flagged_and_parenthesized_when_they_must_be() {
        let e = parse(
            "set search_path to app;\n\
             create table t (ctx jsonb, name text);\n\
             create index t_ix on t ((ctx ->> 'module'), lower(name));",
        );
        let cols = &e.table_def.as_ref().unwrap().indexes[0].columns;
        assert!(cols[0].is_expression);
        assert_eq!(cols[0].name, "(ctx ->> 'module')");
        assert_eq!(crate::emit::emit_index_column(&cols[0]), "(ctx ->> 'module')");
        assert!(cols[1].is_expression);
        assert_eq!(cols[1].name, "lower(name)");
        // The function an index key calls must exist before the table is built.
        assert!(e.refers.contains(&"app.lower".to_string()), "got {:?}", e.refers);
    }

    #[test]
    fn include_nulls_not_distinct_and_storage_parameters_are_captured() {
        let d = def(
            "create table t (a int, b int);\n\
             create unique index t_ix on t (a) include (b) nulls not distinct \
               with (fillfactor = 70, deduplicate_items = off);",
        );
        let ix = &d.indexes[0];
        assert_eq!(ix.include, vec!["b".to_string()]);
        assert!(ix.nulls_not_distinct);
        assert_eq!(ix.with_options.get("fillfactor"), Some(&"70".to_string()));
        assert_eq!(ix.with_options.get("deduplicate_items"), Some(&"off".to_string()));
    }

    /// The authored predicate is canonicalized on the way in so it matches the
    /// analyzed form `pg_get_expr` reports for the same index.
    #[test]
    fn a_partial_index_predicate_is_canonicalized() {
        let d = def(
            "create table t (id int, scope text);\n\
             create index t_ix on t (id) where scope in ('user', 'project');",
        );
        assert_eq!(
            d.indexes[0].predicate.as_deref(),
            crate::sql_expr::canonicalize_predicate(
                "scope = ANY (ARRAY['user'::text, 'project'::text])"
            )
            .as_deref(),
        );
    }

    // ── Refusal: a table that cannot be fully read must error, not degrade ───

    /// `raw_snapshot_from_entities` filters on `table_def.is_some()`, so a table
    /// dbd cannot read is absent from the desired snapshot, reads as an orphan,
    /// and `--prune` DROPs it. Anything unextractable therefore has to error.
    #[test]
    fn a_constraint_dbd_cannot_represent_errors_rather_than_dropping_silently() {
        let e = parse(
            "create table t (id int primary key, r int4range,\
             exclude using gist (r with &&));",
        );
        assert!(!e.errors.is_empty(), "an EXCLUDE constraint must not be dropped silently");
        assert!(
            e.table_def.is_none(),
            "a partially-read table must not reach the desired snapshot"
        );
    }

    /// A table whose columns come from elsewhere reads here as having none, and
    /// a zero-column desired snapshot makes reconcile offer to DROP every live
    /// column — so these must error too.
    #[test]
    fn a_table_inheriting_its_columns_errors_rather_than_reading_as_empty() {
        for sql in [
            "create table c partition of p for values from (1) to (2);",
            "create table c () inherits (p);",
            "create table c of my_type;",
            "create table c (like p including all);",
        ] {
            let e = parse(sql);
            assert!(!e.errors.is_empty(), "{sql} must error");
            assert!(e.table_def.is_none(), "{sql} must carry no table_def");
        }
    }

    #[test]
    fn invalid_sql_records_a_parse_error_naming_the_token() {
        let e = parse("create table t (");
        assert!(!e.errors.is_empty());
        assert!(e.errors[0].contains("syntax error"), "got {:?}", e.errors);
        assert!(e.table_def.is_none());
    }

    #[test]
    fn a_file_declaring_no_table_records_an_error() {
        let e = parse("select 1;");
        assert!(!e.errors.is_empty());
        assert!(e.table_def.is_none());
    }

    /// Mirrors every other native parser: an errored entity still carries the
    /// `["public"]` default, because references are qualified against it.
    #[test]
    fn search_paths_survive_both_the_happy_and_the_error_path() {
        assert_eq!(parse("set search_path to app;\ncreate table t (a int);").search_paths, vec!["app".to_string()]);
        assert_eq!(parse("create table t (").search_paths, vec!["public".to_string()]);
    }
}
