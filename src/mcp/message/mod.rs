//! Message building utilities for MCP responses.
//!
//! This module provides builder patterns for constructing human-readable
//! messages that help LLMs remember important identifiers and understand
//! available operations.

mod builder;

pub use builder::{
    AgentDisconnectMessageBuilder, ConnectMessageBuilder, ExecuteMessageBuilder,
    ShellOpenMessageBuilder,
};
