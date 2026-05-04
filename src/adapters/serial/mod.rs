//! Serial / UART / TTY / COM adapter (v5.2 — ADR 0009).
//!
//! Lock-free hot path: `history` lives on `ArcSwap<RingBuffer>`, writes
//! are funnelled through a bounded `mpsc` so subscribers never contend
//! with the OS-side serial read loop, and the per-port reader appends
//! bytes to the history via `ArcSwap::rcu` (no `Mutex`). The
//! subscription pipeline (`SUBSCRIPTION_REGISTRY` v4 / `MemoryRegistry`
//! v5 lane) sees serial-port output via the same `ResourceKind::Serial`
//! / `serial://<id>/output` URI as any other push resource — including
//! the ADR 0006 Amendment 1 byte-threshold flush.

#![allow(
    clippy::module_name_repetitions,
    reason = "Serial* prefix mirrors the existing Shell* / Command* aggregates"
)]

pub mod state;
