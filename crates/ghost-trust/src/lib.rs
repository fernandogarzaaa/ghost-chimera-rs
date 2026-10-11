//! Trust Runtime: hash-chained JSONL run journals.
//!
//! Mirrors `ghostchimera/trust_runtime.py`. The journal format is
//! field-for-field compatible with the Python implementation:
//! - Each journal line is JSON with sorted keys (Python `sort_keys=True`).
//! - `record_hash = sha256(canonical_compact_json(record_without_record_hash))`.
//! - `previous_hash` chains records; the first record uses `""`.
//! - `input_ref` / `output_ref` are `_safe_snippet` payloads:
//!   `{sha256, preview (280 chars), truncated}` computed from the
//!   Python-default (spaced) JSON rendering of the redacted value.
//!
//! Rust-written journals verify under Python's `verify_run_integrity`
//! and vice versa (see `tests/py_journal_compat.rs`).

pub mod store;
pub mod types;

pub use store::{RecordStepArgs, RunView, TrustRuntimeStore, VerificationReport};
pub use types::{
    DurableRun, ReplayComparison, ReplayExecution, ReplayReport, RunStepRecord, SafeSnippet,
};
