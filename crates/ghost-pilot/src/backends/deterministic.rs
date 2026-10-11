//! Deterministic offline backend.
//!
//! Mirrors `chimera_pilot/backends/deterministic.py` (PR #116 semantics).
//!
//! When `fulfill` is true, the backend actually executes the deterministic
//! tasks it understands (file listing via `std::fs`, arithmetic via a
//! hand-written recursive-descent evaluator — never `eval`). Anything else
//! returns `ok=false` with a machine-readable `deterministic_unfulfilled`
//! error. The backend NEVER reports false success.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::backends::{Backend, BackendHealth, ExecutionResult};
use crate::task::{TaskKind, TaskSpec};

/// Machine-readable error code when the backend cannot fulfill a task.
/// Every unfulfilled error message starts with this prefix.
pub const UNFULFILLED_ERROR: &str = "deterministic_unfulfilled";

/// A deterministic backend that can honestly fulfill simple tasks.
pub struct DeterministicBackend {
    fulfill: bool,
    workspace_root: PathBuf,
    kinds: Vec<TaskKind>,
}

impl DeterministicBackend {
    /// The backend id, matching Python.
    pub const ID: &'static str = "deterministic.local";

    /// Create with default settings (fulfill off, workspace root = cwd,
    /// all task kinds supported).
    pub fn new() -> Self {
        Self {
            fulfill: false,
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            kinds: vec![TaskKind::Reasoning, TaskKind::ToolCall, TaskKind::RagQuery],
        }
    }

    /// Create with honest fulfillment enabled.
    pub fn fulfilling() -> Self {
        Self {
            fulfill: true,
            ..Self::new()
        }
    }

    /// Restrict the task kinds this backend accepts (mirrors Python's `kinds` param).
    pub fn with_kinds(mut self, kinds: Vec<TaskKind>) -> Self {
        self.kinds = kinds;
        self
    }

    /// Override the workspace root used to sandbox file listing.
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = root;
        self
    }

    /// Attempt to actually execute a deterministic task.
    ///
    /// Returns `(fulfilled, output, error)`. When not fulfilled, `output`
    /// is empty and `error` starts with `deterministic_unfulfilled`.
    fn try_fulfill(&self, task: &TaskSpec) -> (bool, String, String) {
        let objective = task.objective.trim();
        if objective.is_empty() {
            return (
                false,
                String::new(),
                format!("{UNFULFILLED_ERROR}: empty objective"),
            );
        }

        if let Some(dir) = match_list_files(objective) {
            return self.list_files(&dir);
        }
        if let Some(expr) = match_arithmetic(objective) {
            return match eval_arithmetic(&expr) {
                Ok(value) => {
                    let output = if value.fract() == 0.0 && value.is_finite() {
                        // Format integers without a decimal point (mirrors Python).
                        format!("{}", value as i64)
                    } else {
                        format!("{value}")
                    };
                    (true, output, String::new())
                }
                Err(err) => (false, String::new(), format!("{UNFULFILLED_ERROR}: {err}")),
            };
        }

        let preview: String = objective.chars().take(100).collect();
        (
            false,
            String::new(),
            format!("{UNFULFILLED_ERROR}: no LLM provider configured; deterministic backend cannot fulfill: {preview}"),
        )
    }

    /// List a directory, sandboxed to the workspace root.
    fn list_files(&self, dir: &str) -> (bool, String, String) {
        let cleaned = dir.trim().trim_matches(|c| c == '\'' || c == '"');
        let joined = if Path::new(cleaned).is_absolute() {
            PathBuf::from(cleaned)
        } else {
            self.workspace_root.join(cleaned)
        };
        // Canonicalize both sides so `..` and symlinks cannot escape.
        let root = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());
        let canonical = match joined.canonicalize() {
            Ok(p) => p,
            Err(_) => {
                return (
                    false,
                    String::new(),
                    format!("{UNFULFILLED_ERROR}: directory not found: {cleaned}"),
                );
            }
        };
        if !canonical.starts_with(&root) {
            return (
                false,
                String::new(),
                format!("{UNFULFILLED_ERROR}: path escapes workspace root: {cleaned}"),
            );
        }
        if !canonical.is_dir() {
            return (
                false,
                String::new(),
                format!("{UNFULFILLED_ERROR}: not a directory: {cleaned}"),
            );
        }
        match std::fs::read_dir(&canonical) {
            Ok(entries) => {
                let mut names: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                (true, names.join("\n"), String::new())
            }
            Err(err) => (
                false,
                String::new(),
                format!("{UNFULFILLED_ERROR}: cannot list directory: {err}"),
            ),
        }
    }
}

impl Default for DeterministicBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for DeterministicBackend {
    fn id(&self) -> &str {
        Self::ID
    }

    fn probe(&self) -> BackendHealth {
        BackendHealth::default()
    }

    fn can_run(&self, task: &TaskSpec) -> bool {
        self.kinds.contains(&task.kind)
    }

    fn execute(&self, task: &TaskSpec) -> ExecutionResult {
        if self.fulfill {
            let (fulfilled, output, error) = self.try_fulfill(task);
            let mut metrics = HashMap::new();
            metrics.insert("deterministic".to_string(), serde_json::Value::Bool(true));
            metrics.insert("fulfilled".to_string(), serde_json::Value::Bool(fulfilled));
            return ExecutionResult {
                backend_id: self.id().to_string(),
                task_id: task.id.clone(),
                ok: fulfilled,
                output: if fulfilled { output } else { String::new() },
                error: if fulfilled { String::new() } else { error },
                metrics,
            };
        }
        // Legacy placeholder behavior (fulfill=false): always ok.
        let mut metrics = HashMap::new();
        metrics.insert("deterministic".to_string(), serde_json::Value::Bool(true));
        ExecutionResult {
            backend_id: self.id().to_string(),
            task_id: task.id.clone(),
            ok: true,
            output: "ok".to_string(),
            error: String::new(),
            metrics,
        }
    }
}

/// Match "list the files in <dir>" (case-insensitive, mirrors Python).
fn match_list_files(objective: &str) -> Option<String> {
    let re = Regex::new(r"(?i)list(?: the)? files in (?:the )?(.+?)(?: directory)?\.?$").ok()?;
    re.captures(objective)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Match "what is <expr>" / "calculate <expr>" etc. (mirrors Python).
fn match_arithmetic(objective: &str) -> Option<String> {
    let re =
        Regex::new(r"(?i)(?:what is|calculate|compute|evaluate)\s+([0-9+\-*/().\s]+)\??$").ok()?;
    re.captures(objective)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
}

// ---------------------------------------------------------------------------
// Safe arithmetic evaluator: hand-written recursive descent.
// Grammar: expr := term (('+'|'-') term)*
//          term := power (('*'|'/') power)*
//          power := unary ('**' power)?
//          unary := ('+'|'-')* primary
//          primary := number | '(' expr ')'
// ---------------------------------------------------------------------------

/// Evaluate a simple arithmetic expression. Only numbers, +-*/(), ** and
/// unary signs are accepted; anything else is an error (never eval).
fn eval_arithmetic(expr: &str) -> Result<f64, String> {
    let mut parser = ArithmeticParser::new(expr);
    let value = parser
        .parse_expr()
        .map_err(|e| format!("invalid arithmetic: {e}"))?;
    parser.skip_ws();
    if parser.pos != parser.chars.len() {
        return Err(format!(
            "invalid arithmetic: unexpected trailing input in {expr:?}"
        ));
    }
    Ok(value)
}

struct ArithmeticParser {
    chars: Vec<char>,
    pos: usize,
}

impl ArithmeticParser {
    fn new(expr: &str) -> Self {
        Self {
            chars: expr.chars().collect(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.skip_ws();
        self.chars.get(self.pos).copied()
    }

    fn eat(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse_expr(&mut self) -> Result<f64, String> {
        let mut value = self.parse_term()?;
        loop {
            if self.eat('+') {
                value += self.parse_term()?;
            } else if self.eat('-') {
                value -= self.parse_term()?;
            } else {
                return Ok(value);
            }
        }
    }

    fn parse_term(&mut self) -> Result<f64, String> {
        let mut value = self.parse_power()?;
        loop {
            if self.eat('*') {
                // Distinguish * from ** (power is handled in parse_power,
                // but a stray ** here would have been consumed there).
                value *= self.parse_power()?;
            } else if self.eat('/') {
                let divisor = self.parse_power()?;
                if divisor == 0.0 {
                    return Err("division by zero".to_string());
                }
                value /= divisor;
            } else {
                return Ok(value);
            }
        }
    }

    fn parse_power(&mut self) -> Result<f64, String> {
        let base = self.parse_unary()?;
        // Right-associative **.
        if self.peek() == Some('*') && self.chars.get(self.pos + 1) == Some(&'*') {
            self.pos += 2;
            let exp = self.parse_power()?;
            return Ok(base.powf(exp));
        }
        Ok(base)
    }

    fn parse_unary(&mut self) -> Result<f64, String> {
        if self.eat('+') {
            return self.parse_unary();
        }
        if self.eat('-') {
            return Ok(-self.parse_unary()?);
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<f64, String> {
        if self.eat('(') {
            let value = self.parse_expr()?;
            if !self.eat(')') {
                return Err("missing closing parenthesis".to_string());
            }
            return Ok(value);
        }
        self.parse_number()
    }

    fn parse_number(&mut self) -> Result<f64, String> {
        self.skip_ws();
        let start = self.pos;
        let mut seen_dot = false;
        while self.pos < self.chars.len() {
            let c = self.chars[self.pos];
            if c.is_ascii_digit() {
                self.pos += 1;
            } else if c == '.' && !seen_dot {
                seen_dot = true;
                self.pos += 1;
            } else {
                break;
            }
        }
        if start == self.pos {
            return Err(format!("expected number at position {start}"));
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        text.parse::<f64>()
            .map_err(|_| format!("invalid number: {text:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(objective: &str) -> TaskSpec {
        TaskSpec::new("t1", TaskKind::Reasoning, objective)
    }

    #[test]
    fn fulfills_arithmetic() {
        let backend = DeterministicBackend::fulfilling();
        // 2 + 3 * 4 = 14 (operator precedence, mirrors Python test).
        let result = backend.execute(&task("what is 2 + 3 * 4"));
        assert!(result.ok, "error: {}", result.error);
        assert_eq!(result.output, "14");
        assert_eq!(result.backend_id, "deterministic.local");
    }

    #[test]
    fn arithmetic_handles_parens_powers_and_unary() {
        let backend = DeterministicBackend::fulfilling();
        let cases = [
            ("calculate (2 + 3) * 4", "20"),
            ("compute 2 ** 3", "8"),
            ("compute 2**3", "8"),
            ("evaluate -5 + 10", "5"),
            ("what is 7 / 2", "3.5"),
            ("what is 1.5 + 1", "2.5"),
            ("what is 10 - 2 - 3", "5"),
        ];
        for (objective, expected) in cases {
            let result = backend.execute(&task(objective));
            assert!(result.ok, "{objective}: {}", result.error);
            assert_eq!(result.output, expected, "{objective}");
        }
    }

    #[test]
    fn arithmetic_rejects_division_by_zero() {
        let backend = DeterministicBackend::fulfilling();
        let result = backend.execute(&task("what is 1 / 0"));
        assert!(!result.ok);
        assert!(
            result.error.starts_with(UNFULFILLED_ERROR),
            "got: {}",
            result.error
        );
    }

    #[test]
    fn honest_failure_for_poem() {
        // The PR #116 honesty rule: creative tasks fail honestly.
        let backend = DeterministicBackend::fulfilling();
        let result = backend.execute(&task("write a poem about the ocean"));
        assert!(!result.ok);
        assert!(result.output.is_empty());
        assert!(
            result.error.starts_with(UNFULFILLED_ERROR),
            "got: {}",
            result.error
        );
        assert!(result.error.contains("no LLM provider configured"));
    }

    #[test]
    fn honest_failure_for_empty_objective() {
        let backend = DeterministicBackend::fulfilling();
        let result = backend.execute(&task("   "));
        assert!(!result.ok);
        assert!(result.error.starts_with(UNFULFILLED_ERROR));
    }

    #[test]
    fn fulfills_file_listing() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("b.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let backend =
            DeterministicBackend::fulfilling().with_workspace_root(dir.path().to_path_buf());
        // List the temp dir by absolute path (inside the workspace root).
        let objective = format!("list the files in {}", dir.path().display());
        let result = backend.execute(&task(&objective));
        assert!(result.ok, "error: {}", result.error);
        assert_eq!(result.output, "a.txt\nb.txt");
    }

    #[test]
    fn file_listing_strips_quotes_from_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("q.txt"), b"x").unwrap();
        let backend =
            DeterministicBackend::fulfilling().with_workspace_root(dir.path().to_path_buf());
        let objective = format!("list the files in \"{}\"", dir.path().display());
        let result = backend.execute(&task(&objective));
        assert!(result.ok, "error: {}", result.error);
        assert_eq!(result.output, "q.txt");
    }

    #[test]
    fn can_run_respects_kind_restriction() {
        use crate::backends::Backend;
        let restricted = DeterministicBackend::new().with_kinds(vec![TaskKind::ToolCall]);
        assert!(!restricted.can_run(&task("what is 1 + 1")));
        assert!(restricted.can_run(&TaskSpec::new("t2", TaskKind::ToolCall, "do it")));
        // Default backend accepts all kinds.
        let open = DeterministicBackend::new();
        assert!(open.can_run(&task("what is 1 + 1")));
    }

    #[test]
    fn rejects_path_traversal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let backend =
            DeterministicBackend::fulfilling().with_workspace_root(dir.path().to_path_buf());
        let result = backend.execute(&task("list the files in ../.."));
        assert!(!result.ok);
        assert!(
            result.error.starts_with(UNFULFILLED_ERROR),
            "got: {}",
            result.error
        );
        assert!(result.error.contains("escapes workspace root"));
    }

    #[test]
    fn file_listing_missing_dir_is_honest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let backend =
            DeterministicBackend::fulfilling().with_workspace_root(dir.path().to_path_buf());
        let result = backend.execute(&task("list the files in no-such-dir-xyz"));
        assert!(!result.ok);
        assert!(result.error.starts_with(UNFULFILLED_ERROR));
    }

    #[test]
    fn placeholder_mode_preserves_legacy_behavior() {
        // fulfill=false: always ok with placeholder output (existing tests rely on this).
        let backend = DeterministicBackend::new();
        let result = backend.execute(&task("write a poem about the ocean"));
        assert!(result.ok);
        assert_eq!(result.output, "ok");
    }

    #[test]
    fn fulfilled_metrics_flag_is_set() {
        let backend = DeterministicBackend::fulfilling();
        let ok_result = backend.execute(&task("what is 1 + 1"));
        assert_eq!(
            ok_result.metrics.get("fulfilled"),
            Some(&serde_json::Value::Bool(true))
        );
        let fail_result = backend.execute(&task("write a novel"));
        assert_eq!(
            fail_result.metrics.get("fulfilled"),
            Some(&serde_json::Value::Bool(false))
        );
    }
}
