use crate::btrfs;
use crate::config::{Config, SortField};
use crate::error::Result;
use crate::pacman::{load_local_db, Filter};
use crate::tree::{build_package_tree, PackageTree};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone, Copy, PartialEq)]
enum StatResult {
    Success {
        apparent: u64,
        real: u64,
        dev: u64,
        ino: u64,
    },
    NotFound,
    PermissionDenied,
    Symlink,
}

fn stat_file(path: &Path) -> StatResult {
    use std::os::unix::fs::MetadataExt;

    // A single lstat is enough: for a regular file, lstat's st_size/st_blocks
    // are exactly what stat would return, and it also lets us detect symlinks
    // without a second syscall.
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                StatResult::Symlink
            } else {
                StatResult::Success {
                    apparent: meta.len(),
                    real: meta.blocks() * 512,
                    dev: meta.dev(),
                    ino: meta.ino(),
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => StatResult::NotFound,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => StatResult::PermissionDenied,
        Err(_) => StatResult::PermissionDenied,
    }
}

/// Result for a single scanned package.
#[derive(Debug, Clone)]
pub struct PackageResult {
    pub name: String,
    pub version: String,
    pub real_size: u64,
    pub apparent_size: u64,
    pub file_count: u64,
    pub metadata_size: u64,
    pub btrfs_disk: Option<u64>,
}

/// Full scan report with metadata about skipped packages and errors.
#[derive(Debug, Default)]
pub struct ScanReport {
    pub packages: Vec<PackageResult>,
    pub skipped_packages: usize,
    pub permission_errors: usize,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// Grand totals over all matching packages, before `limit` is applied.
    pub total_packages: usize,
    pub total_real: u64,
    pub total_apparent: u64,
    pub total_files: u64,
    pub total_disk: u64,
    /// Per-package file trees for the shown packages (empty unless the tree view is on).
    pub trees: Vec<PackageTree>,
    /// Explicitly requested package names that matched nothing in the database.
    pub missing_targets: Vec<String>,
}

impl PackageResult {
    /// btrfs on-disk usage as a percentage of apparent size, so a smaller
    /// value means better compression (100% = uncompressed). `None` when
    /// btrfs data is unavailable or apparent size is zero (ratio undefined).
    pub fn btrfs_ratio_percent(&self) -> Option<f64> {
        match self.btrfs_disk {
            Some(compressed) if self.apparent_size > 0 => {
                Some(compressed as f64 / self.apparent_size as f64 * 100.0)
            }
            _ => None,
        }
    }
}

/// Result of stat-ing one package: aggregates plus optional per-file sizes.
struct PkgStat {
    apparent: u64,
    real: u64,
    count: u64,
    warns: Vec<String>,
    /// Per-file attributed bytes, aligned with `PackageEntry.files`; empty
    /// unless per-file sizes were requested (tree view).
    sizes: Vec<u64>,
}

fn stat_package(
    entry: &crate::pacman::PackageEntry,
    root: &Path,
    collect_sizes: bool,
    use_apparent: bool,
) -> PkgStat {
    let mut apparent = 0u64;
    let mut real = 0u64;
    let mut count = 0u64;
    let mut warns: Vec<String> = Vec::new();
    let mut sizes = if collect_sizes {
        vec![0u64; entry.files.len()]
    } else {
        Vec::new()
    };
    // Hardlinks within a package share an inode; count each inode only once.
    let mut seen_inodes: HashSet<(u64, u64)> = HashSet::new();

    for (i, file_path) in entry.files.iter().enumerate() {
        // Resolve relative paths against root
        let full = if file_path.is_absolute() {
            file_path.to_path_buf()
        } else {
            root.join(file_path)
        };

        // Skip .FILESYSTEM marker entries
        if file_path
            .file_name()
            .map(|n| n == ".FILESYSTEM")
            .unwrap_or(false)
        {
            continue;
        }

        match stat_file(&full) {
            StatResult::Success {
                apparent: app,
                real: r,
                dev,
                ino,
            } => {
                if seen_inodes.insert((dev, ino)) {
                    apparent += app;
                    real += r;
                    count += 1;
                    if collect_sizes {
                        sizes[i] = if use_apparent { app } else { r };
                    }
                }
            }
            StatResult::NotFound => {
                // Missing file — not an error, skip silently
            }
            StatResult::PermissionDenied => {
                warns.push(format!(
                    "Permission error reading file: {}",
                    file_path.display()
                ));
            }
            StatResult::Symlink => {
                // Symlinks are intentionally skipped
            }
        }
    }

    PkgStat {
        apparent,
        real,
        count,
        warns,
        sizes,
    }
}

/// Run a full scan over the configured package set.
pub fn scan_packages(config: &Config) -> Result<ScanReport> {
    // Config validation guarantees the dbpath exists; a missing or unreadable
    // directory is a fatal error surfaced by load_local_db.
    let dbpath = &config.dbpath;

    // Build filter from targets or search
    let filter = Filter {
        targets: config.targets.clone(),
        search: config.search.clone(),
    };

    // Load and parse pacman local DB (parallel over parsed pkg lists)
    let (entries, skipped, load_errors) = load_local_db(dbpath, &filter)?;

    // Explicit targets that are not installed at all are a user error worth
    // reporting, not a silently empty result. Checked against the whole
    // database (not the filtered entries), so a target that merely doesn't
    // match --search — or whose desc is malformed — is not misreported.
    let missing_targets: Vec<String> = if config.targets.is_empty() {
        Vec::new()
    } else {
        let installed = crate::pacman::db_package_names(dbpath)?;
        config
            .targets
            .iter()
            .filter(|t| !installed.contains(t.as_str()))
            .cloned()
            .collect()
    };

    if entries.is_empty() {
        return Ok(ScanReport {
            packages: vec![],
            skipped_packages: skipped,
            permission_errors: 0,
            errors: load_errors,
            warnings: Vec::new(),
            missing_targets,
            ..Default::default()
        });
    }

    // Sort results
    let sort = config.sort;

    // Collect warnings from file scanning
    let mut all_warns_count = 0usize;
    let mut all_warns: Vec<String> = Vec::new();

    // Query btrfs on-disk sizes if enabled
    let mut btrfs_errors: Vec<String> = Vec::new();
    let btrfs_disk: Vec<Option<u64>> = if config.btrfs {
        match btrfs::detect_btrfs(&config.root) {
            btrfs::BtrfsStatus::Yes => btrfs::compressed_sizes(&entries, config),
            btrfs::BtrfsStatus::No => {
                all_warns.push(format!(
                    "{} is not on a btrfs filesystem; ignoring --btrfs",
                    config.root.display()
                ));
                vec![None; entries.len()]
            }
            btrfs::BtrfsStatus::PermissionDenied => {
                btrfs_errors.push(format!(
                    "Permission denied accessing btrfs data on {} (try running with sudo)",
                    config.root.display()
                ));
                vec![None; entries.len()]
            }
        }
    } else {
        vec![None; entries.len()]
    };

    // Stat all files of all packages in parallel. rayon's `collect` preserves
    // the order of `entries`, so output stays deterministic before sorting.
    let show_progress = std::io::stderr().is_terminal();
    let total_entries = entries.len();
    let progress = AtomicUsize::new(0);

    let collect_sizes = config.depth.is_some();
    let use_apparent = config.apparent_size;
    let scanned: Vec<PkgStat> = entries
        .par_iter()
        .map(|entry| {
            let stat = stat_package(entry, &config.root, collect_sizes, use_apparent);
            if show_progress {
                let done = progress.fetch_add(1, Ordering::Relaxed) + 1;
                if done % 128 == 0 || done == total_entries {
                    eprint!("\r\x1B[KScanning ({}/{})...", done, total_entries);
                    let _ = std::io::stderr().flush();
                }
            }
            stat
        })
        .collect();

    // Build scan report (sequential; combines parallel results in entry order)
    let packages: Vec<PackageResult> = entries
        .iter()
        .zip(scanned.iter())
        .zip(btrfs_disk.iter())
        .map(|((entry, stat), &comp)| -> PackageResult {
            all_warns_count += stat.warns.len();
            all_warns.extend(stat.warns.iter().cloned());

            PackageResult {
                name: entry.name.clone(),
                version: entry.version.clone(),
                real_size: stat.real,
                apparent_size: stat.apparent,
                file_count: stat.count,
                metadata_size: entry.metadata_size,
                btrfs_disk: comp,
            }
        })
        .collect();

    // Grand totals over all matching packages (before `limit`).
    let total_packages = packages.len();
    let total_real = packages.iter().map(|p| p.real_size).sum();
    let total_apparent = packages.iter().map(|p| p.apparent_size).sum();
    let total_files = packages.iter().map(|p| p.file_count).sum();
    let total_disk = packages.iter().filter_map(|p| p.btrfs_disk).sum();

    let mut result = ScanReport {
        packages,
        skipped_packages: skipped,
        permission_errors: all_warns_count,
        errors: {
            // Malformed-package diagnostics plus btrfs errors.
            let mut errors = load_errors;
            errors.append(&mut btrfs_errors);
            errors
        },
        warnings: all_warns,
        total_packages,
        total_real,
        total_apparent,
        total_files,
        total_disk,
        trees: Vec::new(),
        missing_targets,
    };

    if show_progress {
        eprint!("\r\x1B[K");
        let _ = std::io::stderr().flush();
    }

    // Apply sort (descending for real/apparent/files; ascending for name and ratio)
    result.sort(sort);

    // Apply limit
    if let Some(limit) = config.limit {
        result.truncate(limit);
    }

    // Build per-package file trees for the shown packages (tree view).
    if collect_sizes {
        let index_by_name: HashMap<&str, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.as_str(), i))
            .collect();
        result.trees = result
            .packages
            .iter()
            .filter_map(|pkg| {
                let idx = *index_by_name.get(pkg.name.as_str())?;
                let sizes = &scanned[idx].sizes;
                let files: Vec<(std::path::PathBuf, u64)> = entries[idx]
                    .files
                    .iter()
                    .cloned()
                    .zip(sizes.iter().copied())
                    .collect();
                let total = if use_apparent {
                    pkg.apparent_size
                } else {
                    pkg.real_size
                };
                Some(build_package_tree(&pkg.name, total, &files))
            })
            .collect();
    }

    Ok(result)
}

impl ScanReport {
    /// Sort packages in descending order by the given field.
    /// Size-based fields (Real, Apparent, Files): primary = value descending,
    /// secondary name ascending for determinism when values tie.
    /// Name: ascending single pass.
    /// Ratio: packages with a defined ratio first, ascending (best compression
    /// first); undefined ratios last; ties by name ascending.
    pub fn sort(&mut self, field: SortField) {
        if matches!(field, SortField::Name) {
            // Name is alphabetical (ascending), simple cmp
            self.packages.sort_by(|a, b| a.name.cmp(&b.name));
            return;
        }

        if matches!(
            field,
            SortField::Real | SortField::Apparent | SortField::Files
        ) {
            let val = |p: &PackageResult| match field {
                SortField::Real => p.real_size as i128,
                SortField::Apparent => p.apparent_size as i128,
                SortField::Files => p.file_count as i128,
                _ => unreachable!(),
            };

            self.packages.sort_by(|a, b| {
                // Primary: value descending (swap cmp order)
                // Secondary name ascending for determinism when values tie
                val(b).cmp(&val(a)).then(a.name.cmp(&b.name))
            });
            return;
        }

        // Ratio sort: defined ratios first in ascending order (best compression
        // first), undefined ratios last, ties broken by name.
        self.packages.sort_by(
            |a, b| match (a.btrfs_ratio_percent(), b.btrfs_ratio_percent()) {
                (Some(ra), Some(rb)) => ra
                    .partial_cmp(&rb)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.name.cmp(&b.name)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.name.cmp(&b.name),
            },
        );
    }

    /// Limit packages to the first N (by order).
    pub fn truncate(&mut self, limit: usize) {
        if self.packages.len() > limit {
            let _extra = self.packages.drain(limit..); // drop extra without unwrapping
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn test_stat_package_empty() {
        use crate::pacman;
        let entry = pacman::PackageEntry {
            name: "empty_pkg".to_string(),
            version: "1.0-1".to_string(),
            metadata_size: 0,
            files: vec![],
        };
        let s = stat_package(&entry, Path::new("/"), false, false);
        let (apparent, real, count, warns) = (s.apparent, s.real, s.count, s.warns);
        assert_eq!(apparent, 0);
        assert_eq!(real, 0);
        assert_eq!(count, 0);
        assert!(warns.is_empty());
    }

    #[test]
    fn test_stat_package_single_file() {
        use crate::pacman;
        // Create a temp file for testing
        let tmpfile = std::env::temp_dir().join("pkgdu_test_unit_1.bin");
        std::fs::write(&tmpfile, b"hello world").unwrap();

        let entry = pacman::PackageEntry {
            name: "test_pkg".to_string(),
            version: "1.0-1".to_string(),
            metadata_size: 100,
            files: vec![tmpfile.clone()],
        };
        let s = stat_package(&entry, Path::new("/"), false, false);
        let (apparent, real, count, warns) = (s.apparent, s.real, s.count, s.warns);
        assert_eq!(apparent, 11); // "hello world" = 11 bytes
        assert!(real >= apparent); // F4: real size via blocks(512) is always >= apparent
        assert_eq!(count, 1);
        assert!(warns.is_empty());

        std::fs::remove_file(&tmpfile).ok();
    }

    #[test]
    fn test_stat_package_missing_file() {
        use crate::pacman;
        let entry = pacman::PackageEntry {
            name: "bad_pkg".to_string(),
            version: "1.0-1".to_string(),
            metadata_size: 50,
            files: vec![PathBuf::from("/tmp/pkgdu_nonexistent_file_12345.txt")],
        };
        let s = stat_package(&entry, Path::new("/"), false, false);
        let (apparent, real, count, warns) = (s.apparent, s.real, s.count, s.warns);
        assert_eq!(apparent, 0);
        assert_eq!(real, 0);
        assert_eq!(count, 0);
        assert_eq!(warns.len(), 0);
    }

    #[test]
    fn test_sort_by_real_descending() {
        let mut report = ScanReport {
            packages: vec![
                PackageResult {
                    name: "a".to_string(),
                    real_size: 100,
                    apparent_size: 100,
                    file_count: 1,
                    metadata_size: 50,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "b".to_string(),
                    real_size: 200,
                    apparent_size: 200,
                    file_count: 2,
                    metadata_size: 60,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "c".to_string(),
                    real_size: 50,
                    apparent_size: 50,
                    file_count: 1,
                    metadata_size: 30,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
            ..Default::default()
        };
        report.sort(SortField::Real);
        assert_eq!(report.packages[0].name, "b"); // real_size 200
        assert_eq!(report.packages[1].name, "a"); // real_size 100
        assert_eq!(report.packages[2].name, "c"); // real_size 50
    }

    #[test]
    fn test_sort_by_name_ascending() {
        let mut report = ScanReport {
            packages: vec![
                PackageResult {
                    name: "z".to_string(),
                    real_size: 100,
                    apparent_size: 100,
                    file_count: 1,
                    metadata_size: 50,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "a".to_string(),
                    real_size: 200,
                    apparent_size: 200,
                    file_count: 2,
                    metadata_size: 60,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "m".to_string(),
                    real_size: 50,
                    apparent_size: 50,
                    file_count: 1,
                    metadata_size: 30,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
            ..Default::default()
        };
        report.sort(SortField::Name);
        assert_eq!(report.packages[0].name, "a");
        assert_eq!(report.packages[1].name, "m");
        assert_eq!(report.packages[2].name, "z");
    }

    #[test]
    fn test_sort_by_apparent_descending() {
        let mut report = ScanReport {
            packages: vec![
                PackageResult {
                    name: "a".to_string(),
                    real_size: 100,
                    apparent_size: 500,
                    file_count: 1,
                    metadata_size: 50,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "b".to_string(),
                    real_size: 200,
                    apparent_size: 1000,
                    file_count: 2,
                    metadata_size: 60,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
            ..Default::default()
        };
        report.sort(SortField::Apparent);
        assert_eq!(report.packages[0].name, "b"); // apparent_size 1000
        assert_eq!(report.packages[1].name, "a"); // apparent_size 500
    }

    #[test]
    fn test_sort_by_files_descending() {
        let mut report = ScanReport {
            packages: vec![
                PackageResult {
                    name: "small_dir".to_string(),
                    real_size: 100,
                    apparent_size: 100,
                    file_count: 5,
                    metadata_size: 30,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "large_dir".to_string(),
                    real_size: 500,
                    apparent_size: 500,
                    file_count: 20,
                    metadata_size: 60,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
            ..Default::default()
        };
        report.sort(SortField::Files);
        assert_eq!(report.packages[0].name, "large_dir"); // 20 files
        assert_eq!(report.packages[1].name, "small_dir"); // 5 files
    }

    #[test]
    fn test_sort_real() {
        let mut report = ScanReport {
            packages: vec![
                PackageResult {
                    name: "alpha".to_string(),
                    real_size: 300,
                    apparent_size: 500,
                    file_count: 1,
                    metadata_size: 50,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "beta".to_string(),
                    real_size: 200,
                    apparent_size: 100,
                    file_count: 2,
                    metadata_size: 60,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "gamma".to_string(),
                    real_size: 200,
                    apparent_size: 800,
                    file_count: 3,
                    metadata_size: 30,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
            ..Default::default()
        };
        report.sort(SortField::Real);
        assert_eq!(report.packages[0].name, "alpha"); // real_size 300
        assert_eq!(report.packages[1].name, "beta"); // real_size 200, name asc (beta < gamma)
        assert_eq!(report.packages[2].name, "gamma"); // real_size 200
    }

    #[test]
    fn test_sort_name() {
        let mut report = ScanReport {
            packages: vec![
                PackageResult {
                    name: "z".to_string(),
                    real_size: 100,
                    apparent_size: 200,
                    file_count: 1,
                    metadata_size: 50,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "a".to_string(),
                    real_size: 300,
                    apparent_size: 400,
                    file_count: 2,
                    metadata_size: 60,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "m".to_string(),
                    real_size: 50,
                    apparent_size: 60,
                    file_count: 3,
                    metadata_size: 30,
                    btrfs_disk: None,
                    version: "1.0-1".into(),
                },
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
            ..Default::default()
        };
        report.sort(SortField::Name);
        assert_eq!(report.packages[0].name, "a");
        assert_eq!(report.packages[1].name, "m");
        assert_eq!(report.packages[2].name, "z");
    }

    fn make_pkg(
        name: &str,
        real: u64,
        apparent: u64,
        files: u64,
        meta: u64,
        comp: Option<u64>,
    ) -> PackageResult {
        PackageResult {
            name: name.into(),
            version: "1.0-1".into(),
            real_size: real,
            apparent_size: apparent,
            file_count: files,
            metadata_size: meta,
            btrfs_disk: comp,
        }
    }

    #[test]
    fn test_sort_ratio_none_last() {
        let mut report = ScanReport {
            packages: vec![
                make_pkg("alpha", 200, 100, 1, 50, None),
                make_pkg("beta", 300, 100, 2, 60, Some(100)),
                make_pkg("gamma", 400, 100, 3, 30, None),
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
            ..Default::default()
        };
        report.sort(SortField::Ratio);
        assert_eq!(report.packages[0].name, "beta"); // has Some(compressed) -> comes first
        assert_eq!(report.packages[1].name, "alpha"); // None last (name asc tie-break)
        assert_eq!(report.packages[2].name, "gamma");
    }

    #[test]
    fn test_sort_ratio_all_none() {
        let mut report = ScanReport {
            packages: vec![
                make_pkg("z", 300, 100, 1, 50, None),
                make_pkg("a", 200, 100, 2, 60, None),
                make_pkg("m", 400, 100, 3, 30, None),
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
            ..Default::default()
        };
        report.sort(SortField::Ratio);
        assert_eq!(report.packages[0].name, "a"); // all None -> name ascending
        assert_eq!(report.packages[1].name, "m");
        assert_eq!(report.packages[2].name, "z");
    }

    #[test]
    fn test_sort_ratio_best_compression_first() {
        // Values chosen so the new ascending key (compressed/apparent) and the
        // old descending key (real/compressed) would produce different orders.
        let mut report = ScanReport {
            packages: vec![
                make_pkg("alpha", 100, 100, 1, 50, Some(50)), // 50%; old key 2.0
                make_pkg("beta", 1000, 100, 2, 60, Some(60)), // 60%; old key 16.7
                make_pkg("gamma", 200, 100, 3, 30, Some(80)), // 80%; old key 2.5
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
            ..Default::default()
        };
        report.sort(SortField::Ratio);
        assert_eq!(report.packages[0].name, "alpha"); // 50% (best)
        assert_eq!(report.packages[1].name, "beta"); // 60%
        assert_eq!(report.packages[2].name, "gamma"); // 80% (worst)
    }

    #[test]
    fn test_sort_ratio_tie_break_on_name() {
        let mut report = ScanReport {
            packages: vec![
                make_pkg("gamma", 300, 100, 1, 50, Some(50)), // all 50% -> name asc
                make_pkg("beta", 200, 100, 2, 60, Some(50)),
                make_pkg("alpha", 400, 100, 3, 30, Some(50)),
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
            ..Default::default()
        };
        report.sort(SortField::Ratio);
        assert_eq!(report.packages[0].name, "alpha");
        assert_eq!(report.packages[1].name, "beta");
        assert_eq!(report.packages[2].name, "gamma");
    }

    #[test]
    fn test_btrfs_ratio_percent() {
        // 50 compressed / 200 apparent => 25%
        assert_eq!(
            make_pkg("a", 100, 200, 1, 0, Some(50)).btrfs_ratio_percent(),
            Some(25.0)
        );
        // no btrfs data
        assert_eq!(
            make_pkg("a", 100, 200, 1, 0, None).btrfs_ratio_percent(),
            None
        );
        // apparent size zero => undefined, not infinity
        assert_eq!(
            make_pkg("a", 100, 0, 1, 0, Some(50)).btrfs_ratio_percent(),
            None
        );
    }

    #[test]
    fn test_limit() {
        let entries = (0u64..10)
            .map(|i| PackageResult {
                name: format!("pkg{}", i),
                version: "1.0-1".into(),
                real_size: i * 10,
                apparent_size: i * 10,
                file_count: i,
                metadata_size: 10,
                btrfs_disk: None,
            })
            .collect();

        let mut report = ScanReport {
            packages: entries,
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
            ..Default::default()
        };
        report.sort(SortField::Real);
        report.truncate(3);
        assert_eq!(report.packages.len(), 3);

        // Verify descending order was maintained (largest real_size first) — since all same ratio=0, secondary is name asc
        assert_ne!(report.packages[0].real_size, report.packages[2].real_size);
    }

    #[test]
    fn test_truncate() {
        let entries = (0u64..10)
            .map(|i| PackageResult {
                name: format!("pkg{}", i),
                version: "1.0-1".into(),
                real_size: i * 10,
                apparent_size: i * 10,
                file_count: i,
                metadata_size: 10,
                btrfs_disk: None,
            })
            .collect();

        let mut report = ScanReport {
            packages: entries,
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
            ..Default::default()
        };
        report.truncate(5);
        assert_eq!(report.packages.len(), 5);

        // Truncate to same size — no change
        report.truncate(5);
        assert_eq!(report.packages.len(), 5);

        // Truncate to larger — no change (just keep as is)
        report.truncate(15);
        assert_eq!(report.packages.len(), 5);
    }

    #[test]
    fn test_skip_filesystem_marker() {
        use crate::pacman;
        // A real file named `.FILESYSTEM` (pacman's marker) must be skipped.
        let dir = std::env::temp_dir().join("pkgdu_test_filesystem_marker");
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join(".FILESYSTEM");
        std::fs::write(&marker, b"marker").unwrap();

        let entry = pacman::PackageEntry {
            name: "pkg".to_string(),
            version: "1.0-1".to_string(),
            metadata_size: 42,
            files: vec![marker.clone()],
        };
        let s = stat_package(&entry, Path::new("/"), false, false);
        let (apparent, real, count, _warns) = (s.apparent, s.real, s.count, s.warns);
        assert_eq!(apparent, 0);
        assert_eq!(real, 0);
        assert_eq!(count, 0);

        std::fs::remove_file(&marker).ok();
        std::fs::remove_dir(&dir).ok();
    }
}
