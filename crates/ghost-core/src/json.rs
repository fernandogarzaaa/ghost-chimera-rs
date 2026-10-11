//! JSON serialization compatible with CPython's `json.dumps`.
//!
//! The trust-journal hash chain is computed over canonical JSON. For the
//! Rust implementation to verify Python-written journals (and vice versa),
//! serialization must be byte-identical to:
//!
//! ```python
//! json.dumps(value, sort_keys=True, separators=(",", ":"), default=str)
//! ```
//!
//! Two flavors are provided:
//! - [`canonical_json`]: compact separators (`,` / `:`), sorted keys —
//!   used for `record_hash` computation and verification.
//! - [`python_json`]: Python's *default* separators (`, ` / `: `), sorted
//!   keys — used for `_safe_snippet` previews, whose `sha256`/`preview`
//!   fields are computed from that exact rendering.
//!
//! Both use `ensure_ascii`-style escaping (non-ASCII becomes `\uXXXX`),
//! matching CPython's default.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Canonical JSON: sorted keys, compact separators, ASCII-escaped strings.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out, false);
    out
}

/// Python-default JSON: sorted keys, `", "` / `": "` separators,
/// ASCII-escaped strings. Matches `json.dumps(v, sort_keys=True)`.
pub fn python_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out, true);
    out
}

/// SHA-256 hex of [`canonical_json`]. Mirrors `_hash_payload`.
pub fn hash_payload(value: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical_json(value).as_bytes());
    format!("{:x}", hasher.finalize())
}

fn write_canonical(value: &Value, out: &mut String, spaced: bool) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_escaped_str(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if spaced {
                        out.push(' ');
                    }
                }
                write_canonical(item, out, spaced);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            // serde_json::Map is a BTreeMap by default: keys already sorted.
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if spaced {
                        out.push(' ');
                    }
                }
                write_escaped_str(k, out);
                out.push(':');
                if spaced {
                    out.push(' ');
                }
                write_canonical(v, out, spaced);
            }
            out.push('}');
        }
    }
}

/// Escape a string exactly like CPython `json.dumps(ensure_ascii=True)`.
fn write_escaped_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c if (c as u32) < 0x7f => out.push(c),
            c => {
                // ensure_ascii: non-ASCII becomes \uXXXX (surrogate pairs above BMP).
                let n = c as u32;
                if n > 0xffff {
                    let v = n - 0x10000;
                    let hi = 0xd800 + (v >> 10);
                    let lo = 0xdc00 + (v & 0x3ff);
                    out.push_str(&format!("\\u{:04x}\\u{:04x}", hi, lo));
                } else {
                    out.push_str(&format!("\\u{:04x}", n));
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_matches_python_separators() {
        // Python: json.dumps({"b": 1, "a": [1, 2]}, sort_keys=True, separators=(",", ":"))
        // == '{"a":[1,2],"b":1}'
        let v = json!({"b": 1, "a": [1, 2]});
        assert_eq!(canonical_json(&v), r#"{"a":[1,2],"b":1}"#);
    }

    #[test]
    fn python_json_uses_spaced_separators() {
        // Python: json.dumps({"b": 1, "a": [1, 2]}, sort_keys=True)
        // == '{"a": [1, 2], "b": 1}'
        let v = json!({"b": 1, "a": [1, 2]});
        assert_eq!(python_json(&v), r#"{"a": [1, 2], "b": 1}"#);
    }

    #[test]
    fn ensure_ascii_escaping_matches_python() {
        // Python: json.dumps({"k": "héllo→"}, sort_keys=True)
        // == '{"k": "h\\u00e9llo\\u2192"}'
        let v = json!({"k": "héllo→"});
        assert_eq!(python_json(&v), r#"{"k": "h\u00e9llo\u2192"}"#);
        assert_eq!(canonical_json(&v), r#"{"k":"h\u00e9llo\u2192"}"#);
    }

    #[test]
    fn control_chars_escaped() {
        let v = json!({"k": "a\nb\tc\x01d"});
        assert_eq!(python_json(&v), "{\"k\": \"a\\nb\\tc\\u0001d\"}");
    }

    #[test]
    fn astral_plane_uses_surrogate_pairs() {
        // Python: json.dumps("😀") == '"\\ud83d\\ude00"'
        let v = json!("😀");
        assert_eq!(canonical_json(&v), r#""\ud83d\ude00""#);
    }

    #[test]
    fn hash_payload_matches_python_sha256() {
        // Verified against CPython 3.12:
        // hashlib.sha256(json.dumps({"a": 1}, sort_keys=True,
        //     separators=(",", ":")).encode()).hexdigest()
        let v = json!({"a": 1});
        assert_eq!(
            hash_payload(&v),
            "015abd7f5cc57a2dd94b7590f04ad8084273905ee33ec5cebeae62276a97f862"
        );
    }

    #[test]
    fn floats_format_like_python() {
        assert_eq!(canonical_json(&json!(1.0)), "1.0");
        assert_eq!(canonical_json(&json!(0.1)), "0.1");
    }
}
