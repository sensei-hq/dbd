//! Corpus gates — the readers measured against real SQL rather than fixtures.
//!
//! A fixture proves a reader handles the cases its author thought of. That is
//! worth having and it is not the same as evidence: every number that justifies
//! dbd's T-SQL design came from a corpus, and a design justified by measurement
//! has to keep being measured or the justification goes stale.
//!
//! These are `#[ignore]`d because the corpus is not in this repository and is
//! not public. Point them at one:
//!
//! ```text
//! DBD_SQL_CORPUS=/path/to/repos cargo test -p dbd-core \
//!   --test tsql_corpus -- --ignored --nocapture
//! ```
//!
//! Unset, they print why they found nothing and pass — a gate that fails when a
//! contributor has no corpus is a gate people learn to skip.
//!
//! **Nothing here prints file contents or paths.** The corpora these read are
//! private client code; the output is counts and ratios, which is what a gate
//! needs and all it needs.

use dbd_core::parser::Dialect;
use std::path::{Path, PathBuf};

fn corpus() -> Option<PathBuf> {
    let root = std::env::var("DBD_SQL_CORPUS").ok()?;
    let path = PathBuf::from(root);
    path.is_dir().then_some(path)
}

fn sql_files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // `.git` holds packed objects, not source.
                if path.file_name().is_some_and(|n| n == ".git") {
                    continue;
                }
                walk(&path, out);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("sql") || e.eq_ignore_ascii_case("ddl"))
            {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

/// What the detector says about a real corpus.
///
/// Reported rather than asserted at first: the point of a measurement is to
/// learn the number, and pinning one before seeing it is how a test gets
/// written to match whatever the code happened to do.
///
/// The one thing it DOES assert is the property the design rests on — that
/// detection is decisive far more often than not. A detector that answered
/// `Unstated` for most of a corpus would be useless without saying so.
#[test]
#[ignore]
fn the_detector_classifies_a_real_corpus() {
    let Some(root) = corpus() else {
        println!("DBD_SQL_CORPUS unset or not a directory — nothing to measure.");
        return;
    };

    let files = sql_files(&root);
    assert!(!files.is_empty(), "the corpus directory holds no .sql or .ddl files");

    let mut tsql = 0usize;
    let mut postgres = 0usize;
    let mut mysql = 0usize;
    let mut sqlite = 0usize;
    let mut unstated = 0usize;
    let mut unreadable = 0usize;

    for file in &files {
        // `source_text`, not `read_to_string` — 16.2% of this corpus is not
        // UTF-8, and measuring the detector through a reader that cannot see
        // those files would measure the reader instead.
        let Ok(sql) = dbd_core::source_text::read_to_string(file) else {
            unreadable += 1;
            continue;
        };
        match Dialect::detect(&sql) {
            Dialect::TSql => tsql += 1,
            Dialect::PostgreSql => postgres += 1,
            Dialect::MySql => mysql += 1,
            Dialect::Sqlite => sqlite += 1,
            Dialect::Unstated => unstated += 1,
        }
    }

    let total = files.len();
    let classified = total - unstated - unreadable;
    println!("\n── Dialect::detect over {total} SQL files ──");
    println!("  t-sql       {tsql:>6}  ({:.1}%)", pct(tsql, total));
    println!("  postgresql  {postgres:>6}  ({:.1}%)", pct(postgres, total));
    println!("  mysql       {mysql:>6}  ({:.1}%)", pct(mysql, total));
    println!("  sqlite      {sqlite:>6}  ({:.1}%)", pct(sqlite, total));
    println!("  unstated    {unstated:>6}  ({:.1}%)", pct(unstated, total));
    println!("  unreadable  {unreadable:>6}");
    println!("  classified  {classified:>6}  ({:.1}%)\n", pct(classified, total));

    assert!(
        classified * 2 > total,
        "detection is decisive for {classified}/{total} files — under half. \
         A detector that mostly answers `Unstated` is not one, and the markers \
         need revisiting rather than the threshold lowering."
    );
}

fn pct(n: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        n as f64 * 100.0 / total as f64
    }
}

/// **UTF-16 is not an edge case in a SQL Server codebase.**
///
/// SSMS saves as UTF-16 by default, and `std::fs::read_to_string` rejects it
/// outright. Measured over the Ethico corpus: of 2,421 files, 377 are UTF-16
/// with a BOM (15.6%) and 14 more are some other non-UTF-8 (0.6%) — **16.2%
/// invisible** to a reader that decodes UTF-8 only.
///
/// This is not only the T-SQL reader's problem. dbd's own project scan
/// (`design::from_config_with_dir`) reads DDL with `read_to_string` and
/// PROPAGATES the error, so a single UTF-16 file in `ddl/` fails the whole
/// load — not that file, the load.
///
/// Recorded as a measurement rather than asserted at a threshold: the number
/// is a fact about this corpus, and the point is that it is far too large to
/// treat as a rounding error.
#[test]
#[ignore]
fn the_share_of_the_corpus_that_is_not_utf8_is_recorded() {
    let Some(root) = corpus() else {
        println!("DBD_SQL_CORPUS unset or not a directory — nothing to measure.");
        return;
    };

    let files = sql_files(&root);
    let mut utf8 = 0usize;
    let mut utf16 = 0usize;
    let mut other = 0usize;

    for file in &files {
        let Ok(bytes) = std::fs::read(file) else { continue };
        if std::str::from_utf8(&bytes).is_ok() {
            utf8 += 1;
        } else if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
            utf16 += 1;
        } else {
            other += 1;
        }
    }

    let total = files.len();
    println!("\n── encoding over {total} SQL files ──");
    println!("  utf-8            {utf8:>6}  ({:.1}%)", pct(utf8, total));
    println!("  utf-16 (BOM)     {utf16:>6}  ({:.1}%)", pct(utf16, total));
    println!("  other non-utf8   {other:>6}  ({:.1}%)", pct(other, total));
    println!(
        "  a reader that decodes UTF-8 only cannot see {:.1}% of this corpus\n",
        pct(utf16 + other, total)
    );

    assert!(
        utf16 + other > 0,
        "this corpus is all UTF-8 — if that is really true, the decode step is \
         unnecessary for it and this gate should be pointed at one that is not"
    );
}

/// The claim that sent dbd down the lexer route rather than a grammar: that
/// `sqlparser`'s `MsSqlDialect` cannot read most real T-SQL files whole.
///
/// It was measured elsewhere at 28%. dbd no longer depends on sqlparser for
/// DDL, so this does not re-run it — instead it records what share of the
/// corpus the CURRENT reader would have to handle, which is the number the
/// T-SQL walk will be judged against when it lands.
#[test]
#[ignore]
fn the_tsql_share_of_the_corpus_is_recorded() {
    let Some(root) = corpus() else {
        println!("DBD_SQL_CORPUS unset or not a directory — nothing to measure.");
        return;
    };

    let files = sql_files(&root);
    let tsql: Vec<&PathBuf> = files
        .iter()
        .filter(|f| {
            dbd_core::source_text::read_to_string(f)
                .map(|s| Dialect::detect(&s) == Dialect::TSql)
                .unwrap_or(false)
        })
        .collect();

    let bytes: u64 = tsql
        .iter()
        .filter_map(|f| std::fs::metadata(f).ok())
        .map(|m| m.len())
        .sum();

    println!("\n── the T-SQL half ──");
    println!("  files  {:>6} of {}", tsql.len(), files.len());
    println!("  bytes  {:>6.1} MB", bytes as f64 / 1_048_576.0);
    println!("  (the set the T-SQL reader must handle)\n");
}
