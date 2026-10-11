//! Ghost Console HTTP API.
//!
//! Axum server exposing console status, the conversation loop, the trust
//! journal, and standing orders. Security posture: `serve()` fails closed
//! (GAP [security/C1]) — non-loopback binds require a bearer token.

pub mod orders;
pub mod routes;
pub mod server;
pub mod state;

pub use orders::{Order, OrderRunOutcome, OrderRunResult, StandingOrderStore};
pub use routes::{build_kernel, build_kernel_async, build_offline_kernel, open_ledger};
pub use server::{is_loopback_host, router, serve};
pub use state::AppState;
