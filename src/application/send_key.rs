//! Use case stub — implementation lands in subsequent etapa.
//!
//! Scaffold file created so parallel agents can populate this module without
//! racing each other on `mod.rs`. The orchestrator reserves the module path
//! upfront; the implementing agent fills in the request DTO, outcome enum,
//! struct, and `execute` method following the [`crate::application::connect_session`]
//! canary pattern.

#![allow(
    dead_code,
    reason = "scaffold awaiting implementation in upcoming etapa"
)]

/// Placeholder marker. Replaced by the real use case struct.
#[derive(Debug, Default, Clone, Copy)]
pub struct Placeholder;
