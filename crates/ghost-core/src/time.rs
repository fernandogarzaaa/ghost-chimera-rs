//! Time helpers.
//!
//! `now()` mirrors `trust_runtime.py::_now()` (`time.time()` — f64 seconds
//! since the Unix epoch).

use std::time::{SystemTime, UNIX_EPOCH};

/// Current time as f64 seconds since the Unix epoch.
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_sane() {
        let t = now();
        // After 2026-01-01 (1767225600).
        assert!(t > 1_767_225_600.0);
        assert!(now() >= t);
    }
}
