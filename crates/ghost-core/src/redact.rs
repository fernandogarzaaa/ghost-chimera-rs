//! Secret redaction — the single source of truth.
//!
//! Mirrors `ghostchimera/redaction.py` exactly:
//! - `BASE_MARKERS` / `SECRET_MARKERS`: token, secret, api_key, apikey,
//!   password, credential, authorization, bearer
//! - `SECRET_PATTERNS`: sk-..., ghp_/github_pat_/xoxb_/xoxp_..., Bearer ...
//! - Mapping keys containing a marker are replaced with `[redacted]`
//!   (falsy values become `""`); free-text strings are pattern-scrubbed.

use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

/// Key markers that trigger redaction (case-insensitive substring match).
pub const SECRET_MARKERS: &[&str] = &[
    "token",
    "secret",
    "api_key",
    "apikey",
    "password",
    "credential",
    "authorization",
    "bearer",
];

/// Replacement for redacted values.
pub const REDACTED: &str = "[redacted]";

fn secret_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"sk-[A-Za-z0-9_\-]{12,}").unwrap(),
            Regex::new(r"(?:ghp|github_pat|xoxb|xoxp)_[A-Za-z0-9_\-]{12,}").unwrap(),
            Regex::new(r"(?i)Bearer\s+[A-Za-z0-9_\-\.]{12,}").unwrap(),
            // M5: additional provider key shapes missed by the Python patterns.
            Regex::new(r"AIza[A-Za-z0-9_\-]{35}").unwrap(),
            Regex::new(r"xai-[A-Za-z0-9_\-]{12,}").unwrap(),
            Regex::new(r"gsk_[A-Za-z0-9_\-]{12,}").unwrap(),
            Regex::new(r"sk-ant-[A-Za-z0-9_\-]{12,}").unwrap(),
        ]
    })
}

/// Replace secret-like substrings in free text.
pub fn redact_text(text: &str) -> String {
    let mut out = text.to_string();
    for pattern in secret_patterns() {
        out = pattern.replace_all(&out, REDACTED).into_owned();
    }
    out
}

/// Redact a JSON-like value without mutating it.
///
/// Mapping keys containing a marker are replaced (`[redacted]`, or `""`
/// when the value is falsy). All other values are recursed into; strings
/// are pattern-scrubbed.
pub fn redact_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, item) in map {
                let lowered = key.to_lowercase();
                if SECRET_MARKERS.iter().any(|m| lowered.contains(m)) {
                    let replacement = if is_falsy(item) { "" } else { REDACTED };
                    out.insert(key.clone(), Value::String(replacement.to_string()));
                } else {
                    out.insert(key.clone(), redact_value(item));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_value).collect()),
        Value::String(s) => Value::String(redact_text(s)),
        other => other.clone(),
    }
}

/// Python falsiness for the marker-key rule: null, false, "", 0, [], {}.
fn is_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64().map(|f| f == 0.0).unwrap_or(false),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn marker_keys_redacted() {
        let v = json!({"api_key": "sk-abcdefghijklmnop", "name": "ghost"});
        let r = redact_value(&v);
        assert_eq!(r["api_key"], json!("[redacted]"));
        assert_eq!(r["name"], json!("ghost"));
    }

    #[test]
    fn falsy_marker_value_becomes_empty_string() {
        let v = json!({"password": "", "token": null});
        let r = redact_value(&v);
        assert_eq!(r["password"], json!(""));
        assert_eq!(r["token"], json!(""));
    }

    #[test]
    fn free_text_patterns_scrubbed() {
        let v = json!({"note": "key is sk-abcdefghijklmnop done"});
        let r = redact_value(&v);
        assert_eq!(r["note"], json!("key is [redacted] done"));
    }

    #[test]
    fn nested_structures_recursed() {
        let v = json!({"a": {"bearer": "xyz", "list": [{"secret": "s"}]}});
        let r = redact_value(&v);
        assert_eq!(r["a"]["bearer"], json!("[redacted]"));
        assert_eq!(r["a"]["list"][0]["secret"], json!("[redacted]"));
    }

    #[test]
    fn bearer_pattern_case_insensitive() {
        assert_eq!(redact_text("bearer ABCDEF1234567890"), "[redacted]");
    }

    #[test]
    fn non_secret_text_untouched() {
        assert_eq!(redact_text("hello world"), "hello world");
    }

    #[test]
    fn provider_key_shapes_redacted_m5() {
        // M5: patterns the Python redaction missed.
        assert_eq!(
            redact_text("key=AIzaSyB12345678901234567890123456789012"),
            "key=[redacted]"
        );
        assert_eq!(
            redact_text("token xai-abc123def456ghi789"),
            "token [redacted]"
        );
        assert_eq!(redact_text("gsk_1234567890abcdef1234567890"), "[redacted]");
        assert_eq!(redact_text("sk-ant-api03-1234567890abcdef"), "[redacted]");
    }
}
