//! Per-peer experimental client-capability adapters (ADR 0012 Phase 3).
//!
//! Houses the lock-free [`registry::CapabilityRegistry`] tracking
//! `experimental.ssh_inline_push` (and future v7.1+ flags) per
//! connected MCP peer.
//!
//! Crate-wide `clippy::pub_use` is denied; consumers import directly
//! from [`registry`] (e.g.
//! `crate::adapters::capability::registry::CapabilityRegistry`).

pub mod registry;
