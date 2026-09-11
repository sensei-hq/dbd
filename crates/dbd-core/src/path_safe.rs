//! Containment for paths dbd builds out of names it did not choose.
//!
//! Two of dbd's inputs are not the local operator's own text:
//!
//! - **A live database's catalog.** `dbd reverse` and `dbd export` name files
//!   after schemas, tables and columns. Postgres and SQLite both accept
//!   `CREATE SCHEMA ".."` and `CREATE TABLE "../../../tmp/x"` — a quoted
//!   identifier may hold any character at all — so an object name reaching
//!   `Path::join` unchecked writes wherever it likes.
//! - **A downloaded `design.yaml`.** `dbd deploy <github-source>` runs one dbd
//!   fetched from a repository, and its hook/policy paths are strings from that
//!   file.
//!
//! Both are handled the same way and for the same reason: **rebuild the path
//! from ordinary segments instead of judging the assembled result.** Checking
//! `joined.starts_with(base)` after the fact does not work — neither
//! `Path::join` nor `Path::starts_with` resolves `..`, so a joined path always
//! lexically starts with its base no matter what is inside it.

use std::path::{Component, Path, PathBuf};

/// Whether `s` is usable as one path segment.
///
/// A single segment is the shape a database object name has, so the rule is
/// stricter than for a multi-segment path: any separator at all disqualifies it,
/// because a name is never meant to span directories.
///
/// Deliberately not a character allow-list. Object names legitimately contain
/// spaces, dots and non-ASCII, and rejecting those would refuse to reverse
/// perfectly ordinary databases. What must not get through is a separator, the
/// two relative-directory names, and the NUL that truncates a C path.
pub fn is_safe_segment(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains('/') && !s.contains('\\') && !s.contains('\0')
}

/// Rebuild a project-relative path from ordinary segments, or `None` if any
/// component is not one.
///
/// `./a/b` → `a/b` (a harmless spelling), while `..`, a leading `/` and a
/// Windows drive prefix all refuse. Returns `None` for a path with no segments
/// at all, since that names the project root rather than a file in it.
pub fn safe_relative_path(input: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut any = false;
    for component in Path::new(input).components() {
        match component {
            Component::Normal(part) => {
                out.push(part);
                any = true;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    any.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_object_names_are_safe_segments() {
        for name in ["orders", "order_status", "v2.1", "naïve", "with space", "-leading-dash"] {
            assert!(is_safe_segment(name), "{name} is an ordinary object name");
        }
    }

    /// Every one of these is a legal quoted identifier in Postgres and SQLite,
    /// and every one escapes the directory it is joined into.
    #[test]
    fn a_separator_or_relative_name_is_not_a_safe_segment() {
        for name in ["", ".", "..", "../etc", "a/b", "a\\b", "/abs", "x\0y"] {
            assert!(!is_safe_segment(name), "{name:?} must not be used as a path segment");
        }
    }

    #[test]
    fn a_relative_path_keeps_its_harmless_spellings() {
        assert_eq!(safe_relative_path("sql/hook.sql"), Some("sql/hook.sql".into()));
        assert_eq!(safe_relative_path("./sql/hook.sql"), Some("sql/hook.sql".into()));
        assert_eq!(safe_relative_path("a/./b"), Some("a/b".into()));
    }

    /// `..` anywhere refuses — including after a segment that would cancel it,
    /// because cancelling is what `Path` does NOT do and what a symlink would
    /// defeat anyway.
    #[test]
    fn a_relative_path_refuses_every_way_out() {
        for input in ["../escape.sql", "sql/../../escape.sql", "/etc/passwd", "", "."] {
            assert_eq!(safe_relative_path(input), None, "{input:?} must be refused");
        }
    }
}
