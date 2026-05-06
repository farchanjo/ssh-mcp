//! Hexagonal ports: trait skeletons that decouple use cases from concrete
//! adapters.
//!
//! ## Rules
//!
//! - Method signatures must not reference `tokio`, `russh`, `rmcp`, `axum`,
//!   or `dashmap`. Allowed runtime types are limited to `std::sync::Arc`,
//!   `std::time::{Duration, Instant}`, `bytes::Bytes`, plus `crate::domain`
//!   types and `trait_variant` machinery.
//! - Async ports are declared with `#[trait_variant::make(Port: Send)]` so
//!   the compiler emits both an AFIT version (kept private) and a
//!   `Send`-bounded re-export (the public alias). Use cases consume the
//!   `Send`-bounded alias as a static-dispatch generic parameter; adapters
//!   are injected at the composition root via concrete type aliases.
//! - Sync ports stay as plain dyn-safe traits so registries (`PeerHandle`,
//!   `ClockPort`, etc.) can be erased behind `Arc<dyn Port>` when needed.

pub mod auth_strategy;
pub mod channel_mux;
pub mod clock;
pub mod command_repo;
pub mod config;
pub mod forward_repo;
pub mod id_generator;
pub mod lifecycle_policy;
pub mod notifier;
pub mod output_stream;
pub mod rsync_repo;
pub mod rsync_sftp_fs;
pub mod rsync_transport;
pub mod session_repo;
pub mod sftp_client;
pub mod shell_repo;
pub mod ssh_client;
pub mod subscriber_lane;
pub mod subscriber_registry;
pub mod transfer_repo;
