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

/// What a real SQL codebase is actually made OF.
///
/// The number that shapes the whole design: if a corpus were mostly
/// declarations, a reader could treat every file as one and be roughly right.
/// It is not. Sensei measured `ALTER TABLE` outnumbering `CREATE TABLE` 159 to
/// 101 on its corpus; this asks the same question of every file here, through
/// dbd's own classifier.
///
/// Reported rather than pinned to a threshold — the ratio is a fact about this
/// corpus, and asserting one would be asserting something about somebody's
/// codebase rather than about dbd.
#[test]
#[ignore]
fn the_file_kinds_of_a_real_corpus_are_recorded() {
    use dbd_core::parser::{FileKind, parse_sql_as};

    let Some(root) = corpus() else {
        println!("DBD_SQL_CORPUS unset or not a directory — nothing to measure.");
        return;
    };

    let mut declaration = 0usize;
    let mut migration = 0usize;
    let mut data = 0usize;
    let mut mixed = 0usize;
    let mut empty = 0usize;
    let mut unreadable = 0usize;
    let mut errored = 0usize;

    let files = sql_files(&root);
    for file in &files {
        let Ok(sql) = dbd_core::source_text::read_to_string(file) else {
            unreadable += 1;
            continue;
        };
        let Ok(parsed) = parse_sql_as(Dialect::detect(&sql), &sql) else {
            errored += 1;
            continue;
        };
        if !parsed.errors.is_empty() {
            errored += 1;
            continue;
        }
        match parsed.kind {
            FileKind::Declaration => declaration += 1,
            FileKind::Migration => migration += 1,
            FileKind::Data => data += 1,
            FileKind::Mixed => mixed += 1,
            FileKind::Empty => empty += 1,
        }
    }

    let total = files.len();
    println!("\n── FileKind over {total} SQL files ──");
    println!("  declaration {declaration:>6}  ({:.1}%)", pct(declaration, total));
    println!("  migration   {migration:>6}  ({:.1}%)", pct(migration, total));
    println!("  data        {data:>6}  ({:.1}%)", pct(data, total));
    println!("  mixed       {mixed:>6}  ({:.1}%)", pct(mixed, total));
    println!("  empty       {empty:>6}  ({:.1}%)", pct(empty, total));
    println!("  ── not classified ──");
    println!("  parse error {errored:>6}  ({:.1}%)", pct(errored, total));
    println!("  unreadable  {unreadable:>6}\n");
    println!("  NOTE: these are T-SQL files read by the PostgreSQL reader —");
    println!("  the parse-error share is the gap the T-SQL reader closes.\n");
}

/// **Is an off-the-shelf parser good enough for T-SQL?**
///
/// The question dbd has to answer before writing a reader of its own, and the
/// answer has to be measured rather than inherited. Sensei measured
/// `sqlparser`'s `MsSqlDialect` at 28% of files parsed whole over 331 files;
/// this asks the same of every T-SQL file here, which is a far larger sample
/// and one that includes the UTF-16 files a UTF-8-only reader never saw.
///
/// `sqlparser` is already a dependency (the formatter uses it), so this costs
/// nothing to ask. If the number were high, dbd should use it and not write a
/// lexer. Reported, not asserted: a threshold here would be a claim about
/// somebody's codebase.
#[test]
#[ignore]
fn how_much_of_the_tsql_corpus_an_off_the_shelf_parser_reads() {
    use sqlparser::dialect::{GenericDialect, MsSqlDialect, PostgreSqlDialect};
    use sqlparser::parser::Parser;

    let Some(root) = corpus() else {
        println!("DBD_SQL_CORPUS unset or not a directory — nothing to measure.");
        return;
    };

    let tsql: Vec<String> = sql_files(&root)
        .iter()
        .filter_map(|f| dbd_core::source_text::read_to_string(f).ok())
        .filter(|s| Dialect::detect(s) == Dialect::TSql)
        .collect();

    let mut mssql_ok = 0usize;
    let mut generic_ok = 0usize;
    let mut pg_ok = 0usize;
    let mut libpg_ok = 0usize;

    for sql in &tsql {
        if Parser::parse_sql(&MsSqlDialect {}, sql).is_ok() {
            mssql_ok += 1;
        }
        if Parser::parse_sql(&GenericDialect {}, sql).is_ok() {
            generic_ok += 1;
        }
        if Parser::parse_sql(&PostgreSqlDialect {}, sql).is_ok() {
            pg_ok += 1;
        }
        if pg_query::parse(sql).is_ok() {
            libpg_ok += 1;
        }
    }

    let n = tsql.len();
    println!("\n── whole-file parse rate over {n} detected T-SQL files ──");
    println!(
        "  sqlparser MsSqlDialect       {mssql_ok:>6}  ({:.1}%)",
        pct(mssql_ok, n)
    );
    println!(
        "  sqlparser GenericDialect     {generic_ok:>6}  ({:.1}%)",
        pct(generic_ok, n)
    );
    println!("  sqlparser PostgreSqlDialect  {pg_ok:>6}  ({:.1}%)", pct(pg_ok, n));
    println!(
        "  libpg_query                  {libpg_ok:>6}  ({:.1}%)",
        pct(libpg_ok, n)
    );
    println!("\n  A reader dbd would rely on has to clear these.\n");
}

/// **Does splitting `GO` batches rescue an off-the-shelf parser?**
///
/// The obvious confound in the measurement above. `GO` is not SQL — it is a
/// client directive that sqlcmd and SSMS use to split a file into batches — so
/// no grammar accepts it, and nearly every file in a SQL Server codebase has
/// one. A whole-file parse rate therefore measures `GO` as much as it measures
/// the grammar.
///
/// This splits first and asks again. It matters: if a batch splitter plus
/// `sqlparser` reads most of a corpus, dbd should write the splitter and stop,
/// rather than write a reader. That is roughly a thousand lines of difference,
/// so it is worth the fifty this costs to ask.
#[test]
#[ignore]
fn whether_splitting_go_batches_rescues_an_off_the_shelf_parser() {
    use sqlparser::dialect::MsSqlDialect;
    use sqlparser::parser::Parser;

    let Some(root) = corpus() else {
        println!("DBD_SQL_CORPUS unset or not a directory — nothing to measure.");
        return;
    };

    /// Split on a line that is exactly `GO` — the same rule `Dialect::detect`
    /// counts batches with.
    fn batches(sql: &str) -> Vec<String> {
        let mut out = vec![String::new()];
        for line in sql.lines() {
            let t = line.trim();
            let mut c = t.chars();
            let is_go = matches!(c.next(), Some('g' | 'G'))
                && matches!(c.next(), Some('o' | 'O'))
                && c.as_str().trim().is_empty();
            if is_go {
                out.push(String::new());
            } else {
                out.last_mut().unwrap().push_str(line);
                out.last_mut().unwrap().push('\n');
            }
        }
        out.retain(|b| !b.trim().is_empty());
        out
    }

    let tsql: Vec<String> = sql_files(&root)
        .iter()
        .filter_map(|f| dbd_core::source_text::read_to_string(f).ok())
        .filter(|s| Dialect::detect(s) == Dialect::TSql)
        .collect();

    let mut whole_file = 0usize;
    let mut total_batches = 0usize;
    let mut ok_batches = 0usize;

    for sql in &tsql {
        let bs = batches(sql);
        let mut all = true;
        for b in &bs {
            total_batches += 1;
            if Parser::parse_sql(&MsSqlDialect {}, b).is_ok() {
                ok_batches += 1;
            } else {
                all = false;
            }
        }
        if all && !bs.is_empty() {
            whole_file += 1;
        }
    }

    let n = tsql.len();
    println!("\n── MsSqlDialect AFTER splitting GO batches, {n} T-SQL files ──");
    println!(
        "  files where every batch parses  {whole_file:>6}  ({:.1}%)",
        pct(whole_file, n)
    );
    println!(
        "  batches that parse              {ok_batches:>6}  ({:.1}%)  of {total_batches}",
        pct(ok_batches, total_batches)
    );
    println!("\n  If this were high, a splitter + sqlparser would beat writing a reader.\n");
}

/// **WHICH batches does an off-the-shelf parser fail on?**
///
/// 81.9% of batches parsing sounds like a usable reader. It only is if the
/// 18.1% that fail are unimportant — and the suspicion is the opposite. A
/// `CREATE PROCEDURE` body is where a T-SQL codebase keeps its table
/// references, and `CREATE PROCEDURE dbo.x @p int AS` is the parenless
/// parameter form `MsSqlDialect` is known to reject. If the failures are
/// concentrated there, an 81.9% batch rate hides a near-total loss of exactly
/// the facts dbd wants.
#[test]
#[ignore]
fn which_batches_an_off_the_shelf_parser_fails_on() {
    use sqlparser::dialect::MsSqlDialect;
    use sqlparser::parser::Parser;

    let Some(root) = corpus() else {
        println!("DBD_SQL_CORPUS unset or not a directory — nothing to measure.");
        return;
    };

    fn batches(sql: &str) -> Vec<String> {
        let mut out = vec![String::new()];
        for line in sql.lines() {
            let t = line.trim();
            let mut c = t.chars();
            let is_go = matches!(c.next(), Some('g' | 'G'))
                && matches!(c.next(), Some('o' | 'O'))
                && c.as_str().trim().is_empty();
            if is_go {
                out.push(String::new());
            } else {
                out.last_mut().unwrap().push_str(line);
                out.last_mut().unwrap().push('\n');
            }
        }
        out.retain(|b| !b.trim().is_empty());
        out
    }

    /// The first two words, which is what a statement head amounts to.
    fn head(batch: &str) -> String {
        let cleaned: String = batch
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join(" ");
        cleaned
            .split_whitespace()
            .take(2)
            .map(|w| w.to_ascii_uppercase())
            .collect::<Vec<_>>()
            .join(" ")
    }

    let tsql: Vec<String> = sql_files(&root)
        .iter()
        .filter_map(|f| dbd_core::source_text::read_to_string(f).ok())
        .filter(|s| Dialect::detect(s) == Dialect::TSql)
        .collect();

    let mut failed: std::collections::BTreeMap<String, usize> = Default::default();
    let mut passed: std::collections::BTreeMap<String, usize> = Default::default();

    for sql in &tsql {
        for b in batches(sql) {
            let h = head(&b);
            if Parser::parse_sql(&MsSqlDialect {}, &b).is_ok() {
                *passed.entry(h).or_default() += 1;
            } else {
                *failed.entry(h).or_default() += 1;
            }
        }
    }

    let mut top: Vec<(&String, &usize)> = failed.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1));

    println!("\n── the batches MsSqlDialect FAILS on, by statement head ──");
    for (h, n) in top.iter().take(12) {
        let ok = passed.get(*h).copied().unwrap_or(0);
        let total = *n + ok;
        println!(
            "  {:<28} failed {:>5} of {:>5}  ({:.0}% lost)",
            h,
            n,
            total,
            pct(**n, total)
        );
    }
    println!("\n  A head that is mostly lost is a fact dbd would not have.\n");
}

/// **Can the lexer get through the corpus at all?**
///
/// The claim behind choosing a lexer over a grammar is that it has no opinion
/// about T-SQL it does not understand — where `MsSqlDialect` loses 99% of
/// `CREATE PROCEDURE` batches, a tokeniser should lose none, because there is
/// nothing for it to reject.
///
/// This checks that directly: every batch of every T-SQL file is tokenised, and
/// what comes out is counted. It also pins the two ways a hand-written lexer
/// can fail silently — producing nothing from a non-empty batch, or hanging on
/// an unterminated construct (the test simply has to finish).
#[test]
#[ignore]
fn the_lexer_gets_through_the_whole_tsql_corpus() {
    use dbd_core::parser::lex;

    let Some(root) = corpus() else {
        println!("DBD_SQL_CORPUS unset or not a directory — nothing to measure.");
        return;
    };

    let tsql: Vec<String> = sql_files(&root)
        .iter()
        .filter_map(|f| dbd_core::source_text::read_to_string(f).ok())
        .filter(|s| Dialect::detect(s) == Dialect::TSql)
        .collect();

    let mut batches = 0usize;
    let mut empty_batches = 0usize;
    let mut toks = 0usize;
    let mut names = 0usize;

    for sql in &tsql {
        for (_line, batch) in lex::batches(sql) {
            batches += 1;
            let t = lex::tokens(batch);
            if t.is_empty() {
                // A non-empty batch that yields no tokens means the lexer
                // consumed everything as comment or literal — possible, but
                // worth counting rather than assuming.
                empty_batches += 1;
            }
            names += t.iter().filter(|t| t.name().is_some()).count();
            toks += t.len();
        }
    }

    println!("\n── the lexer over {} T-SQL files ──", tsql.len());
    println!("  batches            {batches:>8}");
    println!(
        "  yielding no tokens {empty_batches:>8}  ({:.2}%)",
        pct(empty_batches, batches)
    );
    println!("  tokens             {toks:>8}");
    println!("  of which names     {names:>8}  ({:.1}%)", pct(names, toks));
    println!("\n  Compare: MsSqlDialect fails 18.1% of these batches outright,");
    println!("  and 99% of the CREATE PROCEDURE ones.\n");

    assert!(
        pct(empty_batches, batches) < 5.0,
        "{empty_batches} of {batches} batches produced no tokens at all — \
         the lexer is swallowing content, not reading it"
    );
}

/// **What the T-SQL reader finds in a real corpus.**
///
/// The number this whole increment exists to move. The same corpus read by
/// libpg_query produced a 94.5% parse-error rate and 13 declarations; an
/// off-the-shelf T-SQL grammar loses 99% of `CREATE PROCEDURE`. This asks what
/// dbd's own reader gets.
#[test]
#[ignore]
fn what_the_tsql_reader_finds_in_a_real_corpus() {
    use dbd_core::entity::EntityType;
    use dbd_core::parser::{FileKind, parse_sql_as};
    use std::collections::BTreeMap;

    let Some(root) = corpus() else {
        println!("DBD_SQL_CORPUS unset or not a directory — nothing to measure.");
        return;
    };

    let tsql: Vec<String> = sql_files(&root)
        .iter()
        .filter_map(|f| dbd_core::source_text::read_to_string(f).ok())
        .filter(|s| Dialect::detect(s) == Dialect::TSql)
        .collect();

    let mut kinds: BTreeMap<String, usize> = Default::default();
    let mut types: BTreeMap<String, usize> = Default::default();
    let mut entities = 0usize;
    let mut reads = 0usize;
    let mut writes = 0usize;
    let mut calls = 0usize;
    let mut with_catalog = 0usize;

    for sql in &tsql {
        let Ok(p) = parse_sql_as(Dialect::TSql, sql) else {
            continue;
        };
        *kinds.entry(format!("{:?}", p.kind)).or_default() += 1;
        for e in &p.entities {
            entities += 1;
            *types.entry(format!("{:?}", e.entity_type)).or_default() += 1;
            reads += e.reads.len();
            writes += e.writes.len();
            calls += e
                .references
                .iter()
                .filter(|r| r.ref_type.as_deref() == Some(dbd_core::entity::REF_TYPE_FUNCTION))
                .count();
            if e.catalog.is_some() {
                with_catalog += 1;
            }
        }
    }

    let n = tsql.len();
    println!("\n── the T-SQL reader over {n} files ──");
    println!("  files by kind:");
    for (k, v) in &kinds {
        println!("    {k:<14} {v:>6}  ({:.1}%)", pct(*v, n));
    }
    println!("  entities declared  {entities:>6}");
    for (t, v) in &types {
        println!("    {t:<14} {v:>6}");
    }
    println!("  edges: reads {reads}, writes {writes}, calls {calls}");
    println!("  entities with a catalog (3-part name)  {with_catalog}");
    println!("\n  Before this reader: 13 declarations, 94.5% parse errors.\n");

    // The one thing worth asserting: a reader that declared nothing would be
    // useless, and one that declared something for nearly every file would be
    // minting identities from change scripts.
    let declared_files = kinds.get("Declaration").copied().unwrap_or(0);
    assert!(entities > 500, "only {entities} entities from {n} T-SQL files");
    assert!(
        declared_files < n,
        "every file declared something — change scripts are being read as declarations"
    );
    let _ = EntityType::Table;
    let _ = FileKind::Declaration;
}
