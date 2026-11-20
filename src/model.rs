mod adapters;
mod client;
mod protocol;

#[cfg(not(unix))]
pub(crate) use adapters::UNSUPPORTED;
#[cfg(unix)]
pub(crate) use adapters::decision;
#[cfg(unix)]
pub use adapters::decision::{serve, status, stop_all};
pub(crate) use adapters::{ADAPTER_SESSION, Reference};
#[cfg(unix)]
pub(crate) use adapters::{DEFAULT_CLAUDE_MODEL, is_claude};
#[cfg(unix)]
pub(crate) use client::timed_out;
pub use client::{CONCURRENCY, Client};
pub use protocol::{Answer, Answers, Question, Questions};
