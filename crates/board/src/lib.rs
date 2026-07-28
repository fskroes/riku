//! Board server internals, exposed as a library so integration tests can drive
//! the same HTTP surface and collector wiring the `board` binary uses.

pub mod http;
pub mod open;
pub mod recap;
pub mod runtime;

pub use http::{router, AppState};
pub use open::{Launcher, TerminalLauncher};
pub use recap::{
    recap, CardJournal, CardResume, OlderJournal, OlderResume, Recap, RecapCard, OLDER_LIMIT,
};
pub use runtime::{init, Started};
