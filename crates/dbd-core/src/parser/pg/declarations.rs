//! What a SQL file *declares* — the identity `Entity::from_file` takes from the
//! path, read off the statements instead.
//!
//! Two jobs, and the split between them is the whole module:
//!
//! - a statement that **declares** an entity (`CREATE TABLE`, `CREATE VIEW`, …)
//!   contributes its type and qualified name;
//! - a statement that **attaches** to one (`CREATE INDEX`, `COMMENT ON`) has no
//!   identity of its own and must be folded into the entity it *names*.
//!
//! Getting the second one wrong is the quiet failure: Postgres cannot nest an
//! index or a comment inside `CREATE TABLE`, so they arrive as siblings, and a
//! grouping that assumed "belongs to the preceding declaration" would silently
//! attach `CREATE INDEX … ON a` to `b` in a file that declares both.
//!
//! Statements are sliced out of the original SQL by `RawStmt.stmt_location` /
//! `stmt_len` rather than deparsed, so an entity's DDL is the author's bytes —
//! which matters because a materialized view's body is stored verbatim and
//! re-emitted (see `matviews`).

use std::ops::Range;

use pg_query::NodeEnum;
use pg_query::protobuf;

use crate::entity::EntityType;

/// One entity a file declares, with every source range that defines it.
pub(in crate::parser) struct Declaration {
    pub entity_type: EntityType,
    /// Schema-qualified for the types that have a schema; bare otherwise.
    pub name: String,
    pub schema: Option<String>,
    /// The declaring statement first, then its attached siblings in source
    /// order.
    pub ranges: Vec<Range<usize>>,
}

/// Everything a file declares, in source order.
///
/// `default_schema` qualifies an unqualified name, matching how Postgres would
/// resolve it against `search_path`. An attachment naming no declaration in
/// this file is dropped: an index on a table defined elsewhere is not this
/// file's entity, and inventing a stub for it would fabricate a node.
pub(in crate::parser) fn declarations(
    parsed: &pg_query::ParseResult,
    sql: &str,
    default_schema: &str,
) -> Vec<Declaration> {
    let mut declared_here: Vec<Declaration> = Vec::new();
    let mut attachments: Vec<(String, Range<usize>)> = Vec::new();

    for raw in &parsed.protobuf.stmts {
        let Some(node) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) else {
            continue;
        };
        let range = stmt_range(raw, sql.len());

        if let Some((entity_type, schema, name)) = declared(node, default_schema) {
            declared_here.push(Declaration {
                entity_type,
                name,
                schema,
                ranges: vec![range],
            });
        } else if let Some(target) = attaches_to(node, default_schema) {
            attachments.push((target, range));
        }
    }

    for (target, range) in attachments {
        if let Some(owner) = declared_here.iter_mut().find(|d| owns(d, &target)) {
            owner.ranges.push(range);
        }
    }
    declared_here
}

/// Source ranges of statements that apply to every entity in the file rather
/// than to one — `SET search_path` is the only one today, and it has to lead
/// each reassembled fragment or unqualified references resolve to the wrong
/// schema.
pub(in crate::parser) fn ambient_ranges(parsed: &pg_query::ParseResult, sql: &str) -> Vec<Range<usize>> {
    parsed
        .protobuf
        .stmts
        .iter()
        .filter(|raw| {
            matches!(
                raw.stmt.as_ref().and_then(|s| s.node.as_ref()),
                Some(NodeEnum::VariableSetStmt(_))
            )
        })
        .map(|raw| stmt_range(raw, sql.len()))
        .collect()
}

/// Reassemble one entity's DDL: the ambient statements, then its own.
///
/// Each slice is trimmed and re-terminated rather than concatenated verbatim,
/// because `stmt_location` points at the statement start — the separator and
/// any run of whitespace before it belong to neither neighbour.
pub(in crate::parser) fn assemble(sql: &str, ambient: &[Range<usize>], ranges: &[Range<usize>]) -> String {
    let mut out = String::new();
    for range in ambient.iter().chain(ranges.iter()) {
        let stmt = sql[range.clone()].trim().trim_end_matches(';').trim_end();
        if stmt.is_empty() {
            continue;
        }
        out.push_str(stmt);
        out.push_str(";\n");
    }
    out
}

/// The byte range a statement occupies in the source.
fn stmt_range(raw: &protobuf::RawStmt, sql_len: usize) -> Range<usize> {
    let start = (raw.stmt_location as usize).min(sql_len);
    // `stmt_len == 0` means "runs to the end of input" — a final statement with
    // no trailing `;`. Same caveat `matviews::extract_body` documents.
    let end = if raw.stmt_len == 0 {
        sql_len
    } else {
        (start + raw.stmt_len as usize).min(sql_len)
    };
    start..end
}

/// The entity a declaring statement declares, or `None` if it declares none.
fn declared(node: &NodeEnum, default_schema: &str) -> Option<(EntityType, Option<String>, String)> {
    match node {
        NodeEnum::CreateStmt(c) => {
            let (schema, name) = from_range_var(c.relation.as_ref()?, default_schema);
            Some((EntityType::Table, schema, name))
        }
        NodeEnum::ViewStmt(v) => {
            let (schema, name) = from_range_var(v.view.as_ref()?, default_schema);
            Some((EntityType::View, schema, name))
        }
        // `CREATE MATERIALIZED VIEW` and `CREATE TABLE AS` share this node in
        // libpg_query's grammar; `objtype` is what tells them apart — the same
        // discrimination `matviews::matview_raw_stmt` makes.
        NodeEnum::CreateTableAsStmt(c) => {
            let (schema, name) = from_range_var(c.into.as_ref()?.rel.as_ref()?, default_schema);
            let entity_type = if c.objtype == protobuf::ObjectType::ObjectMatview as i32 {
                EntityType::MaterializedView
            } else {
                EntityType::Table
            };
            Some((entity_type, schema, name))
        }
        NodeEnum::CreateEnumStmt(c) => {
            let (schema, name) = from_parts(&name_parts(&c.type_name), default_schema)?;
            Some((EntityType::Enum, schema, name))
        }
        NodeEnum::CreateFunctionStmt(c) => {
            let (schema, name) = from_parts(&name_parts(&c.funcname), default_schema)?;
            let entity_type = if c.is_procedure {
                EntityType::Procedure
            } else {
                EntityType::Function
            };
            Some((entity_type, schema, name))
        }
        NodeEnum::CreateSeqStmt(c) => {
            let (schema, name) = from_range_var(c.sequence.as_ref()?, default_schema);
            Some((EntityType::Sequence, schema, name))
        }
        // Unqualified by nature — these live outside any schema, so a bare name
        // is the whole identity and must not be qualified with `default_schema`.
        NodeEnum::CreateRoleStmt(c) => Some((EntityType::Role, None, c.role.clone())),
        NodeEnum::CreateSchemaStmt(c) => Some((EntityType::Schema, None, c.schemaname.clone())),
        NodeEnum::CreateExtensionStmt(c) => Some((EntityType::Extension, None, c.extname.clone())),
        _ => None,
    }
}

/// The qualified name a subordinate statement belongs to.
fn attaches_to(node: &NodeEnum, default_schema: &str) -> Option<String> {
    match node {
        NodeEnum::IndexStmt(ix) => Some(from_range_var(ix.relation.as_ref()?, default_schema).1),
        NodeEnum::CommentStmt(c) => comment_target(c, default_schema),
        _ => None,
    }
}

/// What a `COMMENT ON` names.
///
/// The object node's shape follows the object type: a list of name parts for a
/// relation, an `ObjectWithArgs` for a routine, a bare string for a role or
/// schema. A column comment carries the column as its last part, which is not
/// part of the owning relation's name.
fn comment_target(stmt: &protobuf::CommentStmt, default_schema: &str) -> Option<String> {
    let parts = match stmt.object.as_ref()?.node.as_ref()? {
        NodeEnum::List(list) => name_parts(&list.items),
        NodeEnum::ObjectWithArgs(owa) => name_parts(&owa.objname),
        NodeEnum::String(s) => vec![s.sval.clone()],
        _ => return None,
    };

    match stmt.objtype() {
        // `[schema, table, column]` or `[table, column]` — the column is last.
        protobuf::ObjectType::ObjectColumn => {
            let end = parts.len().checked_sub(1)?;
            from_parts(&parts[..end], default_schema).map(|(_, name)| name)
        }
        // Schema-less objects: the bare name is the identity.
        protobuf::ObjectType::ObjectRole
        | protobuf::ObjectType::ObjectSchema
        | protobuf::ObjectType::ObjectExtension => parts.last().cloned(),
        _ => from_parts(&parts, default_schema).map(|(_, name)| name),
    }
}

/// Whether a declaration owns an attachment naming `target`.
///
/// A schema-less entity (role, schema, extension) is named bare, so an
/// attachment that arrived qualified must still match it.
fn owns(decl: &Declaration, target: &str) -> bool {
    decl.name == target || (decl.schema.is_none() && target.rsplit('.').next() == Some(decl.name.as_str()))
}

fn from_range_var(rv: &protobuf::RangeVar, default_schema: &str) -> (Option<String>, String) {
    qualify(&rv.schemaname, &rv.relname, default_schema)
}

fn qualify(schema: &str, name: &str, default_schema: &str) -> (Option<String>, String) {
    let schema = if schema.is_empty() { default_schema } else { schema };
    (Some(schema.to_string()), format!("{schema}.{name}"))
}

fn name_parts(nodes: &[protobuf::Node]) -> Vec<String> {
    nodes
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(NodeEnum::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// A dotted name as Postgres spells it: `name`, `schema.name`, or
/// `database.schema.name`.
fn from_parts(parts: &[String], default_schema: &str) -> Option<(Option<String>, String)> {
    match parts {
        [name] => Some(qualify("", name, default_schema)),
        [schema, name] => Some(qualify(schema, name, default_schema)),
        [.., schema, name] => Some(qualify(schema, name, default_schema)),
        [] => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decls(sql: &str) -> Vec<(EntityType, String, usize)> {
        let parsed = pg_query::parse(sql).expect("valid SQL");
        declarations(&parsed, sql, "public")
            .into_iter()
            .map(|d| (d.entity_type, d.name, d.ranges.len()))
            .collect()
    }

    #[test]
    fn a_create_table_declares_a_table() {
        assert_eq!(
            decls("create table app.t (id int);"),
            vec![(EntityType::Table, "app.t".to_string(), 1)]
        );
    }

    /// The trap this module exists for: attachments follow the name, not the
    /// source order.
    ///
    /// The index names the **second** table deliberately. Naming the first
    /// would make "match by name" and "match the first declaration"
    /// indistinguishable, and the test would pass against an implementation
    /// that ignores the name entirely — verified by mutating `owns` to `true`,
    /// which this catches and the earlier fixture did not.
    #[test]
    fn an_attachment_follows_the_name_not_the_position() {
        let got = decls(
            "create table app.a (id int, x text);\n\
             create table app.b (id int, y text);\n\
             create index b_y on app.b (y);",
        );
        assert_eq!(
            got,
            vec![
                (EntityType::Table, "app.a".to_string(), 1),
                (EntityType::Table, "app.b".to_string(), 2),
            ]
        );
    }

    /// An index whose table is not declared here belongs to no entity in this
    /// file. Dropping it is correct; inventing a stub would fabricate a node.
    ///
    /// A declaration is present so the orphan has somewhere wrong to land — an
    /// empty file would pass this against any implementation.
    #[test]
    fn an_orphan_attachment_is_dropped_not_reassigned() {
        assert_eq!(
            decls(
                "create table app.t (id int);\n\
                 create index i on other.elsewhere (x);"
            ),
            vec![(EntityType::Table, "app.t".to_string(), 1)]
        );
    }

    #[test]
    fn a_matview_is_not_a_table_despite_sharing_the_node() {
        assert_eq!(
            decls("create materialized view app.mv as select 1;"),
            vec![(EntityType::MaterializedView, "app.mv".to_string(), 1)]
        );
        // `CREATE TABLE AS` uses the same node with a different objtype.
        assert_eq!(
            decls("create table app.t as select 1;"),
            vec![(EntityType::Table, "app.t".to_string(), 1)]
        );
    }

    #[test]
    fn a_schema_less_entity_keeps_its_bare_name() {
        assert_eq!(decls("create role r;"), vec![(EntityType::Role, "r".to_string(), 1)]);
        assert_eq!(
            decls("create schema s;"),
            vec![(EntityType::Schema, "s".to_string(), 1)]
        );
    }

    #[test]
    fn a_column_comment_attaches_to_its_table_not_a_phantom() {
        let got = decls(
            "create table app.t (id int);\n\
             comment on column app.t.id is 'pk';",
        );
        assert_eq!(got, vec![(EntityType::Table, "app.t".to_string(), 2)]);
    }

    #[test]
    fn a_final_statement_without_a_semicolon_is_not_truncated() {
        let sql = "create table app.t (id int)";
        let parsed = pg_query::parse(sql).unwrap();
        let d = declarations(&parsed, sql, "public");
        let assembled = assemble(sql, &[], &d[0].ranges);
        assert!(assembled.contains("(id int)"), "got: {assembled}");
    }

    #[test]
    fn ambient_set_leads_every_reassembled_fragment() {
        let sql = "set search_path to app;\ncreate table t (id int);";
        let parsed = pg_query::parse(sql).unwrap();
        let ambient = ambient_ranges(&parsed, sql);
        let d = declarations(&parsed, sql, "app");
        let assembled = assemble(sql, &ambient, &d[0].ranges);
        assert!(assembled.starts_with("set search_path to app;"), "got: {assembled}");
    }
}
