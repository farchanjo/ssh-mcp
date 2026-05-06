//! Recursive remote directory walker for the SFTP rsync transport.
//!
//! BFS-style traversal driven by [`crate::ports::rsync_sftp_fs::RsyncSftpFsPort`]
//! `readdir` + `lstat`. Each entry is converted into an [`RsyncEntry`]
//! carrying the relative path (POSIX `/`-separated) plus the metadata
//! the comparator needs to decide skip / upload / setstat.
//!
//! Filtering: gitignore-style include / exclude using [`globset`]. Match
//! semantics:
//!
//! - `excludes` matched first; matched entries are dropped.
//! - `includes`, when non-empty, override an exclude — the entry is
//!   restored.
//! - Excluded directories are NOT recursed into unless an include
//!   pattern brings them back.
//!
//! File-list cap: refuse with [`DomainError::RsyncFileListTooLarge`]
//! once the gathered list grows beyond `file_list_limit` entries.

use std::sync::Arc;

use globset::GlobSet;

use crate::adapters::rsync::types::FileKind;
use crate::domain::error::DomainError;
use crate::domain::ids::SessionId;
use crate::ports::rsync_sftp_fs::{RemoteDirEntry, RsyncSftpFsPort};

/// One entry in the walked tree. The relative path is always
/// `/`-separated and never starts with a slash; the empty string
/// represents the walk root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RsyncEntry {
    /// Path relative to the walk root.
    pub rel_path: String,
    /// Kind of filesystem entry.
    pub kind: FileKind,
    /// Size in bytes (`0` for directories / symlinks).
    pub size: u64,
    /// Modification time in unix seconds.
    pub mtime: i64,
    /// POSIX mode bits.
    pub mode: u32,
    /// Numeric owner uid.
    pub uid: u32,
    /// Numeric owner gid.
    pub gid: u32,
    /// Symlink target when [`Self::kind`] is [`FileKind::Symlink`].
    pub link_target: Option<String>,
}

/// Walker driven by an [`RsyncSftpFsPort`].
#[derive(Debug)]
pub struct SftpWalker<F> {
    fs: Arc<F>,
    excludes: GlobSet,
    includes: GlobSet,
    file_list_limit: u64,
}

impl<F> SftpWalker<F>
where
    F: RsyncSftpFsPort + 'static,
{
    /// Build a walker.
    #[must_use]
    pub const fn new(
        fs: Arc<F>,
        excludes: GlobSet,
        includes: GlobSet,
        file_list_limit: u64,
    ) -> Self {
        Self {
            fs,
            excludes,
            includes,
            file_list_limit,
        }
    }

    /// Walk `root` recursively.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Sftp`] for any underlying SFTP error and
    /// [`DomainError::RsyncFileListTooLarge`] when the entry budget is
    /// exhausted.
    pub async fn walk(
        &self,
        session_id: &SessionId,
        root: &str,
    ) -> Result<Vec<RsyncEntry>, DomainError> {
        let mut out: Vec<RsyncEntry> = Vec::new();
        let mut worklist: Vec<(String, String)> = vec![(root.to_string(), String::new())];
        while let Some((abs_path, rel_path)) = worklist.pop() {
            self.walk_one_dir(session_id, &abs_path, &rel_path, &mut out, &mut worklist)
                .await?;
        }
        Ok(out)
    }

    async fn walk_one_dir(
        &self,
        session_id: &SessionId,
        abs_path: &str,
        rel_path: &str,
        out: &mut Vec<RsyncEntry>,
        worklist: &mut Vec<(String, String)>,
    ) -> Result<(), DomainError> {
        let entries = self.fs.readdir(session_id, abs_path).await?;
        for entry in entries {
            let child_rel = join_rel(rel_path, &entry.name);
            if !self.is_included(&child_rel, entry.is_dir) {
                continue;
            }
            let child_abs = join_abs(abs_path, &entry.name);
            let rsync_entry = self
                .build_entry(session_id, &child_abs, &child_rel, &entry)
                .await?;
            let is_dir = matches!(rsync_entry.kind, FileKind::Directory);
            out.push(rsync_entry);
            if u64::try_from(out.len()).unwrap_or(u64::MAX) > self.file_list_limit {
                return Err(DomainError::RsyncFileListTooLarge {
                    limit: self.file_list_limit,
                });
            }
            if is_dir {
                worklist.push((child_abs, child_rel));
            }
        }
        Ok(())
    }

    fn is_included(&self, rel_path: &str, _is_dir: bool) -> bool {
        let excluded = !self.excludes.is_empty() && self.excludes.is_match(rel_path);
        if !excluded {
            return true;
        }
        // Excluded — but a non-empty include set may rescue it.
        if self.includes.is_empty() {
            return false;
        }
        self.includes.is_match(rel_path)
    }

    async fn build_entry(
        &self,
        session_id: &SessionId,
        abs_path: &str,
        rel_path: &str,
        dir_entry: &RemoteDirEntry,
    ) -> Result<RsyncEntry, DomainError> {
        let kind = if dir_entry.is_symlink {
            FileKind::Symlink
        } else if dir_entry.is_dir {
            FileKind::Directory
        } else {
            FileKind::File
        };
        let link_target = if dir_entry.is_symlink {
            Some(self.fs.read_link(session_id, abs_path).await?)
        } else {
            None
        };
        Ok(RsyncEntry {
            rel_path: rel_path.to_string(),
            kind,
            size: dir_entry.size,
            mtime: dir_entry.mtime,
            mode: dir_entry.mode,
            uid: dir_entry.uid,
            gid: dir_entry.gid,
            link_target,
        })
    }
}

fn join_rel(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn join_abs(parent: &str, name: &str) -> String {
    if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

/// Compile a list of glob patterns into a [`GlobSet`].
///
/// # Errors
///
/// Returns [`DomainError::InvalidArgument`] when any pattern fails to
/// compile.
pub fn build_globset(patterns: &[String]) -> Result<GlobSet, DomainError> {
    let mut builder = globset::GlobSetBuilder::new();
    for raw in patterns {
        let glob = globset::Glob::new(raw)
            .map_err(|e| DomainError::InvalidArgument(format!("invalid glob '{raw}': {e}")))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| DomainError::InvalidArgument(format!("globset build: {e}")))
}

#[cfg(test)]
mod tests {
    use super::{SftpWalker, build_globset};
    use crate::adapters::rsync::sftp::fake::FakeRsyncSftpFs;
    use crate::adapters::rsync::types::FileKind;
    use crate::domain::error::DomainError;
    use crate::domain::ids::SessionId;
    use globset::GlobSet;
    use std::sync::Arc;

    fn s() -> SessionId {
        SessionId::new("sess".to_string())
    }

    fn empty_globset() -> GlobSet {
        GlobSet::empty()
    }

    fn fixture() -> Arc<FakeRsyncSftpFs> {
        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/root", 0o755);
        fs.put_dir("/root/sub", 0o755);
        fs.put_file("/root/a.txt", b"a", 0o644, 100);
        fs.put_file("/root/sub/b.txt", b"b", 0o644, 100);
        fs.put_symlink("/root/link", "../target");
        Arc::new(fs)
    }

    #[tokio::test]
    async fn walk_yields_three_levels() {
        let fs = fixture();
        let w = SftpWalker::new(Arc::clone(&fs), empty_globset(), empty_globset(), 1_000);
        let entries = w.walk(&s(), "/root").await.expect("walk");
        let names: Vec<&str> = entries.iter().map(|e| e.rel_path.as_str()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"sub"));
        assert!(names.contains(&"sub/b.txt"));
        assert!(names.contains(&"link"));
    }

    #[tokio::test]
    async fn walk_classifies_kinds() {
        let fs = fixture();
        let w = SftpWalker::new(Arc::clone(&fs), empty_globset(), empty_globset(), 1_000);
        let entries = w.walk(&s(), "/root").await.expect("walk");
        for entry in &entries {
            match entry.rel_path.as_str() {
                "a.txt" | "sub/b.txt" => {
                    assert_eq!(entry.kind, FileKind::File);
                }
                "sub" => {
                    assert_eq!(entry.kind, FileKind::Directory);
                }
                "link" => {
                    assert_eq!(entry.kind, FileKind::Symlink);
                    assert_eq!(entry.link_target.as_deref(), Some("../target"));
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn walk_respects_exclude_pattern() {
        let fs = fixture();
        let excludes =
            build_globset(&["**/sub".to_string(), "**/sub/**".to_string()]).expect("globset");
        let w = SftpWalker::new(Arc::clone(&fs), excludes, empty_globset(), 1_000);
        let entries = w.walk(&s(), "/root").await.expect("walk");
        for entry in &entries {
            assert!(
                !entry.rel_path.starts_with("sub"),
                "unexpected: {}",
                entry.rel_path
            );
        }
    }

    #[tokio::test]
    async fn walk_include_overrides_exclude() {
        let fs = fixture();
        let excludes = build_globset(&["**/*".to_string()]).expect("excludes");
        let includes = build_globset(&["a.txt".to_string()]).expect("includes");
        let w = SftpWalker::new(Arc::clone(&fs), excludes, includes, 1_000);
        let entries = w.walk(&s(), "/root").await.expect("walk");
        let names: Vec<&str> = entries.iter().map(|e| e.rel_path.as_str()).collect();
        assert_eq!(names, vec!["a.txt"]);
    }

    #[tokio::test]
    async fn walk_refuses_when_file_list_cap_exceeded() {
        let fs = fixture();
        let w = SftpWalker::new(Arc::clone(&fs), empty_globset(), empty_globset(), 1);
        let err = w.walk(&s(), "/root").await.expect_err("must err");
        match err {
            DomainError::RsyncFileListTooLarge { limit } => assert_eq!(limit, 1),
            other => panic!("expected file-list cap, got {other:?}"),
        }
    }
}
