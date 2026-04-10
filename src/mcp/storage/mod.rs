//! Storage abstractions for session and command management.
//!
//! This module provides trait-based storage abstractions that enable:
//! - Dependency injection for testability
//! - Lock-free concurrent access via `DashMap` implementations
//! - Clean separation between storage and business logic

pub mod command;
pub mod session;
pub mod shell;
pub mod traits;
pub mod transfer;
