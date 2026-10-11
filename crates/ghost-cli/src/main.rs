//! `ghost` CLI: ask, trust replay, standing-orders, console.
//!
//! All command logic lives in `cmd` so it is unit-testable: each command
//! handler returns `(exit_code, serde_json::Value)` and the binary only
//! prints and exits.
//!
//! NOTE: `main` is intentionally NOT `#[tokio::main]`. The `ask`/`trust`/
//! `standing-orders` commands construct model providers via
//! `ProviderRegistry::from_env()`, which eagerly builds reqwest *blocking*
//! clients — those panic when built inside a tokio async context. Only the
//! `console` subcommand needs a runtime, and it builds one explicitly.

mod cmd;

use std::path::PathBuf;

use ghost_core::GhostConfig;

/// Install the crash-reporting panic hook before doing anything else.
///
/// On panic, a TOML crash report (built with `human-panic`, including a
/// backtrace) is written to `<state_dir>/crashes/report-<ts>-<pid>.toml` and
/// a friendly message is printed. Reports stay on disk — no telemetry is
/// ever sent anywhere (SPEC section 7: local-first product).
///
/// The crashes directory is resolved once at install time so the hook never
/// depends on ambient environment state while unwinding.
fn install_crash_hook() {
    let crashes_dir = GhostConfig::from_env().crashes_dir();
    install_crash_hook_with_dir(crashes_dir);
}

/// Install the hook with an explicit crashes directory (used by tests).
fn install_crash_hook_with_dir(crashes_dir: PathBuf) {
    let meta = human_panic::Metadata::new("ghost", env!("CARGO_PKG_VERSION"))
        .authors("Fernando Garza <fernandogarzaaa@gmail.com>")
        .homepage("https://github.com/fernandogarzaaa/GHOST-Chimera");
    std::panic::set_hook(Box::new(move |info| {
        let report = human_panic::report::Report::with_panic(&meta, info);
        let path = write_crash_report_to(&report, &crashes_dir);
        // Never panic inside the panic hook: fall back to stderr text.
        let _ = human_panic::print_msg(path.as_deref(), &meta);
    }));
}

/// Write the crash report TOML into the given crashes directory.
/// Returns the report path, or `None` when the disk write failed (the
/// caller then prints the report text to stderr instead).
fn write_crash_report_to(
    report: &human_panic::report::Report,
    crashes_dir: &PathBuf,
) -> Option<PathBuf> {
    let toml = report.serialize()?;
    std::fs::create_dir_all(crashes_dir).ok()?;
    // Timestamp + pid keeps filenames unique without a uuid dependency.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = crashes_dir.join(format!("report-{now_ms}-{}.toml", std::process::id()));
    std::fs::write(&path, toml).ok()?;
    Some(path)
}

fn main() {
    install_crash_hook();
    // First-run migration from the Python layout (`~/.ghostchimera`):
    // copy, never move; a no-op when there is nothing to migrate.
    let config = GhostConfig::from_env();
    if let Err(e) = config.migrate_legacy_state() {
        eprintln!("warning: state migration failed: {e}");
    }
    std::process::exit(cmd::run());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_hook_writes_crash_report_to_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        let crashes_dir = dir.path().join("crashes");

        // Install our hook, saving the test harness's hook to restore after.
        let previous = std::panic::take_hook();
        install_crash_hook_with_dir(crashes_dir.clone());

        // A real panic through the real hook. catch_unwind lets the hook run
        // (it writes the report) and then contains the unwind.
        let _ = std::panic::catch_unwind(|| {
            panic!("simulated crash for crash-report test");
        });

        std::panic::set_hook(previous);

        // The report file must exist on disk in the crashes dir.
        assert!(crashes_dir.is_dir(), "crashes dir was not created");
        let reports: Vec<PathBuf> = std::fs::read_dir(&crashes_dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
            .collect();
        assert_eq!(reports.len(), 1, "expected exactly one crash report");
        let content = std::fs::read_to_string(&reports[0]).unwrap();
        assert!(content.contains("simulated crash for crash-report test"));
        assert!(content.contains("ghost"), "report should name the crate");
    }

    #[test]
    fn write_crash_report_fails_closed_without_panicking() {
        // An unwritable crashes dir must yield None, never a panic inside
        // the panic hook (which would abort the process).
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "not a dir").unwrap();

        let report = human_panic::report::Report::new(
            "ghost",
            env!("CARGO_PKG_VERSION"),
            human_panic::report::Method::Panic,
            "test".to_string(),
            "test cause".to_string(),
        );
        assert!(write_crash_report_to(&report, &blocker).is_none());
    }
}
