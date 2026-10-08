//! Turning a file's bytes into SQL a reader can look at.
//!
//! `std::fs::read_to_string` rejects anything that is not UTF-8, and that is
//! not a rare shape in this domain: SSMS writes UTF-16LE by default. Measured
//! over a real SQL Server corpus of 2,421 `.sql`/`.ddl` files — **377 UTF-16
//! with a BOM (15.6%) and 14 other non-UTF-8 (0.6%)**, so 16.2% of it was
//! invisible before any grammar was involved.
//!
//! It is not only a parser's problem. dbd's project scan reads DDL with
//! `read_to_string` and *propagates* the error, so one UTF-16 file under `ddl/`
//! failed the whole load — not that file, the load.
//!
//! # A BOM is a positive statement of encoding, and it is read FIRST
//!
//! UTF-16LE ASCII is `X 00 X 00`, so any null-byte test fires on every UTF-16
//! file and calls it binary. Reading the BOM first explains the nulls. It also
//! has to be *consumed*: a parser handed `\u{feff}CREATE` sees an identifier it
//! cannot match, and reports a syntax error on line 1 of a perfectly valid
//! file.
//!
//! # No BOM means UTF-8, and nothing is guessed
//!
//! Charset detection — inferring latin-1 from byte frequencies — is
//! deliberately not done. It is a guess, it is wrong often enough to matter,
//! and [`Decoded::NotUtf8`] is already the actionable answer: re-encode the
//! file. Sensei measured the same corpus shape from the other side (388
//! UTF-16LE, all BOM-carrying, against 11 files of everything else), so
//! detection would buy a handful of files at the cost of inventing characters
//! the source never carried.
//!
//! Ported from sensei's `classifiers::decode_source`, which this has to agree
//! with: the two read the same trees.

use std::path::Path;

use crate::error::{DbdError, Result};

/// What a file's bytes turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    /// Decoded text, with any BOM consumed.
    Text(String),
    /// Null bytes and no BOM to explain them — an opaque binary. Expected, and
    /// quiet.
    Binary,
    /// Text in an encoding that is not UTF-8 and did not say which it is.
    /// **Actionable**: the file can be re-encoded.
    NotUtf8,
}

/// Decode a source file's bytes.
///
/// See the module documentation for why the BOM is read first and why nothing
/// is guessed.
pub fn decode(bytes: &[u8]) -> Decoded {
    // `for_bom` answers with the encoding AND the BOM's length, so the marker
    // itself is never handed on as content.
    if let Some((encoding, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        let (text, _, had_errors) = encoding.decode(&bytes[bom_len..]);
        if had_errors {
            // A lossy decode substitutes U+FFFD. In an identifier that is a
            // name no use site could mint and a caller cannot tell from one the
            // source really carried, so the file is refused rather than
            // silently altered.
            return Decoded::NotUtf8;
        }
        return Decoded::Text(text.into_owned());
    }
    // No BOM. A null byte here is unexplained, which is what an opaque binary
    // looks like.
    if bytes.contains(&0) {
        return Decoded::Binary;
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Decoded::Text(text.to_string()),
        Err(_) => Decoded::NotUtf8,
    }
}

/// How a source file sits on disk: the byte-order mark it opened with, if any.
///
/// What a tool that rewrites a file needs to write it back the way it found
/// it. Formatting rewrote an SSMS file (UTF-16LE) as UTF-8, changing every byte
/// of it for a change of layout nobody asked to be a change of encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    /// UTF-8 without a BOM — everything dbd writes itself.
    #[default]
    Utf8,
    /// UTF-8 behind a BOM.
    Utf8Bom,
    /// UTF-16LE behind a BOM — what SSMS saves.
    Utf16Le,
    /// UTF-16BE behind a BOM.
    Utf16Be,
}

impl Encoding {
    /// The encoding `bytes` announce with their BOM; UTF-8 when they have none.
    fn of(bytes: &[u8]) -> Self {
        match encoding_rs::Encoding::for_bom(bytes) {
            Some((e, _)) if e == encoding_rs::UTF_16LE => Self::Utf16Le,
            Some((e, _)) if e == encoding_rs::UTF_16BE => Self::Utf16Be,
            Some(_) => Self::Utf8Bom,
            None => Self::Utf8,
        }
    }

    /// `text` in this encoding, behind its BOM — the inverse of reading it.
    ///
    /// UTF-16 by hand: `encoding_rs` implements the WHATWG Encoding Standard,
    /// whose UTF-16 encoders output UTF-8.
    pub fn encode(self, text: &str) -> Vec<u8> {
        let utf16 = |bom: [u8; 2], unit_bytes: fn(u16) -> [u8; 2]| {
            let mut bytes = bom.to_vec();
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&unit_bytes(unit));
            }
            bytes
        };
        match self {
            Self::Utf8 => text.as_bytes().to_vec(),
            Self::Utf8Bom => [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
            Self::Utf16Le => utf16([0xFF, 0xFE], u16::to_le_bytes),
            Self::Utf16Be => utf16([0xFE, 0xFF], u16::to_be_bytes),
        }
    }
}

/// Read a source file, decoding by [`decode`].
///
/// The replacement for `std::fs::read_to_string` on any path that might hold
/// SQL somebody else's tooling wrote. Both failure modes name the file and say
/// which they are, because they call for different things: a binary under
/// `ddl/` is a mistake to remove, and a non-UTF-8 file is one to re-encode.
pub fn read_to_string(path: &Path) -> Result<String> {
    read_with_encoding(path).map(|(text, _)| text)
}

/// [`read_to_string`], and the [`Encoding`] the file was in — for a tool that
/// rewrites the file and must leave its encoding as it was.
pub fn read_with_encoding(path: &Path) -> Result<(String, Encoding)> {
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let bytes = std::fs::read(path).map_err(|e| DbdError::Config(format!("read {}: {e}", path.display())))?;
    match decode(&bytes) {
        Decoded::Text(text) => Ok((text, Encoding::of(&bytes))),
        Decoded::Binary => Err(DbdError::Config(format!(
            "read {}: this is binary, not SQL — it has null bytes and no byte-order mark",
            path.display()
        ))),
        Decoded::NotUtf8 => Err(DbdError::Config(format!(
            "read {}: not UTF-8, and it carries no byte-order mark saying what it is. \
             Re-encode it as UTF-8 (or UTF-16 with a BOM, which SSMS writes by default).",
            path.display()
        ))),
    }
}

/// `text` as SSMS saves it: UTF-16LE behind a byte-order mark. For the tests
/// of every path that has to read such a file the way [`read_to_string`] does.
#[cfg(test)]
pub(crate) fn utf16le(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file that is *only* a BOM decodes to nothing, rather than to a
    /// one-character string a lexer would choke on.
    #[test]
    fn a_bom_and_nothing_else_is_empty_text() {
        assert_eq!(decode(&[0xFF, 0xFE]), Decoded::Text(String::new()));
        assert_eq!(decode(&[0xEF, 0xBB, 0xBF]), Decoded::Text(String::new()));
    }

    /// UTF-16 content that legitimately contains a NUL is still text — the BOM
    /// has already explained the encoding, so the null test must not run.
    #[test]
    fn the_null_test_does_not_run_once_a_bom_has_spoken() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "CREATE TABLE t".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        assert!(bytes.contains(&0), "precondition: UTF-16LE ASCII is full of nulls");
        assert!(matches!(decode(&bytes), Decoded::Text(_)));
    }
}
