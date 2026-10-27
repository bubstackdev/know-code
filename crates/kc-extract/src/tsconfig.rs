//! TypeScript project configuration (`tsconfig.json`, `tsconfig.*.json`, `jsconfig.json`): the settings module
//! resolution reads, `compilerOptions.baseUrl` and `paths`, and the configs it `extends` and `references`.
//!
//! The files are JSON with comments and trailing commas, which the compiler accepts; both are removed before parsing.

use crate::{FileFacts, TsConfig};

pub const VERSION: u32 = 1;

pub fn extract(source: &str) -> FileFacts {
    let mut facts = FileFacts::default();
    let Ok(serde_json::Value::Object(doc)) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(source)) else {
        facts.parse_errors = 1;
        return facts;
    };
    let options = doc.get("compilerOptions").and_then(|o| o.as_object());
    let base_url = options.and_then(|o| o.get("baseUrl")).and_then(|b| b.as_str()).map(str::to_string);
    let paths = options.and_then(|o| o.get("paths")).and_then(|p| p.as_object()).map(|m| {
        m.iter()
            .map(|(pattern, subs)| {
                let subs = subs.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default();
                (pattern.clone(), subs)
            })
            .collect()
    });
    let extends = match doc.get("extends") {
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect(),
        _ => Vec::new(),
    };
    let references = doc
        .get("references")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|r| r.get("path").and_then(|p| p.as_str()).map(str::to_string)).collect())
        .unwrap_or_default();
    facts.ts_config = Some(TsConfig { base_url, paths, extends, references });
    facts
}

/// `source` without `//` and `/* */` comments and without commas before a closing `}` or `]`, outside strings.
fn strip_jsonc(source: &str) -> String {
    let b = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    // Where in `out` the last comma went, while only whitespace and comments have followed it.
    let mut comma: Option<usize> = None;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(b.len());
                out.push_str(&source[start..i]);
                comma = None;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            b',' => {
                comma = Some(out.len());
                out.push(',');
                i += 1;
            }
            c @ (b'}' | b']') => {
                if let Some(at) = comma.take() {
                    out.replace_range(at..at + 1, " ");
                }
                out.push(c as char);
                i += 1;
            }
            c if c.is_ascii_whitespace() => {
                out.push(c as char);
                i += 1;
            }
            _ => {
                // Copy up to the next character that matters, whole UTF-8 sequences included.
                let start = i;
                while i < b.len() && !matches!(b[i], b'"' | b'/' | b',' | b'}' | b']') && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i == start {
                    i += 1;
                }
                out.push_str(&source[start..i]);
                comma = None;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_paths_base_url_extends_and_references_through_comments() {
        let f = extract(
            r#"/* generated */
{
  // the base
  "extends": "./tsconfig.base.json",
  "compilerOptions": {
    "baseUrl": "./src", // trailing comment
    "paths": {
      "@app/*": ["app/*", "fallback/*",],
      "@x": ["lib/x/index.ts"],
    },
    "outDir": "http://not-a-comment",
  },
  "references": [{ "path": "./tsconfig.app.json" }],
}"#,
        );
        let c = f.ts_config.unwrap();
        assert_eq!(c.base_url.as_deref(), Some("./src"));
        let paths = c.paths.unwrap();
        assert!(paths.contains(&("@app/*".to_string(), vec!["app/*".to_string(), "fallback/*".to_string()])));
        assert!(paths.contains(&("@x".to_string(), vec!["lib/x/index.ts".to_string()])));
        assert_eq!(c.extends, ["./tsconfig.base.json"]);
        assert_eq!(c.references, ["./tsconfig.app.json"]);
        assert_eq!(f.parse_errors, 0);
    }

    #[test]
    fn a_config_without_paths_says_so() {
        let c = extract(r#"{ "extends": ["a.json", "b.json"], "compilerOptions": { "strict": true } }"#).ts_config.unwrap();
        assert_eq!((c.base_url, c.paths, c.extends), (None, None, vec!["a.json".to_string(), "b.json".to_string()]));
        assert_eq!(extract("not json").parse_errors, 1);
    }
}
