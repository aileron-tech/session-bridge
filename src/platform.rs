//! Operating-system adapters: local IPC endpoints, process identity, private
//! files, and termination signals. Unix uses owner-only Unix sockets; Windows
//! uses named pipes restricted to the current user.
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
