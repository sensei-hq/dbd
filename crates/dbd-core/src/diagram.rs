//! Render a self-contained HTML schema explorer: the committed viewer bundle
//! plus the model JSON, inlined into a template (no network at runtime).
use crate::schema_model::SchemaModel;

const TEMPLATE: &str = include_str!("../assets/diagram.html");
const VIEWER: &str = include_str!("../assets/diagram_viewer.js");

/// HTML-escape the few characters that matter inside a `<title>` element.
fn escape_title(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Render a self-contained HTML page embedding `model` + the viewer bundle.
pub fn render_html(model: &SchemaModel) -> Result<String, serde_json::Error> {
    let json = serde_json::to_string(model)?;
    // The model is embedded in <script type="application/json">. The only way to
    // break out of that element is the byte sequence "</" — escaping it as "<\/"
    // keeps the JSON byte-identical when parsed (JSON treats \/ as /).
    let json = json.replace("</", "<\\/");
    // The bundle is embedded in a <script>. If it contains the literal
    // "</script" (only possible inside a JS string/regex literal in minified
    // code), escaping to "<\/script" is equivalent there and prevents the tag
    // from being closed early. Case-insensitive per the HTML spec.
    let viewer = replace_close_script(VIEWER);
    Ok(TEMPLATE
        .replace("__DBD_TITLE__", &escape_title(&model.project.name))
        .replace("__DBD_MODEL__", &json)
        .replace("__DBD_VIEWER__", &viewer))
}

/// Replace any `</script` (any case) with `<\/script` so an embedded script
/// payload can't terminate the surrounding `<script>` element. The following
/// character (e.g. the `>` in `</script>` or whitespace in `</script >`) is
/// preserved, and casing of the matched run is left untouched.
fn replace_close_script(s: &str) -> String {
    const NEEDLE: &str = "</script";
    let lower = s.to_ascii_lowercase();
    if !lower.contains(NEEDLE) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    let mut i = 0;
    while i < s.len() {
        if lower[i..].starts_with(NEEDLE) {
            // Preserve the original casing of the matched run; only inject the
            // escaping backslash after the `<`.
            out.push('<');
            out.push('\\');
            out.push_str(&s[i + 1..i + NEEDLE.len()]);
            i += NEEDLE.len();
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_model::{Column, ProjectInfo, SchemaInfo, SchemaModel, TableNode};

    fn sample_model() -> SchemaModel {
        SchemaModel {
            project: ProjectInfo {
                name: "Acme <Shop> & Co".to_string(),
                db: "postgres".to_string(),
                note: None,
            },
            schemas: vec![SchemaInfo { name: "public".to_string(), tables: 1, enums: 0 }],
            tables: vec![TableNode {
                schema: "public".to_string(),
                name: "users".to_string(),
                kind: "table".to_string(),
                note: None,
                note_md: None,
                columns: vec![Column {
                    name: "id".to_string(),
                    ty: "uuid".to_string(),
                    pk: true,
                    nn: true,
                    en: false,
                    def: None,
                    note: None,
                }],
            }],
            refs: vec![],
        }
    }

    /// Extract the JSON text embedded between the model `<script>` open/close.
    fn model_payload(html: &str) -> &str {
        let open = "id=\"dbd-model\">";
        let start = html.find(open).expect("model script open tag") + open.len();
        let end = start + html[start..].find("</script>").expect("model script close tag");
        &html[start..end]
    }

    #[test]
    fn render_html_is_well_formed_and_self_contained() {
        let m = sample_model();
        let html = render_html(&m).unwrap();

        // Core wiring is present.
        assert!(html.contains("id=\"dbd-model\""));
        assert!(html.contains("mountViewer"));

        // Title carries the escaped project name.
        assert!(html.contains("Acme &lt;Shop&gt; &amp; Co — schema"));

        // No network references — fully offline.
        assert!(!html.contains("src=\"http"));
        assert!(!html.contains("href=\"http"));
        assert!(!html.contains("@import url(http"));
    }

    #[test]
    fn model_payload_has_no_unescaped_close_sequences() {
        let m = sample_model();
        let html = render_html(&m).unwrap();
        let payload = model_payload(&html);
        assert!(!payload.contains("</script"));
        assert!(!payload.contains("</"));
    }

    #[test]
    fn model_payload_round_trips_through_serde() {
        let m = sample_model();
        let html = render_html(&m).unwrap();
        let payload = model_payload(&html);
        // Reverse the `<\/` → `</` escape applied during embedding.
        let unescaped = payload.replace("<\\/", "</");
        let v: serde_json::Value = serde_json::from_str(&unescaped).unwrap();
        assert_eq!(v["project"]["name"], "Acme <Shop> & Co");
        assert_eq!(v["tables"][0]["name"], "users");
        assert_eq!(v["tables"][0]["schema"], "public");
    }

    #[test]
    fn replace_close_script_escapes_all_cases_and_preserves_following_chars() {
        let out = replace_close_script("a</script>b</SCRIPT >c");
        assert_eq!(out, "a<\\/script>b<\\/SCRIPT >c");
    }

    #[test]
    fn replace_close_script_leaves_clean_input_unchanged() {
        let clean = "function f() { return '<div>ok</div>'; }";
        assert_eq!(replace_close_script(clean), clean);
    }
}
