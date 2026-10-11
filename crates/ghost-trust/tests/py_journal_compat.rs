//! Golden-file compatibility with Python-written trust journals.
//!
//! `fixtures/py_journal.jsonl` is a real journal written by the Python
//! `TrustRuntimeStore` (from `~/.ghostchimera/trust_runtime/runs/`).
//! These tests prove the Rust implementation reads Python journals and
//! verifies their hash chains — the core of the zero-migration promise
//! in SPEC.md Section 4.

use ghost_trust::TrustRuntimeStore;
use std::path::PathBuf;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Copy the Python journal into a fresh store layout and verify it.
#[test]
fn rust_verifies_python_written_journal() {
    let tmp = tempfile::tempdir().unwrap();
    let store = TrustRuntimeStore::new(tmp.path()).unwrap();

    // The fixture run id is the file stem.
    let run_id = "007bca1a72c30385c33e";
    let dest = tmp
        .path()
        .join("trust_runtime")
        .join("runs")
        .join(format!("{run_id}.jsonl"));
    std::fs::copy(fixture_dir().join("py_journal.jsonl"), &dest).unwrap();

    let report = store.verify_run_integrity(run_id).unwrap();
    assert!(report.ok, "verification failed: {}", report.error);
    assert!(report.verified);
    assert!(report.step_count > 0);
    assert!(!report.latest_hash.is_empty());
}

/// Rust can parse every step of the Python journal into typed records.
#[test]
fn rust_parses_python_journal_steps() {
    let tmp = tempfile::tempdir().unwrap();
    let store = TrustRuntimeStore::new(tmp.path()).unwrap();

    let run_id = "007bca1a72c30385c33e";
    let dest = tmp
        .path()
        .join("trust_runtime")
        .join("runs")
        .join(format!("{run_id}.jsonl"));
    std::fs::copy(fixture_dir().join("py_journal.jsonl"), &dest).unwrap();

    // Register the run in the index so get_run finds it.
    let index_path = tmp.path().join("trust_runtime").join("run_index.json");
    std::fs::write(
        &index_path,
        format!(
            r#"{{"{run_id}": {{"run_id": "{run_id}", "objective": "summarize runtime status", "source": "console", "status": "ok", "created_at": 1.0, "updated_at": 1.0, "agent_name": "ghost-chimera", "ghost_path": "", "model_provider": "", "model_name": "", "step_count": 1, "pending_approval_count": 0}}}}"#
        ),
    )
    .unwrap();

    let view = store.get_run(run_id).unwrap().expect("run should exist");
    assert_eq!(view.run.run_id, run_id);
    assert!(!view.steps.is_empty());
    // First step is run_created with an empty previous_hash.
    assert_eq!(view.steps[0].step_type, "run_created");
    assert_eq!(view.steps[0].previous_hash, "");
    assert!(!view.steps[0].record_hash.is_empty());
}

/// A journal written by Rust verifies cleanly (round-trip sanity).
#[test]
fn rust_written_journal_verifies() {
    let tmp = tempfile::tempdir().unwrap();
    let store = TrustRuntimeStore::new(tmp.path()).unwrap();
    let run = store
        .create_run("round trip objective", "test", "ghost-chimera", None)
        .unwrap();
    store
        .record_step(
            &run.run_id,
            ghost_trust::RecordStepArgs::simple("task_executed", "ok"),
        )
        .unwrap();

    let report = store.verify_run_integrity(&run.run_id).unwrap();
    assert!(report.ok && report.verified);
    assert_eq!(report.step_count, 2);
}
