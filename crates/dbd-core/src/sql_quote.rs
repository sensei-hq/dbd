//! Rendering values and names into SQL text without letting them escape their
//! syntactic slot.
//!
//! dbd writes SQL by interpolation — into `ALTER` statements it executes, and
//! into migration/data scripts it hands to an operator. Every one of those
//! interpolations is a place where a name or a value carrying a quote produces
//! SQL that means something other than what dbd intended. The inputs are not
//! obviously hostile (schema names, enum labels, column names), but they are all
//! attacker-influenceable in any project where more than one person can add a
//! migration, and a generated script is often run by a superuser.
//!
//! Postgres itself is the reference here: these mirror `quote_literal` and
//! `quote_ident`.

/// Render a string as a Postgres string literal, doubling embedded quotes.
///
/// `O'Brien` → `'O''Brien'`. A backslash needs no special handling:
/// `standard_conforming_strings` has been on by default since 9.1, so `\` in a
/// plain `'…'` literal is an ordinary character.
pub fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Render one identifier, double-quoted, doubling embedded double quotes.
///
/// Always quotes rather than quoting only when needed: an unquoted identifier is
/// folded to lower case by Postgres, so quoting conditionally would silently
/// change which object a mixed-case name resolves to.
pub fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Render a possibly-qualified name (`schema.table`) by quoting each part.
///
/// Splitting on `.` is right for names dbd itself composed from catalog parts,
/// which is every caller — a name that arrived already quoted would be
/// double-quoted here, so pass the parts, not a rendered name.
pub fn qualified(name: &str) -> String {
    name.split('.').map(ident).collect::<Vec<_>>().join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The injection case: a value that closes its own literal and appends a
    /// statement must come back as one inert literal.
    #[test]
    fn a_quote_in_a_value_cannot_close_the_literal() {
        assert_eq!(literal("x'; DROP TABLE t; --"), "'x''; DROP TABLE t; --'");
    }

    #[test]
    fn an_ordinary_value_is_just_quoted() {
        assert_eq!(literal("red"), "'red'");
        assert_eq!(literal(""), "''");
    }

    /// A backslash is literal under `standard_conforming_strings`, so it must NOT
    /// be doubled — doing so would change the value.
    #[test]
    fn a_backslash_is_left_alone() {
        assert_eq!(literal(r"a\b"), r"'a\b'");
    }

    #[test]
    fn a_quote_in_an_identifier_cannot_close_the_quoting() {
        assert_eq!(ident(r#"t" ; DROP TABLE x; --"#), r#""t"" ; DROP TABLE x; --""#);
    }

    #[test]
    fn identifiers_are_always_quoted_so_case_is_preserved() {
        assert_eq!(ident("MyTable"), "\"MyTable\"");
    }

    #[test]
    fn each_part_of_a_qualified_name_is_quoted_separately() {
        assert_eq!(qualified("app.thing"), "\"app\".\"thing\"");
        assert_eq!(qualified("thing"), "\"thing\"");
    }
}
