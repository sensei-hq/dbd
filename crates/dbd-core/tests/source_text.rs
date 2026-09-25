//! Decoding a source file — the step before any parser sees it.
//!
//! SSMS writes UTF-16LE by default, and `std::fs::read_to_string` rejects it
//! outright. Measured over a real SQL Server corpus: of 2,421 `.sql`/`.ddl`
//! files, **377 are UTF-16 with a BOM and 14 are some other non-UTF-8** — 16.2%
//! invisible to a UTF-8-only reader, before any grammar is involved.
//!
//! That is not only a parser problem. dbd's project scan reads DDL with
//! `read_to_string` and *propagates* the error, so one UTF-16 file in `ddl/`
//! fails the whole load — not that file, the load.
//!
//! The rule, ported from sensei's `classifiers::decode_source`: **a BOM is a
//! positive statement of encoding and it is read first.** UTF-16LE ASCII is
//! `X 00 X 00`, so a null-byte test fires on every UTF-16 file; once the BOM is
//! seen the nulls are explained. No BOM means UTF-8 is required, and charset
//! *detection* — guessing latin-1 from byte frequencies — is deliberately not
//! done. Sensei measured the same split (97% BOM-carrying), so detection buys a
//! handful of files at the cost of a guess.

use dbd_core::source_text::{self, Decoded};

fn utf16le(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn utf16be(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    bytes
}

// ── The case the corpus is full of ──────────────────────────────────────────

#[test]
fn utf16le_with_a_bom_decodes() {
    let sql = "CREATE PROCEDURE [dbo].[sp_X] @p int AS SELECT 1";
    assert_eq!(source_text::decode(&utf16le(sql)), Decoded::Text(sql.to_string()));
}

#[test]
fn utf16be_with_a_bom_decodes() {
    let sql = "CREATE TABLE [dbo].[Users] (Id int)";
    assert_eq!(source_text::decode(&utf16be(sql)), Decoded::Text(sql.to_string()));
}

/// The BOM must not survive into the text. A parser handed `\u{feff}CREATE`
/// sees an identifier it cannot match, and the failure reads as a syntax error
/// on line 1 of a file that is perfectly valid.
#[test]
fn the_bom_is_not_handed_on_as_content() {
    let Decoded::Text(text) = source_text::decode(&utf16le("CREATE TABLE t (id int)")) else {
        panic!("expected text");
    };
    assert!(
        !text.starts_with('\u{feff}'),
        "the BOM leaked into the content: {text:?}"
    );
    assert!(text.starts_with("CREATE"), "got {text:?}");
}

/// A UTF-8 BOM is a statement too, and the same rule strips it.
#[test]
fn a_utf8_bom_is_stripped_rather_than_parsed() {
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(b"CREATE TABLE t (id int)");
    let Decoded::Text(text) = source_text::decode(&bytes) else {
        panic!("expected text");
    };
    assert_eq!(text, "CREATE TABLE t (id int)");
}

// ── No BOM ──────────────────────────────────────────────────────────────────

#[test]
fn plain_utf8_decodes_unchanged() {
    let sql = "create table t (id int); -- héllo";
    assert_eq!(source_text::decode(sql.as_bytes()), Decoded::Text(sql.to_string()));
}

/// Null bytes with no BOM are unexplained, which is what an opaque binary looks
/// like. Distinguishing this from `NotUtf8` matters: one is expected and quiet,
/// the other is actionable — the user can re-encode the file.
#[test]
fn null_bytes_without_a_bom_are_binary() {
    assert_eq!(source_text::decode(&[0x00, 0x01, 0x02, b'a']), Decoded::Binary);
}

#[test]
fn invalid_utf8_without_a_bom_is_reported_as_such() {
    // 0xE9 is 'é' in latin-1 and not valid standalone UTF-8.
    assert_eq!(source_text::decode(&[b'-', b'-', b' ', 0xE9, 0xE9]), Decoded::NotUtf8);
}

/// Charset detection is deliberately absent. Latin-1 is *decodable* as
/// something, and decoding it would silently invent characters the source never
/// carried; `NotUtf8` is the actionable answer instead.
#[test]
fn latin1_is_refused_rather_than_guessed_at() {
    let latin1 = [b'C', b'R', b'E', b'A', b'T', b'E', b' ', 0xE9];
    assert_eq!(source_text::decode(&latin1), Decoded::NotUtf8);
}

/// A decode that had to substitute is refused. A replacement character in an
/// identifier is a name no use site could ever mint, and a caller cannot tell
/// it from a name the source really carried.
#[test]
fn a_lossy_decode_is_refused_rather_than_returned_with_replacements() {
    // A BOM promising UTF-16LE, then an odd number of trailing bytes.
    let mut bytes = utf16le("CREATE");
    bytes.push(0x41);
    assert_eq!(source_text::decode(&bytes), Decoded::NotUtf8);
}

#[test]
fn an_empty_file_is_empty_text_not_an_error() {
    assert_eq!(source_text::decode(&[]), Decoded::Text(String::new()));
}

// ── Reading a file ──────────────────────────────────────────────────────────

#[test]
fn reading_a_utf16_file_succeeds_where_read_to_string_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sp_X.sql");
    std::fs::write(&path, utf16le("CREATE PROCEDURE [dbo].[sp_X] AS SELECT 1")).unwrap();

    assert!(
        std::fs::read_to_string(&path).is_err(),
        "precondition: the stdlib must reject this, or the test proves nothing"
    );
    let text = source_text::read_to_string(&path).expect("dbd must read it");
    assert!(text.starts_with("CREATE PROCEDURE"), "got {text:?}");
}

#[test]
fn reading_a_binary_file_fails_with_a_reason_naming_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blob.sql");
    std::fs::write(&path, [0x00u8, 0x01, 0x02]).unwrap();

    let err = source_text::read_to_string(&path).unwrap_err().to_string();
    assert!(err.contains("blob.sql"), "must name the file: {err}");
}

/// The actionable case gets an actionable message — "re-encode it" is something
/// a user can do; "invalid input" is not.
#[test]
fn a_non_utf8_file_says_what_to_do_about_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("latin.sql");
    std::fs::write(&path, [b'-', b'-', b' ', 0xE9, 0xE9]).unwrap();

    let err = source_text::read_to_string(&path).unwrap_err().to_string();
    assert!(
        err.to_lowercase().contains("utf-8") || err.to_lowercase().contains("encod"),
        "must point at the encoding: {err}"
    );
}

// ── dbd's own scan path ─────────────────────────────────────────────────────

/// The live defect this decode exists to fix. `Design::from_config` read DDL
/// with `read_to_string` and PROPAGATED the error, so a single UTF-16 file
/// under `ddl/` failed the whole load — not that file, the load. A project
/// authored in SSMS could not be opened at all.
#[test]
fn a_utf16_ddl_file_does_not_fail_the_whole_project_load() {
    use dbd_core::Design;

    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir_all(d.join("ddl/table/app")).unwrap();
    std::fs::write(
        d.join("design.yaml"),
        "project:\n  name: ssms\n  version: 1\n\ntarget:\n  postgres:\n    url: $DATABASE_URL\n\nschemas:\n  - app\n",
    )
    .unwrap();

    // One ordinary UTF-8 file, one written the way SSMS writes them.
    std::fs::write(
        d.join("ddl/table/app/plain.ddl"),
        "set search_path to app;\ncreate table plain (id int primary key);\n",
    )
    .unwrap();
    std::fs::write(
        d.join("ddl/table/app/wide.ddl"),
        utf16le("set search_path to app;\ncreate table wide (id int primary key);\n"),
    )
    .unwrap();

    let design = Design::from_config(&d.join("design.yaml"), "dev")
        .expect("one UTF-16 file must not fail the load of a whole project");

    let names: Vec<&str> = design.entities().iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"app.plain"), "got {names:?}");
    assert!(
        names.contains(&"app.wide"),
        "the UTF-16 table must be read, not skipped: {names:?}"
    );

    let wide = design.entities().iter().find(|e| e.name == "app.wide").unwrap();
    assert!(wide.errors.is_empty(), "and read cleanly: {:?}", wide.errors);
    assert!(wide.table_def.is_some(), "with its structure intact");
}
