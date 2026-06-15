//! Encode a schema model into a URL fragment for the hosted diagram viewer.
//! The CLI builds `{site}/diagram#1.<payload>` where `<payload>` is the model
//! JSON gzip-compressed and base64url-encoded; the site decodes it client-side.
use base64::Engine;
use flate2::{write::GzEncoder, Compression};
use std::io::Write;

use crate::schema_model::SchemaModel;

/// Fragment format version. Bump when the payload encoding changes.
pub const FRAGMENT_VERSION: &str = "1";

/// Encode `model` as base64url(gzip(json)) — the fragment payload (no `1.` prefix).
pub fn encode_payload(model: &SchemaModel) -> Result<String, serde_json::Error> {
    let json = serde_json::to_vec(model)?;
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    // Writing to / finishing an in-memory Vec is infallible.
    enc.write_all(&json).expect("gzip write to Vec");
    let gz = enc.finish().expect("gzip finish to Vec");
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(gz))
}

/// Build the full hosted-viewer URL: `{base}/diagram#1.<payload>`.
pub fn fragment_url(base: &str, model: &SchemaModel) -> Result<String, serde_json::Error> {
    let payload = encode_payload(model)?;
    Ok(format!("{}/diagram#{}.{}", base.trim_end_matches('/'), FRAGMENT_VERSION, payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_model::{Column, ProjectInfo, SchemaInfo, SchemaModel, TableNode};
    use flate2::read::GzDecoder;
    use std::io::Read;

    fn sample_model() -> SchemaModel {
        SchemaModel {
            project: ProjectInfo { name: "Acme".to_string(), db: "postgres".to_string(), note: None },
            schemas: vec![SchemaInfo { name: "public".to_string(), tables: 1, enums: 0 }],
            tables: vec![TableNode {
                schema: "public".to_string(),
                name: "users".to_string(),
                kind: "table".to_string(),
                note: None,
                note_md: None,
                columns: vec![Column {
                    name: "id".to_string(), ty: "uuid".to_string(),
                    pk: true, nn: true, en: false, def: None, note: None,
                }],
                indexes: vec![],
            }],
            refs: vec![],
        }
    }

    fn decode_payload(payload: &str) -> SchemaModel {
        let gz = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).unwrap();
        let mut s = String::new();
        GzDecoder::new(&gz[..]).read_to_string(&mut s).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn encode_payload_round_trips_through_gzip_base64url() {
        let m = sample_model();
        let payload = encode_payload(&m).unwrap();
        let back = decode_payload(&payload);
        assert_eq!(back.project.name, "Acme");
        assert_eq!(back.tables[0].name, "users");
        assert!(back.tables[0].columns[0].pk);
    }

    #[test]
    fn fragment_url_has_expected_shape_and_trims_slash() {
        let m = sample_model();
        let url = fragment_url("https://dbd.example/", &m).unwrap();
        assert!(url.starts_with("https://dbd.example/diagram#1."), "got: {url}");
        assert!(!url.contains("//diagram"), "trailing slash not trimmed: {url}");
    }
}
