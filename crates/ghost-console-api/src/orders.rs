//! Standing orders: durable, operator-owned recurring instructions.
//!
//! Single implementation backing both the HTTP console API and the CLI.
//! State lives in two files under `<state_dir>`:
//!
//! - `standing_orders.json` — `{orders: {id: Order}}`
//! - `standing_orders_events.jsonl` — one JSON event per lifecycle change
//!
//! Mirrors `ghostchimera/control_plane/standing_orders.py`, including the
//! HON-F2 honesty fix: a preview run (`runner=None`) returns `executed: false`
//! and NEVER bumps `run_count` / `last_run_at`.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use ghost_core::{now, stable_id, GhostError, Result};

const MAX_TITLE_LEN: usize = 160;
const MAX_SCOPE_LEN: usize = 160;
const MAX_OBJECTIVE_LEN: usize = 4000;

/// A standing order. Field names mirror the Python `StandingOrder.to_dict()`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Order {
    pub id: String,
    pub title: String,
    pub scope: String,
    pub objective: String,
    pub allowed_actions: Vec<String>,
    pub approval_gates: Vec<String>,
    pub delivery_channel: String,
    pub delivery_target: String,
    pub status: String,
    pub created_at: f64,
    pub updated_at: f64,
    pub last_run_at: f64,
    pub run_count: i64,
    pub failure_count: i64,
    pub last_result: Value,
}

/// Result produced by a runner executing an order's objective.
#[derive(Debug, Clone)]
pub struct OrderRunResult {
    pub ok: bool,
    pub output: String,
}

/// Outcome of [`StandingOrderStore::run_order`].
#[derive(Debug, Clone, Serialize)]
pub struct OrderRunOutcome {
    pub order: Order,
    pub executed: bool,
    pub result: Value,
}

/// Durable JSON-file store for standing orders.
pub struct StandingOrderStore {
    orders_path: PathBuf,
    events_path: PathBuf,
    /// Guards file read-modify-write sequences.
    lock: Mutex<()>,
}

impl StandingOrderStore {
    /// Open (creating files as needed) a store under `state_dir`.
    pub fn new(state_dir: impl Into<PathBuf>) -> Result<Self> {
        let state_dir = state_dir.into();
        fs::create_dir_all(&state_dir)?;
        let orders_path = state_dir.join("standing_orders.json");
        let events_path = state_dir.join("standing_orders_events.jsonl");
        if !orders_path.exists() {
            fs::write(&orders_path, "{\"orders\": {}}")?;
        }
        if !events_path.exists() {
            fs::write(&events_path, "")?;
        }
        Ok(Self {
            orders_path,
            events_path,
            lock: Mutex::new(()),
        })
    }

    /// Create a new (disabled) order. Title and objective are required.
    pub fn create_order(&self, title: &str, objective: &str, scope: Option<&str>) -> Result<Order> {
        let title = title.trim();
        let objective = objective.trim();
        let scope = scope.unwrap_or("").trim();
        if title.is_empty() {
            return Err(GhostError::Config("title is required".to_string()));
        }
        if objective.is_empty() {
            return Err(GhostError::Config("objective is required".to_string()));
        }
        if title.chars().count() > MAX_TITLE_LEN {
            return Err(GhostError::Config(format!(
                "title exceeds {MAX_TITLE_LEN} characters"
            )));
        }
        if scope.chars().count() > MAX_SCOPE_LEN {
            return Err(GhostError::Config(format!(
                "scope exceeds {MAX_SCOPE_LEN} characters"
            )));
        }
        if objective.chars().count() > MAX_OBJECTIVE_LEN {
            return Err(GhostError::Config(format!(
                "objective exceeds {MAX_OBJECTIVE_LEN} characters"
            )));
        }
        let created_at = now();
        let id = stable_id(
            &[Some(title), Some(scope), Some(&created_at.to_string())],
            16,
        );
        let order = Order {
            id: id.clone(),
            title: title.to_string(),
            scope: scope.to_string(),
            objective: objective.to_string(),
            allowed_actions: Vec::new(),
            approval_gates: Vec::new(),
            delivery_channel: String::new(),
            delivery_target: String::new(),
            status: "disabled".to_string(),
            created_at,
            updated_at: created_at,
            last_run_at: 0.0,
            run_count: 0,
            failure_count: 0,
            last_result: Value::Null,
        };
        let _guard = self.lock.lock().unwrap();
        let mut orders = self.load_orders()?;
        orders.insert(id.clone(), order.clone());
        self.save_orders(&orders)?;
        drop(_guard);
        self.record_event("order_created", &id, &serde_json::json!({"title": title}))?;
        Ok(order)
    }

    /// List orders sorted by title, plus aggregate counts.
    pub fn list_orders(&self) -> Result<(Vec<Order>, Value)> {
        let orders = self.load_orders()?;
        let mut list: Vec<Order> = orders.into_values().collect();
        list.sort_by(|a, b| a.title.cmp(&b.title));
        let enabled = list.iter().filter(|o| o.status == "enabled").count();
        let counts = serde_json::json!({
            "orders": list.len(),
            "enabled": enabled,
            "disabled": list.len() - enabled,
        });
        Ok((list, counts))
    }

    /// Get one order by id.
    pub fn get_order(&self, id: &str) -> Result<Option<Order>> {
        Ok(self.load_orders()?.remove(id))
    }

    /// Enable or disable an order. Returns [`GhostError::NotFound`] when missing.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<Order> {
        let _guard = self.lock.lock().unwrap();
        let mut orders = self.load_orders()?;
        let order = orders
            .get_mut(id)
            .ok_or_else(|| GhostError::NotFound(format!("standing order not found: {id}")))?;
        order.status = if enabled { "enabled" } else { "disabled" }.to_string();
        order.updated_at = now();
        let updated = order.clone();
        self.save_orders(&orders)?;
        drop(_guard);
        let event = if enabled {
            "order_enabled"
        } else {
            "order_disabled"
        };
        self.record_event(event, id, &serde_json::json!({"status": updated.status}))?;
        Ok(updated)
    }

    /// Run an order against `runner`.
    ///
    /// HON-F2: with `runner=None` this is an honest preview — it returns
    /// `executed: false` and never touches `run_count`, `last_run_at`,
    /// `failure_count`, or `last_result`. Disabled orders are refused.
    pub fn run_order(
        &self,
        id: &str,
        runner: Option<&dyn Fn(&str) -> OrderRunResult>,
    ) -> Result<OrderRunOutcome> {
        let order = self
            .load_orders()?
            .remove(id)
            .ok_or_else(|| GhostError::NotFound(format!("standing order not found: {id}")))?;
        if order.status != "enabled" {
            return Err(GhostError::Config(format!(
                "standing order is not enabled: {id}"
            )));
        }
        let Some(run) = runner else {
            // Honest preview: no execution, no counter changes.
            return Ok(OrderRunOutcome {
                executed: false,
                result: serde_json::json!({
                    "preview": true,
                    "objective": order.objective,
                }),
                order,
            });
        };
        let result = run(&order.objective);
        let _guard = self.lock.lock().unwrap();
        let mut orders = self.load_orders()?;
        let stored = orders
            .get_mut(id)
            .ok_or_else(|| GhostError::NotFound(format!("standing order not found: {id}")))?;
        stored.run_count += 1;
        stored.last_run_at = now();
        stored.updated_at = stored.last_run_at;
        if !result.ok {
            stored.failure_count += 1;
        }
        stored.last_result = serde_json::json!({"ok": result.ok, "output": result.output});
        let updated = stored.clone();
        self.save_orders(&orders)?;
        drop(_guard);
        self.record_event(
            "order_run",
            id,
            &serde_json::json!({"ok": result.ok, "run_count": updated.run_count}),
        )?;
        Ok(OrderRunOutcome {
            executed: true,
            result: serde_json::json!({"ok": result.ok, "output": result.output}),
            order: updated,
        })
    }

    /// The base state directory file this store writes to (orders JSON).
    pub fn orders_path(&self) -> &Path {
        &self.orders_path
    }

    // -- internals ------------------------------------------------------

    fn load_orders(&self) -> Result<BTreeMap<String, Order>> {
        let text = fs::read_to_string(&self.orders_path)?;
        let doc: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let mut orders = BTreeMap::new();
        if let Some(map) = doc.get("orders").and_then(Value::as_object) {
            for (id, value) in map {
                if let Ok(order) = serde_json::from_value(value.clone()) {
                    orders.insert(id.clone(), order);
                }
            }
        }
        Ok(orders)
    }

    fn save_orders(&self, orders: &BTreeMap<String, Order>) -> Result<()> {
        let doc = serde_json::json!({"orders": orders});
        let tmp = self.orders_path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&doc)?)?;
        fs::rename(&tmp, &self.orders_path)?;
        Ok(())
    }

    fn record_event(&self, event_type: &str, order_id: &str, data: &Value) -> Result<()> {
        let event = serde_json::json!({
            "type": event_type,
            "order_id": order_id,
            "timestamp": now(),
            "data": data,
        });
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events_path)?;
        writeln!(file, "{}", serde_json::to_string(&event)?)?;
        file.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> (tempfile::TempDir, StandingOrderStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = StandingOrderStore::new(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn create_order_starts_disabled() {
        let (_dir, store) = test_store();
        let order = store
            .create_order("Daily scan", "scan for news", None)
            .unwrap();
        assert_eq!(order.status, "disabled");
        assert_eq!(order.title, "Daily scan");
        assert_eq!(order.run_count, 0);
        assert_eq!(order.id.len(), 16);
        assert!(store.get_order(&order.id).unwrap().is_some());
    }

    #[test]
    fn create_order_validates_inputs() {
        let (_dir, store) = test_store();
        assert!(matches!(
            store.create_order("", "obj", None),
            Err(GhostError::Config(_))
        ));
        assert!(matches!(
            store.create_order("t", "", None),
            Err(GhostError::Config(_))
        ));
        assert!(matches!(
            store.create_order(&"t".repeat(161), "obj", None),
            Err(GhostError::Config(_))
        ));
        assert!(matches!(
            store.create_order("t", &"o".repeat(4001), None),
            Err(GhostError::Config(_))
        ));
    }

    #[test]
    fn list_orders_sorted_with_counts() {
        let (_dir, store) = test_store();
        let b = store.create_order("beta", "obj", None).unwrap();
        store.create_order("alpha", "obj", None).unwrap();
        store.set_enabled(&b.id, true).unwrap();
        let (list, counts) = store.list_orders().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].title, "alpha");
        assert_eq!(counts["orders"], 2);
        assert_eq!(counts["enabled"], 1);
        assert_eq!(counts["disabled"], 1);
    }

    #[test]
    fn set_enabled_missing_is_not_found() {
        let (_dir, store) = test_store();
        assert!(matches!(
            store.set_enabled("nope", true),
            Err(GhostError::NotFound(_))
        ));
    }

    #[test]
    fn preview_run_does_not_bump_counters() {
        // HON-F2: preview must not touch run_count/last_run_at.
        let (_dir, store) = test_store();
        let order = store.create_order("scan", "scan for news", None).unwrap();
        store.set_enabled(&order.id, true).unwrap();
        let outcome = store.run_order(&order.id, None).unwrap();
        assert!(!outcome.executed);
        assert_eq!(outcome.result["preview"], true);
        let reloaded = store.get_order(&order.id).unwrap().unwrap();
        assert_eq!(reloaded.run_count, 0);
        assert_eq!(reloaded.last_run_at, 0.0);
        assert_eq!(reloaded.failure_count, 0);
    }

    #[test]
    fn run_with_runner_bumps_counters() {
        let (_dir, store) = test_store();
        let order = store.create_order("scan", "scan for news", None).unwrap();
        store.set_enabled(&order.id, true).unwrap();
        let ok_runner = |_objective: &str| OrderRunResult {
            ok: true,
            output: "done".to_string(),
        };
        let outcome = store.run_order(&order.id, Some(&ok_runner)).unwrap();
        assert!(outcome.executed);
        assert_eq!(outcome.order.run_count, 1);
        assert!(outcome.order.last_run_at > 0.0);
        assert_eq!(outcome.order.failure_count, 0);

        let fail_runner = |_objective: &str| OrderRunResult {
            ok: false,
            output: "bad".to_string(),
        };
        let outcome2 = store.run_order(&order.id, Some(&fail_runner)).unwrap();
        assert!(outcome2.executed);
        assert_eq!(outcome2.order.run_count, 2);
        assert_eq!(outcome2.order.failure_count, 1);
        assert_eq!(outcome2.order.last_result["ok"], false);
    }

    #[test]
    fn disabled_order_run_is_refused() {
        let (_dir, store) = test_store();
        let order = store.create_order("scan", "scan for news", None).unwrap();
        let runner = |_objective: &str| OrderRunResult {
            ok: true,
            output: "done".to_string(),
        };
        assert!(matches!(
            store.run_order(&order.id, Some(&runner)),
            Err(GhostError::Config(_))
        ));
    }

    #[test]
    fn missing_order_run_is_not_found() {
        let (_dir, store) = test_store();
        assert!(matches!(
            store.run_order("nope", None),
            Err(GhostError::NotFound(_))
        ));
    }
}
