//! [`crate::ports::sftp_client::SftpClientPort`] adapters.
//!
//! - [`russh_sftp_adapter::RusshSftpAdapter`] — production adapter
//!   wrapping [`russh_sftp::client::SftpSession`] streams behind opaque
//!   in-flight transfer state. Hides every russh / russh-sftp type from
//!   the use case layer.
//!
//! ## SSH handle bridging
//!
//! The port API receives a [`crate::domain::ids::SessionId`] but the
//! russh-sftp library needs an active `russh::client::Handle`. The
//! [`russh_sftp_adapter::SshHandleRegistry`] type carries that lookup as
//! an internal abstraction shared between this adapter and the H6 SSH
//! adapter. The H10 wiring step reconciles the registry into the
//! composition root so both adapters reference the same instance.

pub mod russh_sftp_adapter;
