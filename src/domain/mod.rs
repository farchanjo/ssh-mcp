//! Pure domain layer for the v4 hexagonal core.
//!
//! Modules under `domain/` MUST NOT import `tokio`, `russh`, `rmcp`, `axum`,
//! or `dashmap`. Allowed dependencies are limited to `std`, `serde`,
//! `serde_json`, `chrono`, `thiserror`, `schemars`, and `bytes` so the layer
//! can be exercised by adapter-free unit tests and reused across binaries.

pub mod auth;
pub mod command;
pub mod error;
pub mod events;
pub mod forward;
pub mod identity;
pub mod ids;
pub mod keys;
pub mod lifecycle;
pub mod policy;
pub mod ringbuffer;
pub mod session;
pub mod shell;
pub mod subscription;
pub mod transfer;
