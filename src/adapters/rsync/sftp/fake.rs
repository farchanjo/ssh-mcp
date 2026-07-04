//! In-memory [`crate::ports::rsync_sftp_fs::RsyncSftpFsPort`] fake.
//!
//! Models a remote filesystem as a single [`dashmap::DashMap`] keyed by
//! absolute path; entries store kind / contents / metadata / link
//! targets. Everything is `DashMap` + atomics — no `Mutex`.
//!
//! The fake is used by:
//!
//! - the [`super::walker`] / [`super::comparator`] / [`super::executor`]
//!   unit tests in this slice;
//! - downstream integration tests under `--features test-fixtures`.
//!
//! Behaviour mirrors a POSIX SFTP server narrowly enough that the
//! executor exercises every interesting branch (mkdir / rmdir /
//! `remove_file` / `symlink` / `setstat` / chunked read+write).

#![cfg(any(test, feature = "test-fixtures"))]
#![allow(
    dead_code,
    reason = "fake adapter exposes scripted helpers that are used selectively per test scenario"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use bytes::{Bytes, BytesMut};
use dashmap::DashMap;

use crate::domain::error::DomainError;
use crate::domain::ids::SessionId;
use crate::ports::rsync_sftp_fs::{RemoteDirEntry, RemoteMetadata, RsyncSftpFsPort};

/// Kind of in-memory entry the fake stores.
#[derive(Debug, Clone)]
enum Entry {
    /// Regular file — owns its bytes.
    File { bytes: Bytes },
    /// Directory — children are tracked by path prefix in the parent
    /// map; the entry exists so `lstat` returns metadata.
    Dir,
    /// Symlink — carries the link target (preserved verbatim).
    Symlink { target: String },
}

/// Stored entry: kind + metadata.
#[derive(Debug, Clone)]
struct Node {
    kind: Entry,
    meta: RemoteMetadata,
}

impl Node {
    fn new_file(bytes: Bytes, mode: u32, mtime: i64, uid: u32, gid: u32) -> Self {
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        Self {
            kind: Entry::File { bytes },
            meta: RemoteMetadata {
                size,
                mode,
                mtime,
                uid,
                gid,
                is_dir: false,
                is_symlink: false,
            },
        }
    }

    const fn new_dir(mode: u32, mtime: i64, uid: u32, gid: u32) -> Self {
        Self {
            kind: Entry::Dir,
            meta: RemoteMetadata {
                size: 0,
                mode,
                mtime,
                uid,
                gid,
                is_dir: true,
                is_symlink: false,
            },
        }
    }

    fn new_symlink(target: String, mtime: i64, uid: u32, gid: u32) -> Self {
        let size = u64::try_from(target.len()).unwrap_or(u64::MAX);
        Self {
            kind: Entry::Symlink { target },
            meta: RemoteMetadata {
                size,
                mode: 0o777,
                mtime,
                uid,
                gid,
                is_dir: false,
                is_symlink: true,
            },
        }
    }
}

/// Scriptable knobs the tests use to inject errors / probe the wire.
#[derive(Debug, Default)]
struct Knobs {
    /// When set, every `symlink` call fails with [`DomainError::Sftp`].
    symlink_unsupported: AtomicBool,
    /// When set, every `set_metadata` call fails with
    /// [`DomainError::Sftp`].
    setstat_unsupported: AtomicBool,
    /// When set, every `mkdir` call fails with [`DomainError::Sftp`],
    /// simulating an infrastructure failure (permission denied, no
    /// writable home dir) unrelated to the path already existing.
    mkdir_unsupported: AtomicBool,
    /// Counter incremented on every `write_chunk` call (debugging).
    write_calls: AtomicU32,
}

/// Inner state shared by every clone of [`FakeRsyncSftpFs`].
#[derive(Debug, Default)]
struct Inner {
    nodes: DashMap<String, Node>,
    knobs: Knobs,
}

/// In-memory remote filesystem fake.
#[derive(Debug, Default, Clone)]
pub struct FakeRsyncSftpFs {
    inner: Arc<Inner>,
}

impl FakeRsyncSftpFs {
    /// Build an empty fake.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a directory at `path`. Intermediate components must already
    /// exist.
    pub fn put_dir(&self, path: &str, mode: u32) {
        self.inner
            .nodes
            .insert(path.to_string(), Node::new_dir(mode, 0, 0, 0));
    }

    /// Seed a regular file at `path` with `bytes`.
    pub fn put_file(&self, path: &str, bytes: &[u8], mode: u32, mtime: i64) {
        self.inner.nodes.insert(
            path.to_string(),
            Node::new_file(Bytes::copy_from_slice(bytes), mode, mtime, 0, 0),
        );
    }

    /// Seed a symbolic link `link_path -> target`.
    pub fn put_symlink(&self, link_path: &str, target: &str) {
        self.inner.nodes.insert(
            link_path.to_string(),
            Node::new_symlink(target.to_string(), 0, 0, 0),
        );
    }

    /// Inject `OP_UNSUPPORTED` style failure on every future
    /// `symlink` call.
    pub fn fail_symlink(&self) {
        self.inner
            .knobs
            .symlink_unsupported
            .store(true, Ordering::Release);
    }

    /// Inject failure on every future `set_metadata` call.
    pub fn fail_setstat(&self) {
        self.inner
            .knobs
            .setstat_unsupported
            .store(true, Ordering::Release);
    }

    /// Inject failure on every future `mkdir` call, regardless of
    /// whether the target path already exists.
    pub fn fail_mkdir(&self) {
        self.inner
            .knobs
            .mkdir_unsupported
            .store(true, Ordering::Release);
    }

    /// Read back the bytes stored at `path`. `None` when the entry is
    /// missing or not a regular file.
    #[must_use]
    pub fn get_file(&self, path: &str) -> Option<Bytes> {
        let node = self.inner.nodes.get(path)?;
        match &node.kind {
            Entry::File { bytes } => Some(bytes.clone()),
            Entry::Dir | Entry::Symlink { .. } => None,
        }
    }

    /// Read back the metadata for `path`. `None` when missing.
    #[must_use]
    pub fn get_meta(&self, path: &str) -> Option<RemoteMetadata> {
        self.inner.nodes.get(path).map(|n| n.meta)
    }

    /// `true` when `path` exists in any form.
    #[must_use]
    pub fn exists(&self, path: &str) -> bool {
        self.inner.nodes.contains_key(path)
    }

    /// Total stored entries (debugging).
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.nodes.len()
    }

    /// `true` when the fake has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.nodes.is_empty()
    }

    /// Number of `write_chunk` invocations against the fake.
    #[must_use]
    pub fn write_calls(&self) -> u32 {
        self.inner.knobs.write_calls.load(Ordering::Acquire)
    }
}

fn join(parent: &str, child: &str) -> String {
    if parent.ends_with('/') {
        format!("{parent}{child}")
    } else {
        format!("{parent}/{child}")
    }
}

fn child_name<'a>(parent: &str, candidate: &'a str) -> Option<&'a str> {
    let prefix = if parent.ends_with('/') {
        parent.to_string()
    } else {
        format!("{parent}/")
    };
    let tail = candidate.strip_prefix(&prefix)?;
    if tail.is_empty() || tail.contains('/') {
        None
    } else {
        Some(tail)
    }
}

/// Merge a setstat payload onto existing node metadata, honouring the `0`
/// skip-sentinel for every field the caller opted out of via `PreserveFlags`
/// (mirrors production `remote_meta_to_attrs` skip-on-zero). Size and the
/// kind flags are never carried by a setstat, so they stay untouched.
const fn merge_setstat_meta(existing: RemoteMetadata, incoming: RemoteMetadata) -> RemoteMetadata {
    RemoteMetadata {
        size: existing.size,
        mode: if incoming.mode == 0 {
            existing.mode
        } else {
            incoming.mode
        },
        mtime: if incoming.mtime == 0 {
            existing.mtime
        } else {
            incoming.mtime
        },
        uid: if incoming.uid == 0 {
            existing.uid
        } else {
            incoming.uid
        },
        gid: if incoming.gid == 0 {
            existing.gid
        } else {
            incoming.gid
        },
        is_dir: existing.is_dir,
        is_symlink: existing.is_symlink,
    }
}

impl RsyncSftpFsPort for FakeRsyncSftpFs {
    async fn readdir(
        &self,
        _session_id: &SessionId,
        path: &str,
    ) -> Result<Vec<RemoteDirEntry>, DomainError> {
        let parent = self
            .inner
            .nodes
            .get(path)
            .ok_or_else(|| DomainError::Sftp(format!("missing dir: {path}")))?;
        if !matches!(parent.kind, Entry::Dir) {
            return Err(DomainError::Sftp(format!("not a directory: {path}")));
        }
        drop(parent);
        let mut out = Vec::new();
        for entry in &self.inner.nodes {
            let key = entry.key();
            if let Some(name) = child_name(path, key) {
                let meta = entry.meta;
                out.push(RemoteDirEntry {
                    name: name.to_string(),
                    is_dir: meta.is_dir,
                    is_symlink: meta.is_symlink,
                    size: meta.size,
                    mode: meta.mode,
                    mtime: meta.mtime,
                    uid: meta.uid,
                    gid: meta.gid,
                });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn lstat(
        &self,
        _session_id: &SessionId,
        path: &str,
    ) -> Result<RemoteMetadata, DomainError> {
        let node = self
            .inner
            .nodes
            .get(path)
            .ok_or_else(|| DomainError::Sftp(format!("missing path: {path}")))?;
        Ok(node.meta)
    }

    async fn read_link(&self, _session_id: &SessionId, path: &str) -> Result<String, DomainError> {
        let node = self
            .inner
            .nodes
            .get(path)
            .ok_or_else(|| DomainError::Sftp(format!("missing link: {path}")))?;
        match &node.kind {
            Entry::Symlink { target } => Ok(target.clone()),
            Entry::File { .. } | Entry::Dir => {
                Err(DomainError::Sftp(format!("not a symlink: {path}")))
            }
        }
    }

    async fn mkdir(
        &self,
        _session_id: &SessionId,
        path: &str,
        mode: u32,
    ) -> Result<(), DomainError> {
        if self.inner.knobs.mkdir_unsupported.load(Ordering::Acquire) {
            return Err(DomainError::Sftp("mkdir unsupported".to_string()));
        }
        if self.inner.nodes.contains_key(path) {
            return Err(DomainError::Sftp(format!("path exists: {path}")));
        }
        self.inner
            .nodes
            .insert(path.to_string(), Node::new_dir(mode, 0, 0, 0));
        Ok(())
    }

    async fn rmdir(&self, _session_id: &SessionId, path: &str) -> Result<(), DomainError> {
        let Some(node) = self.inner.nodes.get(path) else {
            return Err(DomainError::Sftp(format!("missing path: {path}")));
        };
        if !matches!(node.kind, Entry::Dir) {
            return Err(DomainError::Sftp(format!("not a directory: {path}")));
        }
        drop(node);
        // Refuse to drop a non-empty directory — mirrors POSIX `rmdir`.
        for entry in &self.inner.nodes {
            if child_name(path, entry.key()).is_some() {
                return Err(DomainError::Sftp(format!("non-empty: {path}")));
            }
        }
        self.inner.nodes.remove(path);
        Ok(())
    }

    async fn remove_file(&self, _session_id: &SessionId, path: &str) -> Result<(), DomainError> {
        let Some(node) = self.inner.nodes.get(path) else {
            return Err(DomainError::Sftp(format!("missing path: {path}")));
        };
        if matches!(node.kind, Entry::Dir) {
            return Err(DomainError::Sftp(format!("is a directory: {path}")));
        }
        drop(node);
        self.inner.nodes.remove(path);
        Ok(())
    }

    async fn symlink(
        &self,
        _session_id: &SessionId,
        target: &str,
        link_path: &str,
    ) -> Result<(), DomainError> {
        if self.inner.knobs.symlink_unsupported.load(Ordering::Acquire) {
            return Err(DomainError::Sftp("symlink unsupported".to_string()));
        }
        if self.inner.nodes.contains_key(link_path) {
            return Err(DomainError::Sftp(format!("path exists: {link_path}")));
        }
        self.inner.nodes.insert(
            link_path.to_string(),
            Node::new_symlink(target.to_string(), 0, 0, 0),
        );
        Ok(())
    }

    async fn set_metadata(
        &self,
        _session_id: &SessionId,
        path: &str,
        meta: RemoteMetadata,
    ) -> Result<(), DomainError> {
        if self.inner.knobs.setstat_unsupported.load(Ordering::Acquire) {
            return Err(DomainError::Sftp("setstat unsupported".to_string()));
        }
        let Some(mut node) = self.inner.nodes.get_mut(path) else {
            return Err(DomainError::Sftp(format!("missing path: {path}")));
        };
        node.meta = merge_setstat_meta(node.meta, meta);
        Ok(())
    }

    async fn read_chunk(
        &self,
        _session_id: &SessionId,
        path: &str,
        offset: u64,
        len: usize,
    ) -> Result<Bytes, DomainError> {
        let node = self
            .inner
            .nodes
            .get(path)
            .ok_or_else(|| DomainError::Sftp(format!("missing path: {path}")))?;
        match &node.kind {
            Entry::File { bytes } => {
                let start = usize::try_from(offset).unwrap_or(usize::MAX);
                if start >= bytes.len() {
                    return Ok(Bytes::new());
                }
                let end = start.saturating_add(len).min(bytes.len());
                Ok(bytes.slice(start..end))
            }
            Entry::Dir | Entry::Symlink { .. } => {
                Err(DomainError::Sftp(format!("not a file: {path}")))
            }
        }
    }

    async fn write_chunk(
        &self,
        _session_id: &SessionId,
        path: &str,
        offset: u64,
        data: Bytes,
    ) -> Result<(), DomainError> {
        self.inner.knobs.write_calls.fetch_add(1, Ordering::AcqRel);
        let mut buf = self.load_buf(path)?;
        splice_buf(&mut buf, offset, &data);
        let bytes = buf.freeze();
        self.inner
            .nodes
            .insert(path.to_string(), Node::new_file(bytes, 0o644, 0, 0, 0));
        Ok(())
    }
}

impl FakeRsyncSftpFs {
    fn load_buf(&self, path: &str) -> Result<BytesMut, DomainError> {
        match self.inner.nodes.get(path) {
            Some(n) => match &n.kind {
                Entry::File { bytes } => Ok(BytesMut::from(bytes.as_ref())),
                Entry::Dir | Entry::Symlink { .. } => {
                    Err(DomainError::Sftp(format!("not a file: {path}")))
                }
            },
            None => Ok(BytesMut::new()),
        }
    }
}

fn splice_buf(buf: &mut BytesMut, offset: u64, data: &Bytes) {
    let target_offset = usize::try_from(offset).unwrap_or(usize::MAX);
    if target_offset == 0 {
        *buf = BytesMut::from(data.as_ref());
        return;
    }
    if buf.len() < target_offset {
        buf.resize(target_offset, 0);
    }
    let tail_start = target_offset.saturating_add(data.len());
    if buf.len() < tail_start {
        buf.resize(tail_start, 0);
    }
    buf[target_offset..tail_start].copy_from_slice(data);
}

#[cfg(test)]
mod tests {
    use super::FakeRsyncSftpFs;
    use crate::domain::ids::SessionId;
    use crate::ports::rsync_sftp_fs::RsyncSftpFsPort;
    use bytes::Bytes;

    fn s() -> SessionId {
        SessionId::new("sess-fs".to_string())
    }

    #[tokio::test]
    async fn readdir_yields_seeded_children_sorted() {
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/root", 0o755);
        fs.put_file("/root/b.txt", b"b", 0o644, 0);
        fs.put_file("/root/a.txt", b"a", 0o644, 0);
        let entries = fs.readdir(&s(), "/root").await.expect("readdir");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[1].name, "b.txt");
    }

    #[tokio::test]
    async fn write_chunk_creates_file_at_offset_zero() {
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/root", 0o755);
        fs.write_chunk(&s(), "/root/f.txt", 0, Bytes::from_static(b"hello"))
            .await
            .expect("write");
        assert_eq!(
            fs.get_file("/root/f.txt"),
            Some(Bytes::from_static(b"hello"))
        );
    }

    #[tokio::test]
    async fn write_chunk_appends_at_offset() {
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/root", 0o755);
        fs.write_chunk(&s(), "/root/f.txt", 0, Bytes::from_static(b"hello"))
            .await
            .expect("first");
        fs.write_chunk(&s(), "/root/f.txt", 5, Bytes::from_static(b" world"))
            .await
            .expect("second");
        assert_eq!(
            fs.get_file("/root/f.txt"),
            Some(Bytes::from_static(b"hello world"))
        );
    }

    #[tokio::test]
    async fn read_chunk_returns_slice_at_offset() {
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/root", 0o755);
        fs.put_file("/root/f.txt", b"hello world", 0o644, 0);
        let chunk = fs
            .read_chunk(&s(), "/root/f.txt", 6, 5)
            .await
            .expect("read");
        assert_eq!(chunk.as_ref(), b"world");
    }

    #[tokio::test]
    async fn rmdir_refuses_non_empty() {
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/root", 0o755);
        fs.put_dir("/root/sub", 0o755);
        fs.put_file("/root/sub/leaf", b"x", 0o644, 0);
        let err = fs.rmdir(&s(), "/root/sub").await.expect_err("must err");
        assert!(format!("{err}").contains("non-empty"));
    }

    #[tokio::test]
    async fn symlink_unsupported_knob_fails_request() {
        let fs = FakeRsyncSftpFs::new();
        fs.fail_symlink();
        let err = fs
            .symlink(&s(), "/target", "/link")
            .await
            .expect_err("must err");
        assert!(format!("{err}").contains("symlink unsupported"));
    }

    #[tokio::test]
    async fn setstat_unsupported_knob_fails_request() {
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/root", 0o755);
        fs.fail_setstat();
        let meta = fs.get_meta("/root").expect("meta");
        let err = fs
            .set_metadata(&s(), "/root", meta)
            .await
            .expect_err("must err");
        assert!(format!("{err}").contains("setstat unsupported"));
    }
}
