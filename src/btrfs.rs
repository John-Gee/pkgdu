use std::collections::HashSet;
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsFd;
use std::path::Path;

use btrfs_disk::items::{CompressionType, FileExtentBody, FileExtentItem};
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

/// Return btrfs compressed sizes for each package entry.
pub fn compressed_sizes(entries: &[PackageEntry], config: &Config) -> Vec<Option<u64>> {
    if entries.is_empty() {
        return vec![];
    }

    let mut results = Vec::with_capacity(entries.len());

    for entry in entries {
        let mut pkg_total = 0u64;
        let mut tracked_extents: HashSet<u64> = HashSet::new();
        let mut has_compressed = false;

        for file_path in &entry.files {
            let full = if file_path.is_absolute() {
                file_path.clone()
            } else {
                config.root.join(file_path)
            };

            let file = match File::open(&full) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let meta = match file.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_file() {
                continue;
            }

            let ino = meta.ino();
            let filter =
                SearchFilter::for_objectid_range(0, BTRFS_EXTENT_DATA_KEY, ino, ino);

            let _ = tree_search_v2(
                file.as_fd(),
                filter,
                None,
                |_hdr, data: &[u8]| -> std::result::Result<(), nix::errno::Errno> {
                    if let Some(extent) = FileExtentItem::parse(data) {
                        if extent.compression != CompressionType::None {
                            has_compressed = true;
                            match &extent.body {
                                FileExtentBody::Regular {
                                    disk_bytenr,
                                    disk_num_bytes,
                                    ..
                                } => {
                                    if tracked_extents.insert(*disk_bytenr) {
                                        pkg_total += disk_num_bytes;
                                    }
                                }
                                FileExtentBody::Inline { inline_size } => {
                                    pkg_total += *inline_size as u64;
                                }
                            }
                        }
                    }
                    Ok(())
                },
            );
        }

        results.push(if has_compressed { Some(pkg_total) } else { None });
    }

    results
}
