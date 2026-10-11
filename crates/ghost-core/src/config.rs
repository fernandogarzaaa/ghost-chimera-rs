//! Runtime configuration.
//!
//! Mirrors `ghostchimera/config.py::GhostChimeraConfig`: environment-driven
//! with safe defaults. `GHOSTCHIMERA_STATE_DIR` overrides everything
//! (operator / CI escape hatch).

use std::env;
use std::path::PathBuf;

use crate::error::{GhostError, Result};

/// Resolved runtime configuration with safe defaults.
#[derive(Debug, Clone)]
pub struct GhostConfig {
    /// Base directory for all Ghost state (journals, sessions, logs).
    pub state_dir: PathBuf,
    /// SQLite database for memory.
    pub memory_db: PathBuf,
    /// Append-only audit log file.
    pub audit_file: PathBuf,
    /// Autonomy level: `supervised` (default), `autonomous`, etc.
    pub autonomy_level: String,
}

impl GhostConfig {
    /// Load configuration from the environment.
    ///
    /// - `GHOSTCHIMERA_STATE_DIR` (default: OS-appropriate data dir, see
    ///   [`default_state_dir`]; the legacy `~/.ghostchimera` is migrated on
    ///   first run via [`GhostConfig::migrate_legacy_state`])
    /// - `GHOSTCHIMERA_MEMORY_DB` (default `<state_dir>/memory.sqlite3`)
    /// - `GHOSTCHIMERA_AUDIT_FILE` (default `<state_dir>/audit.json`)
    /// - `GHOSTCHIMERA_AUTONOMY_LEVEL` (default `supervised`)
    pub fn from_env() -> Self {
        let state_dir = match env::var("GHOSTCHIMERA_STATE_DIR") {
            Ok(s) if !s.trim().is_empty() => expand_path(&s),
            _ => default_state_dir(),
        };
        let memory_db = env::var("GHOSTCHIMERA_MEMORY_DB")
            .map(|s| expand_path(&s))
            .unwrap_or_else(|_| state_dir.join("memory.sqlite3"));
        let audit_file = env::var("GHOSTCHIMERA_AUDIT_FILE")
            .map(|s| expand_path(&s))
            .unwrap_or_else(|_| state_dir.join("audit.json"));
        let autonomy_level =
            env::var("GHOSTCHIMERA_AUTONOMY_LEVEL").unwrap_or_else(|_| "supervised".into());
        Self {
            state_dir,
            memory_db,
            audit_file,
            autonomy_level,
        }
    }

    /// Create the state directories (idempotent).
    pub fn ensure_state_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.state_dir)?;
        std::fs::create_dir_all(self.crashes_dir())?;
        std::fs::create_dir_all(self.logs_dir())?;
        std::fs::create_dir_all(self.stealth_dir())?;
        if let Some(parent) = self.memory_db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(parent) = self.audit_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(())
    }

    /// Trust-runtime journal directory: `<state_dir>/trust_runtime`.
    pub fn trust_dir(&self) -> PathBuf {
        self.state_dir.join("trust_runtime")
    }

    /// Run journals directory: `<state_dir>/trust_runtime/runs`.
    pub fn runs_dir(&self) -> PathBuf {
        self.trust_dir().join("runs")
    }

    /// Crash reports directory: `<state_dir>/crashes`.
    pub fn crashes_dir(&self) -> PathBuf {
        self.state_dir.join("crashes")
    }

    /// Log directory: `<state_dir>/logs`.
    pub fn logs_dir(&self) -> PathBuf {
        self.state_dir.join("logs")
    }

    /// Stealth hook journal directory: `<state_dir>/stealth`.
    pub fn stealth_dir(&self) -> PathBuf {
        self.state_dir.join("stealth")
    }

    /// First-run migration from the Python layout (`~/.ghostchimera`).
    ///
    /// Copies (never moves) the legacy tree into the new state dir when:
    /// the legacy dir exists and is non-empty, the new state dir does not
    /// exist yet, and they are not the same directory. Returns `true` when
    /// a migration actually ran. Journals are field-compatible between the
    /// Python and Rust runtimes, so the copy is immediately usable.
    pub fn migrate_legacy_state(&self) -> Result<bool> {
        let legacy = expand_path("~/.ghostchimera");
        if !legacy.is_dir() {
            return Ok(false);
        }
        if self.state_dir.exists() || legacy == self.state_dir {
            return Ok(false);
        }
        let non_empty = std::fs::read_dir(&legacy)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false);
        if !non_empty {
            return Ok(false);
        }
        copy_dir_recursive(&legacy, &self.state_dir)?;
        Ok(true)
    }
}

/// OS-appropriate default state directory (SPEC section 7):
/// - Windows: `%LOCALAPPDATA%/ghost-chimera`
/// - macOS: `~/Library/Application Support/ghost-chimera`
/// - Linux: `~/.local/share/ghost-chimera` (or `$XDG_DATA_HOME`)
fn default_state_dir() -> PathBuf {
    #[cfg(windows)]
    let base = dirs::data_local_dir();
    #[cfg(not(windows))]
    let base = dirs::data_dir();
    match base {
        Some(dir) => dir.join("ghost-chimera"),
        None => expand_path("~/.ghostchimera"),
    }
}

/// Recursively copy a directory tree. Fails loudly on any IO error —
/// a half-copied migration must never look like success.
fn copy_dir_recursive(src: &PathBuf, dst: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &dst_path)?;
        }
        // Symlinks and other special files are skipped deliberately:
        // the legacy tree should only contain regular files and dirs.
    }
    Ok(())
}

/// Expand a leading `~` to the user's home directory.
fn expand_path(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    } else if raw == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(raw)
}

/// Parse an env var as a `GhostConfig`, returning a typed error on failure.
#[allow(dead_code)]
pub fn config_from_env_strict() -> Result<GhostConfig> {
    let cfg = GhostConfig::from_env();
    if cfg.state_dir.as_os_str().is_empty() {
        return Err(GhostError::Config("state_dir is empty".into()));
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Env-var tests mutate process-global state; serialize them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn env_loading() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("GHOSTCHIMERA_STATE_DIR", "/tmp/ghost-test-state");
        std::env::remove_var("GHOSTCHIMERA_AUTONOMY_LEVEL");
        let cfg = GhostConfig::from_env();
        assert_eq!(cfg.state_dir, PathBuf::from("/tmp/ghost-test-state"));
        assert_eq!(
            cfg.memory_db,
            PathBuf::from("/tmp/ghost-test-state/memory.sqlite3")
        );
        assert_eq!(cfg.autonomy_level, "supervised");

        std::env::set_var("GHOSTCHIMERA_AUTONOMY_LEVEL", "autonomous");
        let cfg2 = GhostConfig::from_env();
        assert_eq!(cfg2.autonomy_level, "autonomous");

        std::env::remove_var("GHOSTCHIMERA_STATE_DIR");
        std::env::remove_var("GHOSTCHIMERA_AUTONOMY_LEVEL");
    }

    #[test]
    fn ensure_state_dirs_creates_tree() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = GhostConfig {
            state_dir: dir.path().join("state"),
            memory_db: dir.path().join("state").join("memory.sqlite3"),
            audit_file: dir.path().join("state").join("audit.json"),
            autonomy_level: "supervised".into(),
        };
        cfg.ensure_state_dirs().unwrap();
        assert!(cfg.state_dir.is_dir());
        assert!(cfg.crashes_dir().is_dir());
        assert!(cfg.logs_dir().is_dir());
        assert!(cfg.stealth_dir().is_dir());
    }

    #[test]
    fn default_state_dir_is_os_appropriate() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("GHOSTCHIMERA_STATE_DIR");
        let dir = default_state_dir();
        let leaf = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
        assert_eq!(leaf, "ghost-chimera");
        // Must not fall back to the legacy dot-dir when the OS provides one.
        if dirs::data_dir().is_some() {
            assert!(!dir.starts_with(dirs::home_dir().unwrap().join(".ghostchimera")));
        }
    }

    #[test]
    fn empty_state_dir_env_falls_back_to_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("GHOSTCHIMERA_STATE_DIR", "   ");
        let cfg = GhostConfig::from_env();
        assert_eq!(cfg.state_dir, default_state_dir());
        std::env::remove_var("GHOSTCHIMERA_STATE_DIR");
    }

    #[test]
    fn migrate_legacy_state_copies_but_never_moves() {
        // Simulate a legacy Python tree under a fake HOME.
        let home = tempfile::tempdir().unwrap();
        let legacy = home.path().join(".ghostchimera");
        std::fs::create_dir_all(legacy.join("trust_runtime")).unwrap();
        std::fs::write(legacy.join("trust_runtime").join("run_index.json"), "{}").unwrap();

        let _guard = ENV_LOCK.lock().unwrap();
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        // Point the new state dir somewhere else entirely.
        let fresh = tempfile::tempdir().unwrap();
        let new_state = fresh.path().join("new-state");
        std::env::set_var("GHOSTCHIMERA_STATE_DIR", &new_state);

        let cfg = GhostConfig::from_env();
        let migrated = cfg.migrate_legacy_state().unwrap();
        assert!(migrated);
        // Copied...
        assert!(new_state
            .join("trust_runtime")
            .join("run_index.json")
            .is_file());
        // ...never moved: the legacy tree is untouched.
        assert!(legacy
            .join("trust_runtime")
            .join("run_index.json")
            .is_file());

        // Second run is a no-op (new state dir now exists).
        assert!(!cfg.migrate_legacy_state().unwrap());

        std::env::remove_var("GHOSTCHIMERA_STATE_DIR");
        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn migrate_legacy_state_noop_without_legacy_tree() {
        let home = tempfile::tempdir().unwrap();
        let _guard = ENV_LOCK.lock().unwrap();
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());
        let fresh = tempfile::tempdir().unwrap();
        std::env::set_var("GHOSTCHIMERA_STATE_DIR", fresh.path().join("s"));

        let cfg = GhostConfig::from_env();
        assert!(!cfg.migrate_legacy_state().unwrap());

        std::env::remove_var("GHOSTCHIMERA_STATE_DIR");
        match old_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}
