    // ── D1: identical snapshots → no diffs ──────────────────

    #[test]
    fn d1_identical_snapshots_produce_no_diff() {
        let a = snap(vec![table("public", "users", vec![col("id", "int")])], vec![]);
        let b = snap(vec![table("public", "users", vec![col("id", "int")])], vec![]);
        let diffs = diff(&a, &b);
        assert!(diffs.is_empty(), "identical snapshots should produce no diffs");
    }

    // ── D2: new table added ─────────────────────────────────

    #[test]
    fn d2_new_table_detected_as_add() {
        let a = snap(vec![], vec![]);
        let b = snap(vec![table("public", "users", vec![col("id", "int")])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].entity_name, "public.users");
        assert!(matches!(diffs[0].action, DiffAction::Add));
    }

    // ── D3: table dropped ───────────────────────────────────

    #[test]
    fn d3_removed_table_detected_as_drop() {
        let a = snap(vec![table("public", "users", vec![col("id", "int")])], vec![]);
        let b = snap(vec![], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].entity_name, "public.users");
        assert!(matches!(diffs[0].action, DiffAction::Drop));
    }

    // ── D4: column added to existing table ──────────────────

    #[test]
    fn d4_added_column_detected() {
        let a = snap(vec![table("public", "users", vec![col("id", "int")])], vec![]);
        let b = snap(vec![table(
            "public",
            "users",
            vec![col("id", "int"), col("email", "text")],
        )], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "email");
            assert_eq!(changes[0].field_type, FieldType::Column);
            assert!(matches!(changes[0].action, ChangeAction::Add(_)));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D5: column dropped from existing table ──────────────

    #[test]
    fn d5_dropped_column_detected() {
        let a = snap(vec![table(
            "public",
            "users",
            vec![col("id", "int"), col("email", "text")],
        )], vec![]);
        let b = snap(vec![table("public", "users", vec![col("id", "int")])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "email");
            assert_eq!(changes[0].field_type, FieldType::Column);
            assert!(matches!(changes[0].action, ChangeAction::Drop));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D6: column altered (type changed) ───────────────────

    #[test]
    fn d6_altered_column_detected() {
        let a = snap(vec![table("public", "users", vec![col("id", "int")])], vec![]);
        let b = snap(vec![table("public", "users", vec![col("id", "bigint")])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "id");
            assert!(matches!(changes[0].action, ChangeAction::Alter { .. }));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D7: constraint added ────────────────────────────────

    #[test]
    fn d7_added_constraint_detected() {
        let t_old = table("public", "users", vec![col("id", "int")]);
        let mut t_new = table("public", "users", vec![col("id", "int")]);
        t_new.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_id".to_string()),
            columns: vec!["id".to_string()],
        });
        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_type, FieldType::Constraint);
            assert!(matches!(changes[0].action, ChangeAction::Add(_)));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D8: index added ─────────────────────────────────────

    #[test]
    fn d8_added_index_detected() {
        let t_old = table("public", "users", vec![col("id", "int")]);
        let mut t_new = table("public", "users", vec![col("id", "int")]);
        t_new.indexes.push(IndexDef {
            name: Some("idx_id".to_string()),
            columns: vec![IndexColumn {
                name: "id".to_string(),
                order: None,
            }],
            unique: false,
            index_type: None,
        });
        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_type, FieldType::Index);
            assert!(matches!(changes[0].action, ChangeAction::Add(_)));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D9: constraint dropped ──────────────────────────────

    #[test]
    fn d9_dropped_constraint_detected() {
        let mut t_old = table("public", "users", vec![col("id", "int")]);
        t_old.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_id".to_string()),
            columns: vec!["id".to_string()],
        });
        let t_new = table("public", "users", vec![col("id", "int")]);
        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "uq_id");
            assert_eq!(changes[0].field_type, FieldType::Constraint);
            assert!(matches!(changes[0].action, ChangeAction::Drop));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D10: index dropped ──────────────────────────────────

    #[test]
    fn d10_dropped_index_detected() {
        let mut t_old = table("public", "users", vec![col("id", "int")]);
        t_old.indexes.push(IndexDef {
            name: Some("idx_id".to_string()),
            columns: vec![IndexColumn {
                name: "id".to_string(),
                order: None,
            }],
            unique: false,
            index_type: None,
        });
        let t_new = table("public", "users", vec![col("id", "int")]);
        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "idx_id");
            assert_eq!(changes[0].field_type, FieldType::Index);
            assert!(matches!(changes[0].action, ChangeAction::Drop));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D11: constraint changed (same name, different definition → Drop + Add) ──

    #[test]
    fn d11_changed_constraint_detected_as_drop_add() {
        let mut t_old = table("public", "users", vec![col("id", "int"), col("email", "text")]);
        t_old.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_email".to_string()),
            columns: vec!["email".to_string()],
        });
        let mut t_new = table("public", "users", vec![col("id", "int"), col("email", "text")]);
        // Same name, but now covers both columns
        t_new.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_email".to_string()),
            columns: vec!["email".to_string(), "id".to_string()],
        });
        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            // Changed constraint = Drop old + Add new
            assert_eq!(changes.len(), 2);
            let drop_count = changes
                .iter()
                .filter(|c| matches!(c.action, ChangeAction::Drop))
                .count();
            let add_count = changes
                .iter()
                .filter(|c| matches!(c.action, ChangeAction::Add(_)))
                .count();
            assert_eq!(drop_count, 1);
            assert_eq!(add_count, 1);
            assert!(changes.iter().all(|c| c.field_name == "uq_email"));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D12: index changed (same name, different type → Drop + Add) ──

    #[test]
    fn d12_changed_index_detected_as_drop_add() {
        let mut t_old = table("public", "users", vec![col("id", "int")]);
        t_old.indexes.push(IndexDef {
            name: Some("idx_id".to_string()),
            columns: vec![IndexColumn {
                name: "id".to_string(),
                order: None,
            }],
            unique: false,
            index_type: None,
        });
        let mut t_new = table("public", "users", vec![col("id", "int")]);
        t_new.indexes.push(IndexDef {
            name: Some("idx_id".to_string()),
            columns: vec![IndexColumn {
                name: "id".to_string(),
                order: None,
            }],
            unique: false,
            index_type: Some(IndexType::Hash),
        });
        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 2);
            let drop_count = changes
                .iter()
                .filter(|c| matches!(c.action, ChangeAction::Drop))
                .count();
            let add_count = changes
                .iter()
                .filter(|c| matches!(c.action, ChangeAction::Add(_)))
                .count();
            assert_eq!(drop_count, 1);
            assert_eq!(add_count, 1);
            assert!(changes.iter().all(|c| c.field_name == "idx_id"));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D13: column nullable changed ────────────────────────

    #[test]
    fn d13_column_nullable_change_detected_as_alter() {
        let a = snap(
            vec![table("public", "users", vec![col_not_null("id", "int")])],
            vec![],
        );
        let b = snap(
            vec![table("public", "users", vec![col("id", "int")])],
            vec![],
        );
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "id");
            assert_eq!(changes[0].field_type, FieldType::Column);
            assert!(matches!(changes[0].action, ChangeAction::Alter { .. }));
            if let ChangeAction::Alter { ref old, ref new } = changes[0].action {
                if let (FieldDetail::Column(old_col), FieldDetail::Column(new_col)) =
                    (old.as_ref(), new.as_ref())
                {
                    assert!(!old_col.nullable);
                    assert!(new_col.nullable);
                } else {
                    panic!("expected Column details");
                }
            }
        } else {
            panic!("expected Change action");
        }
    }

    // ── D14: column default changed ─────────────────────────

    #[test]
    fn d14_column_default_change_detected_as_alter() {
        let a = snap(
            vec![table("public", "users", vec![col("status", "text")])],
            vec![],
        );
        let b = snap(
            vec![table(
                "public",
                "users",
                vec![col_with_default("status", "text", "'active'")],
            )],
            vec![],
        );
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "status");
            assert!(matches!(changes[0].action, ChangeAction::Alter { .. }));
            if let ChangeAction::Alter { ref old, ref new } = changes[0].action {
                if let (FieldDetail::Column(old_col), FieldDetail::Column(new_col)) =
                    (old.as_ref(), new.as_ref())
                {
                    assert!(old_col.default_value.is_none());
                    assert_eq!(new_col.default_value.as_deref(), Some("'active'"));
                } else {
                    panic!("expected Column details");
                }
            }
        } else {
            panic!("expected Change action");
        }
    }

    // ── D15: multiple changes on same table ─────────────────

    #[test]
    fn d15_multiple_changes_on_same_table() {
        let mut t_old = table("public", "users", vec![col("id", "int"), col("email", "text")]);
        t_old.table_constraints.push(TableConstraint::Unique {
            name: Some("uq_email".to_string()),
            columns: vec!["email".to_string()],
        });

        let mut t_new = table(
            "public",
            "users",
            vec![col("id", "int"), col("email", "text"), col("name", "text")],
        );
        // constraint dropped (not added back), index added
        t_new.indexes.push(IndexDef {
            name: Some("idx_name".to_string()),
            columns: vec![IndexColumn {
                name: "name".to_string(),
                order: None,
            }],
            unique: false,
            index_type: None,
        });

        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            // add col "name" + drop constraint "uq_email" + add index "idx_name"
            assert_eq!(changes.len(), 3);
            let col_add = changes
                .iter()
                .find(|c| c.field_type == FieldType::Column && c.field_name == "name")
                .expect("should have column add");
            assert!(matches!(col_add.action, ChangeAction::Add(_)));
            let con_drop = changes
                .iter()
                .find(|c| c.field_type == FieldType::Constraint && c.field_name == "uq_email")
                .expect("should have constraint drop");
            assert!(matches!(con_drop.action, ChangeAction::Drop));
            let idx_add = changes
                .iter()
                .find(|c| c.field_type == FieldType::Index && c.field_name == "idx_name")
                .expect("should have index add");
            assert!(matches!(idx_add.action, ChangeAction::Add(_)));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D16: multiple tables changed simultaneously ─────────

    #[test]
    fn d16_multiple_tables_changed() {
        let a = snap(
            vec![
                table("public", "users", vec![col("id", "int")]),
                table("public", "orders", vec![col("id", "int")]),
            ],
            vec![],
        );
        let b = snap(
            vec![
                table(
                    "public",
                    "users",
                    vec![col("id", "int"), col("email", "text")],
                ),
                table(
                    "public",
                    "orders",
                    vec![col("id", "int"), col("total", "numeric")],
                ),
            ],
            vec![],
        );
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 2);
        let names: Vec<&str> = diffs.iter().map(|d| d.entity_name.as_str()).collect();
        assert!(names.contains(&"public.users"));
        assert!(names.contains(&"public.orders"));
        for d in &diffs {
            assert!(matches!(d.action, DiffAction::Change(_)));
        }
    }

    // ── D17: mixed add/alter/drop across entities ───────────

    #[test]
    fn d17_mixed_add_alter_drop_across_entities() {
        let a = snap(
            vec![
                table("public", "users", vec![col("id", "int")]),
                table("public", "legacy", vec![col("id", "int")]),
            ],
            vec![],
        );
        let b = snap(
            vec![
                // users modified (column added)
                table(
                    "public",
                    "users",
                    vec![col("id", "int"), col("name", "text")],
                ),
                // legacy dropped (absent in new)
                // orders added (new table)
                table("public", "orders", vec![col("id", "int")]),
            ],
            vec![],
        );
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 3);

        let added = diffs
            .iter()
            .find(|d| matches!(d.action, DiffAction::Add))
            .expect("should have an Add");
        assert_eq!(added.entity_name, "public.orders");

        let dropped = diffs
            .iter()
            .find(|d| matches!(d.action, DiffAction::Drop))
            .expect("should have a Drop");
        assert_eq!(dropped.entity_name, "public.legacy");

        let changed = diffs
            .iter()
            .find(|d| matches!(d.action, DiffAction::Change(_)))
            .expect("should have a Change");
        assert_eq!(changed.entity_name, "public.users");
    }

    // ── D18: unnamed constraint matching ────────────────────

    #[test]
    fn d18_unnamed_pk_with_same_columns_no_diff() {
        let mut t_old = table("public", "users", vec![col("id", "int")]);
        t_old.table_constraints.push(TableConstraint::PrimaryKey {
            name: None,
            columns: vec!["id".to_string()],
        });
        let mut t_new = table("public", "users", vec![col("id", "int")]);
        t_new.table_constraints.push(TableConstraint::PrimaryKey {
            name: None,
            columns: vec!["id".to_string()],
        });
        let diffs = diff(&snap(vec![t_old], vec![]), &snap(vec![t_new], vec![]));
        assert!(diffs.is_empty(), "identical unnamed PKs should produce no diff");
    }

    // ── D19: enum value added ───────────────────────────────

    #[test]
    fn d19_enum_value_added() {
        let old_enum = EnumSnapshot {
            name: "status".to_string(),
            schema: "public".to_string(),
            values: vec!["active".to_string(), "inactive".to_string()],
        };
        let new_enum = EnumSnapshot {
            name: "status".to_string(),
            schema: "public".to_string(),
            values: vec![
                "active".to_string(),
                "inactive".to_string(),
                "pending".to_string(),
            ],
        };
        let diffs = diff(
            &snap(vec![], vec![old_enum]),
            &snap(vec![], vec![new_enum]),
        );
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].entity_name, "public.status");
        assert_eq!(diffs[0].entity_type, EntityType::Enum);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "pending");
            assert_eq!(changes[0].field_type, FieldType::EnumValue);
            assert!(matches!(changes[0].action, ChangeAction::Add(_)));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D20: enum value dropped ─────────────────────────────

    #[test]
    fn d20_enum_value_dropped() {
        let old_enum = EnumSnapshot {
            name: "status".to_string(),
            schema: "public".to_string(),
            values: vec!["active".to_string(), "inactive".to_string()],
        };
        let new_enum = EnumSnapshot {
            name: "status".to_string(),
            schema: "public".to_string(),
            values: vec!["active".to_string()],
        };
        let diffs = diff(
            &snap(vec![], vec![old_enum]),
            &snap(vec![], vec![new_enum]),
        );
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].entity_name, "public.status");
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "inactive");
            assert_eq!(changes[0].field_type, FieldType::EnumValue);
            assert!(matches!(changes[0].action, ChangeAction::Drop));
        } else {
            panic!("expected Change action");
        }
    }

    // ── D21: new enum added / enum dropped ──────────────────

    #[test]
    fn d21_enum_added_and_dropped() {
        let old_enum = EnumSnapshot {
            name: "old_type".to_string(),
            schema: "public".to_string(),
            values: vec!["a".to_string()],
        };
        let new_enum = EnumSnapshot {
            name: "new_type".to_string(),
            schema: "public".to_string(),
            values: vec!["x".to_string()],
        };
        let diffs = diff(
            &snap(vec![], vec![old_enum]),
            &snap(vec![], vec![new_enum]),
        );
        assert_eq!(diffs.len(), 2);

        let added = diffs
            .iter()
            .find(|d| matches!(d.action, DiffAction::Add))
            .expect("should have an Add");
        assert_eq!(added.entity_name, "public.new_type");
        assert_eq!(added.entity_type, EntityType::Enum);

        let dropped = diffs
            .iter()
            .find(|d| matches!(d.action, DiffAction::Drop))
            .expect("should have a Drop");
        assert_eq!(dropped.entity_name, "public.old_type");
        assert_eq!(dropped.entity_type, EntityType::Enum);
    }

    // ════════════════════════════════════════════════════════
    // SQL Generation Tests (S1-S14)
    // ════════════════════════════════════════════════════════

    // ── S1: Add action produces no SQL ──────────────────────

    #[test]
    fn s1_add_action_produces_no_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Add,
        }];
        let sql = generate_migration_sql(&diffs);
        assert!(sql.is_empty(), "Add action should produce no SQL");
    }

    // ── S2: Drop table produces DROP TABLE CASCADE ──────────

    #[test]
    fn s2_drop_table_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Drop,
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(sql, "DROP TABLE public.users CASCADE;");
    }

    // ── S3: Drop enum produces warning comment ──────────────

    #[test]
    fn s3_drop_enum_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.status".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Drop,
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(
            sql,
            "-- WARNING: manual migration required for dropped enum public.status"
        );
    }

    // ── S4: Column add SQL ──────────────────────────────────

    #[test]
    fn s4_column_add_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Add(Box::new(FieldDetail::Column(col("email", "text")))),
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(sql, "ALTER TABLE public.users ADD COLUMN email text;");
    }

    // ── S5: Column add with NOT NULL and DEFAULT ────────────

    #[test]
    fn s5_column_add_not_null_with_default_sql() {
        let c = ColumnDef {
            nullable: false,
            default_value: Some("'active'".to_string()),
            ..col("status", "text")
        };
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "status".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Add(Box::new(FieldDetail::Column(c))),
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(
            sql,
            "ALTER TABLE public.users ADD COLUMN status text NOT NULL DEFAULT 'active';"
        );
    }

    // ── S6: Column drop SQL ─────────────────────────────────

    #[test]
    fn s6_column_drop_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Drop,
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(sql, "ALTER TABLE public.users DROP COLUMN email;");
    }

    // ── S7: Column alter type SQL ───────────────────────────

    #[test]
    fn s7_column_alter_type_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "id".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column(col("id", "int"))),
                    new: Box::new(FieldDetail::Column(col("id", "bigint"))),
                },
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(
            sql,
            "ALTER TABLE public.users ALTER COLUMN id TYPE bigint;"
        );
    }

    // ── S8: Column alter nullable SQL ───────────────────────

    #[test]
    fn s8_column_alter_nullable_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "name".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column(col_not_null("name", "text"))),
                    new: Box::new(FieldDetail::Column(col("name", "text"))),
                },
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(
            sql,
            "ALTER TABLE public.users ALTER COLUMN name DROP NOT NULL;"
        );
    }

    // ── S9: Column alter default SQL ────────────────────────

    #[test]
    fn s9_column_alter_default_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "status".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column(col("status", "text"))),
                    new: Box::new(FieldDetail::Column(col_with_default(
                        "status", "text", "'active'",
                    ))),
                },
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(
            sql,
            "ALTER TABLE public.users ALTER COLUMN status SET DEFAULT 'active';"
        );
    }

    // ── S10: Constraint add SQL (PK, Unique, FK, Check) ─────

    #[test]
    fn s10_constraint_add_sql() {
        // PK
        let pk_diff = MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "pk_users".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint(
                    TableConstraint::PrimaryKey {
                        name: Some("pk_users".to_string()),
                        columns: vec!["id".to_string()],
                    },
                ))),
            }]),
        };
        let sql = generate_migration_sql(&[pk_diff]);
        assert_eq!(
            sql,
            "ALTER TABLE public.users ADD CONSTRAINT pk_users PRIMARY KEY (id);"
        );

        // Unique
        let uq_diff = MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "uq_email".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint(
                    TableConstraint::Unique {
                        name: Some("uq_email".to_string()),
                        columns: vec!["email".to_string()],
                    },
                ))),
            }]),
        };
        let sql = generate_migration_sql(&[uq_diff]);
        assert_eq!(
            sql,
            "ALTER TABLE public.users ADD CONSTRAINT uq_email UNIQUE (email);"
        );

        // FK
        let fk_diff = MigrationDiff {
            entity_name: "public.orders".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "fk_user".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint(
                    TableConstraint::ForeignKey(ForeignKey {
                        name: Some("fk_user".to_string()),
                        columns: vec!["user_id".to_string()],
                        ref_schema: Some("public".to_string()),
                        ref_table: "users".to_string(),
                        ref_columns: vec!["id".to_string()],
                        on_delete: None,
                        on_update: None,
                    }),
                ))),
            }]),
        };
        let sql = generate_migration_sql(&[fk_diff]);
        assert_eq!(
            sql,
            "ALTER TABLE public.orders ADD CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES public.users(id);"
        );

        // Check
        let ck_diff = MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "ck_age".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint(
                    TableConstraint::Check {
                        name: Some("ck_age".to_string()),
                        expression: "age > 0".to_string(),
                    },
                ))),
            }]),
        };
        let sql = generate_migration_sql(&[ck_diff]);
        assert_eq!(
            sql,
            "ALTER TABLE public.users ADD CONSTRAINT ck_age CHECK (age > 0);"
        );
    }

    // ── S11: Constraint drop SQL ────────────────────────────

    #[test]
    fn s11_constraint_drop_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "uq_email".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Drop,
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(sql, "ALTER TABLE public.users DROP CONSTRAINT uq_email;");
    }

    // ── S12: Index add SQL ──────────────────────────────────

    #[test]
    fn s12_index_add_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "idx_email".to_string(),
                field_type: FieldType::Index,
                action: ChangeAction::Add(Box::new(FieldDetail::Index(IndexDef {
                    name: Some("idx_email".to_string()),
                    columns: vec![IndexColumn {
                        name: "email".to_string(),
                        order: None,
                    }],
                    unique: false,
                    index_type: None,
                }))),
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(sql, "CREATE INDEX idx_email ON public.users (email);");

        // Unique index
        let diffs_unique = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "idx_email_unique".to_string(),
                field_type: FieldType::Index,
                action: ChangeAction::Add(Box::new(FieldDetail::Index(IndexDef {
                    name: Some("idx_email_unique".to_string()),
                    columns: vec![IndexColumn {
                        name: "email".to_string(),
                        order: None,
                    }],
                    unique: true,
                    index_type: None,
                }))),
            }]),
        }];
        let sql = generate_migration_sql(&diffs_unique);
        assert_eq!(
            sql,
            "CREATE UNIQUE INDEX idx_email_unique ON public.users (email);"
        );
    }

    // ── S13: Index drop SQL ─────────────────────────────────

    #[test]
    fn s13_index_drop_sql() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "idx_email".to_string(),
                field_type: FieldType::Index,
                action: ChangeAction::Drop,
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(sql, "DROP INDEX idx_email;");
    }

    // ── S14: Enum value add / drop SQL ──────────────────────

    #[test]
    fn s14_enum_value_add_and_drop_sql() {
        // Add value
        let diffs_add = vec![MigrationDiff {
            entity_name: "public.status".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "pending".to_string(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Add(Box::new(FieldDetail::EnumValue(
                    "pending".to_string(),
                ))),
            }]),
        }];
        let sql = generate_migration_sql(&diffs_add);
        assert_eq!(sql, "ALTER TYPE public.status ADD VALUE 'pending';");

        // Drop value — should produce no SQL
        let diffs_drop = vec![MigrationDiff {
            entity_name: "public.status".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "inactive".to_string(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Drop,
            }]),
        }];
        let sql = generate_migration_sql(&diffs_drop);
        assert!(sql.is_empty(), "enum value drop should produce no SQL");
    }

    // ════════════════════════════════════════════════════════
    // Scenario Tests: Column property change edge cases
    // ════════════════════════════════════════════════════════

    // M1.1: inline FK change detected
    #[test]
    fn d_column_inline_fk_changed() {
        let old_col = ColumnDef {
            inline_fk: Some(ForeignKey {
                name: Some("fk_a".to_string()),
                columns: vec!["user_id".to_string()],
                ref_schema: None,
                ref_table: "table_a".to_string(),
                ref_columns: vec!["id".to_string()],
                on_delete: None,
                on_update: None,
            }),
            ..col("user_id", "int")
        };
        let new_col = ColumnDef {
            inline_fk: Some(ForeignKey {
                name: Some("fk_a".to_string()),
                columns: vec!["user_id".to_string()],
                ref_schema: None,
                ref_table: "table_b".to_string(),
                ref_columns: vec!["id".to_string()],
                on_delete: None,
                on_update: None,
            }),
            ..col("user_id", "int")
        };
        let a = snap(vec![table("public", "orders", vec![old_col])], vec![]);
        let b = snap(vec![table("public", "orders", vec![new_col])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "user_id");
            assert!(matches!(changes[0].action, ChangeAction::Alter { .. }));
        } else {
            panic!("expected Change action");
        }
    }

    // M1.2: is_pk changed
    #[test]
    fn d_column_is_pk_changed() {
        let old_col = ColumnDef {
            is_pk: false,
            ..col("id", "int")
        };
        let new_col = ColumnDef {
            is_pk: true,
            ..col("id", "int")
        };
        let a = snap(vec![table("public", "users", vec![old_col])], vec![]);
        let b = snap(vec![table("public", "users", vec![new_col])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "id");
            assert!(matches!(changes[0].action, ChangeAction::Alter { .. }));
        } else {
            panic!("expected Change action");
        }
    }

    // M1.3: identity changed
    #[test]
    fn d_column_is_identity_changed() {
        let old_col = ColumnDef {
            identity: None,
            ..col("id", "int")
        };
        let new_col = ColumnDef {
            identity: Some(IdentityKind::Always),
            ..col("id", "int")
        };
        let a = snap(vec![table("public", "users", vec![old_col])], vec![]);
        let b = snap(vec![table("public", "users", vec![new_col])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "id");
            assert!(matches!(changes[0].action, ChangeAction::Alter { .. }));
        } else {
            panic!("expected Change action");
        }
    }

    // M1.4: comment changed
    #[test]
    fn d_column_comment_changed() {
        let old_col = ColumnDef {
            comment: None,
            ..col("email", "text")
        };
        let new_col = ColumnDef {
            comment: Some("user email".to_string()),
            ..col("email", "text")
        };
        let a = snap(vec![table("public", "users", vec![old_col])], vec![]);
        let b = snap(vec![table("public", "users", vec![new_col])], vec![]);
        let diffs = diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        if let DiffAction::Change(ref changes) = diffs[0].action {
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].field_name, "email");
            assert!(matches!(changes[0].action, ChangeAction::Alter { .. }));
        } else {
            panic!("expected Change action");
        }
    }

    // M1.7: empty table (zero columns)
    #[test]
    fn d_empty_table_no_diff() {
        let a = snap(vec![table("public", "empty", vec![])], vec![]);
        let b = snap(vec![table("public", "empty", vec![])], vec![]);
        let diffs = diff(&a, &b);
        assert!(diffs.is_empty(), "identical empty tables should produce no diff");
    }

    // M1.8: enum with zero values
    #[test]
    fn d_enum_zero_values_no_diff() {
        let e1 = EnumSnapshot {
            name: "empty_enum".to_string(),
            schema: "public".to_string(),
            values: vec![],
        };
        let e2 = EnumSnapshot {
            name: "empty_enum".to_string(),
            schema: "public".to_string(),
            values: vec![],
        };
        let a = snap(vec![], vec![e1]);
        let b = snap(vec![], vec![e2]);
        let diffs = diff(&a, &b);
        assert!(diffs.is_empty(), "identical empty enums should produce no diff");
    }

    // ════════════════════════════════════════════════════════
    // Scenario Tests: SQL generation edge cases
    // ════════════════════════════════════════════════════════

    // M2.1: Column alter with type + nullable + default all changed
    #[test]
    fn s_column_alter_multiple_changes_at_once() {
        let old_col = ColumnDef {
            nullable: false,
            default_value: Some("'x'".to_string()),
            ..col("status", "VARCHAR(50)")
        };
        let new_col = ColumnDef {
            nullable: true,
            default_value: None,
            ..col("status", "TEXT")
        };
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "status".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column(old_col)),
                    new: Box::new(FieldDetail::Column(new_col)),
                },
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert!(sql.contains("ALTER TABLE public.users ALTER COLUMN status TYPE TEXT;"));
        assert!(sql.contains("ALTER TABLE public.users ALTER COLUMN status DROP NOT NULL;"));
        assert!(sql.contains("ALTER TABLE public.users ALTER COLUMN status DROP DEFAULT;"));
        // Should have exactly 3 ALTER statements
        let line_count = sql.lines().count();
        assert_eq!(line_count, 3, "expected 3 ALTER statements, got {}", line_count);
    }

    // M2.2: FK with on_delete and on_update
    #[test]
    fn s_fk_constraint_with_on_delete_on_update() {
        let fk_diff = MigrationDiff {
            entity_name: "public.orders".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "fk_user".to_string(),
                field_type: FieldType::Constraint,
                action: ChangeAction::Add(Box::new(FieldDetail::Constraint(
                    TableConstraint::ForeignKey(ForeignKey {
                        name: Some("fk_user".to_string()),
                        columns: vec!["user_id".to_string()],
                        ref_schema: Some("public".to_string()),
                        ref_table: "users".to_string(),
                        ref_columns: vec!["id".to_string()],
                        on_delete: Some(FkAction::Cascade),
                        on_update: Some(FkAction::Restrict),
                    }),
                ))),
            }]),
        };
        let sql = generate_migration_sql(&[fk_diff]);
        assert!(
            sql.contains("ON DELETE CASCADE"),
            "SQL should include ON DELETE CASCADE, got: {sql}"
        );
        assert!(
            sql.contains("ON UPDATE RESTRICT"),
            "SQL should include ON UPDATE RESTRICT, got: {sql}"
        );
        assert_eq!(
            sql,
            "ALTER TABLE public.orders ADD CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE ON UPDATE RESTRICT;"
        );
    }

    // M2.6: Index with ASC/DESC
    #[test]
    fn s_index_with_column_ordering() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "idx_email_name".to_string(),
                field_type: FieldType::Index,
                action: ChangeAction::Add(Box::new(FieldDetail::Index(IndexDef {
                    name: Some("idx_email_name".to_string()),
                    columns: vec![
                        IndexColumn {
                            name: "email".to_string(),
                            order: Some(SortOrder::Desc),
                        },
                        IndexColumn {
                            name: "name".to_string(),
                            order: Some(SortOrder::Asc),
                        },
                    ],
                    unique: false,
                    index_type: None,
                }))),
            }]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert_eq!(
            sql,
            "CREATE INDEX idx_email_name ON public.users (email DESC, name ASC);"
        );
    }

    // M2.8: Multiple enum values added
    #[test]
    fn s_enum_multiple_values_added() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.status".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "pending".to_string(),
                    field_type: FieldType::EnumValue,
                    action: ChangeAction::Add(Box::new(FieldDetail::EnumValue("pending".to_string()))),
                },
                FieldChange {
                    field_name: "archived".to_string(),
                    field_type: FieldType::EnumValue,
                    action: ChangeAction::Add(Box::new(FieldDetail::EnumValue("archived".to_string()))),
                },
                FieldChange {
                    field_name: "deleted".to_string(),
                    field_type: FieldType::EnumValue,
                    action: ChangeAction::Add(Box::new(FieldDetail::EnumValue("deleted".to_string()))),
                },
            ]),
        }];
        let sql = generate_migration_sql(&diffs);
        assert!(sql.contains("ALTER TYPE public.status ADD VALUE 'pending';"));
        assert!(sql.contains("ALTER TYPE public.status ADD VALUE 'archived';"));
        assert!(sql.contains("ALTER TYPE public.status ADD VALUE 'deleted';"));
        let line_count = sql.lines().count();
        assert_eq!(line_count, 3, "expected 3 ALTER TYPE statements, got {}", line_count);
    }

    // ════════════════════════════════════════════════════════
    // Migration Warnings Tests
    // ════════════════════════════════════════════════════════

    #[test]
    fn warn_column_type_change() {
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column(col("email", "VARCHAR(100)"))),
                    new: Box::new(FieldDetail::Column(col("email", "TEXT"))),
                },
            }]),
        }];
        let warnings = migration_warnings(&diffs);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("type change"));
        assert!(warnings[0].contains("VARCHAR(100)"));
        assert!(warnings[0].contains("TEXT"));
        assert!(warnings[0].contains("splitting"));
    }

    #[test]
    fn warn_possible_rename_drop_plus_add() {
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "display_name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Add(Box::new(FieldDetail::Column(col("display_name", "TEXT")))),
                },
            ]),
        }];
        let warnings = migration_warnings(&diffs);
        assert!(warnings.iter().any(|w| w.contains("'name' dropped") && w.contains("'display_name' added")));
    }

    #[test]
    fn warn_enum_value_dropped() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.status".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "deleted".to_string(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Drop,
            }]),
        }];
        let warnings = migration_warnings(&diffs);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("enum value 'deleted' dropped"));
    }

    #[test]
    fn warn_enum_type_dropped() {
        let diffs = vec![MigrationDiff {
            entity_name: "public.status".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Drop,
        }];
        let warnings = migration_warnings(&diffs);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Enum 'public.status' dropped"));
    }

    #[test]
    fn no_warnings_for_simple_column_add() {
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Add(Box::new(FieldDetail::Column(col("email", "TEXT")))),
            }]),
        }];
        let warnings = migration_warnings(&diffs);
        assert!(warnings.is_empty(), "simple column add should not produce warnings");
    }

    // ════════════════════════════════════════════════════════
    // Task 1: is_castable() tests
    // ════════════════════════════════════════════════════════

    #[test]
    fn ca1_integer_to_text_castable() {
        assert!(is_castable("INTEGER", "TEXT"));
        assert!(is_castable("BIGINT", "TEXT"));
        assert!(is_castable("SMALLINT", "TEXT"));
    }

    #[test]
    fn ca2_varchar_to_text_castable() {
        assert!(is_castable("VARCHAR(100)", "TEXT"));
    }

    #[test]
    fn ca3_text_to_varchar_castable() {
        assert!(is_castable("TEXT", "VARCHAR(50)"));
    }

    #[test]
    fn ca4_jsonb_to_integer_not_castable() {
        assert!(!is_castable("JSONB", "INTEGER"));
        assert!(!is_castable("JSON", "INTEGER"));
    }

    #[test]
    fn ca5_array_to_scalar_not_castable() {
        assert!(!is_castable("TEXT[]", "TEXT"));
    }

    #[test]
    fn ca_numeric_to_text_castable() {
        assert!(is_castable("NUMERIC", "TEXT"));
        assert!(is_castable("DECIMAL", "TEXT"));
    }

    #[test]
    fn ca_boolean_castable() {
        assert!(is_castable("BOOLEAN", "TEXT"));
        assert!(is_castable("BOOLEAN", "INTEGER"));
    }

    #[test]
    fn ca_timestamp_to_text_castable() {
        assert!(is_castable("TIMESTAMP", "TEXT"));
        assert!(is_castable("TIMESTAMPTZ", "TEXT"));
    }

    #[test]
    fn ca_same_category_castable() {
        assert!(is_castable("INTEGER", "BIGINT"));
        assert!(is_castable("TEXT", "TEXT"));
    }

    // ════════════════════════════════════════════════════════
    // Task 2: classify_changes() tests
    // ════════════════════════════════════════════════════════

    #[test]
    fn c1_simple_changes_only() {
        // Use different types for drop (INTEGER) and add (TEXT) so it's not a rename
        let old_snap = snap(
            vec![table("config", "users", vec![col("id", "int"), col("age", "INTEGER")])],
            vec![],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "email".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Add(Box::new(FieldDetail::Column(col("email", "TEXT")))),
                },
                FieldChange {
                    field_name: "age".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "idx_email".to_string(),
                    field_type: FieldType::Index,
                    action: ChangeAction::Add(Box::new(FieldDetail::Index(IndexDef {
                        name: Some("idx_email".to_string()),
                        columns: vec![IndexColumn { name: "email".to_string(), order: None }],
                        unique: false,
                        index_type: None,
                    }))),
                },
            ]),
        }];

        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert!(complex.is_empty(), "no complex changes expected");
        assert_eq!(simple.len(), 1);
        if let DiffAction::Change(ref changes) = simple[0].action {
            assert_eq!(changes.len(), 3);
        } else {
            panic!("expected Change action");
        }
    }

    #[test]
    fn c2_column_type_change_detected() {
        let old_snap = snap(
            vec![table("config", "users", vec![col("id", "int"), col("email", "VARCHAR(100)")])],
            vec![],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "email".to_string(),
                field_type: FieldType::Column,
                action: ChangeAction::Alter {
                    old: Box::new(FieldDetail::Column(col("email", "VARCHAR(100)"))),
                    new: Box::new(FieldDetail::Column(col("email", "TEXT"))),
                },
            }]),
        }];

        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert_eq!(complex.len(), 1);
        if let ComplexChange::ColumnTypeChange { ref old_type, ref new_type, .. } = complex[0] {
            assert_eq!(old_type, "VARCHAR(100)");
            assert_eq!(new_type, "TEXT");
        } else {
            panic!("expected ColumnTypeChange");
        }
        // No remaining simple changes for this table
        assert!(simple.is_empty() || simple.iter().all(|d| {
            if let DiffAction::Change(ref c) = d.action { !c.is_empty() } else { true }
        }));
    }

    #[test]
    fn c3_column_rename_detected() {
        let old_snap = snap(
            vec![table("config", "users", vec![col("id", "int"), col("name", "TEXT")])],
            vec![],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "display_name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Add(Box::new(FieldDetail::Column(col("display_name", "TEXT")))),
                },
            ]),
        }];

        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert_eq!(complex.len(), 1);
        if let ComplexChange::ColumnRename { ref old_name, ref new_name, .. } = complex[0] {
            assert_eq!(old_name, "name");
            assert_eq!(new_name, "display_name");
        } else {
            panic!("expected ColumnRename");
        }
        assert!(simple.is_empty());
    }

    #[test]
    fn c4_different_types_not_rename() {
        let old_snap = snap(
            vec![table("config", "users", vec![col("id", "int"), col("name", "TEXT")])],
            vec![],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "age".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Add(Box::new(FieldDetail::Column(col("age", "INTEGER")))),
                },
            ]),
        }];

        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert!(complex.is_empty(), "different types should not be detected as rename");
        assert_eq!(simple.len(), 1);
        if let DiffAction::Change(ref changes) = simple[0].action {
            assert_eq!(changes.len(), 2);
        } else {
            panic!("expected Change action");
        }
    }

    #[test]
    fn c5_multiple_drops_adds_not_rename() {
        let old_snap = snap(
            vec![table("config", "users", vec![
                col("id", "int"),
                col("first_name", "TEXT"),
                col("last_name", "TEXT"),
            ])],
            vec![],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "config.users".to_string(),
            entity_type: EntityType::Table,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "first_name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "last_name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "given_name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Add(Box::new(FieldDetail::Column(col("given_name", "TEXT")))),
                },
                FieldChange {
                    field_name: "family_name".to_string(),
                    field_type: FieldType::Column,
                    action: ChangeAction::Add(Box::new(FieldDetail::Column(col("family_name", "TEXT")))),
                },
            ]),
        }];

        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert!(complex.is_empty(), "multiple drops+adds should not be detected as rename");
        assert_eq!(simple.len(), 1);
        if let DiffAction::Change(ref changes) = simple[0].action {
            assert_eq!(changes.len(), 4);
        } else {
            panic!("expected Change action");
        }
    }

    #[test]
    fn c6_enum_value_removal_detected() {
        let old_snap = snap(
            vec![],
            vec![EnumSnapshot {
                name: "status_type".to_string(),
                schema: "public".to_string(),
                values: vec!["active".to_string(), "inactive".to_string(), "deleted".to_string()],
            }],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "public.status_type".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "deleted".to_string(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Drop,
            }]),
        }];

        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert_eq!(complex.len(), 1);
        if let ComplexChange::EnumValueRemoval {
            ref removed_values,
            ref remaining_values,
            ..
        } = complex[0]
        {
            assert_eq!(removed_values, &vec!["deleted".to_string()]);
            assert!(remaining_values.contains(&"active".to_string()));
            assert!(remaining_values.contains(&"inactive".to_string()));
            assert!(!remaining_values.contains(&"deleted".to_string()));
        } else {
            panic!("expected EnumValueRemoval");
        }
        assert!(simple.is_empty());
    }

    #[test]
    fn c7_enum_removal_identifies_affected_columns() {
        let old_snap = snap(
            vec![table("public", "users", vec![
                col("id", "int"),
                col("status", "status_type"),
            ])],
            vec![EnumSnapshot {
                name: "status_type".to_string(),
                schema: "public".to_string(),
                values: vec!["active".to_string(), "inactive".to_string(), "deleted".to_string()],
            }],
        );
        let diffs = vec![MigrationDiff {
            entity_name: "public.status_type".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![FieldChange {
                field_name: "deleted".to_string(),
                field_type: FieldType::EnumValue,
                action: ChangeAction::Drop,
            }]),
        }];

        let (_, complex) = classify_changes(&diffs, &old_snap);
        assert_eq!(complex.len(), 1);
        if let ComplexChange::EnumValueRemoval {
            ref affected_columns,
            ..
        } = complex[0]
        {
            assert!(!affected_columns.is_empty());
            assert!(affected_columns.contains(&("public.users".to_string(), "status".to_string())));
        } else {
            panic!("expected EnumValueRemoval");
        }
    }

    #[test]
    fn c_enum_value_rename_is_simple() {
        let old_snap = snap(
            vec![],
            vec![EnumSnapshot {
                name: "status_type".to_string(),
                schema: "public".to_string(),
                values: vec!["active".to_string(), "inactive".to_string(), "deleted".to_string()],
            }],
        );
        // 1:1 swap: drop "deleted", add "archived"
        let diffs = vec![MigrationDiff {
            entity_name: "public.status_type".to_string(),
            entity_type: EntityType::Enum,
            action: DiffAction::Change(vec![
                FieldChange {
                    field_name: "deleted".to_string(),
                    field_type: FieldType::EnumValue,
                    action: ChangeAction::Drop,
                },
                FieldChange {
                    field_name: "archived".to_string(),
                    field_type: FieldType::EnumValue,
                    action: ChangeAction::Add(Box::new(FieldDetail::EnumValue("archived".to_string()))),
                },
            ]),
        }];

        let (simple, complex) = classify_changes(&diffs, &old_snap);
        assert!(complex.is_empty(), "1:1 enum value swap should be treated as simple");
        assert_eq!(simple.len(), 1);
    }

    // ════════════════════════════════════════════════════════
    // Task 3: generate_data_sql() tests
    // ════════════════════════════════════════════════════════

    #[test]
    fn d_data_column_rename_generates_copy() {
        let change = ComplexChange::ColumnRename {
            table_name: "config.users".to_string(),
            old_name: "name".to_string(),
            new_name: "display_name".to_string(),
            col_def: Box::new(col("display_name", "TEXT")),
        };
        let sql = generate_data_sql(&change);
        assert_eq!(sql, "UPDATE config.users SET display_name = name;\n");
    }

    #[test]
    fn d_data_castable_type_change() {
        let change = ComplexChange::ColumnTypeChange {
            table_name: "config.orders".to_string(),
            column_name: "total".to_string(),
            old_type: "INTEGER".to_string(),
            new_type: "TEXT".to_string(),
            old_col: Box::new(col("total", "INTEGER")),
            new_col: Box::new(col("total_text", "TEXT")),
        };
        let sql = generate_data_sql(&change);
        assert!(sql.contains("UPDATE config.orders SET total_text = total::TEXT;"));
    }

    #[test]
    fn d_data_non_castable_type_change_generates_todo() {
        let change = ComplexChange::ColumnTypeChange {
            table_name: "config.data".to_string(),
            column_name: "payload".to_string(),
            old_type: "JSONB".to_string(),
            new_type: "INTEGER".to_string(),
            old_col: Box::new(col("payload", "JSONB")),
            new_col: Box::new(col("payload", "INTEGER")),
        };
        let sql = generate_data_sql(&change);
        assert!(sql.contains("-- TODO:"), "should contain TODO comment: {sql}");
    }

    #[test]
    fn d_data_enum_value_removal_generates_todo() {
        let change = ComplexChange::EnumValueRemoval {
            enum_name: "public.status_type".to_string(),
            removed_values: vec!["deleted".to_string()],
            remaining_values: vec!["active".to_string(), "inactive".to_string()],
            affected_columns: vec![("public.users".to_string(), "status".to_string())],
        };
        let sql = generate_data_sql(&change);
        assert!(sql.contains("Removed: deleted"), "sql: {sql}");
        assert!(sql.contains("Remaining: active, inactive"), "sql: {sql}");
        assert!(sql.contains("UPDATE public.users SET status = '???' WHERE status = 'deleted';"));
    }

    #[test]
    fn d_data_text_to_varchar_has_truncation_warning() {
        let change = ComplexChange::ColumnTypeChange {
            table_name: "config.users".to_string(),
            column_name: "name".to_string(),
            old_type: "TEXT".to_string(),
            new_type: "VARCHAR(50)".to_string(),
            old_col: Box::new(col("name", "TEXT")),
            new_col: Box::new(col("name", "VARCHAR(50)")),
        };
        let sql = generate_data_sql(&change);
        assert!(sql.contains("WARNING"), "should contain WARNING: {sql}");
        assert!(sql.contains("truncate"), "should mention truncate: {sql}");
    }
