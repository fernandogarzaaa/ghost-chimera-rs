//! Durable spend ledger with per-provider budgets.
//!
//! Mirrors `model_layer/cost_monitor.py::CostLedger`, with the durability
//! gap closed (GAP_ANALYSIS.md H1): every `record()` appends to a JSONL
//! journal and `set_budget()` persists immediately, so spend records and
//! budget caps survive restarts. The journal is the source of truth;
//! in-memory aggregates are rebuilt by replaying it on `load()`.

use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use ghost_core::GhostError;

/// Maximum latency samples kept per provider (mirrors Python's 200).
const MAX_LATENCY_SAMPLES: usize = 200;

/// One journal line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum JournalEntry {
    Record {
        provider: String,
        model: String,
        cost_usd: f64,
        input_tokens: u64,
        output_tokens: u64,
        latency_s: Option<f64>,
        day: String,
    },
    Budget {
        provider: String,
        usd: f64,
    },
}

/// In-memory aggregates, rebuilt from the journal on load.
#[derive(Debug, Default)]
struct LedgerState {
    spend: HashMap<String, f64>,
    calls: HashMap<String, u64>,
    budgets: HashMap<String, f64>,
    by_model: HashMap<String, f64>,
    tokens_in: HashMap<String, u64>,
    tokens_out: HashMap<String, u64>,
    latency_s: HashMap<String, VecDeque<f64>>,
    daily: HashMap<String, f64>,
}

/// Thread-safe, durable spend ledger.
pub struct CostLedger {
    journal_path: PathBuf,
    state: Mutex<LedgerState>,
}

impl CostLedger {
    /// Open (or create) the ledger at `<state_dir>/cost_ledger.jsonl`.
    pub fn open(state_dir: &Path) -> Result<Self, GhostError> {
        let journal_path = state_dir.join("cost_ledger.jsonl");
        let ledger = Self {
            journal_path,
            state: Mutex::new(LedgerState::default()),
        };
        ledger.replay()?;
        Ok(ledger)
    }

    /// Rebuild aggregates by replaying the journal.
    fn replay(&self) -> Result<(), GhostError> {
        if !self.journal_path.exists() {
            return Ok(());
        }
        let file = std::fs::File::open(&self.journal_path)?;
        let reader = BufReader::new(file);
        let mut state = self.state.lock().expect("ledger lock");
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: JournalEntry = serde_json::from_str(&line)?;
            Self::apply(&mut state, &entry);
        }
        Ok(())
    }

    fn apply(state: &mut LedgerState, entry: &JournalEntry) {
        match entry {
            JournalEntry::Record {
                provider,
                model,
                cost_usd,
                input_tokens,
                output_tokens,
                latency_s,
                day,
            } => {
                *state.spend.entry(provider.clone()).or_insert(0.0) += cost_usd;
                *state.calls.entry(provider.clone()).or_insert(0) += 1;
                *state
                    .by_model
                    .entry(format!("{provider}/{model}"))
                    .or_insert(0.0) += cost_usd;
                *state.tokens_in.entry(provider.clone()).or_insert(0) += input_tokens;
                *state.tokens_out.entry(provider.clone()).or_insert(0) += output_tokens;
                *state.daily.entry(day.clone()).or_insert(0.0) += cost_usd;
                if let Some(lat) = latency_s {
                    if *lat >= 0.0 {
                        let samples = state.latency_s.entry(provider.clone()).or_default();
                        samples.push_back(*lat);
                        while samples.len() > MAX_LATENCY_SAMPLES {
                            samples.pop_front();
                        }
                    }
                }
            }
            JournalEntry::Budget { provider, usd } => {
                state.budgets.insert(provider.clone(), *usd);
            }
        }
    }

    fn append(&self, entry: &JournalEntry) -> Result<(), GhostError> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.journal_path)?;
        let line = serde_json::to_string(entry)?;
        writeln!(file, "{line}")?;
        file.sync_all()?;
        Ok(())
    }

    /// Record a provider call. Returns the catalog-priced cost in USD.
    ///
    /// Token counts are explicit when provided, else estimated at 4 chars
    /// per token (mirrors Python's `estimate_tokens`).
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        provider: &str,
        model: &str,
        input_text: &str,
        output_text: &str,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        latency_s: Option<f64>,
        price_in_per_1k: f64,
        price_out_per_1k: f64,
    ) -> Result<f64, GhostError> {
        for (label, price) in [("input", price_in_per_1k), ("output", price_out_per_1k)] {
            if !price.is_finite() || price < 0.0 {
                return Err(GhostError::Config(format!(
                    "{label} price for {provider} must be finite and non-negative (got {price})"
                )));
            }
        }
        let in_tokens = input_tokens.unwrap_or_else(|| estimate_tokens(input_text));
        let out_tokens = output_tokens.unwrap_or_else(|| estimate_tokens(output_text));
        let cost = (in_tokens as f64 / 1000.0) * price_in_per_1k
            + (out_tokens as f64 / 1000.0) * price_out_per_1k;
        let entry = JournalEntry::Record {
            provider: provider.to_string(),
            model: model.to_string(),
            cost_usd: cost,
            input_tokens: in_tokens,
            output_tokens: out_tokens,
            latency_s,
            day: Utc::now().format("%Y-%m-%d").to_string(),
        };
        self.append(&entry)?;
        let mut state = self.state.lock().expect("ledger lock");
        Self::apply(&mut state, &entry);
        Ok(cost)
    }

    /// Set a per-provider budget cap (persisted immediately).
    ///
    /// The cap must be a finite, non-negative USD amount: a NaN cap would
    /// make every `spent >= cap` comparison false and silently disable the
    /// guard, so it is refused instead.
    pub fn set_budget(&self, provider: &str, usd: f64) -> Result<(), GhostError> {
        if provider.trim().is_empty() {
            return Err(GhostError::Config(
                "budget provider id is empty".to_string(),
            ));
        }
        if !usd.is_finite() || usd < 0.0 {
            return Err(GhostError::Config(format!(
                "budget for {provider} must be a finite, non-negative USD amount (got {usd})"
            )));
        }
        let entry = JournalEntry::Budget {
            provider: provider.to_string(),
            usd,
        };
        self.append(&entry)?;
        let mut state = self.state.lock().expect("ledger lock");
        Self::apply(&mut state, &entry);
        Ok(())
    }

    /// Fail when `provider` already spent at or above its budget cap.
    /// Providers without a cap always pass.
    pub fn check_budget(&self, provider: &str) -> Result<(), GhostError> {
        let state = self.state.lock().expect("ledger lock");
        if let Some(cap) = state.budgets.get(provider) {
            let spent = state.spend.get(provider).copied().unwrap_or(0.0);
            if spent >= *cap {
                return Err(GhostError::PolicyDenied(format!(
                    "provider {provider} budget exceeded: spent ${spent:.4} of ${cap:.4}"
                )));
            }
        }
        Ok(())
    }

    /// Total spend for a provider in USD.
    pub fn spend(&self, provider: &str) -> f64 {
        self.state
            .lock()
            .expect("ledger lock")
            .spend
            .get(provider)
            .copied()
            .unwrap_or(0.0)
    }

    /// Total calls for a provider.
    pub fn calls(&self, provider: &str) -> u64 {
        self.state
            .lock()
            .expect("ledger lock")
            .calls
            .get(provider)
            .copied()
            .unwrap_or(0)
    }

    /// Total spend across all providers today (UTC).
    pub fn daily_spend(&self) -> f64 {
        let day = Utc::now().format("%Y-%m-%d").to_string();
        self.state
            .lock()
            .expect("ledger lock")
            .daily
            .get(&day)
            .copied()
            .unwrap_or(0.0)
    }

    /// Latency samples for a provider (oldest first, capped).
    pub fn latency_samples(&self, provider: &str) -> Vec<f64> {
        self.state
            .lock()
            .expect("ledger lock")
            .latency_s
            .get(provider)
            .map(|q| q.iter().copied().collect())
            .unwrap_or_default()
    }
}

/// Per-provider totals, as reported by [`CostLedger::snapshot`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProviderSpend {
    /// Provider id.
    pub provider: String,
    /// Total spend in USD.
    pub spend_usd: f64,
    /// Number of journaled calls.
    pub calls: u64,
    /// Total input tokens.
    pub input_tokens: u64,
    /// Total output tokens.
    pub output_tokens: u64,
    /// Budget cap in USD, when one is set.
    pub budget_usd: Option<f64>,
}

impl CostLedger {
    /// Totals for every provider seen in the journal (spend or budget),
    /// sorted by provider id.
    pub fn snapshot(&self) -> Vec<ProviderSpend> {
        let state = self.state.lock().expect("ledger lock");
        let mut names: Vec<&String> = state.spend.keys().chain(state.budgets.keys()).collect();
        names.sort();
        names.dedup();
        names
            .into_iter()
            .map(|name| ProviderSpend {
                provider: name.clone(),
                spend_usd: state.spend.get(name).copied().unwrap_or(0.0),
                calls: state.calls.get(name).copied().unwrap_or(0),
                input_tokens: state.tokens_in.get(name).copied().unwrap_or(0),
                output_tokens: state.tokens_out.get(name).copied().unwrap_or(0),
                budget_usd: state.budgets.get(name).copied(),
            })
            .collect()
    }

    /// Path of the JSONL journal backing this ledger.
    pub fn journal_path(&self) -> &Path {
        &self.journal_path
    }
}

/// Rough token estimate: 4 characters per token (mirrors Python).
fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_ledger() -> (tempfile::TempDir, CostLedger) {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = CostLedger::open(dir.path()).expect("open ledger");
        (dir, ledger)
    }

    #[test]
    fn record_accumulates_spend_and_calls() {
        let (_dir, ledger) = open_ledger();
        let cost = ledger
            .record(
                "nebius",
                "m",
                "hello",
                "world",
                Some(100),
                Some(50),
                Some(0.5),
                1.0,
                2.0,
            )
            .expect("record");
        // (100/1000)*1.0 + (50/1000)*2.0 = 0.1 + 0.1 = 0.2
        assert!((cost - 0.2).abs() < 1e-9, "cost was {cost}");
        assert!((ledger.spend("nebius") - 0.2).abs() < 1e-9);
        assert_eq!(ledger.calls("nebius"), 1);
        assert_eq!(ledger.latency_samples("nebius"), vec![0.5]);
    }

    #[test]
    fn estimates_tokens_when_not_provided() {
        let (_dir, ledger) = open_ledger();
        // 8 chars in, 4 chars out -> 2 + 1 tokens at $1/1k each = 0.003
        let cost = ledger
            .record("x", "m", "12345678", "1234", None, None, None, 1.0, 1.0)
            .expect("record");
        assert!((cost - 0.003).abs() < 1e-9, "cost was {cost}");
    }

    #[test]
    fn budget_blocks_overspend_and_survives_reload() {
        let (dir, ledger) = open_ledger();
        ledger.set_budget("groq", 0.001).expect("set budget");
        ledger
            .record("groq", "m", "", "", Some(10), Some(10), None, 1.0, 1.0)
            .expect("record under budget");
        // (10/1000)*1 + (10/1000)*1 = 0.02 > 0.001 cap
        let err = ledger
            .check_budget("groq")
            .expect_err("must be over budget");
        assert!(matches!(err, GhostError::PolicyDenied(_)));

        // Drop and reload: budget AND spend survive.
        drop(ledger);
        let reloaded = CostLedger::open(dir.path()).expect("reopen");
        assert!((reloaded.spend("groq") - 0.02).abs() < 1e-9);
        assert_eq!(reloaded.calls("groq"), 1);
        assert!(reloaded.check_budget("groq").is_err());
    }

    #[test]
    fn provider_without_budget_always_passes() {
        let (_dir, ledger) = open_ledger();
        ledger
            .record(
                "x",
                "m",
                "",
                "",
                Some(1_000_000),
                Some(0),
                None,
                100.0,
                100.0,
            )
            .expect("record");
        assert!(ledger.check_budget("x").is_ok());
    }

    #[test]
    fn latency_samples_are_bounded() {
        let (_dir, ledger) = open_ledger();
        for i in 0..250 {
            ledger
                .record("p", "m", "", "", Some(1), Some(1), Some(i as f64), 0.0, 0.0)
                .expect("record");
        }
        let samples = ledger.latency_samples("p");
        assert_eq!(samples.len(), MAX_LATENCY_SAMPLES);
        assert_eq!(samples[0], 50.0, "oldest samples evicted first");
    }

    #[test]
    fn daily_spend_tracks_today() {
        let (_dir, ledger) = open_ledger();
        ledger
            .record("a", "m", "", "", Some(1000), Some(0), None, 1.0, 0.0)
            .expect("record");
        ledger
            .record("b", "m", "", "", Some(1000), Some(0), None, 1.0, 0.0)
            .expect("record");
        assert!((ledger.daily_spend() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn invalid_budgets_are_refused_and_not_persisted() {
        let (dir, ledger) = open_ledger();
        for bad in [f64::NAN, f64::INFINITY, -0.01] {
            let err = ledger.set_budget("groq", bad).expect_err("must refuse");
            assert!(matches!(err, GhostError::Config(_)), "{err}");
        }
        assert!(ledger.set_budget("  ", 1.0).is_err());
        assert!(ledger.snapshot().is_empty());
        drop(ledger);
        let reopened = CostLedger::open(dir.path()).unwrap();
        assert!(reopened.snapshot().is_empty(), "nothing journaled");
    }

    #[test]
    fn invalid_prices_are_refused() {
        let (_dir, ledger) = open_ledger();
        for (pin, pout) in [(f64::NAN, 0.0), (0.0, -1.0), (f64::INFINITY, 0.0)] {
            assert!(ledger
                .record("p", "m", "", "", Some(1), Some(1), None, pin, pout)
                .is_err());
        }
        assert_eq!(ledger.calls("p"), 0);
    }

    #[test]
    fn zero_budget_blocks_immediately() {
        let (_dir, ledger) = open_ledger();
        ledger.set_budget("p", 0.0).unwrap();
        assert!(ledger.check_budget("p").is_err());
    }

    #[test]
    fn snapshot_reports_every_provider_sorted() {
        let (_dir, ledger) = open_ledger();
        ledger
            .record("zeta", "m", "", "", Some(1000), Some(500), None, 1.0, 2.0)
            .unwrap();
        ledger.set_budget("alpha", 5.0).unwrap();
        let snap = ledger.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(
            snap[0],
            ProviderSpend {
                provider: "alpha".to_string(),
                spend_usd: 0.0,
                calls: 0,
                input_tokens: 0,
                output_tokens: 0,
                budget_usd: Some(5.0),
            }
        );
        assert_eq!(snap[1].provider, "zeta");
        assert!((snap[1].spend_usd - 2.0).abs() < 1e-9);
        assert_eq!(snap[1].calls, 1);
        assert_eq!(snap[1].input_tokens, 1000);
        assert_eq!(snap[1].output_tokens, 500);
        assert_eq!(snap[1].budget_usd, None);
        assert!(ledger.journal_path().ends_with("cost_ledger.jsonl"));
    }
}
