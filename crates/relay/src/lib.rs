//! Team/multi-machine transport for the Agent Board (C7).
//!
//! Two roles, one wire protocol:
//!
//! - The **Relay** ([`server`]) is a lightweight process run once, anywhere
//!   reachable. It fans in live `Event`s from every connected Collector and fans
//!   them out to any subscribing board, holding only in-memory state (ADR 0004).
//! - A **Collector** ([`collect`]) runs headless on a machine, watches that
//!   machine's Agent Sessions (reusing the `collector` crate), and pushes them to
//!   the Relay.
//!
//! The board subscribes with [`subscribe`], feeding remote sessions into the same
//! path its local watcher uses — so remote and local cards travel one identical
//! pipeline (no second rendering path). Everything is one-way, read-only state
//! transport: no command ever flows back to a session (ADR 0002).

pub mod collect;
pub mod server;
pub mod subscribe;
pub mod wire;

pub use collect::{run as run_collector, CollectorConfig};
pub use server::{router, run as run_relay, RelayState};
pub use subscribe::{subscribe, Update};
