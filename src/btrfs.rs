use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsFd;
use std::path::{Path, PathBuf};

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
/// Hybrid, because the `BTRFS_IOC_TREE_SEARCH` ioctl is ~1-2 ms per call and
/// serialises in the kernel:
/// - **full scan** (no explicit package names): one bulk sweep of the
///   subvolume's extents, building an inode→bytes table (`compsize`-style);
/// - **targeted query** (package names given): read extents per file, whose
///   cost scales with the (small) number of requested files.
pub fn compressed_sizes(entries: &[PackageEntry], config: &Config) -> Vec<Option<u64>> {
    if config.targets.is_empty() {
        match build_inode_map(&config.root) {
            Some(map) => sweep_sizes(entries, config, &map),
            None => vec![None; entries.len()],
        }
    } else {
        entries
            .iter()
            .map(|entry| package_disk_usage(entry, config))
            .collect()
    }
}

/// One pass over the subvolume's `EXTENT_DATA` items, producing
/// `inode -> on-disk bytes` (holes skipped, inline counted). `None` if the
/// sweep fails.
fn build_inode_map(root: &Path) -> Option<HashMap<u64, u64>> {
    let file = File::open(root).ok()?;
    let filter = SearchFilter::for_type(0, BTRFS_EXTENT_DATA_KEY);
    let mut map: HashMap<u64, u64> = HashMap::new();
    let res = tree_search_v2(
        file.as_fd(),
        filter,
        None,
        |hdr, data: &[u8]| -> std::result::Result<(), nix::errno::Errno> {
            // A compound-key search can return other item types; keep only
            // extent-data items.
            if hdr.item_type == BTRFS_EXTENT_DATA_KEY {
                if let Some(extent) = FileExtentItem::parse(data) {
                    let bytes = match &extent.body {
                        FileExtentBody::Regular {
                            disk_bytenr,
                            disk_num_bytes,
                            ..
                        } => {
                            if *disk_bytenr == 0 {
                                0 // hole
                            } else {
                                *disk_num_bytes
                            }
                        }
                        FileExtentBody::Inline { inline_size } => *inline_size as u64,
                    };
                    if bytes > 0 {
                        *map.entry(hdr.objectid).or_default() += bytes;
                    }
                }
            }
            Ok(())
        },
    );
    res.ok().map(|_| map)
}

/// Sum a package's on-disk bytes from a prebuilt inode map. Hardlinked paths
/// are counted once; files not on the swept subvolume are skipped. `None` when
/// none of the package's files were found in the map.
fn sum_package_files(
    files: &[PathBuf],
    root: &Path,
    root_dev: Option<u64>,
    map: &HashMap<u64, u64>,
) -> Option<u64> {
    let mut seen: HashSet<u64> = HashSet::new();
    let mut total = 0u64;
    let mut found = false;

    for file_path in files {
        if file_path
            .file_name()
            .map(|n| n == ".FILESYSTEM")
            .unwrap_or(false)
        {
            continue;
        }
        let full = if file_path.is_absolute() {
            file_path.clone()
        } else {
            root.join(file_path)
        };
        let meta = match std::fs::symlink_metadata(&full) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        if root_dev.is_some() && Some(meta.dev()) != root_dev {
            // On a different subvolume than the one we swept.
            continue;
        }
        let ino = meta.ino();
        if seen.insert(ino) {
            if let Some(&bytes) = map.get(&ino) {
                total += bytes;
                found = true;
            }
        }
    }

    found.then_some(total)
}

/// Sweep-based sizes for a full scan.
fn sweep_sizes(
    entries: &[PackageEntry],
    config: &Config,
    map: &HashMap<u64, u64>,
) -> Vec<Option<u64>> {
    let root_dev = std::fs::metadata(&config.root).ok().map(|m| m.dev());
    entries
        .iter()
        .map(|entry| sum_package_files(&entry.files, &config.root, root_dev, map))
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

    #[test]
    fn test_sum_package_files_dedups_hardlinks_and_sums() {
        let dir = std::env::temp_dir().join("pkgdu_test_sweep");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a");
        let b = dir.join("b");
        let c = dir.join("c_link");
        std::fs::write(&a, b"aaaa").unwrap();
        std::fs::write(&b, b"bbbb").unwrap();
        std::fs::hard_link(&a, &c).unwrap();

        let dev = std::fs::metadata(&dir).unwrap().dev();
        let ino_a = std::fs::metadata(&a).unwrap().ino();
        let ino_b = std::fs::metadata(&b).unwrap().ino();
        let mut map = HashMap::new();
        map.insert(ino_a, 100u64);
        map.insert(ino_b, 200u64);

        // `a` and its hardlink share an inode -> counted once; `b` adds 200.
        let files = vec![a.clone(), b.clone(), c.clone()];
        assert_eq!(sum_package_files(&files, &dir, Some(dev), &map), Some(300));

        // Nothing in the map -> None.
        let empty: HashMap<u64, u64> = HashMap::new();
        assert_eq!(sum_package_files(&files, &dir, Some(dev), &empty), None);

        std::fs::remove_dir_all(&dir).ok();
    }
}
