use pkgdu::config::{Config, SortField};
use pkgdu::pacman::{load_local_db, Filter};
use pkgdu::scan;
use std::fs;
use std::io::Write;
use tempfile::TempDir;

/// Create a test chroot with:
///   - {tmp}/root/usr/lib/testlib.so (1024 bytes of 0xAB)
///   - {tmp}/root/usr/bin/testbin (4096 bytes of 0xCD)
///   - {tmp}/db/local/testpkg-1.0-1/desc
///   - {tmp}/db/local/testpkg-1.0-1/files
///   - {tmp}/db/local/secpkg-2.0-1/desc
///   - {tmp}/db/local/secpkg-2.0-1/files
///   - {tmp}/db/local/emptypkg-1.0-1/desc
///   - {tmp}/db/local/emptypkg-1.0-1/files
///
/// Returns (TempDir, Config) with --root/--dbpath pointing at the chroot.
fn create_test_chroot() -> (TempDir, Config) {
    let tmp = TempDir::new().expect("create temp dir");
    let path = tmp.path();

    // Create root directory structure
    let root = path.join("root");
    fs::create_dir_all(root.join("usr/lib")).unwrap();
    fs::create_dir_all(root.join("usr/bin")).unwrap();

    // Create test files with non-zero byte patterns (to avoid sparse files)
    {
        let mut f = fs::File::create(root.join("usr/lib/testlib.so")).unwrap();
        let data: Vec<u8> = vec![0xAB; 1024];
        f.write_all(&data).unwrap();
        f.flush().unwrap();
    }
    {
        let mut f = fs::File::create(root.join("usr/bin/testbin")).unwrap();
        let data: Vec<u8> = vec![0xCD; 4096];
        f.write_all(&data).unwrap();
        f.flush().unwrap();
    }

    // Create DB directory structure
    let db = path.join("db");
    let testpkg_dir = db.join("local/testpkg-1.0-1");
    let secpkg_dir = db.join("local/secpkg-2.0-1");
    let emptypkg_dir = db.join("local/emptypkg-1.0-1");
    fs::create_dir_all(&testpkg_dir).unwrap();
    fs::create_dir_all(&secpkg_dir).unwrap();
    fs::create_dir_all(&emptypkg_dir).unwrap();

    // Write desc files
    let testpkg_desc = r###"%NAME%
testpkg
%VERSION%
1.0-1
%SIZE%
5120
%DESC%
Test package for integration testing
%URL%
https://example.com
%FILES%
usr/lib/testlib.so
usr/bin/testbin
"###;
    let secpkg_desc = r###"%NAME%
secpkg
%VERSION%
2.0-1
%SIZE%
2048
%DESC%
Security package with extra file
%FILES%
usr/bin/testbin
usr/lib/secret.bin
"###;
    let emptypkg_desc = r###"%NAME%
emptypkg
%VERSION%
1.0-1
%SIZE%
1024
%DESC%
Package with zero files
%FILES%
"###;
    fs::write(testpkg_dir.join("desc"), testpkg_desc).unwrap();
    fs::write(secpkg_dir.join("desc"), secpkg_desc).unwrap();
    fs::write(emptypkg_dir.join("desc"), emptypkg_desc).unwrap();

    // Write files manifests
    fs::write(
        testpkg_dir.join("files"),
        "usr/lib/testlib.so\nusr/bin/testbin\n",
    )
    .unwrap();
    fs::write(
        secpkg_dir.join("files"),
        "usr/bin/testbin\nusr/lib/secret.bin\n",
    )
    .unwrap();
    fs::write(emptypkg_dir.join("files"), "").unwrap();

    let config = Config {
        root,
        dbpath: db,
        targets: Vec::new(),
        search: None,
        sort: SortField::Real,
        limit: None,
        btrfs: false,
        verbose: false,
        format: None,
        humansize: None,
        delim: "\n".to_string(),
        no_color: false,
        apparent_size: false,
        total: false,
        files: false,
    };

    (tmp, config)
}

#[test]
fn test_chroot_creation() {
    let (tmp, config) = create_test_chroot();
    let path = tmp.path();

    // Verify root files exist
    assert!(
        path.join("root/usr/lib/testlib.so").exists(),
        "testlib.so should exist"
    );
    assert!(
        path.join("root/usr/bin/testbin").exists(),
        "testbin should exist"
    );

    // Verify DB files exist
    assert!(
        path.join("db/local/testpkg-1.0-1/desc").exists(),
        "testpkg desc should exist"
    );
    assert!(
        path.join("db/local/testpkg-1.0-1/files").exists(),
        "testpkg files should exist"
    );
    assert!(
        path.join("db/local/secpkg-2.0-1/desc").exists(),
        "secpkg desc should exist"
    );
    assert!(
        path.join("db/local/emptypkg-1.0-1/desc").exists(),
        "emptypkg desc should exist"
    );

    // Verify config paths
    assert_eq!(config.root, path.join("root"));
    assert_eq!(config.dbpath, path.join("db"));
}

#[test]
fn test_load_local_db() {
    let (_tmp, config) = create_test_chroot();

    let filter = Filter {
        targets: vec![],
        search: None,
    };

    let (entries, _skipped, _errors) = load_local_db(&config.dbpath, &filter).unwrap();

    // Should have 3 packages
    assert_eq!(entries.len(), 3);

    // Check package names
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"testpkg"));
    assert!(names.contains(&"secpkg"));
    assert!(names.contains(&"emptypkg"));
}

#[test]
fn test_scan_single_package() {
    let (_tmp, mut config) = create_test_chroot();

    // Filter to only testpkg
    config.targets = vec!["testpkg".to_string()];

    let report = scan::scan_packages(&config).unwrap();

    // Should have exactly 1 package
    assert_eq!(report.packages.len(), 1);

    let pkg = &report.packages[0];
    assert_eq!(pkg.name, "testpkg");
    assert_eq!(pkg.version, "1.0-1");

    // Apparent size should be exact file sizes (1024 + 4096 = 5120)
    assert_eq!(pkg.apparent_size, 5120);

    // File count should be 2
    assert_eq!(pkg.file_count, 2);

    // Real size should be > 0 (st_blocks varies by FS, so just check > 0)
    assert!(pkg.real_size > 0);
}

#[test]
fn test_scan_missing_files() {
    let (_tmp, mut config) = create_test_chroot();

    // secpkg references usr/lib/secret.bin which doesn't exist
    config.targets = vec!["secpkg".to_string()];

    let report = scan::scan_packages(&config).unwrap();

    assert_eq!(report.packages.len(), 1);
    let pkg = &report.packages[0];
    assert_eq!(pkg.name, "secpkg");

    // Only testbin exists, secret.bin is missing (silently skipped)
    assert_eq!(pkg.apparent_size, 4096); // only testbin's size
    assert_eq!(pkg.file_count, 1); // only 1 file was successfully stat'd
}

#[test]
fn test_scan_empty_package() {
    let (_tmp, mut config) = create_test_chroot();

    config.targets = vec!["emptypkg".to_string()];

    let report = scan::scan_packages(&config).unwrap();

    assert_eq!(report.packages.len(), 1);
    let pkg = &report.packages[0];
    assert_eq!(pkg.name, "emptypkg");
    assert_eq!(pkg.apparent_size, 0);
    assert_eq!(pkg.real_size, 0);
    assert_eq!(pkg.file_count, 0);
}

#[test]
fn test_scan_multiple_packages() {
    let (_tmp, mut config) = create_test_chroot();

    // Scan all packages
    let report = scan::scan_packages(&config).unwrap();

    assert_eq!(report.packages.len(), 3);

    // Collect apparent sizes
    let total_apparent: u64 = report.packages.iter().map(|p| p.apparent_size).sum();

    // testpkg: 5120, secpkg: 4096 (1 file missing), emptypkg: 0
    assert_eq!(total_apparent, 5120 + 4096 + 0);
}

#[test]
fn test_scan_limit() {
    let (_tmp, mut config) = create_test_chroot();

    // Set limit to 2
    config.limit = Some(2);

    let report = scan::scan_packages(&config).unwrap();

    assert_eq!(report.packages.len(), 2);
}

#[test]
fn test_scan_search_regex() {
    use regex::Regex;
    let (_tmp, mut config) = create_test_chroot();

    config.search = Some(Regex::new("test").unwrap());

    let report = scan::scan_packages(&config).unwrap();

    // Should only have testpkg (secpkg doesn't match "test")
    assert_eq!(report.packages.len(), 1);
    assert_eq!(report.packages[0].name, "testpkg");
}

#[test]
fn test_scan_sort_by_real() {
    let (_tmp, mut config) = create_test_chroot();

    let report = scan::scan_packages(&config).unwrap();

    // Packages should be sorted by real size descending
    // testpkg (5120 apparent) > secpkg (4096 apparent) > emptypkg (0)
    assert_eq!(report.packages[0].name, "testpkg");
    assert_eq!(report.packages[1].name, "secpkg");
    assert_eq!(report.packages[2].name, "emptypkg");
}

#[test]
fn test_scan_symlinks_skipped() {
    let (_tmp, mut config) = create_test_chroot();
    config.targets = vec!["testpkg".to_string()];

    // Add a symlink under the root and reference it from testpkg's manifest.
    let link = config.root.join("usr/lib/testlink.so");
    std::os::unix::fs::symlink("testlib.so", &link).unwrap();
    let files_path = config.dbpath.join("local/testpkg-1.0-1/files");
    let mut manifest = fs::read_to_string(&files_path).unwrap();
    manifest.push_str("usr/lib/testlink.so\n");
    fs::write(&files_path, manifest).unwrap();

    let report = scan::scan_packages(&config).unwrap();
    let pkg = &report.packages[0];

    // The symlink must not contribute size or count.
    assert_eq!(pkg.apparent_size, 5120);
    assert_eq!(pkg.file_count, 2);
}

#[test]
fn test_scan_malformed_desc_is_reported_and_counted() {
    let (_tmp, config) = create_test_chroot();

    // A package dir whose desc has a NAME header but no value => malformed.
    let bad = config.dbpath.join("local").join("badpkg-1.0-1");
    fs::create_dir_all(&bad).unwrap();
    fs::write(
        bad.join("desc"),
        "%NAME%\n%VERSION%\n%SIZE%\n10\n",
    )
    .unwrap();

    let report = scan::scan_packages(&config).unwrap();

    // The malformed package is skipped, counted, and its error is surfaced.
    assert!(report.skipped_packages >= 1);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("badpkg") && e.contains("Malformed")),
        "expected a malformed-desc diagnostic, got: {:?}",
        report.errors
    );
}
