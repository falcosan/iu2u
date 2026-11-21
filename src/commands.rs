#[cfg(unix)]
mod act;
mod filter;

#[cfg(unix)]
mod init;

#[cfg(unix)]
pub use act::{Outcome, Reporter, Suite, act};
pub use filter::filter;

pub const EXIT_FAILED: u8 = 1;
pub const EXIT_ERROR: u8 = 2;
#[cfg(unix)]
pub use init::init;
