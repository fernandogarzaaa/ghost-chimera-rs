//! Secret redaction — the single source of truth.
//!
//! Mirrors `ghostchimera/redaction.py` exactly:
//! - `BASE_MARKERS` / `SECRET_MARKERS`: token, secret, api_key, apikey,
//!   password, credential, authorization, bearer
//! - `SECRET_PATTERNS`: sk-..., ghp_/github_pat_/xoxb_/xoxp_..., Bearer ...
//! - Mapping keys containing a marker are replaced with `[redacted]`
//!   (falsy values become `""`); free-text strings are pattern-scrubbed.
//!
//! Widened beyond Python (GAP [security/M5]):
//! - a bare `key` *word* in a mapping key (`key`, `private_key`,
//!   `accessKey`, `x-api-key`) is a marker, except for well-known
//!   non-secret identifiers such as `idempotency_key` or `public_key`
//!   (see [`NON_SECRET_KEY_NAMES`]); substrings like `monkey` or
//!   `keyboard` are not words and never match;
//! - more provider token shapes (Google `AIza`, xAI, Groq, Anthropic,
//!   NVIDIA `nvapi-`, Hugging Face, Slack `xox?-`, every GitHub prefix,
//!   GitLab, Stripe, AWS access key ids, JWTs, PEM private keys);
//! - `name=value` / `name: value` assignments of secret-named fields in
//!   free text (`password=hunter2`, `api_key: abc`).

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

/// Mapping keys whose `key` word does NOT denote a secret. Compared
/// against the normalized key (lowercase, `-`/camelCase folded to `_`).
/// Journals rely on this: `idempotency_key` is part of every trust step
/// and must survive redaction unchanged.
pub const NON_SECRET_KEY_NAMES: &[&str] = &[
    "idempotency_key",
    "public_key",
    "primary_key",
    "foreign_key",
    "sort_key",
    "partition_key",
    "cache_key",
    "dedup_key",
    "dedupe_key",
    "lookup_key",
    "group_key",
    "routing_key",
    "key_id",
    "key_name",
    "keys",
];

/// Replacement for redacted values.
pub const REDACTED: &str = "[redacted]";

fn secret_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            // PEM private key blocks first (multi-line).
            Regex::new(
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
            )
            .unwrap(),
            Regex::new(r"sk-[A-Za-z0-9_\-]{12,}").unwrap(),
            Regex::new(r"(?:ghp|github_pat|xoxb|xoxp)_[A-Za-z0-9_\-]{12,}").unwrap(),
            Regex::new(r"(?i)Bearer\s+[A-Za-z0-9_\-\.]{12,}").unwrap(),
            // M5: additional provider key shapes missed by the Python patterns.
            Regex::new(r"AIza[A-Za-z0-9_\-]{35}").unwrap(),
            Regex::new(r"xai-[A-Za-z0-9_\-]{12,}").unwrap(),
            Regex::new(r"gsk_[A-Za-z0-9_\-]{12,}").unwrap(),
            Regex::new(r"sk-ant-[A-Za-z0-9_\-]{12,}").unwrap(),
            // GitHub: every documented token prefix (ghp/gho/ghu/ghs/ghr).
            Regex::new(r"\bgh[opusr]_[A-Za-z0-9]{20,}").unwrap(),
            // Slack tokens really use a hyphen (xoxb-...); the Python
            // pattern only matched an underscore.
            Regex::new(r"\bxox[abprse]-[A-Za-z0-9\-]{10,}").unwrap(),
            Regex::new(r"\bglpat-[A-Za-z0-9_\-]{20,}").unwrap(),
            Regex::new(r"\bnvapi-[A-Za-z0-9_\-]{20,}").unwrap(),
            Regex::new(r"\bhf_[A-Za-z0-9]{30,}").unwrap(),
            Regex::new(r"\b[sr]k_(?:live|test)_[A-Za-z0-9]{16,}").unwrap(),
            Regex::new(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b").unwrap(),
            // JWTs (header.payload.signature, header always starts eyJ).
            Regex::new(r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}").unwrap(),
        ]
    })
}

/// `name=value` / `name: value` assignments of secret-named fields in
/// free text. Group 1 is the name, group 2 the separator; the value is
/// replaced. Bare `key` is deliberately excluded here (prose such as
/// "Key: takeaways" is common); it is covered in mapping keys instead.
fn assignment_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r#"(?i)\b((?:api|access|secret|private|client)[_\-]?key|apikey|secret|token|password|passwd|pwd)(\s*[:=]\s*)["']?[^\s"',;&]+"#,
        )
        .unwrap()
    })
}

/// Replace secret-like substrings in free text.
pub fn redact_text(text: &str) -> String {
    let mut out = text.to_string();
    for pattern in secret_patterns() {
        out = pattern.replace_all(&out, REDACTED).into_owned();
    }
    assignment_pattern()
        .replace_all(&out, format!("${{1}}${{2}}{REDACTED}").as_str())
        .into_owned()
}

/// Normalize a mapping key: camelCase boundaries and `-`/space/`.` become
/// `_`, everything lowercased (`accessKey` -> `access_key`).
fn normalize_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    let mut prev_lower_or_digit = false;
    for ch in key.chars() {
        if ch.is_ascii_uppercase() && prev_lower_or_digit {
            out.push('_');
        }
        prev_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        match ch {
            '-' | ' ' | '.' => out.push('_'),
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// True when a mapping key names a secret.
///
/// Python parity: any [`SECRET_MARKERS`] substring. Widened (M5): a bare
/// `key` word anywhere in the normalized key, unless the whole key is a
/// known non-secret identifier ([`NON_SECRET_KEY_NAMES`]).
pub fn is_secret_key(key: &str) -> bool {
    let lowered = key.to_lowercase();
    if SECRET_MARKERS.iter().any(|m| lowered.contains(m)) {
        return true;
    }
    let normalized = normalize_key(key);
    if NON_SECRET_KEY_NAMES.contains(&normalized.as_str()) {
        return false;
    }
    normalized.split('_').any(|word| word == "key")
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
                if is_secret_key(key) {
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

    #[test]
    fn bare_key_word_is_a_marker_m5() {
        let v = json!({
            "key": "plainvalue",
            "private_key": "abc",
            "accessKey": "abc",
            "x-api-key": "abc",
            "Key": "abc",
        });
        let r = redact_value(&v);
        for k in ["key", "private_key", "accessKey", "x-api-key", "Key"] {
            assert_eq!(r[k], json!("[redacted]"), "{k} must be redacted");
        }
    }

    #[test]
    fn non_secret_key_names_survive_m5() {
        // Trust journals carry idempotency_key on every step.
        let v = json!({
            "idempotency_key": "run:created",
            "public_key": "ssh-ed25519 AAAA",
            "monkey": "banana",
            "keyboard": "qwerty",
            "keys": 3,
            "key_id": "k1",
            "turkey": "bird",
        });
        assert_eq!(redact_value(&v), v);
    }

    #[test]
    fn is_secret_key_classifies() {
        assert!(is_secret_key("key"));
        assert!(is_secret_key("signing_key"));
        assert!(is_secret_key("sessionKey"));
        assert!(is_secret_key("API-Key"));
        assert!(is_secret_key("auth_token"));
        assert!(!is_secret_key("idempotencyKey"));
        assert!(!is_secret_key("donkey"));
        assert!(!is_secret_key("keystroke"));
        assert!(!is_secret_key("name"));
    }

    #[test]
    fn normalize_key_folds_case_and_separators() {
        assert_eq!(normalize_key("accessKey"), "access_key");
        assert_eq!(normalize_key("X-Api-Key"), "x_api_key");
        assert_eq!(normalize_key("a.b c"), "a_b_c");
        assert_eq!(normalize_key("v2Key"), "v2_key");
        assert_eq!(normalize_key("ABC"), "abc");
    }

    #[test]
    fn more_token_shapes_redacted_m5() {
        let cases = [
            "gho_abcdefghijklmnopqrstuvwx",
            "ghs_abcdefghijklmnopqrstuvwx",
            "xoxb-1234567890-abcdefghij",
            "glpat-abcdefghijklmnopqrst",
            "nvapi-abcdefghijklmnopqrstuvwxyz",
            "hf_abcdefghijklmnopqrstuvwxyzABCDEF",
            "sk_live_abcdefghijklmnop1234",
            "rk_test_abcdefghijklmnop1234",
            "AKIAABCDEFGHIJKLMNOP",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NSJ9.abcdefghijklmnop",
        ];
        for secret in cases {
            assert_eq!(
                redact_text(&format!("x {secret} y")),
                "x [redacted] y",
                "{secret} must be redacted"
            );
        }
    }

    #[test]
    fn pem_private_key_redacted() {
        let text = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIBAAJBAK\n-----END RSA PRIVATE KEY-----\nafter";
        assert_eq!(redact_text(text), "before\n[redacted]\nafter");
    }

    #[test]
    fn secret_assignments_in_free_text_redacted() {
        assert_eq!(
            redact_text("curl -u admin password=hunter2 now"),
            "curl -u admin password=[redacted] now"
        );
        assert_eq!(redact_text("api_key: abc123"), "api_key: [redacted]");
        assert_eq!(redact_text("TOKEN=\"xyz\""), "TOKEN=[redacted]\"");
        assert_eq!(
            redact_text("a=1&secret=s3&b=2"),
            "a=1&secret=[redacted]&b=2"
        );
        // Idempotent: redacting twice changes nothing more.
        let once = redact_text("pwd=abc");
        assert_eq!(redact_text(&once), once);
        // Prose with a bare "Key:" is left alone.
        assert_eq!(redact_text("Key: takeaways"), "Key: takeaways");
    }
}
