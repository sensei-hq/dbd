//! Which SQL a file is written in — stated, or detected.
//!
//! Distinct from [`super::ParserChoice`], which is *which reader dbd runs*.
//! They are different questions: several dialects can share a reader (SQLite
//! and anything else applied verbatim), and a dialect dbd has no reader for
//! still has a name. [`ParserChoice::for_dialect_typed`] is the one place the
//! mapping lives, so a stated label and a detected dialect cannot select
//! different readers for the same SQL.
//!
//! # Stated outranks detected
//!
//! A `design.yaml` carrying `source.dialect: postgresql` is the source saying
//! what it is; detection is inference, and a statement outranks an inference.
//! `project::survey` reports the stated one; this is for everything else —
//! measured at 4,832 SQL files in a single repository with no manifest of any
//! kind.
//!
//! # Detection fails closed
//!
//! [`Dialect::detect`] scores markers that exist in exactly one dialect. No
//! marker, or a tie, is [`Dialect::Unstated`] — **not** a default and not a
//! guess. `CREATE TABLE t (id int)` is valid everywhere and says nothing about
//! which dialect it is in, and answering "PostgreSQL" there would be inventing
//! a fact about the file.
//!
//! The marker lists are ported from sensei's `indexer::lang::sql`, where they
//! were scored against a real multi-dialect corpus, and are kept as measured
//! rather than re-derived — a list invented from memory recognises the SQL its
//! author happened to think of. Two of them encode corpus findings that are
//! easy to get wrong and are pinned by tests: `GO` matches only as a whole
//! line (as a substring it is inside `category`, `logo`, `go_live`), and a
//! backtick is **not** a MySQL marker, because it is also what everyone writes
//! around a word in a comment.

use serde::{Deserialize, Serialize};

/// A SQL dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    /// PostgreSQL — `$$` bodies, `::` casts, `plpgsql`. Supabase is this.
    PostgreSql,
    /// Microsoft SQL Server. `GO` batches, `[bracketed]` names, `@@ROWCOUNT`.
    TSql,
    MySql,
    Sqlite,
    /// No marker of any dialect, or markers for more than one in equal
    /// measure. Not a default *answer* — see the module note on failing closed
    /// — but it is the `Default` value, because "nothing has said" is the
    /// honest starting point for a field nobody has filled in yet.
    #[default]
    Unstated,
}

impl Dialect {
    /// The dialect a `source.dialect` label names, or `None` if the label is
    /// not one dbd knows.
    ///
    /// `None` rather than [`Self::Unstated`] on purpose: "I do not know this
    /// word" and "this text states no dialect" are different facts, and a
    /// caller that cannot tell them apart cannot report a typo in a config.
    pub fn from_label(label: &str) -> Option<Self> {
        match label.trim().to_ascii_lowercase().as_str() {
            "postgresql" | "postgres" | "supabase" => Some(Self::PostgreSql),
            "tsql" | "t-sql" | "mssql" | "sqlserver" | "sql_server" => Some(Self::TSql),
            "mysql" | "mariadb" => Some(Self::MySql),
            "sqlite" | "sqlite3" => Some(Self::Sqlite),
            _ => None,
        }
    }

    /// The canonical label, which [`Self::from_label`] accepts back.
    pub fn as_label(&self) -> &'static str {
        match self {
            Self::PostgreSql => "postgresql",
            Self::TSql => "tsql",
            Self::MySql => "mysql",
            Self::Sqlite => "sqlite",
            Self::Unstated => "unstated",
        }
    }

    /// Read the dialect out of the SQL itself.
    ///
    /// Returns [`Self::Unstated`] when nothing distinguishes it, or when two
    /// dialects score equally — ambiguity is not a dialect, and resolving a tie
    /// by declaration order would make the answer depend on how this enum
    /// happens to be written.
    pub fn detect(sql: &str) -> Self {
        let lower = sql.to_ascii_lowercase();
        let count = |needles: &[&str]| -> usize { needles.iter().map(|n| lower.matches(n).count()).sum() };

        let scores = [
            (count(TSQL_MARKERS) + go_batches(sql), Self::TSql),
            (count(POSTGRES_MARKERS), Self::PostgreSql),
            (count(MYSQL_MARKERS), Self::MySql),
            (count(SQLITE_MARKERS), Self::Sqlite),
        ];

        let Some(&(best, dialect)) = scores.iter().max_by_key(|(n, _)| *n) else {
            return Self::Unstated;
        };
        if best == 0 {
            return Self::Unstated;
        }
        if scores.iter().filter(|(n, _)| *n == best).count() > 1 {
            return Self::Unstated;
        }
        dialect
    }
}

/// `GO` batch separators — matched as a whole line.
///
/// As a substring `go` is inside `category`, `logo` and `go_live`, so a loose
/// match would read half a corpus as T-SQL.
fn go_batches(sql: &str) -> usize {
    sql.lines()
        .filter(|line| {
            let mut chars = line.trim().chars();
            matches!(chars.next(), Some('g' | 'G'))
                && matches!(chars.next(), Some('o' | 'O'))
                && chars.as_str().trim().is_empty()
        })
        .count()
}

const TSQL_MARKERS: &[&str] = &[
    "set ansi_nulls",
    "set quoted_identifier",
    "nvarchar",
    "[dbo]",
    "@@rowcount",
    "@@identity",
    "getdate()",
    "isnull(",
    "nonclustered",
    "uniqueidentifier",
    "begin tran",
    "sp_executesql",
];

const POSTGRES_MARKERS: &[&str] = &[
    "language plpgsql",
    "search_path",
    "returns trigger",
    "create extension",
    "jsonb",
    "serial primary key",
    "$$",
    "::text",
    "::uuid",
    "::int",
    "on conflict",
    "returning ",
];

/// No backtick. It is MySQL's identifier quote, but it is also what everyone
/// writes around a word in a comment — a marker has to be something only that
/// dialect *writes*.
const MYSQL_MARKERS: &[&str] = &["auto_increment", "engine=innodb", "unsigned int"];

const SQLITE_MARKERS: &[&str] = &["autoincrement", "pragma ", "without rowid"];

#[cfg(test)]
mod tests {
    use super::*;

    /// `AUTOINCREMENT` is SQLite's and `AUTO_INCREMENT` is MySQL's, and the
    /// first is a substring of neither — but the underscore is the only thing
    /// telling them apart, so pin it.
    #[test]
    fn mysqls_auto_increment_is_not_sqlites_autoincrement() {
        assert_eq!(
            Dialect::detect("create table t (id integer primary key autoincrement);"),
            Dialect::Sqlite
        );
        assert_eq!(
            Dialect::detect("create table t (id int auto_increment) engine=innodb;"),
            Dialect::MySql
        );
    }

    /// Case is not a marker. A corpus writes keywords both ways and the scoring
    /// lowercases first; this pins that it really does.
    #[test]
    fn markers_match_regardless_of_case() {
        assert_eq!(Dialect::detect("CREATE TABLE t (n NVARCHAR(10));"), Dialect::TSql);
        assert_eq!(Dialect::detect("create table t (n nvarchar(10));"), Dialect::TSql);
    }
}
