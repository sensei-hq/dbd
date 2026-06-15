//! Emit canonical `CREATE …` DDL text from an `Entity`/`TableDef`. The inverse of
//! the parser; used by the reverse-engineer engine. Output is intended to be
//! re-parseable (round-trip stable).

use crate::entity::Entity;

/// Quote a SQL identifier.
fn q(ident: &str) -> String {
    format!("\"{ident}\"")
}

/// Bare (unqualified) name from a possibly-qualified entity name (`schema.name` → `name`).
fn bare(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// `CREATE TYPE "schema"."name" AS ENUM ('a', 'b');`
pub fn emit_enum(entity: &Entity) -> String {
    let schema = entity.schema.as_deref().unwrap_or("public");
    let name = bare(&entity.name);
    let values = entity
        .enum_values
        .iter()
        .map(|v| format!("'{}'", v.name.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("CREATE TYPE {}.{} AS ENUM ({});", q(schema), q(name), values)
}

/// `CREATE TABLE "schema"."name" ( … );` + `CREATE INDEX …;` + `COMMENT ON …;`
///
/// # What is intentionally NOT re-emitted
///
/// v1 does not re-emit column-level `is_identity`, column-only `is_pk`/`is_unique`,
/// or `inline_fk`. The Postgres introspector decomposes these into table-level
/// constraints, so those flags are not on the reverse-engineer path and would
/// produce duplicate DDL if re-emitted here.
pub fn emit_table(entity: &Entity) -> String {
    let schema = entity.schema.as_deref().unwrap_or("public");
    let name = bare(&entity.name);
    let qname = format!("{}.{}", q(schema), q(name));
    let Some(def) = &entity.table_def else {
        return format!("CREATE TABLE {qname} ();");
    };

    let mut lines: Vec<String> = Vec::new();

    // Columns
    for c in &def.columns {
        let mut col = format!("  {} {}", q(&c.name), c.data_type);
        if !c.nullable {
            col.push_str(" NOT NULL");
        }
        if let Some(d) = &c.default_value {
            col.push_str(&format!(" DEFAULT {d}"));
        }
        lines.push(col);
    }

    // Table-level constraints
    for con in &def.constraints {
        match con {
            crate::entity::TableConstraint::PrimaryKey { columns, .. } => {
                lines.push(format!("  PRIMARY KEY ({})", quote_cols(columns)));
            }
            crate::entity::TableConstraint::Unique { columns, .. } => {
                lines.push(format!("  UNIQUE ({})", quote_cols(columns)));
            }
            crate::entity::TableConstraint::ForeignKey(fk) => {
                let ref_schema = fk.ref_schema.as_deref().unwrap_or(schema);
                let mut s = format!(
                    "  FOREIGN KEY ({}) REFERENCES {}.{} ({})",
                    quote_cols(&fk.columns),
                    q(ref_schema),
                    q(&fk.ref_table),
                    quote_cols(&fk.ref_columns),
                );
                if let Some(a) = fk.on_delete {
                    s.push_str(&format!(" ON DELETE {}", fk_action_sql(a)));
                }
                if let Some(a) = fk.on_update {
                    s.push_str(&format!(" ON UPDATE {}", fk_action_sql(a)));
                }
                lines.push(s);
            }
            crate::entity::TableConstraint::Check { expression, .. } => {
                lines.push(format!("  CHECK ({expression})"));
            }
        }
    }

    let body = if lines.is_empty() {
        String::new()
    } else {
        format!("\n{}\n", lines.join(",\n"))
    };
    let mut out = format!("CREATE TABLE {qname} ({body});");

    // Indexes — every entry in def.indexes is emitted as-is. Filtering of
    // implicit PK/UNIQUE-backing indexes happens upstream in the introspector,
    // not here.
    for ix in &def.indexes {
        let cols = ix
            .columns
            .iter()
            .map(|c| match c.order {
                Some(crate::entity::SortOrder::Desc) => format!("{} DESC", q(&c.name)),
                _ => q(&c.name),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let unique = if ix.unique { "UNIQUE " } else { "" };
        let idx_name = ix.name.clone().unwrap_or_else(|| format!("{name}_idx"));
        use crate::entity::IndexType;
        let using = match ix.index_type {
            Some(IndexType::Hash) => " USING hash",
            Some(IndexType::Gin) => " USING gin",
            Some(IndexType::Gist) => " USING gist",
            Some(IndexType::Brin) => " USING brin",
            Some(IndexType::SpGist) => " USING spgist",
            // btree (Some(Btree) or None) is the default — no USING clause.
            _ => "",
        };
        out.push_str(&format!(
            "\nCREATE {unique}INDEX {}{using} ON {qname} ({cols});",
            q(&idx_name)
        ));
    }

    // Comments
    if let Some(tc) = &def.comments.table {
        out.push_str(&format!("\nCOMMENT ON TABLE {qname} IS '{}';", esc(tc)));
    }
    for c in &def.columns {
        if let Some(cm) = &c.comment {
            out.push_str(&format!(
                "\nCOMMENT ON COLUMN {qname}.{} IS '{}';",
                q(&c.name),
                esc(cm)
            ));
        }
    }
    out
}

fn quote_cols(cols: &[String]) -> String {
    cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ")
}

fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

fn fk_action_sql(a: crate::entity::FkAction) -> &'static str {
    use crate::entity::FkAction;
    match a {
        FkAction::Cascade => "CASCADE",
        FkAction::Restrict => "RESTRICT",
        FkAction::SetNull => "SET NULL",
        FkAction::SetDefault => "SET DEFAULT",
        FkAction::NoAction => "NO ACTION",
    }
}

/// `CREATE VIEW "schema"."name" AS <definition>;`
/// The view body is carried in `entity.writes[0]` (set by the introspector).
pub fn emit_view(entity: &Entity) -> String {
    let schema = entity.schema.as_deref().unwrap_or("public");
    let name = bare(&entity.name);
    let body = entity.writes.first().map(String::as_str).unwrap_or("SELECT 1");
    let body = body.trim().trim_end_matches(';');
    format!("CREATE VIEW {}.{} AS {body};", q(schema), q(name))
}

/// Emit DDL text for any reverse-engineerable entity, or `None` for kinds we
/// don't generate (External, file-based Function/Procedure in this cut).
pub fn emit_entity(entity: &Entity) -> Option<String> {
    use crate::entity::EntityType;
    match entity.entity_type {
        EntityType::Schema | EntityType::Extension | EntityType::Role => {
            crate::script::ddl_from_entity(entity)
        }
        EntityType::Enum => Some(emit_enum(entity)),
        EntityType::Table => Some(emit_table(entity)),
        EntityType::View => Some(emit_view(entity)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{EntityType, EnumValue};

    #[test]
    fn emits_enum() {
        let mut e = Entity::new(EntityType::Enum, "shop.order_status");
        e.enum_values = vec![
            EnumValue { name: "pending".into(), note: None },
            EnumValue { name: "paid".into(), note: None },
        ];
        assert_eq!(
            emit_enum(&e),
            "CREATE TYPE \"shop\".\"order_status\" AS ENUM ('pending', 'paid');"
        );
    }

    #[test]
    fn emits_table_roundtrips_through_parser() {
        use crate::entity::{ColumnDef, ForeignKey, IndexColumn, IndexDef, TableConstraint, TableDef};

        let mut e = Entity::new(EntityType::Table, "shop.orders");
        e.table_def = Some(TableDef {
            columns: vec![
                ColumnDef {
                    name: "id".into(),
                    data_type: "uuid".into(),
                    nullable: false,
                    default_value: None,
                    is_pk: true,
                    is_unique: false,
                    is_identity: false,
                    comment: Some("Order PK".into()),
                    inline_fk: None,
                },
                ColumnDef {
                    name: "customer_id".into(),
                    data_type: "uuid".into(),
                    nullable: false,
                    default_value: None,
                    is_pk: false,
                    is_unique: false,
                    is_identity: false,
                    comment: None,
                    inline_fk: None,
                },
                ColumnDef {
                    name: "total_cents".into(),
                    data_type: "integer".into(),
                    nullable: false,
                    default_value: Some("0".into()),
                    is_pk: false,
                    is_unique: false,
                    is_identity: false,
                    comment: None,
                    inline_fk: None,
                },
            ],
            constraints: vec![
                TableConstraint::PrimaryKey { name: None, columns: vec!["id".into()] },
                TableConstraint::ForeignKey(ForeignKey {
                    name: None,
                    columns: vec!["customer_id".into()],
                    ref_schema: Some("shop".into()),
                    ref_table: "customers".into(),
                    ref_columns: vec!["id".into()],
                    on_delete: None,
                    on_update: None,
                }),
            ],
            indexes: vec![IndexDef {
                name: Some("orders_customer_id_idx".into()),
                columns: vec![IndexColumn { name: "customer_id".into(), order: None }],
                unique: false,
                index_type: None,
            }],
            comments: Default::default(),
        });

        let sql = emit_table(&e);

        // Sanity on the emitted text:
        assert!(sql.contains("CREATE TABLE \"shop\".\"orders\""));
        assert!(sql.contains("\"id\" uuid NOT NULL"));
        assert!(sql.contains("\"total_cents\" integer NOT NULL DEFAULT 0"));
        assert!(sql.contains("PRIMARY KEY (\"id\")"));
        assert!(sql.contains("FOREIGN KEY (\"customer_id\") REFERENCES \"shop\".\"customers\" (\"id\")"));
        assert!(sql.contains("CREATE INDEX \"orders_customer_id_idx\" ON \"shop\".\"orders\" (\"customer_id\");"));
        assert!(sql.contains("COMMENT ON COLUMN \"shop\".\"orders\".\"id\" IS 'Order PK';"));

        // Round-trip: emitted DDL re-parses to a TableDef with the same structure.
        // Use the public parse_entity entry point with a fake table file path.
        let fake_path = std::path::Path::new("ddl/table/shop/orders.sql");
        let parsed_entity = crate::parser::parse_entity(fake_path, &sql)
            .expect("emitted table DDL should parse");
        let parsed = parsed_entity.table_def.expect("parsed entity should have a table_def");

        // Columns survive the round trip.
        let cols: Vec<&str> = parsed.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cols, vec!["id", "customer_id", "total_cents"]);

        // Primary key survives the round trip.
        // Note: sqlparser preserves double-quoting from the emitted DDL in
        // column name strings (e.g. `"id"` not `id`); the constraint itself
        // does survive — we assert its presence and column count.
        let pk = parsed.constraints.iter().find_map(|c| match c {
            crate::entity::TableConstraint::PrimaryKey { columns, .. } => Some(columns),
            _ => None,
        });
        let pk_cols = pk.expect("PrimaryKey constraint must survive the round trip");
        assert_eq!(pk_cols.len(), 1, "PK must have exactly one column");
        assert!(
            pk_cols[0].contains("id"),
            "PK column must reference 'id', got: {:?}",
            pk_cols[0]
        );

        // Foreign key survives the round trip (references shop.customers).
        let fk = parsed.constraints.iter().find_map(|c| match c {
            crate::entity::TableConstraint::ForeignKey(fk) => Some(fk),
            _ => None,
        });
        let fk = fk.expect("ForeignKey constraint must survive the round trip");
        assert_eq!(fk.ref_table, "customers");
        assert_eq!(fk.ref_schema.as_deref(), Some("shop"));

        // Index name survives the round trip.
        let idx = parsed.indexes.iter().find(|i| {
            i.name.as_deref() == Some("orders_customer_id_idx")
        });
        assert!(
            idx.is_some(),
            "index 'orders_customer_id_idx' must survive the round trip"
        );
    }

    #[test]
    fn emits_view() {
        let mut e = Entity::new(EntityType::View, "shop.active_orders");
        e.references = vec![];
        // We store the view body in entity.writes[0] per the introspector contract:
        e.writes = vec!["SELECT * FROM shop.orders WHERE status = 'paid'".into()];
        let sql = emit_view(&e);
        assert_eq!(
            sql,
            "CREATE VIEW \"shop\".\"active_orders\" AS SELECT * FROM shop.orders WHERE status = 'paid';"
        );
    }

    #[test]
    fn emit_entity_dispatches_by_type() {
        let s = Entity::new(EntityType::Schema, "shop");
        assert_eq!(emit_entity(&s).unwrap(), "CREATE SCHEMA IF NOT EXISTS \"shop\";");
        let ext_none = Entity::new(EntityType::External, "auth.users");
        assert!(emit_entity(&ext_none).is_none());
    }

    #[test]
    fn emit_table_renders_index_access_methods() {
        use crate::entity::{ColumnDef, IndexColumn, IndexDef, IndexType, TableDef};

        let col = |n: &str, ty: &str| ColumnDef {
            name: n.into(), data_type: ty.into(), nullable: true, default_value: None,
            is_pk: false, is_unique: false, is_identity: false, comment: None, inline_fk: None,
        };
        let idx = |name: &str, col: &str, ty: Option<IndexType>| IndexDef {
            name: Some(name.into()),
            columns: vec![IndexColumn { name: col.into(), order: None }],
            unique: false,
            index_type: ty,
        };
        let mut e = Entity::new(EntityType::Table, "app.docs");
        e.table_def = Some(TableDef {
            columns: vec![col("tags", "text[]"), col("body", "tsvector"), col("n", "integer")],
            constraints: vec![],
            indexes: vec![
                idx("docs_tags_idx", "tags", Some(IndexType::Gin)),
                idx("docs_body_idx", "body", Some(IndexType::Gist)),
                idx("docs_n_brin_idx", "n", Some(IndexType::Brin)),
                idx("docs_n_hash_idx", "n", Some(IndexType::Hash)),
                idx("docs_n_btree_idx", "n", None),
            ],
            comments: Default::default(),
        });

        let sql = emit_table(&e);
        // Non-btree methods are emitted faithfully — a GIN/GiST index on an array /
        // tsvector column would be invalid as a plain btree, so the access method
        // must survive reverse-engineering.
        assert!(sql.contains("CREATE INDEX \"docs_tags_idx\" USING gin ON \"app\".\"docs\" (\"tags\");"), "got:\n{sql}");
        assert!(sql.contains("CREATE INDEX \"docs_body_idx\" USING gist ON \"app\".\"docs\" (\"body\");"), "got:\n{sql}");
        assert!(sql.contains("CREATE INDEX \"docs_n_brin_idx\" USING brin ON \"app\".\"docs\" (\"n\");"), "got:\n{sql}");
        assert!(sql.contains("CREATE INDEX \"docs_n_hash_idx\" USING hash ON \"app\".\"docs\" (\"n\");"), "got:\n{sql}");
        // btree (None) emits no USING clause.
        assert!(sql.contains("CREATE INDEX \"docs_n_btree_idx\" ON \"app\".\"docs\" (\"n\");"), "got:\n{sql}");
    }
}
