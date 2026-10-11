//! Stable id generation.
//!
//! Mirrors `trust_runtime.py::_stable_id`: SHA-1 over `|`-joined stringified
//! parts (skipping nulls), hex-encoded, truncated to `length`.

use sha1::{Digest, Sha1};

/// Generate a stable hex id from parts.
///
/// `None` parts are skipped, matching Python's `if part is not None`.
/// The default length used by the trust runtime is 16 (runs) and 18 (steps).
pub fn stable_id(parts: &[Option<&str>], length: usize) -> String {
    let joined = parts
        .iter()
        .filter_map(|p| *p)
        .collect::<Vec<_>>()
        .join("|");
    let mut hasher = Sha1::new();
    hasher.update(joined.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    hex[..length.min(hex.len())].to_string()
}

/// Convenience: stable id from a slice of `&str` (no nulls).
pub fn new_id(parts: &[&str], length: usize) -> String {
    stable_id(&parts.iter().map(|s| Some(*s)).collect::<Vec<_>>(), length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_stable_id() {
        // Verified against CPython 3.12:
        // hashlib.sha1("a|b".encode()).hexdigest()[:16] == "9abe6de24a871364"
        assert_eq!(stable_id(&[Some("a"), Some("b")], 16), "9abe6de24a871364");
        // Deterministic.
        assert_eq!(
            stable_id(&[Some("a"), Some("b")], 16),
            stable_id(&[Some("a"), Some("b")], 16)
        );
    }

    #[test]
    fn skips_none_parts() {
        assert_eq!(
            stable_id(&[Some("a"), None, Some("b")], 16),
            stable_id(&[Some("a"), Some("b")], 16)
        );
    }

    #[test]
    fn different_inputs_different_ids() {
        assert_ne!(new_id(&["x"], 16), new_id(&["y"], 16));
    }

    #[test]
    fn length_respected() {
        assert_eq!(new_id(&["a"], 18).len(), 18);
        assert_eq!(new_id(&["a"], 8).len(), 8);
    }
}
