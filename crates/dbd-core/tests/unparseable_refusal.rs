//! A design dbd could not fully read must never be acted on silently.
//!
//! An entity carrying a parse error is filtered out of the desired set by
//! `Design::entities_in_scope`. Without a guard that filter is invisible:
//! `dbd apply` reported "N entities applied" and exited 0 while never creating
//! the object, and `dbd reconcile` reported no drift for it. These tests pin the
//! refusal so the false green cannot come back.
//!
//! They drive the real `Design` API against a mock adapter, so they assert what
//! the commands actually do — including that nothing is written.

use dbd_core::Design;
use dbd_core::adapter::mock::MockAdapter;
use dbd_core::design::Progress;
use std::path::PathBuf;

/// Write a throwaway dbd project under `tests/.tmp/<name>` and load it.
fn design_for(name: &str, files: &[(&str, &str)]) -> Design {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/.tmp")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(
        dir.join("design.yaml"),
        "project:\n  name: unparseable\nsource:\n  dialect: postgresql\n\
         target:\n  postgres:\n    url: postgres://localhost/unused\nschemas:\n  - app\n",
    )
    .unwrap();

    for (rel, sql) in files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, sql).unwrap();
    }

    Design::from_config(&dir.join("design.yaml"), "dev").unwrap()
}

const GOOD_TABLE: (&str, &str) = (
    "ddl/table/app/t.ddl",
    "set search_path to app;\ncreate table t (id int primary key);\n",
);

/// Neither sqlparser nor libpg_query accepts this, so it is a genuine error —
/// not a parser-coverage gap the libpg_query fallback would recover.
const BROKEN_TABLE: (&str, &str) = (
    "ddl/table/app/broken.ddl",
    "set search_path to app;\ncreate table broken (id int,,, ;\n",
);

#[tokio::test]
async fn apply_refuses_and_writes_nothing_when_a_file_cannot_be_parsed() {
    let design = design_for("refuse_apply", &[GOOD_TABLE, BROKEN_TABLE]);
    let mock = MockAdapter::new();

    let err = design
        .apply(&mock, None, false, None, Progress::none())
        .await
        .expect_err("apply must refuse a design it could not fully read");

    let msg = err.to_string();
    assert!(msg.contains("broken.ddl"), "the message must name the file, got: {msg}");
    assert!(
        mock.applied_names().is_empty(),
        "apply must refuse BEFORE writing anything, applied: {:?}",
        mock.applied_names()
    );
}

#[tokio::test]
async fn reconcile_refuses_when_a_file_cannot_be_parsed() {
    let design = design_for("refuse_reconcile", &[GOOD_TABLE, BROKEN_TABLE]);
    let mock = MockAdapter::new();

    let err = design
        .reconcile(&mock, false, false, false, None, Progress::none())
        .await
        .expect_err("reconcile must refuse a design it could not fully read");

    assert!(err.to_string().contains("broken.ddl"), "got: {err}");
    assert!(mock.applied_names().is_empty(), "reconcile must not write");
}

/// A dry run is still a refusal: reporting a plan built from a design with a
/// hole in it is exactly the false picture this guard exists to stop.
#[tokio::test]
async fn reconcile_dry_run_also_refuses() {
    let design = design_for("refuse_reconcile_dry", &[GOOD_TABLE, BROKEN_TABLE]);
    let mock = MockAdapter::new();

    assert!(
        design
            .reconcile(&mock, true, false, false, None, Progress::none())
            .await
            .is_err(),
        "a dry run must not present a plan built from an incomplete design"
    );
}

#[tokio::test]
async fn diff_refuses_when_a_file_cannot_be_parsed() {
    let design = design_for("refuse_diff", &[GOOD_TABLE, BROKEN_TABLE]);
    let mock = MockAdapter::new();

    let err = design
        .diff_live(&mock, None)
        .await
        .expect_err("diff must not report 'in sync' for a design with an unread file");

    assert!(err.to_string().contains("broken.ddl"), "got: {err}");
}

/// The guard must not fire on valid SQL that only sqlparser rejects — that is
/// the libpg_query fallback's job, and re-blocking those files here would make
/// the fallback pointless.
#[tokio::test]
async fn valid_postgres_sqlparser_rejects_does_not_trip_the_guard() {
    let design = design_for(
        "refuse_not_valid_pg",
        &[
            GOOD_TABLE,
            (
                "ddl/view/app/v.ddl",
                "set search_path to app;\n\
                 create view v as select * from t where id > 1 with cascaded check option;\n",
            ),
        ],
    );
    let mock = MockAdapter::new();

    design
        .apply(&mock, None, false, None, Progress::none())
        .await
        .expect("valid Postgres must apply even when sqlparser cannot read it");
    assert!(!mock.applied_names().is_empty());
}

/// A clean design is unaffected.
#[tokio::test]
async fn a_fully_parseable_design_still_applies() {
    let design = design_for("refuse_clean", &[GOOD_TABLE]);
    let mock = MockAdapter::new();

    design
        .apply(&mock, None, false, None, Progress::none())
        .await
        .expect("a clean design must still apply");
    assert!(!mock.applied_names().is_empty());
}
