//! Shared application state for the console API.
//!
//! `AppState` is `Clone` via `Arc` interiors so every axum handler can take
//! it by value.

use std::sync::Arc;

use ghost_core::{GhostConfig, GhostError};
use ghost_trust::TrustRuntimeStore;

use crate::orders::StandingOrderStore;

/// Shared state for all console API handlers.
#[derive(Clone)]
pub struct AppState {
    /// Hash-chained trust-runtime journal.
    pub trust: Arc<TrustRuntimeStore>,
    /// Standing-order store.
    pub orders: Arc<StandingOrderStore>,
    /// Resolved runtime configuration.
    pub config: Arc<GhostConfig>,
    /// Console bearer token. `None`/empty means open (loopback dev
    /// convenience); see the auth middleware in `server.rs`.
    pub token: Option<String>,
    /// Crate version, served on `/api/console/status`.
    pub version: &'static str,
}

impl AppState {
    /// Build state from a config and an optional console token.
    pub fn new(config: GhostConfig, token: Option<String>) -> Result<Self, GhostError> {
        Ok(Self {
            trust: Arc::new(TrustRuntimeStore::new(&config.state_dir)?),
            orders: Arc::new(StandingOrderStore::new(&config.state_dir)?),
            config: Arc::new(config),
            token,
            version: env!("CARGO_PKG_VERSION"),
        })
    }
}
