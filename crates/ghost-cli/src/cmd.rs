//! CLI command implementations, kept separate from `main.rs` so tests
//! can call them directly.
//!
//! Exit-code contract:
//!
//! - `0` — success (every execution fulfilled its objective).
//! - `1` — honest failure (unfulfilled objective, validation error, ...).
//! - `2` — usage error (e.g. `ask` with no objective).

use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use ghost_console_api::{build_kernel, serve, StandingOrderStore};
use ghost_core::{GhostConfig, GhostError};
use ghost_trust::{RecordStepArgs, ReplayExecution, TrustRuntimeStore};

/// `ghost` — the GHOST-Chimera command line.
#[derive(Debug, Parser)]
#[command(name = "ghost", version, about = "GHOST-Chimera control CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run an objective through the pilot kernel (journaled in trust).
    Ask {
        /// The objective, joined with spaces.
        objective: Vec<String>,
        /// Print the full executions JSON instead of the reply text.
        #[arg(long)]
        json: bool,
    },
    /// Trust-journal operations.
    Trust {
        #[command(subcommand)]
        command: TrustCommands,
    },
    /// Standing-order lifecycle.
    #[command(name = "standing-orders")]
    StandingOrders {
        #[command(subcommand)]
        command: StandingOrderCommands,
    },
    /// Serve the console HTTP API.
    Console {
        /// Bind host (loopback by default).
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Bind port.
        #[arg(long, default_value_t = 8766)]
        port: u16,
        /// Bearer token (overrides GHOST_CONSOLE_TOKEN).
        #[arg(long)]
        token: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum TrustCommands {
    /// Re-execute a journaled run's objective and compare outcomes.
    Replay {
        /// The original run id.
        run_id: String,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum StandingOrderCommands {
    /// List all standing orders with counts.
    List,
    /// Create a new (disabled) standing order.
    Create {
        #[arg(long)]
        title: String,
        #[arg(long)]
        objective: String,
        #[arg(long)]
        scope: Option<String>,
    },
    /// Enable a standing order.
    Enable {
        #[arg(long)]
        id: String,
    },
    /// Disable a standing order.
    Disable {
        #[arg(long)]
        id: String,
    },
    /// Preview-run a standing order (no runner: honest preview, no counters).
    Run {
        #[arg(long)]
        id: String,
    },
}

/// Entry point called by `main`: parse, dispatch, print, return exit code.
///
/// Runs synchronously. Only the `console` subcommand builds a tokio
/// runtime (see the module docs in `main.rs` for why the process itself
/// must not run inside one).
pub(crate) fn run() -> i32 {
    let cli = Cli::parse();
    let config = GhostConfig::from_env();
    match cli.command {
        Commands::Ask { objective, json } => {
            let (code, value) = run_ask(&objective, json, &config);
            if json {
                println!("{}", serde_json::to_string_pretty(&value).unwrap());
            } else if code == 2 {
                eprintln!("{}", serde_json::to_string_pretty(&value).unwrap());
            } else {
                println!("{}", value["text"].as_str().unwrap_or(""));
            }
            code
        }
        Commands::Trust {
            command: TrustCommands::Replay { run_id },
        } => {
            let (code, value) = run_trust_replay(&run_id, &config);
            println!("{}", serde_json::to_string_pretty(&value).unwrap());
            code
        }
        Commands::StandingOrders { command } => {
            let (code, value) = run_standing_orders(&command, &config);
            println!("{}", serde_json::to_string_pretty(&value).unwrap());
            code
        }
        Commands::Console { host, port, token } => {
            // --token overrides the environment (empty flag value = unset).
            let token = token
                .filter(|t| !t.is_empty())
                .or_else(|| std::env::var("GHOST_CONSOLE_TOKEN").ok())
                .filter(|t| !t.is_empty());
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    eprintln!("failed to start async runtime: {err}");
                    return 1;
                }
            };
            match runtime.block_on(serve(&host, port, token, config.state_dir.clone())) {
                Ok(()) => 0,
                Err(err) => {
                    eprintln!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "ok": false,
                            "error": err.to_string(),
                        }))
                        .unwrap()
                    );
                    1
                }
            }
        }
    }
}

// -- ask -----------------------------------------------------------------

/// Run an objective and journal it. Returns `(exit_code, output)`.
///
/// Exit `2` when the objective is empty, `1` when any execution failed,
/// `0` when every execution fulfilled the objective.
pub(crate) fn run_ask(objective: &[String], as_json: bool, config: &GhostConfig) -> (i32, Value) {
    let objective = objective.join(" ").trim().to_string();
    if objective.is_empty() {
        return (2, json!({"ok": false, "error": "objective is required"}));
    }
    let store = match TrustRuntimeStore::new(&config.state_dir) {
        Ok(store) => store,
        Err(err) => return (1, json!({"ok": false, "error": err.to_string()})),
    };
    let run = match store.create_run(&objective, "ask-cli", "ghost-chimera", None) {
        Ok(run) => run,
        Err(err) => return (1, json!({"ok": false, "error": err.to_string()})),
    };
    let (kernel, _note) = build_kernel(&config.autonomy_level);
    let executions = match kernel.run(&objective) {
        Ok(executions) => executions,
        Err(err) => {
            return (
                1,
                json!({"ok": false, "error": err.to_string(), "trust_run_id": run.run_id}),
            )
        }
    };
    for exec in &executions {
        let output: String = exec.output.chars().take(2000).collect();
        let args = RecordStepArgs {
            step_type: "task_executed",
            status: if exec.ok { "ok" } else { "error" },
            input_payload: Some(json!({
                "task_id": exec.task_id,
                "backend_id": exec.backend_id,
            })),
            output_payload: Some(json!({
                "ok": exec.ok,
                "output": output,
                "error": exec.error,
            })),
            policy_decision: None,
            idempotency_key: None,
            retryable: false,
            duration_ms: 0.0,
            error: &exec.error,
        };
        if store.record_step(&run.run_id, args).is_err() {
            break;
        }
    }
    let all_ok = !executions.is_empty() && executions.iter().all(|e| e.ok);
    let code = if all_ok { 0 } else { 1 };
    let text = executions
        .iter()
        .find(|e| e.ok)
        .map(|e| e.output.clone())
        .unwrap_or_else(|| {
            executions
                .first()
                .map(|e| format!("error: {}", e.error))
                .unwrap_or_else(|| "no executions were recorded".to_string())
        });
    let value = if as_json {
        json!({
            "ok": all_ok,
            "trust_run_id": run.run_id,
            "executions": executions,
        })
    } else {
        json!({
            "ok": all_ok,
            "trust_run_id": run.run_id,
            "text": text,
        })
    };
    (code, value)
}

// -- trust ---------------------------------------------------------------

/// Replay a journaled run through the kernel. Returns `(exit_code, report)`.
pub(crate) fn run_trust_replay(run_id: &str, config: &GhostConfig) -> (i32, Value) {
    let store = match TrustRuntimeStore::new(&config.state_dir) {
        Ok(store) => store,
        Err(err) => return (1, json!({"ok": false, "error": err.to_string()})),
    };
    let (kernel, _note) = build_kernel(&config.autonomy_level);
    match store.replay_run(run_id, &|objective| {
        kernel.run(objective).map(|executions| {
            executions
                .into_iter()
                .map(|e| ReplayExecution {
                    task_id: e.task_id,
                    backend_id: e.backend_id,
                    ok: e.ok,
                    output: e.output,
                    error: e.error,
                })
                .collect()
        })
    }) {
        Ok(report) => (
            0,
            serde_json::to_value(&report)
                .unwrap_or(json!({"ok": false, "error": "serialization failed"})),
        ),
        Err(err) => (1, json!({"ok": false, "error": err.to_string()})),
    }
}

// -- standing orders -----------------------------------------------------

/// Standing-order lifecycle against the durable store.
pub(crate) fn run_standing_orders(
    command: &StandingOrderCommands,
    config: &GhostConfig,
) -> (i32, Value) {
    let store = match StandingOrderStore::new(&config.state_dir) {
        Ok(store) => store,
        Err(err) => return (1, json!({"ok": false, "error": err.to_string()})),
    };
    match command {
        StandingOrderCommands::List => match store.list_orders() {
            Ok((orders, counts)) => (0, json!({"ok": true, "orders": orders, "counts": counts})),
            Err(err) => (1, err_json(&err)),
        },
        StandingOrderCommands::Create {
            title,
            objective,
            scope,
        } => match store.create_order(title, objective, scope.as_deref()) {
            Ok(order) => (0, json!({"ok": true, "order": order})),
            Err(err) => (1, err_json(&err)),
        },
        StandingOrderCommands::Enable { id } => match store.set_enabled(id, true) {
            Ok(order) => (0, json!({"ok": true, "order": order})),
            Err(err) => (1, err_json(&err)),
        },
        StandingOrderCommands::Disable { id } => match store.set_enabled(id, false) {
            Ok(order) => (0, json!({"ok": true, "order": order})),
            Err(err) => (1, err_json(&err)),
        },
        // The CLI has no runner: this is always an honest preview
        // (executed=false, no counter changes — HON-F2).
        StandingOrderCommands::Run { id } => match store.run_order(id, None) {
            Ok(outcome) => (
                0,
                json!({
                    "ok": true,
                    "executed": outcome.executed,
                    "preview": !outcome.executed,
                    "objective": outcome.order.objective,
                    "order": outcome.order,
                }),
            ),
            Err(err) => (1, err_json(&err)),
        },
    }
}

fn err_json(err: &GhostError) -> Value {
    json!({"ok": false, "error": err.to_string()})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> (tempfile::TempDir, GhostConfig) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = GhostConfig::from_env();
        config.state_dir = dir.path().to_path_buf();
        (dir, config)
    }

    #[test]
    fn ask_with_unfulfillable_objective_returns_exit_1_and_journals() {
        let (_dir, config) = test_config();
        let (code, value) = run_ask(
            &["write".to_string(), "a".to_string(), "poem".to_string()],
            false,
            &config,
        );
        assert_eq!(code, 1);
        assert_eq!(value["ok"], false);
        let text = value["text"].as_str().unwrap();
        assert!(
            text.contains("deterministic_unfulfilled"),
            "honest reason required, got: {text}"
        );
        // Journaled in the trust store.
        let run_id = value["trust_run_id"].as_str().unwrap();
        let store = TrustRuntimeStore::new(&config.state_dir).unwrap();
        let view = store.get_run(run_id).unwrap().unwrap();
        assert_eq!(view.run.source, "ask-cli");
        assert!(view.steps.iter().any(|s| s.step_type == "task_executed"));
    }

    #[test]
    fn ask_with_fulfillable_objective_returns_exit_0() {
        let (_dir, config) = test_config();
        let registry = ghost_providers::ProviderRegistry::from_env();
        if registry.is_empty() {
            let (code, value) = run_ask(&["what is 6 * 7".to_string()], false, &config);
            assert_eq!(code, 0);
            assert_eq!(value["text"], "42");
        }
    }

    #[test]
    fn ask_with_no_objective_returns_exit_2() {
        let (_dir, config) = test_config();
        let (code, value) = run_ask(&[], false, &config);
        assert_eq!(code, 2);
        assert_eq!(
            value,
            json!({"ok": false, "error": "objective is required"})
        );
    }

    #[test]
    fn ask_json_output_carries_executions() {
        let (_dir, config) = test_config();
        let (code, value) = run_ask(&["write a poem".to_string()], true, &config);
        assert_eq!(code, 1);
        assert!(value["executions"].is_array());
        assert!(value["trust_run_id"].as_str().is_some());
        assert!(value.get("text").is_none());
    }

    #[test]
    fn trust_replay_unknown_run_returns_exit_1() {
        let (_dir, config) = test_config();
        let (code, value) = run_trust_replay("nope", &config);
        assert_eq!(code, 1);
        assert_eq!(value["ok"], false);
        assert!(value["error"].as_str().unwrap().contains("not found"));
    }

    #[test]
    fn trust_replay_journaled_run_returns_report() {
        let (_dir, config) = test_config();
        let store = TrustRuntimeStore::new(&config.state_dir).unwrap();
        let run = store
            .create_run("what is 6 * 7", "ask-cli", "ghost-chimera", None)
            .unwrap();
        let (code, value) = run_trust_replay(&run.run_id, &config);
        assert_eq!(code, 0, "{value}");
        assert_eq!(value["replayed_from"], run.run_id);
        assert!(value["comparison"].is_object());
    }

    #[test]
    fn standing_orders_create_validation() {
        let (_dir, config) = test_config();
        let cmd = StandingOrderCommands::Create {
            title: String::new(),
            objective: "obj".to_string(),
            scope: None,
        };
        let (code, value) = run_standing_orders(&cmd, &config);
        assert_eq!(code, 1);
        assert_eq!(value["ok"], false);
        assert!(value["error"].as_str().unwrap().contains("title"));
    }

    #[test]
    fn standing_orders_full_lifecycle() {
        let (_dir, config) = test_config();
        // Create.
        let (code, value) = run_standing_orders(
            &StandingOrderCommands::Create {
                title: "Daily scan".to_string(),
                objective: "scan for news".to_string(),
                scope: None,
            },
            &config,
        );
        assert_eq!(code, 0, "{value}");
        let id = value["order"]["id"].as_str().unwrap().to_string();
        // List.
        let (code, value) = run_standing_orders(&StandingOrderCommands::List, &config);
        assert_eq!(code, 0);
        assert_eq!(value["counts"]["orders"], 1);
        // Enable.
        let (code, value) =
            run_standing_orders(&StandingOrderCommands::Enable { id: id.clone() }, &config);
        assert_eq!(code, 0);
        assert_eq!(value["order"]["status"], "enabled");
        // Run: honest preview, no counter bump.
        let (code, value) =
            run_standing_orders(&StandingOrderCommands::Run { id: id.clone() }, &config);
        assert_eq!(code, 0, "{value}");
        assert_eq!(value["executed"], false);
        assert_eq!(value["preview"], true);
        assert_eq!(value["order"]["run_count"], 0);
        // Disable.
        let (code, _) =
            run_standing_orders(&StandingOrderCommands::Disable { id: id.clone() }, &config);
        assert_eq!(code, 0);
        // Enable unknown -> 1.
        let (code, value) = run_standing_orders(
            &StandingOrderCommands::Enable { id: "nope".into() },
            &config,
        );
        assert_eq!(code, 1);
        assert!(value["error"].as_str().unwrap().contains("not found"));
    }

    #[test]
    fn ask_value_is_object_not_null() {
        // Kills the `(i32, Value::default())` mutants: the Value must be a
        // JSON object with the expected shape, never Null.
        let (_dir, config) = test_config();
        let (code, value) = run_ask(&[], false, &config);
        assert_eq!(code, 2);
        assert!(value.is_object(), "value must be an object, got: {value}");
        assert_eq!(value["ok"], false);
    }

    #[test]
    fn ask_success_value_has_text_and_ok_true() {
        // Kills mutants that replace the success tuple: both the code
        // AND the value content are asserted.
        let (_dir, config) = test_config();
        let (code, value) = run_ask(&["what is 6 * 7".to_string()], false, &config);
        // Deterministic backend: 6*7=42, exit 0. If providers are configured
        // the test is skipped (non-deterministic).
        let registry = ghost_providers::ProviderRegistry::from_env();
        if registry.is_empty() {
            assert_eq!(code, 0);
            assert!(value.is_object());
            assert_eq!(value["ok"], true);
            assert_eq!(value["text"], "42");
        }
    }

    #[test]
    fn trust_replay_value_is_object_not_null() {
        // Kills the `(i32, Value::default())` mutants for run_trust_replay.
        let (_dir, config) = test_config();
        let (code, value) = run_trust_replay("nope", &config);
        assert_eq!(code, 1);
        assert!(value.is_object(), "value must be an object, got: {value}");
        assert_eq!(value["ok"], false);
    }
}
