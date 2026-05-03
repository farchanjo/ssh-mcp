//! Concrete adapters for [`crate::ports::output_stream::OutputStreamPort`].
//!
//! The single production adapter, [`russh_output::RusshOutputAdapter`],
//! snapshots the lock-free per-command and per-shell records owned by
//! the [`crate::adapters::ssh::russh_adapter::RusshAdapter`]. Both
//! adapters share the same `Arc<DashMap>` instances so there is exactly
//! one source of truth for in-flight async commands and open PTY
//! shells.
//!
//! ## Module shape
//!
//! - `russh_output` — production adapter that consumes
//!   [`crate::adapters::ssh::russh_adapter::RusshAdapter::command_table`]
//!   and [`crate::adapters::ssh::russh_adapter::RusshAdapter::shell_table`]
//!   to surface a lock-free [`crate::ports::output_stream::OutputSnapshot`]
//!   per call.
//!
//! Alternate backends (e.g. a fake registry for tests that does not
//! need a russh handle) land as additional submodules without touching
//! this layout.

pub mod russh_output;
