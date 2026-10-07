use crate::btrfs;
use crate::config::{Config, SortField};
use crate::error::Result;
use crate::pacman::{load_local_db, Filter};
use rayon::prelude::*;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone, Copy, PartialEq)]
enum StatResult {
    Success(u64, u64),
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
                StatResult::Success(meta.len(), meta.blocks() * 512)
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
    pub btrfs_compressed: Option<u64>,
}

/// Full scan report with metadata about skipped packages and errors.
#[derive(Debug)]
pub struct ScanReport {
    pub packages: Vec<PackageResult>,
    pub skipped_packages: usize,
    pub permission_errors: usize,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl PackageResult {
    /// btrfs compression ratio as a percentage of apparent size, so a smaller
    /// value means better compression. `None` when compressed data is
    /// unavailable or apparent size is zero (ratio undefined).
    pub fn btrfs_ratio_percent(&self) -> Option<f64> {
        match self.btrfs_compressed {
            Some(compressed) if self.apparent_size > 0 => {
                Some(compressed as f64 / self.apparent_size as f64 * 100.0)
            }
            _ => None,
        }
    }
}

/// Stat all files of a package entry, collecting sizes and warnings.
fn stat_package(entry: &crate::pacman::PackageEntry, root: &Path) -> (u64, u64, u64, Vec<String>) {
    let mut apparent = 0u64;
    let mut real = 0u64;
    let mut count = 0u64;
    let mut warns: Vec<String> = Vec::new();

    for file_path in &entry.files {
        // Resolve relative paths against root
        let full = if file_path.is_absolute() {
            file_path.to_path_buf()
        } else {
            root.join(file_path)
        };

        // Skip .FILESYSTEM marker entries
        if file_path
            .file_name()
            .map(|n| n == "FILESYSTEM")
            .unwrap_or(false)
        {
            continue;
        }

        match stat_file(&full) {
            StatResult::Success(app, r) => {
                apparent += app;
                real += r;
                count += 1;
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

    (apparent, real, count, warns)
}

/// Resolve dbpath from config.
fn resolve_dbpath(config: &Config) -> PathBuf {
    if !config.dbpath.exists() {
        // Fallback to default DB path when overridden path doesn't exist
        std::path::Path::new("/var/lib/pacman").to_path_buf()
    } else {
        config.dbpath.clone()
    }
}
/// Run a full scan over the configured package set.
pub fn scan_packages(config: &Config) -> Result<ScanReport> {
    let dbpath = resolve_dbpath(config);

    // Build filter from targets or search
    let filter = Filter {
        targets: config.targets.clone(),
        search: config.search.clone(),
    };

    // Load and parse pacman local DB (parallel over parsed pkg lists)
    let (entries, skipped, load_errors) = load_local_db(&dbpath, &filter)?;

    if entries.is_empty() {
        return Ok(ScanReport {
            packages: vec![],
            skipped_packages: skipped,
            permission_errors: 0,
            errors: load_errors,
            warnings: Vec::new(),
        });
    }

    // Sort results
    let sort = config.sort;

    // Collect warnings from file scanning
    let mut all_warns_count = 0usize;
    let mut all_warns: Vec<String> = Vec::new();

    // Query btrfs compressed sizes if enabled
    let mut btrfs_errors: Vec<String> = Vec::new();
    let btrfs_compressed: Vec<Option<u64>> = if config.btrfs {
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

    let scanned: Vec<(u64, u64, u64, Vec<String>)> = entries
        .par_iter()
        .map(|entry| {
            let stat = stat_package(entry, &config.root);
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
    let mut result = ScanReport {
        packages: entries
            .iter()
            .zip(scanned.iter())
            .zip(btrfs_compressed.iter())
            .map(
                |((entry, (apparent, real, file_count, warns)), &comp)| -> PackageResult {
                    all_warns_count += warns.len();
                    all_warns.extend(warns.iter().cloned());

                    PackageResult {
                        name: entry.name.clone(),
                        version: entry.version.clone(),
                        real_size: *real,
                        apparent_size: *apparent,
                        file_count: *file_count,
                        metadata_size: entry.metadata_size,
                        btrfs_compressed: comp,
                    }
                },
            )
            .collect(),
        skipped_packages: skipped,
        permission_errors: all_warns_count,
        errors: {
            // Malformed-package diagnostics plus btrfs errors.
            let mut errors = load_errors;
            errors.append(&mut btrfs_errors);
            errors
        },
        warnings: all_warns,
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
        self.packages.sort_by(|a, b| {
            match (a.btrfs_ratio_percent(), b.btrfs_ratio_percent()) {
                (Some(ra), Some(rb)) => ra
                    .partial_cmp(&rb)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.name.cmp(&b.name)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.name.cmp(&b.name),
            }
        });
    }

    /// Limit packages to the first N (by order).
    pub fn truncate(&mut self, limit: usize) {
        if self.packages.len() > limit {
            let _extra = self.packages.drain(limit..); // drop extra without unwrapping
        }
    }
}

/// Resolve system path against root prefix.
#[allow(dead_code)]
pub(crate) fn resolve_path(root: &Path, rel: &str) -> PathBuf {
    if let Some(p) = rel.strip_prefix("/") {
        root.join(p)
    } else {
        root.join(rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_resolve_path_absolute() {
        assert_eq!(
            resolve_path(Path::new("/mnt"), "/usr/bin/ls"),
            PathBuf::from("/mnt/usr/bin/ls")
        );
    }

    #[test]
    fn test_resolve_path_relative() {
        assert_eq!(
            resolve_path(Path::new("/mnt"), "etc/passwd"),
            PathBuf::from("/mnt/etc/passwd")
        );
    }

    #[test]
    fn test_stat_package_empty() {
        use crate::pacman;
        let entry = pacman::PackageEntry {
            name: "empty_pkg".to_string(),
            version: "1.0-1".to_string(),
            metadata_size: 0,
            files: vec![],
        };
        let (apparent, real, count, warns) = stat_package(&entry, Path::new("/"));
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
        let (apparent, real, count, warns) = stat_package(&entry, Path::new("/"));
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
        let (apparent, real, count, warns) = stat_package(&entry, Path::new("/"));
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
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "b".to_string(),
                    real_size: 200,
                    apparent_size: 200,
                    file_count: 2,
                    metadata_size: 60,
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "c".to_string(),
                    real_size: 50,
                    apparent_size: 50,
                    file_count: 1,
                    metadata_size: 30,
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
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
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "a".to_string(),
                    real_size: 200,
                    apparent_size: 200,
                    file_count: 2,
                    metadata_size: 60,
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "m".to_string(),
                    real_size: 50,
                    apparent_size: 50,
                    file_count: 1,
                    metadata_size: 30,
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
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
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "b".to_string(),
                    real_size: 200,
                    apparent_size: 1000,
                    file_count: 2,
                    metadata_size: 60,
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
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
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "large_dir".to_string(),
                    real_size: 500,
                    apparent_size: 500,
                    file_count: 20,
                    metadata_size: 60,
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
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
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "beta".to_string(),
                    real_size: 200,
                    apparent_size: 100,
                    file_count: 2,
                    metadata_size: 60,
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "gamma".to_string(),
                    real_size: 200,
                    apparent_size: 800,
                    file_count: 3,
                    metadata_size: 30,
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
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
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "a".to_string(),
                    real_size: 300,
                    apparent_size: 400,
                    file_count: 2,
                    metadata_size: 60,
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
                PackageResult {
                    name: "m".to_string(),
                    real_size: 50,
                    apparent_size: 60,
                    file_count: 3,
                    metadata_size: 30,
                    btrfs_compressed: None,
                    version: "1.0-1".into(),
                },
            ],
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
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
            btrfs_compressed: comp,
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
        // no compressed data
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
                btrfs_compressed: None,
            })
            .collect();

        let mut report = ScanReport {
            packages: entries,
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
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
                btrfs_compressed: None,
            })
            .collect();

        let mut report = ScanReport {
            packages: entries,
            skipped_packages: 0,
            permission_errors: 0,
            errors: vec![],
            warnings: vec![],
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
        // Test package entry with .FILESYSTEM marker
        let entry = pacman::PackageEntry {
            name: "pkg".to_string(),
            version: "1.0-1".to_string(),
            metadata_size: 42,
            files: vec![PathBuf::from("/.FILESYSTEM")],
        };
        let (apparent, real, count, _warns) = stat_package(&entry, Path::new("/"));
        assert_eq!(apparent, 0); // FILESYSTEM should be skipped
        assert_eq!(real, 0);
        assert_eq!(count, 0);
    }
}
