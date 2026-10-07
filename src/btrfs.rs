use std::collections::HashSet;
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsFd;
use std::path::Path;

use btrfs_disk::items::{FileExtentBody, FileExtentItem};
use btrfs_uapi::raw::BTRFS_EXTENT_DATA_KEY;
use btrfs_uapi::tree_search::{tree_search_v2, SearchFilter};

use crate::config::Config;
use crate::pacman::PackageEntry;

/// Result of probing whether a path is on btrfs.
pub enum BtrfsStatus {
    Yes,
    No,
    PermissionDenied,
}

/// Probe whether `root` is on a btrfs filesystem by attempting the tree
/// search ioctl on the root directory.
pub fn detect_btrfs(root: &Path) -> BtrfsStatus {
    let file = match File::open(root) {
        Ok(f) => f,
        Err(_) => return BtrfsStatus::No,
    };
    let filter = SearchFilter::for_type(0, BTRFS_EXTENT_DATA_KEY);
    match tree_search_v2(file.as_fd(), filter, Some(4096), |_, _| Ok(())) {
        Ok(()) => BtrfsStatus::Yes,
        Err(nix::errno::Errno::ENOTTY)
        | Err(nix::errno::Errno::ENOPROTOOPT)
        | Err(nix::errno::Errno::ENOSYS) => BtrfsStatus::No,
        Err(nix::errno::Errno::EPERM) => BtrfsStatus::PermissionDenied,
        Err(_) => BtrfsStatus::Yes,
    }
}

/// Return the btrfs on-disk size (from file extents) for each package entry.
///
/// This is the physical disk usage of every extent the package's files
/// reference — compressed or not — i.e. what `compsize` reports as "Disk
/// Usage".
///
/// Serial on purpose: the `BTRFS_IOC_TREE_SEARCH` ioctl serializes in the
/// kernel, so parallelising the per-file searches only causes lock contention
/// (measured ~4x slower with rayon).
pub fn compressed_sizes(entries: &[PackageEntry], config: &Config) -> Vec<Option<u64>> {
    entries
        .iter()
        .map(|entry| package_disk_usage(entry, config))
        .collect()
}

/// Raw extent records for one file, before cross-file dedup.
#[derive(Default)]
struct FileExtents {
    /// `(disk_bytenr, disk_num_bytes)` for each regular extent; holes appear as
    /// `disk_bytenr == 0`.
    regular: Vec<(u64, u64)>,
    /// Total inline (in-tree) data size.
    inline_bytes: u64,
    /// Whether any extent item was seen for this file.
    found: bool,
}

/// Read one file's extent records via the btrfs tree-search ioctl.
fn file_extents(file_path: &Path, root: &Path) -> FileExtents {
    let full = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        root.join(file_path)
    };
    let mut out = FileExtents::default();

    let file = match File::open(&full) {
        Ok(f) => f,
        Err(_) => return out,
    };
    let meta = match file.metadata() {
        Ok(m) => m,
        Err(_) => return out,
    };
    if !meta.is_file() {
        return out;
    }

    let ino = meta.ino();
    let filter = SearchFilter::for_objectid_range(0, BTRFS_EXTENT_DATA_KEY, ino, ino);

    let _ = tree_search_v2(
        file.as_fd(),
        filter,
        None,
        |_hdr, data: &[u8]| -> std::result::Result<(), nix::errno::Errno> {
            if let Some(extent) = FileExtentItem::parse(data) {
                out.found = true;
                match &extent.body {
                    FileExtentBody::Regular {
                        disk_bytenr,
                        disk_num_bytes,
                        ..
                    } => out.regular.push((*disk_bytenr, *disk_num_bytes)),
                    FileExtentBody::Inline { inline_size } => {
                        out.inline_bytes += *inline_size as u64;
                    }
                }
            }
            Ok(())
        },
    );

    out
}

/// Sum a package's on-disk usage from its files' extent records: skip holes
/// (`disk_bytenr == 0`), count each physical extent once, add inline data.
/// `None` when no extent was found (e.g. every file was unreadable).
fn total_from_files(files: &[FileExtents]) -> Option<u64> {
    let mut seen: HashSet<u64> = HashSet::new();
    let mut total = 0u64;
    let mut has_extent = false;

    for file in files {
        has_extent |= file.found;
        total += file.inline_bytes;
        for &(disk_bytenr, disk_num_bytes) in &file.regular {
            if disk_bytenr != 0 && seen.insert(disk_bytenr) {
                total += disk_num_bytes;
            }
        }
    }

    has_extent.then_some(total)
}

/// Total btrfs on-disk usage of one package.
fn package_disk_usage(entry: &PackageEntry, config: &Config) -> Option<u64> {
    let files: Vec<FileExtents> = entry
        .files
        .iter()
        .map(|file_path| file_extents(file_path, &config.root))
        .collect();
    total_from_files(&files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(regular: &[(u64, u64)], inline_bytes: u64, found: bool) -> FileExtents {
        FileExtents {
            regular: regular.to_vec(),
            inline_bytes,
            found,
        }
    }

    #[test]
    fn test_total_from_files_counts_and_dedups() {
        // Two extents, with the first repeated across files (a shared/reflinked
        // physical extent) -> counted once.
        let files = vec![
            file(&[(100, 4096), (200, 8192)], 0, true),
            file(&[(100, 4096)], 0, true),
        ];
        assert_eq!(total_from_files(&files), Some(4096 + 8192));
    }

    #[test]
    fn test_total_from_files_skips_holes_and_counts_inline() {
        // disk_bytenr == 0 is a hole/sparse range: no disk usage; inline is counted.
        let files = vec![file(&[(0, 65536)], 50, true)];
        assert_eq!(total_from_files(&files), Some(50));
    }

    #[test]
    fn test_total_from_files_none_when_no_extent() {
        assert_eq!(total_from_files(&[file(&[], 0, false)]), None);
    }
}
