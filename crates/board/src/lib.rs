//! Board server internals, exposed as a library so integration tests can drive
//! the same HTTP surface and collector wiring the `board` binary uses.

pub mod http;
pub mod runtime;

pub use http::{router, AppState};
pub use runtime::{init, Started};
