/// Stub implementation — returns `vec![None; entries.len()]`.
/// Compressed size lookup will be implemented in Phase 2 (statx + rayon, F7/F8).
use crate::config::Config;
use crate::pacman::PackageEntry;

/// Return btrfs compressed sizes for each package entry (stub: all None).
#[allow(dead_code)]
pub fn compressed_sizes(_entries: &[PackageEntry], _config: &Config) -> Vec<Option<u64>> {
    vec![None; _entries.len()]
}
