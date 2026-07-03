//! Compare source and destination tree snapshots, derive an
//! [`SyncAction`] list.
//!
//! Source-of-truth heuristic mirrors rsync — entries match when both
//! `size` and `mtime` are equal; otherwise the file is treated as
//! changed and queued for transfer. Symlinks compare by link target.
//! When `delete = true`, entries present on the destination but absent
//! from the source surface as a delete-action.
//!
//! Hardlinks are intentionally **not** handled here: SFTP `Attributes`
//! does not surface inode numbers, so the use-case layer rejects
//! `preserve.hardlinks=true` against the SFTP transport with
//! [`crate::domain::error::DomainError::SftpFeatureMissing`] before the
//! comparator ever runs.

use std::collections::HashMap;

use crate::adapters::rsync::sftp::walker::RsyncEntry;
use crate::adapters::rsync::types::{FileKind, PreserveFlags};

/// Direction of the sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Push from local to remote.
    Push,
    /// Pull from remote to local.
    Pull,
}

/// One unit of work derived by the comparator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAction {
    /// Both sides match — emit `FileSkipped` and do nothing else.
    Skip {
        /// Path relative to the sync root.
        rel_path: String,
    },
    /// Create a directory on the destination.
    Mkdir {
        /// Path relative to the sync root.
        rel_path: String,
        /// POSIX mode bits to apply at create-time.
        mode: u32,
    },
    /// Transfer the file payload.
    Transfer {
        /// Path relative to the sync root.
        rel_path: String,
        /// Total bytes to copy.
        size: u64,
    },
    /// Recreate / refresh a symlink.
    Symlink {
        /// Path relative to the sync root.
        rel_path: String,
        /// Link target.
        target: String,
    },
    /// Apply metadata after the payload (perms / mtime / owner / group).
    Setstat {
        /// Path relative to the sync root.
        rel_path: String,
        /// Mode bits.
        mode: u32,
        /// Modification time (unix seconds).
        mtime: i64,
        /// Numeric owner uid.
        uid: u32,
        /// Numeric owner gid.
        gid: u32,
    },
    /// Remove an entry from the destination.
    Delete {
        /// Path relative to the sync root.
        rel_path: String,
        /// `true` when the entry is a directory (drives recursive remove).
        is_dir: bool,
    },
}

/// Comparator options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompareOpts {
    /// `--delete` — remove destination entries missing from source.
    pub delete: bool,
    /// `--checksum` — compare by content hash even when size+mtime match
    /// (the SFTP transport flags this back to the use-case layer; it is
    /// surfaced here only so the comparator does NOT skip files it would
    /// otherwise have skipped).
    pub force_transfer: bool,
    /// Attribute-preservation mask.
    pub preserve: PreserveFlags,
}

/// Compare `src` against `dst` and emit ordered [`SyncAction`]s.
///
/// Order invariant: directories are created before their children; the
/// payload transfer comes before its setstat; deletes (when enabled)
/// land last so the destination tree is never half-broken mid-sync.
#[must_use]
pub fn compare_trees(
    src: &[RsyncEntry],
    dst: &[RsyncEntry],
    _direction: Direction,
    opts: CompareOpts,
) -> Vec<SyncAction> {
    let dst_by_path: HashMap<&str, &RsyncEntry> =
        dst.iter().map(|e| (e.rel_path.as_str(), e)).collect();
    let src_by_path: HashMap<&str, &RsyncEntry> =
        src.iter().map(|e| (e.rel_path.as_str(), e)).collect();

    let mut actions = Vec::with_capacity(src.len());
    let mut src_sorted: Vec<&RsyncEntry> = src.iter().collect();
    // Borrowed-key sort: `(depth, &str)` avoids the `String` clone the
    // old owned-key `sort_by_key` paid per element while keeping the
    // exact same `Ord` — depth first, then lexicographic path — so the
    // resulting order is unchanged.
    src_sorted.sort_by_cached_key(|e| (depth(&e.rel_path), e.rel_path.as_str()));

    for entry in &src_sorted {
        let dst_match = dst_by_path.get(entry.rel_path.as_str()).copied();
        push_kind_actions(&mut actions, entry, dst_match, opts);
    }
    if opts.delete {
        push_delete_actions(&mut actions, dst, &src_by_path);
    }
    actions
}

fn push_kind_actions(
    actions: &mut Vec<SyncAction>,
    entry: &RsyncEntry,
    dst_match: Option<&RsyncEntry>,
    opts: CompareOpts,
) {
    match entry.kind {
        FileKind::Directory => {
            if dst_match.is_none() {
                actions.push(SyncAction::Mkdir {
                    rel_path: entry.rel_path.clone(),
                    mode: entry.mode,
                });
            }
            if needs_setstat(entry, dst_match, opts.preserve) {
                actions.push(make_setstat(entry));
            }
        }
        FileKind::Symlink => {
            actions.extend(symlink_actions(entry, dst_match));
        }
        FileKind::File => {
            actions.extend(file_actions(entry, dst_match, opts));
        }
        FileKind::Hardlink | FileKind::Device | FileKind::Fifo | FileKind::Socket => {}
    }
}

fn push_delete_actions(
    actions: &mut Vec<SyncAction>,
    dst: &[RsyncEntry],
    src_by_path: &HashMap<&str, &RsyncEntry>,
) {
    let mut extraneous: Vec<&RsyncEntry> = dst
        .iter()
        .filter(|entry| !src_by_path.contains_key(entry.rel_path.as_str()))
        .collect();
    // Deepest-first: the walker lists a directory before its children, so
    // deleting in that order would run a non-recursive rmdir on a
    // still-non-empty directory. Sorting by descending depth (with a
    // lexicographic tie-break for determinism) guarantees every child —
    // file or directory — is removed before its parent's rmdir runs.
    extraneous.sort_by(|a, b| {
        depth(&b.rel_path)
            .cmp(&depth(&a.rel_path))
            .then_with(|| b.rel_path.cmp(&a.rel_path))
    });
    for entry in extraneous {
        actions.push(SyncAction::Delete {
            rel_path: entry.rel_path.clone(),
            is_dir: matches!(entry.kind, FileKind::Directory),
        });
    }
}

fn depth(path: &str) -> usize {
    if path.is_empty() {
        0
    } else {
        path.chars().filter(|c| *c == '/').count() + 1
    }
}

const fn needs_setstat(
    src: &RsyncEntry,
    dst: Option<&RsyncEntry>,
    preserve: PreserveFlags,
) -> bool {
    if !preserve.perms && !preserve.mtime && !preserve.owner && !preserve.group {
        return false;
    }
    let Some(dst) = dst else {
        return preserve.perms || preserve.mtime || preserve.owner || preserve.group;
    };
    (preserve.perms && src.mode != dst.mode)
        || (preserve.mtime && src.mtime != dst.mtime)
        || (preserve.owner && src.uid != dst.uid)
        || (preserve.group && src.gid != dst.gid)
}

fn make_setstat(src: &RsyncEntry) -> SyncAction {
    SyncAction::Setstat {
        rel_path: src.rel_path.clone(),
        mode: src.mode,
        mtime: src.mtime,
        uid: src.uid,
        gid: src.gid,
    }
}

fn file_actions(src: &RsyncEntry, dst: Option<&RsyncEntry>, opts: CompareOpts) -> Vec<SyncAction> {
    let mut out = Vec::new();
    let identical = matches!(dst, Some(d)
        if d.size == src.size && d.mtime == src.mtime && matches!(d.kind, FileKind::File));
    if identical && !opts.force_transfer {
        out.push(SyncAction::Skip {
            rel_path: src.rel_path.clone(),
        });
        if needs_setstat(src, dst, opts.preserve) {
            out.push(make_setstat(src));
        }
        return out;
    }
    out.push(SyncAction::Transfer {
        rel_path: src.rel_path.clone(),
        size: src.size,
    });
    if needs_setstat(src, dst, opts.preserve) {
        out.push(make_setstat(src));
    }
    out
}

fn symlink_actions(src: &RsyncEntry, dst: Option<&RsyncEntry>) -> Vec<SyncAction> {
    let target = src.link_target.clone().unwrap_or_default();
    if let Some(dst) = dst {
        if matches!(dst.kind, FileKind::Symlink) && dst.link_target.as_deref() == Some(&target) {
            return vec![SyncAction::Skip {
                rel_path: src.rel_path.clone(),
            }];
        }
        // Wrong target / wrong kind — replace.
        return vec![
            SyncAction::Delete {
                rel_path: src.rel_path.clone(),
                is_dir: matches!(dst.kind, FileKind::Directory),
            },
            SyncAction::Symlink {
                rel_path: src.rel_path.clone(),
                target,
            },
        ];
    }
    vec![SyncAction::Symlink {
        rel_path: src.rel_path.clone(),
        target,
    }]
}

#[cfg(test)]
mod tests {
    use super::{CompareOpts, Direction, SyncAction, compare_trees};
    use crate::adapters::rsync::sftp::walker::RsyncEntry;
    use crate::adapters::rsync::types::{FileKind, PreserveFlags};

    fn entry_file(rel: &str, size: u64, mtime: i64) -> RsyncEntry {
        RsyncEntry {
            rel_path: rel.to_string(),
            kind: FileKind::File,
            size,
            mtime,
            mode: 0o644,
            uid: 0,
            gid: 0,
            link_target: None,
        }
    }

    fn entry_dir(rel: &str) -> RsyncEntry {
        RsyncEntry {
            rel_path: rel.to_string(),
            kind: FileKind::Directory,
            size: 0,
            mtime: 0,
            mode: 0o755,
            uid: 0,
            gid: 0,
            link_target: None,
        }
    }

    fn entry_symlink(rel: &str, target: &str) -> RsyncEntry {
        RsyncEntry {
            rel_path: rel.to_string(),
            kind: FileKind::Symlink,
            size: target.len() as u64,
            mtime: 0,
            mode: 0o777,
            uid: 0,
            gid: 0,
            link_target: Some(target.to_string()),
        }
    }

    fn opts_default() -> CompareOpts {
        CompareOpts {
            delete: false,
            force_transfer: false,
            preserve: PreserveFlags::none(),
        }
    }

    #[test]
    fn fresh_destination_emits_mkdir_then_transfer() {
        let src = vec![entry_dir("sub"), entry_file("sub/a.txt", 4, 100)];
        let actions = compare_trees(&src, &[], Direction::Push, opts_default());
        assert!(matches!(actions[0], SyncAction::Mkdir { .. }));
        assert!(
            matches!(actions[1], SyncAction::Transfer { ref rel_path, .. } if rel_path == "sub/a.txt")
        );
    }

    #[test]
    fn unchanged_file_is_skipped() {
        let src = vec![entry_file("a.txt", 4, 100)];
        let dst = vec![entry_file("a.txt", 4, 100)];
        let actions = compare_trees(&src, &dst, Direction::Push, opts_default());
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], SyncAction::Skip { ref rel_path } if rel_path == "a.txt"));
    }

    #[test]
    fn changed_size_triggers_transfer() {
        let src = vec![entry_file("a.txt", 8, 100)];
        let dst = vec![entry_file("a.txt", 4, 100)];
        let actions = compare_trees(&src, &dst, Direction::Push, opts_default());
        assert!(
            matches!(actions[0], SyncAction::Transfer { ref rel_path, size: 8 } if rel_path == "a.txt")
        );
    }

    #[test]
    fn force_transfer_bypasses_skip() {
        let src = vec![entry_file("a.txt", 4, 100)];
        let dst = vec![entry_file("a.txt", 4, 100)];
        let mut o = opts_default();
        o.force_transfer = true;
        let actions = compare_trees(&src, &dst, Direction::Push, o);
        assert!(matches!(actions[0], SyncAction::Transfer { .. }));
    }

    #[test]
    fn delete_emits_for_extraneous_destination_entries() {
        let src = vec![entry_file("a.txt", 4, 100)];
        let dst = vec![entry_file("a.txt", 4, 100), entry_file("b.txt", 4, 100)];
        let mut o = opts_default();
        o.delete = true;
        let actions = compare_trees(&src, &dst, Direction::Push, o);
        let last = actions.last().expect("non-empty");
        assert!(matches!(last, SyncAction::Delete { rel_path, .. } if rel_path == "b.txt"));
    }

    #[test]
    fn symlink_with_matching_target_is_skipped() {
        let src = vec![entry_symlink("link", "../target")];
        let dst = vec![entry_symlink("link", "../target")];
        let actions = compare_trees(&src, &dst, Direction::Push, opts_default());
        assert!(matches!(actions[0], SyncAction::Skip { .. }));
    }

    #[test]
    fn symlink_with_diverging_target_emits_replace_pair() {
        let src = vec![entry_symlink("link", "../A")];
        let dst = vec![entry_symlink("link", "../B")];
        let actions = compare_trees(&src, &dst, Direction::Push, opts_default());
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], SyncAction::Delete { .. }));
        assert!(matches!(actions[1], SyncAction::Symlink { ref target, .. } if target == "../A"));
    }

    #[test]
    fn setstat_emitted_when_mode_changes_under_preserve_perms() {
        let mut src = entry_file("a.txt", 4, 100);
        src.mode = 0o600;
        let dst = entry_file("a.txt", 4, 100);
        let mut opts = opts_default();
        opts.preserve.perms = true;
        let actions = compare_trees(&[src], &[dst], Direction::Push, opts);
        assert!(matches!(actions[0], SyncAction::Skip { .. }));
        assert!(matches!(
            actions[1],
            SyncAction::Setstat { mode: 0o600, .. }
        ));
    }

    #[test]
    fn delete_actions_order_children_before_nested_parent_dir() {
        // Mirrors the walker's own emission order: a directory is listed
        // before its children (BFS worklist push-then-pop). Feeding that
        // order straight into the delete pass without re-sorting would
        // schedule `Delete { rel_path: "sub" }` before
        // `Delete { rel_path: "sub/leaf.txt" }`, which fails a
        // non-recursive `rmdir` on a still-non-empty directory.
        let dst = vec![
            entry_dir("sub"),
            entry_file("sub/leaf.txt", 4, 100),
            entry_dir("sub/nested"),
            entry_file("sub/nested/deep.txt", 4, 100),
        ];
        let mut o = opts_default();
        o.delete = true;
        let actions = compare_trees(&[], &dst, Direction::Push, o);
        assert_eq!(actions.len(), 4);
        let pos = |needle: &str| {
            actions
                .iter()
                .position(
                    |a| matches!(a, SyncAction::Delete { rel_path, .. } if rel_path == needle),
                )
                .unwrap_or_else(|| panic!("missing delete action for {needle}"))
        };
        let sub = pos("sub");
        let sub_leaf = pos("sub/leaf.txt");
        let sub_nested = pos("sub/nested");
        let sub_nested_deep = pos("sub/nested/deep.txt");
        assert!(sub_nested_deep < sub_nested, "deepest file before its dir");
        assert!(sub_leaf < sub, "leaf file before its parent dir");
        assert!(sub_nested < sub, "nested dir before its parent dir");
    }

    #[tokio::test]
    async fn delete_actions_execute_deepest_first_for_nested_dirs() {
        // End-to-end repro of BUG #1: walk a real nested tree with the
        // FakeRsyncSftpFs (whose `rmdir` refuses non-empty directories,
        // exactly like a real SFTP server), derive delete actions, and
        // run them through the executor. Before the deepest-first sort
        // this failed with `files_failed > 0` and a stray "sub" dir left
        // behind because the walker lists parents before children.
        use crate::adapters::rsync::sftp::executor::SftpExecutor;
        use crate::adapters::rsync::sftp::fake::FakeRsyncSftpFs;
        use crate::adapters::rsync::sftp::walker::{SftpWalker, build_globset};
        use crate::domain::ids::SessionId;
        use std::sync::Arc;
        use tokio::sync::mpsc;
        use tokio_util::sync::CancellationToken;

        let fs = FakeRsyncSftpFs::new();
        fs.put_dir("/dst", 0o755);
        fs.put_dir("/dst/sub", 0o755);
        fs.put_file("/dst/sub/leaf.txt", b"gone", 0o644, 0);
        let fs = Arc::new(fs);

        let session = SessionId::new("del-nested".to_string());
        let empty: Vec<String> = Vec::new();
        let walker = SftpWalker::new(
            Arc::clone(&fs),
            build_globset(&empty).expect("empty globset"),
            build_globset(&empty).expect("empty globset"),
            1_000,
        );
        let dst_entries = walker.walk(&session, "/dst").await.expect("walk dst");
        // Sanity: confirm the walker really does list the parent before
        // the child, which is the precondition for the bug.
        assert_eq!(dst_entries[0].rel_path, "sub");
        assert_eq!(dst_entries[1].rel_path, "sub/leaf.txt");

        let mut opts = opts_default();
        opts.delete = true;
        let actions = compare_trees(&[], &dst_entries, Direction::Push, opts);

        let (tx, _rx) = mpsc::channel(8);
        let exec = SftpExecutor::new(
            Arc::clone(&fs),
            None,
            false,
            tx,
            CancellationToken::new(),
            "/src".to_string(),
            "/dst".to_string(),
        );
        let stats = exec.execute(&session, &actions).await.expect("execute");
        assert_eq!(
            stats.files_failed, 0,
            "rmdir must never hit a non-empty dir"
        );
        assert_eq!(stats.files_deleted, 2);
        assert!(!fs.exists("/dst/sub/leaf.txt"));
        assert!(!fs.exists("/dst/sub"));
    }

    #[test]
    fn directories_created_before_children() {
        let src = vec![entry_file("sub/a.txt", 1, 1), entry_dir("sub")];
        let actions = compare_trees(&src, &[], Direction::Push, opts_default());
        // Mkdir for sub must come before Transfer for sub/a.txt.
        let pos_dir = actions
            .iter()
            .position(|a| matches!(a, SyncAction::Mkdir { rel_path, .. } if rel_path == "sub"))
            .expect("mkdir present");
        let pos_file = actions
            .iter()
            .position(
                |a| matches!(a, SyncAction::Transfer { rel_path, .. } if rel_path == "sub/a.txt"),
            )
            .expect("transfer present");
        assert!(pos_dir < pos_file);
    }
}
