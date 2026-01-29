//! Storage abstractions for session and command management.
//!
//! This module provides trait-based storage abstractions that enable:
//! - Dependency injection for testability
//! - Lock-free concurrent access via DashMap implementations
//! - Clean separation between storage and business logic

mod command;
mod session;
mod shell;
mod traits;

pub use command::COMMAND_STORAGE;
#[allow(unused_imports)]
pub use command::DashMapCommandStorage;
#[allow(unused_imports)]
pub use session::DashMapSessionStorage;
pub use session::SESSION_STORAGE;
#[allow(unused_imports)]
pub use shell::DashMapShellStorage;
pub use shell::SHELL_STORAGE;
pub use shell::ShellStorage;
pub use traits::{CommandStorage, SessionStorage};
