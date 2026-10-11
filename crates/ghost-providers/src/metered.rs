//! Budget-enforcing, spend-journaling provider wrapper.
//!
//! GAP [durability/H1] was "the durable ledger exists but nothing calls
//! it". [`MeteredProvider`] is the production caller: every model call
//! made through the kernel goes through it, so
//!
//! 1. the provider's budget cap is checked *before* the request is sent
//!    (fail closed: an exhausted budget never reaches the network), and
//! 2. every successful call is appended to the durable [`CostLedger`]
//!    journal (calls, tokens, latency, USD at the configured price).
//!
//! If the spend cannot be journaled the reply is withheld and an error is
//! returned: a ledger that cannot write cannot enforce a budget, so the
//! wrapper fails loud instead of silently under-counting (the
//! `FreeRouter._save` anti-pattern from [durability/M5]).
//!
//! Pricing is explicit, never guessed. Per provider, set
//! `GHOSTCHIMERA_<PROVIDER>_PRICE_IN_PER_1K` and
//! `GHOSTCHIMERA_<PROVIDER>_PRICE_OUT_PER_1K` (USD per 1k tokens;
//! `<PROVIDER>` is the provider id upper-cased with `-` as `_`). Unset
//! prices count as 0.0: calls/tokens/latency are still journaled, but a
//! USD budget only trips once prices are configured.

use std::sync::Arc;
use std::time::Instant;

use crate::error::ProviderError;
use crate::ledger::CostLedger;
use crate::traits::ModelProvider;

/// USD per 1k tokens for one provider.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pricing {
    /// USD per 1k input (prompt) tokens.
    pub in_per_1k: f64,
    /// USD per 1k output (completion) tokens.
    pub out_per_1k: f64,
}

impl Pricing {
    /// Read `GHOSTCHIMERA_<PROVIDER>_PRICE_{IN,OUT}_PER_1K` from the
    /// environment. Missing, unparsable, negative, or non-finite values
    /// fall back to 0.0.
    pub fn from_env(provider: &str) -> Self {
        let prefix = env_prefix(provider);
        Self {
            in_per_1k: read_price(&format!("{prefix}_PRICE_IN_PER_1K")),
            out_per_1k: read_price(&format!("{prefix}_PRICE_OUT_PER_1K")),
        }
    }
}

/// `GHOSTCHIMERA_<PROVIDER>` with the provider id upper-cased and every
/// non-alphanumeric character mapped to `_`.
pub fn env_prefix(provider: &str) -> String {
    let id: String = provider
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("GHOSTCHIMERA_{id}")
}

fn read_price(var: &str) -> f64 {
    parse_price(std::env::var(var).ok().as_deref())
}

/// Parse a price string; anything that is not a finite, non-negative
/// number is treated as unset (0.0).
pub fn parse_price(raw: Option<&str>) -> f64 {
    raw.and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(0.0)
}

/// A provider whose calls are budget-checked and journaled.
pub struct MeteredProvider {
    inner: Arc<dyn ModelProvider>,
    ledger: Arc<CostLedger>,
    pricing: Pricing,
}

impl MeteredProvider {
    /// Wrap `inner`, journaling into `ledger` at `pricing`.
    pub fn new(inner: Arc<dyn ModelProvider>, ledger: Arc<CostLedger>, pricing: Pricing) -> Self {
        Self {
            inner,
            ledger,
            pricing,
        }
    }

    /// Wrap `inner` with pricing read from the environment.
    pub fn from_env(inner: Arc<dyn ModelProvider>, ledger: Arc<CostLedger>) -> Self {
        let pricing = Pricing::from_env(inner.name());
        Self::new(inner, ledger, pricing)
    }

    /// The pricing this wrapper journals at.
    pub fn pricing(&self) -> Pricing {
        self.pricing
    }
}

impl ModelProvider for MeteredProvider {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model(&self) -> &str {
        self.inner.model()
    }

    fn chat(&self, system: &str, user: &str) -> Result<String, ProviderError> {
        // 1. Fail closed BEFORE the request leaves the machine.
        self.ledger
            .check_budget(self.inner.name())
            .map_err(|err| ProviderError::BudgetExceeded(err.to_string()))?;
        // 2. The real call.
        let started = Instant::now();
        let reply = self.inner.chat(system, user)?;
        let latency_s = started.elapsed().as_secs_f64();
        // 3. Journal it durably, or refuse to hand back an unaccounted reply.
        let input_text = format!("{system}\n{user}");
        self.ledger
            .record(
                self.inner.name(),
                self.inner.model(),
                &input_text,
                &reply,
                None,
                None,
                Some(latency_s),
                self.pricing.in_per_1k,
                self.pricing.out_per_1k,
            )
            .map_err(|err| {
                ProviderError::Ledger(format!(
                    "reply withheld: spend could not be journaled: {err}"
                ))
            })?;
        Ok(reply)
    }

    fn validate_config(&self) -> Vec<String> {
        self.inner.validate_config()
    }

    fn is_available(&self) -> bool {
        self.inner.is_available()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Test double: counts calls, replies with a fixed string or fails.
    struct FakeProvider {
        calls: AtomicUsize,
        fail: bool,
    }

    impl FakeProvider {
        fn new(fail: bool) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                fail,
            })
        }
    }

    impl ModelProvider for FakeProvider {
        fn name(&self) -> &str {
            "fake"
        }
        fn model(&self) -> &str {
            "fake-model"
        }
        fn chat(&self, _system: &str, _user: &str) -> Result<String, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(ProviderError::Network("down".to_string()))
            } else {
                Ok("12345678".to_string()) // 8 chars -> 2 estimated tokens
            }
        }
        fn validate_config(&self) -> Vec<String> {
            vec!["fake-issue".to_string()]
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    fn ledger() -> (tempfile::TempDir, Arc<CostLedger>) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(CostLedger::open(dir.path()).unwrap());
        (dir, ledger)
    }

    const PRICE: Pricing = Pricing {
        in_per_1k: 1.0,
        out_per_1k: 2.0,
    };

    #[test]
    fn successful_call_is_journaled_durably() {
        let (dir, ledger) = ledger();
        let fake = FakeProvider::new(false);
        let metered = MeteredProvider::new(fake.clone(), ledger.clone(), PRICE);
        assert_eq!(metered.chat("s", "u").unwrap(), "12345678");
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        assert_eq!(ledger.calls("fake"), 1);
        // input "s\nu" = 3 chars -> 1 token @ $1/1k; output 2 tokens @ $2/1k.
        let expected = 0.001 + 0.004;
        assert!((ledger.spend("fake") - expected).abs() < 1e-12);
        assert_eq!(ledger.latency_samples("fake").len(), 1);
        // Survives a restart: this is the H1 production wiring.
        drop(metered);
        drop(ledger);
        let reopened = CostLedger::open(dir.path()).unwrap();
        assert_eq!(reopened.calls("fake"), 1);
        assert!((reopened.spend("fake") - expected).abs() < 1e-12);
    }

    #[test]
    fn exhausted_budget_blocks_before_the_network() {
        let (_dir, ledger) = ledger();
        ledger.set_budget("fake", 0.0).unwrap();
        let fake = FakeProvider::new(false);
        let metered = MeteredProvider::new(fake.clone(), ledger.clone(), PRICE);
        let err = metered.chat("s", "u").unwrap_err();
        assert!(matches!(err, ProviderError::BudgetExceeded(_)), "{err}");
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0, "no request sent");
        assert_eq!(ledger.calls("fake"), 0);
    }

    #[test]
    fn budget_trips_after_spend_reaches_cap() {
        let (_dir, ledger) = ledger();
        ledger.set_budget("fake", 0.004).unwrap();
        let fake = FakeProvider::new(false);
        let metered = MeteredProvider::new(fake.clone(), ledger.clone(), PRICE);
        metered.chat("s", "u").unwrap(); // spends 0.005 >= 0.004
        assert!(matches!(
            metered.chat("s", "u").unwrap_err(),
            ProviderError::BudgetExceeded(_)
        ));
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_call_is_not_journaled_as_spend() {
        let (_dir, ledger) = ledger();
        let fake = FakeProvider::new(true);
        let metered = MeteredProvider::new(fake.clone(), ledger.clone(), PRICE);
        assert!(matches!(
            metered.chat("s", "u").unwrap_err(),
            ProviderError::Network(_)
        ));
        assert_eq!(ledger.calls("fake"), 0);
        assert_eq!(ledger.spend("fake"), 0.0);
    }

    #[test]
    fn unwritable_ledger_withholds_the_reply() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Arc::new(CostLedger::open(dir.path()).unwrap());
        // Make the journal path a directory so appends fail.
        std::fs::create_dir(dir.path().join("cost_ledger.jsonl")).unwrap();
        let metered = MeteredProvider::new(FakeProvider::new(false), ledger, PRICE);
        let err = metered.chat("s", "u").unwrap_err();
        assert!(matches!(err, ProviderError::Ledger(_)), "{err}");
        assert!(err.to_string().contains("reply withheld"));
    }

    #[test]
    fn delegates_identity_and_config() {
        let (_dir, ledger) = ledger();
        let metered = MeteredProvider::new(FakeProvider::new(false), ledger, PRICE);
        assert_eq!(metered.name(), "fake");
        assert_eq!(metered.model(), "fake-model");
        assert!(metered.is_available());
        assert_eq!(metered.validate_config(), vec!["fake-issue".to_string()]);
        assert_eq!(metered.pricing(), PRICE);
    }

    #[test]
    fn env_prefix_normalizes_provider_ids() {
        assert_eq!(env_prefix("groq"), "GHOSTCHIMERA_GROQ");
        assert_eq!(env_prefix("cloudflare-ai"), "GHOSTCHIMERA_CLOUDFLARE_AI");
        assert_eq!(env_prefix("a.b"), "GHOSTCHIMERA_A_B");
    }

    #[test]
    fn parse_price_rejects_garbage() {
        assert_eq!(parse_price(Some("0.5")), 0.5);
        assert_eq!(parse_price(Some(" 2 ")), 2.0);
        assert_eq!(parse_price(Some("-1")), 0.0);
        assert_eq!(parse_price(Some("NaN")), 0.0);
        assert_eq!(parse_price(Some("inf")), 0.0);
        assert_eq!(parse_price(Some("abc")), 0.0);
        assert_eq!(parse_price(None), 0.0);
    }

    #[test]
    fn pricing_from_env_reads_both_prices() {
        // Unique provider id so no other test races on these vars.
        std::env::set_var("GHOSTCHIMERA_METERTEST_PRICE_IN_PER_1K", "0.25");
        std::env::set_var("GHOSTCHIMERA_METERTEST_PRICE_OUT_PER_1K", "1.5");
        let p = Pricing::from_env("metertest");
        assert_eq!(
            p,
            Pricing {
                in_per_1k: 0.25,
                out_per_1k: 1.5
            }
        );
        assert_eq!(Pricing::from_env("metertest-unset"), Pricing::default());
    }
}
