//! `source.parser` is validated when the design loads.

use dbd_core::Design;
use std::path::PathBuf;

fn project_with_source_block(name: &str, source_block: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/.tmp")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("ddl/table/app")).unwrap();
    std::fs::write(
        dir.join("design.yaml"),
        format!(
            "project:\n  name: parser_choice\nsource:\n{source_block}\
             target:\n  postgres:\n    url: postgres://localhost/unused\nschemas:\n  - app\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("ddl/table/app/t.ddl"),
        "set search_path to app;\ncreate table t (id int primary key);\n",
    )
    .unwrap();
    dir.join("design.yaml")
}

#[test]
fn an_unknown_source_parser_fails_to_load() {
    let config = project_with_source_block("parser_choice_bad", "  dialect: postgresql\n  parser: pgquery\n");
    // `Design` does not derive `Debug`, so `expect_err` (which needs `T: Debug`
    // to format the Ok case) does not compile here; match instead.
    let err = match Design::from_config(&config, "dev") {
        Ok(_) => panic!("an unrecognised source.parser must not load"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("pg_query"), "must name the valid values, got: {err}");
    assert!(err.contains("sqlparser"), "must name the valid values, got: {err}");
}

#[test]
fn an_explicit_valid_parser_loads() {
    let config = project_with_source_block("parser_choice_ok", "  dialect: postgresql\n  parser: sqlparser\n");
    let design = Design::from_config(&config, "dev").expect("an explicit valid parser must load");
    // Loading `Ok` is not enough: the scan loop drops a failed parse silently
    // (`if let Ok(entity) = …`), so a Design loads empty when the parser is
    // broken. Assert the chosen parser actually produced the entity.
    assert!(
        design.entities().iter().any(|e| e.name == "app.t"),
        "chosen parser must produce the table, got: {:?}",
        design.entities().iter().map(|e| &e.name).collect::<Vec<_>>()
    );
}

#[test]
fn omitting_source_parser_loads() {
    let config = project_with_source_block("parser_choice_absent", "  dialect: postgresql\n");
    let design = Design::from_config(&config, "dev").expect("source.parser is optional");
    // Same reasoning as above: assert the entity was actually produced, not
    // just that loading returned `Ok`.
    assert!(
        design.entities().iter().any(|e| e.name == "app.t"),
        "default parser must produce the table, got: {:?}",
        design.entities().iter().map(|e| &e.name).collect::<Vec<_>>()
    );
}
