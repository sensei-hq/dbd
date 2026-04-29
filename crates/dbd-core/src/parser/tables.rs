use sqlparser::ast::{
    ColumnDef as SqlColumnDef, ColumnOption, ColumnOptionDef, ReferentialAction, Statement,
    TableConstraint as SqlTableConstraint,
};

use crate::entity::{
    ColumnDef, FkAction, ForeignKey, IndexColumn, IndexDef, Reference, TableComments, TableConstraint, TableDef,
};

/// Extract table definition and references from parsed statements.
pub fn extract_table(
    statements: &[Statement],
    search_paths: &[String],
) -> (TableDef, Vec<Reference>) {
    let mut columns = Vec::new();
    let mut constraints = Vec::new();
    let mut indexes = Vec::new();
    let mut comments = TableComments::default();
    let mut references = Vec::new();

    let default_schema = search_paths.first().map(|s| s.as_str()).unwrap_or("public");

    for stmt in statements {
        match stmt {
            Statement::CreateTable(create_table) => {
                for col_def in &create_table.columns {
                    let (col, col_refs) = extract_column(col_def, default_schema);
                    columns.push(col);
                    references.extend(col_refs);
                }

                for constraint in &create_table.constraints {
                    if let Some((tc, tc_refs)) = extract_table_constraint(constraint, default_schema)
                    {
                        // Mark columns as PK if part of a table-level PRIMARY KEY
                        if let TableConstraint::PrimaryKey { columns: ref pk_cols, .. } = tc {
                            for col in &mut columns {
                                if pk_cols.contains(&col.name) {
                                    col.is_pk = true;
                                    col.nullable = false;
                                }
                            }
                        }
                        constraints.push(tc);
                        references.extend(tc_refs);
                    }
                }
            }
            Statement::CreateIndex(create_index) => {
                let idx = extract_index(create_index);
                indexes.push(idx);
            }
            Statement::Comment {
                object_type,
                object_name,
                comment,
                ..
            } => {
                if let Some(comment_text) = comment {
                    let parts: Vec<&str> = object_name.0.iter().filter_map(|part| part.as_ident()).map(|i| i.value.as_str()).collect();
                    match object_type {
                        sqlparser::ast::CommentObject::Table => {
                            comments.table = Some(comment_text.clone());
                        }
                        sqlparser::ast::CommentObject::Column => {
                            // Column comment: last part is column name
                            if let Some(col_name) = parts.last() {
                                comments
                                    .columns
                                    .insert(col_name.to_string(), comment_text.clone());
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    // Apply column comments to column defs
    for col in &mut columns {
        if let Some(comment) = comments.columns.get(&col.name) {
            col.comment = Some(comment.clone());
        }
    }

    let table_def = TableDef {
        columns,
        constraints,
        indexes,
        comments,
    };

    (table_def, references)
}

/// Extract a column definition from a sqlparser ColumnDef.
fn extract_column(col_def: &SqlColumnDef, default_schema: &str) -> (ColumnDef, Vec<Reference>) {
    let name = col_def.name.value.clone();
    let data_type = col_def.data_type.to_string();
    let mut nullable = true;
    let mut is_pk = false;
    let mut is_unique = false;
    let mut is_identity = false;
    let mut default_value = None;
    let mut inline_fk = None;
    let mut references = Vec::new();

    for ColumnOptionDef { option, .. } in &col_def.options {
        match option {
            ColumnOption::PrimaryKey(_) => {
                is_pk = true;
                nullable = false;
            }
            ColumnOption::Unique(_) => {
                is_unique = true;
            }
            ColumnOption::NotNull => {
                nullable = false;
            }
            ColumnOption::Null => {
                nullable = true;
            }
            ColumnOption::Default(expr) => {
                default_value = Some(expr.to_string());
            }
            ColumnOption::ForeignKey(fk_constraint) => {
                let ref_table_parts: Vec<&str> =
                    fk_constraint.foreign_table.0.iter().filter_map(|part| part.as_ident()).map(|i| i.value.as_str()).collect();
                let (ref_schema, ref_table_name) = if ref_table_parts.len() > 1 {
                    (
                        Some(ref_table_parts[0].to_string()),
                        ref_table_parts[1].to_string(),
                    )
                } else {
                    (
                        Some(default_schema.to_string()),
                        ref_table_parts[0].to_string(),
                    )
                };

                let ref_cols: Vec<String> = fk_constraint.referred_columns
                    .iter()
                    .map(|i| i.value.clone())
                    .collect();

                let qualified_ref = match &ref_schema {
                    Some(s) => format!("{s}.{ref_table_name}"),
                    None => ref_table_name.clone(),
                };

                references.push(Reference {
                    name: qualified_ref,
                    ref_type: Some("table".to_string()),
                });

                inline_fk = Some(ForeignKey {
                    name: None,
                    columns: vec![name.clone()],
                    ref_schema,
                    ref_table: ref_table_name,
                    ref_columns: if ref_cols.is_empty() {
                        vec!["id".to_string()]
                    } else {
                        ref_cols
                    },
                    on_delete: convert_referential_action(&fk_constraint.on_delete),
                    on_update: convert_referential_action(&fk_constraint.on_update),
                });
            }
            ColumnOption::Generated { .. } => {
                is_identity = true;
            }
            _ => {}
        }
    }

    // Detect SERIAL-like types
    let upper_type = data_type.to_uppercase();
    if upper_type.contains("SERIAL") {
        is_identity = true;
        is_pk = true;
        nullable = false;
    }

    let col = ColumnDef {
        name,
        data_type,
        nullable,
        default_value,
        is_pk,
        is_unique,
        is_identity,
        comment: None,
        inline_fk,
    };

    (col, references)
}

/// Extract a table-level constraint.
fn extract_table_constraint(
    constraint: &SqlTableConstraint,
    default_schema: &str,
) -> Option<(TableConstraint, Vec<Reference>)> {
    match constraint {
        SqlTableConstraint::PrimaryKey(pk) => {
            let col_names: Vec<String> = pk.columns.iter().map(|c| c.column.expr.to_string()).collect();
            Some((
                TableConstraint::PrimaryKey {
                    name: pk.name.as_ref().map(|n| n.value.clone()),
                    columns: col_names,
                },
                Vec::new(),
            ))
        }
        SqlTableConstraint::Unique(uc) => {
            let col_names: Vec<String> = uc.columns.iter().map(|c| c.column.expr.to_string()).collect();
            Some((
                TableConstraint::Unique {
                    name: uc.name.as_ref().map(|n| n.value.clone()),
                    columns: col_names,
                },
                Vec::new(),
            ))
        }
        SqlTableConstraint::ForeignKey(fk_constraint) => {
            let fk_cols: Vec<String> = fk_constraint.columns.iter().map(|c| c.value.clone()).collect();
            let ref_table_parts: Vec<&str> =
                fk_constraint.foreign_table.0.iter().filter_map(|part| part.as_ident()).map(|i| i.value.as_str()).collect();
            let (ref_schema, ref_table_name) = if ref_table_parts.len() > 1 {
                (
                    Some(ref_table_parts[0].to_string()),
                    ref_table_parts[1].to_string(),
                )
            } else {
                (
                    Some(default_schema.to_string()),
                    ref_table_parts[0].to_string(),
                )
            };
            let ref_cols: Vec<String> = fk_constraint.referred_columns.iter().map(|c| c.value.clone()).collect();

            let qualified_ref = match &ref_schema {
                Some(s) => format!("{s}.{ref_table_name}"),
                None => ref_table_name.clone(),
            };

            let references = vec![Reference {
                name: qualified_ref,
                ref_type: Some("table".to_string()),
            }];

            let fk = ForeignKey {
                name: fk_constraint.name.as_ref().map(|n| n.value.clone()),
                columns: fk_cols,
                ref_schema,
                ref_table: ref_table_name,
                ref_columns: ref_cols,
                on_delete: convert_referential_action(&fk_constraint.on_delete),
                on_update: convert_referential_action(&fk_constraint.on_update),
            };

            Some((TableConstraint::ForeignKey(fk), references))
        }
        SqlTableConstraint::Check(chk) => Some((
            TableConstraint::Check {
                name: chk.name.as_ref().map(|n| n.value.clone()),
                expression: chk.expr.to_string(),
            },
            Vec::new(),
        )),
        _ => None,
    }
}

/// Extract an index definition from a CREATE INDEX statement.
fn extract_index(create_index: &sqlparser::ast::CreateIndex) -> IndexDef {
    let name = create_index
        .name
        .as_ref()
        .map(|n| n.0.iter().filter_map(|part| part.as_ident()).map(|i| i.value.clone()).collect::<Vec<_>>().join("."));

    let columns: Vec<IndexColumn> = create_index
        .columns
        .iter()
        .map(|col| IndexColumn {
            name: col.column.expr.to_string(),
            order: col.column.options.asc.map(|asc| {
                if asc {
                    crate::entity::SortOrder::Asc
                } else {
                    crate::entity::SortOrder::Desc
                }
            }),
        })
        .collect();

    IndexDef {
        name,
        columns,
        unique: create_index.unique,
        index_type: None, // sqlparser doesn't expose USING method easily
    }
}

/// Convert sqlparser's ReferentialAction to our FkAction.
fn convert_referential_action(action: &Option<ReferentialAction>) -> Option<FkAction> {
    match action {
        Some(ReferentialAction::Cascade) => Some(FkAction::Cascade),
        Some(ReferentialAction::Restrict) => Some(FkAction::Restrict),
        Some(ReferentialAction::SetNull) => Some(FkAction::SetNull),
        Some(ReferentialAction::SetDefault) => Some(FkAction::SetDefault),
        Some(ReferentialAction::NoAction) => Some(FkAction::NoAction),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::PostgreSqlDialect;
    use sqlparser::parser::Parser;

    fn parse(sql: &str) -> Vec<Statement> {
        Parser::parse_sql(&PostgreSqlDialect {}, sql).unwrap()
    }

    #[test]
    fn extracts_simple_table_columns() {
        let stmts = parse("CREATE TABLE foo (id int PRIMARY KEY, name varchar(100) NOT NULL);");
        let (def, _) = extract_table(&stmts, &["public".to_string()]);

        assert_eq!(def.columns.len(), 2);
        assert_eq!(def.columns[0].name, "id");
        assert!(def.columns[0].is_pk);
        assert!(!def.columns[0].nullable);
        assert_eq!(def.columns[1].name, "name");
        assert!(!def.columns[1].nullable);
    }

    #[test]
    fn extracts_default_values() {
        let stmts = parse("CREATE TABLE foo (active boolean DEFAULT true, count int DEFAULT 0);");
        let (def, _) = extract_table(&stmts, &["public".to_string()]);

        assert_eq!(def.columns[0].default_value, Some("true".to_string()));
        assert_eq!(def.columns[1].default_value, Some("0".to_string()));
    }

    #[test]
    fn extracts_inline_fk_with_schema_qualification() {
        let stmts = parse(
            "SET search_path TO config; CREATE TABLE bar (id int, foo_id int REFERENCES foo(id));",
        );
        let (def, refs) = extract_table(&stmts, &["config".to_string()]);

        let fk_col = def.columns.iter().find(|c| c.name == "foo_id").unwrap();
        let fk = fk_col.inline_fk.as_ref().unwrap();
        assert_eq!(fk.ref_table, "foo");
        assert_eq!(fk.ref_schema, Some("config".to_string()));
        assert_eq!(fk.ref_columns, vec!["id"]);

        // Reference should be schema-qualified
        assert!(refs.iter().any(|r| r.name == "config.foo"));
    }

    #[test]
    fn extracts_table_level_fk() {
        let stmts = parse(
            "CREATE TABLE orders (
                id int PRIMARY KEY,
                user_id int,
                FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE ON UPDATE NO ACTION
            );",
        );
        let (def, refs) = extract_table(&stmts, &["public".to_string()]);

        let fk_constraint = def.constraints.iter().find_map(|c| match c {
            TableConstraint::ForeignKey(fk) => Some(fk),
            _ => None,
        });
        let fk = fk_constraint.unwrap();
        assert_eq!(fk.columns, vec!["user_id"]);
        assert_eq!(fk.ref_table, "users");
        assert_eq!(fk.on_delete, Some(FkAction::Cascade));
        assert_eq!(fk.on_update, Some(FkAction::NoAction));

        assert!(refs.iter().any(|r| r.name.contains("users")));
    }

    #[test]
    fn extracts_unique_constraint() {
        let stmts = parse(
            "CREATE TABLE foo (id int, name varchar, UNIQUE (name));",
        );
        let (def, _) = extract_table(&stmts, &["public".to_string()]);

        let unique = def.constraints.iter().find(|c| matches!(c, TableConstraint::Unique { .. }));
        assert!(unique.is_some());
    }

    #[test]
    fn extracts_index() {
        let stmts = parse(
            "CREATE TABLE foo (id int, name varchar);
             CREATE UNIQUE INDEX foo_name_idx ON foo(name);",
        );
        let (def, _) = extract_table(&stmts, &["public".to_string()]);

        assert_eq!(def.indexes.len(), 1);
        assert!(def.indexes[0].unique);
        assert_eq!(def.indexes[0].name, Some("foo_name_idx".to_string()));
        assert_eq!(def.indexes[0].columns[0].name, "name");
    }

    #[test]
    fn extracts_table_comment() {
        let stmts = parse(
            "CREATE TABLE foo (id int);
             COMMENT ON TABLE foo IS 'A test table';",
        );
        let (def, _) = extract_table(&stmts, &["public".to_string()]);
        assert_eq!(def.comments.table, Some("A test table".to_string()));
    }

    #[test]
    fn extracts_column_comments() {
        let stmts = parse(
            "CREATE TABLE foo (id int, name varchar);
             COMMENT ON COLUMN foo.id IS 'Primary key';
             COMMENT ON COLUMN foo.name IS 'Display name';",
        );
        let (def, _) = extract_table(&stmts, &["public".to_string()]);

        let id_col = def.columns.iter().find(|c| c.name == "id").unwrap();
        assert_eq!(id_col.comment, Some("Primary key".to_string()));

        let name_col = def.columns.iter().find(|c| c.name == "name").unwrap();
        assert_eq!(name_col.comment, Some("Display name".to_string()));
    }

    #[test]
    fn nullable_defaults_to_true() {
        let stmts = parse("CREATE TABLE foo (name varchar);");
        let (def, _) = extract_table(&stmts, &["public".to_string()]);
        assert!(def.columns[0].nullable);
    }
}
