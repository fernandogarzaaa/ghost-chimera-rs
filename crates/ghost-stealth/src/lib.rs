//! Stealth Loop: ambient event fabric, host adapters, learning.
//!
//! Mirrors `ghostchimera/stealth/`. The loop pipeline is:
//! EVENT → UNDERSTAND → UPDATE STATE → RECALL EXPERIENCE → MATCH WORKFLOW
//! → PREDICT → DECIDE → PREPARE/INJECT/ACT → OBSERVE OUTCOME → LEARN.

pub mod bus;
pub mod event;
pub mod hook;
pub mod hosts;
pub mod stealth_loop;

pub use bus::EventBus;
pub use event::{new_event, Event, NewEventArgs};
pub use hosts::{AdapterRegistry, HostAdapter, InstallPayload};
pub use stealth_loop::{Intervention, InterventionOutcome, StealthLoop};
