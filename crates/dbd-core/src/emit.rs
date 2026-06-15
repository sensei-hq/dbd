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
                lines.push(s);
            }
            crate::entity::TableConstraint::Check { expression, .. } => {
                lines.push(format!("  CHECK ({expression})"));
            }
        }
    }

    let mut out = format!("CREATE TABLE {qname} (\n{}\n);", lines.join(",\n"));

    // Indexes (skip ones that merely back a PK/UNIQUE — emit explicit indexes only)
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
        out.push_str(&format!(
            "\nCREATE {unique}INDEX {} ON {qname} ({cols});",
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

        // Round-trip: emitted DDL re-parses to a TableDef with the same column set.
        // Use the public parse_entity entry point with a fake table file path.
        let fake_path = std::path::Path::new("ddl/table/shop/orders.sql");
        let parsed_entity = crate::parser::parse_entity(fake_path, &sql)
            .expect("emitted table DDL should parse");
        let parsed = parsed_entity.table_def.expect("parsed entity should have a table_def");
        let cols: Vec<&str> = parsed.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(cols, vec!["id", "customer_id", "total_cents"]);
    }
}
