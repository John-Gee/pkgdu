use std::path::{Path, PathBuf};

/// Fields extracted from a `%desc%` package metadata file.
#[derive(Debug, Clone)]
pub struct DescFields {
    pub name: String,
    pub version: String,
    pub size: u64,
}

/// Search/filter configuration — by target name(s) and/or a regex search pattern.
#[derive(Debug, Clone)]
pub struct Filter {
    pub targets: Vec<String>,
    pub search: Option<regex::Regex>,
}

/// A single package entry as loaded from the local database.
#[derive(Debug, Clone)]
pub struct PackageEntry {
    pub name: String,
    pub version: String,
    pub metadata_size: u64,
    pub files: Vec<PathBuf>,
}

/// Minimal pacman configuration read from `pacman.conf`.
#[derive(Debug, Clone)]
pub struct PacmanConf {
    pub dbpath: PathBuf,
}

/// Parse a pacman desc file and extract %NAME%, %VERSION%, %SIZE% fields.
///
/// The desc format uses `%KEYWORD%` headers followed by value lines.
/// Empty NAME or VERSION returns an error. Missing/unparseable SIZE defaults to 0.
pub fn parse_desc(text: &str) -> crate::error::Result<DescFields> {
    let mut current_key: Option<String> = None;
    let mut values: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for line in text.lines() {
        if let Some(stripped) = line.strip_prefix('%') {
            if stripped.ends_with('%') {
                let key = stripped.strip_suffix('%').unwrap_or("").to_string();
                if !key.is_empty() {
                    if let Some(prev_key) = current_key.take() {
                        values.entry(prev_key).or_default();
                    }
                    current_key = Some(key);
                } else {
                    // `%` followed by `%` — ignore as malformed header
                }
            } else {
                // `%FOOBAR` without closing `%` — treat as value of current key
                if let Some(k) = &current_key {
                    values.entry(k.clone()).or_default().push(line.to_string());
                }
            }
        } else {
            // Value line for the current key
            if let Some(k) = &current_key {
                values.entry(k.clone()).or_default().push(line.to_string());
            }
        }
    }

    // Capture final accumulated values for any remaining key
    if let Some(prev_key) = current_key.take() {
        values.entry(prev_key).or_default();
    }

    // Extract NAME — must be present and non-empty
    let name_lines = values.get("NAME").cloned().unwrap_or_default();
    let name = last_non_empty(&name_lines);
    if name.is_none() || name.as_ref().map(|s| s.trim().is_empty()).unwrap_or(true) {
        return Err(crate::error::PkgduError::ParsePacmanConf(
            "empty NAME".to_string(),
        ));
    }

    // Extract VERSION — must be present and non-empty
    let version_lines = values.get("VERSION").cloned().unwrap_or_default();
    let version = last_non_empty(&version_lines);
    if version.is_none() || version.as_ref().map(|s| s.trim().is_empty()).unwrap_or(true) {
        return Err(crate::error::PkgduError::ParsePacmanConf(
            "empty VERSION".to_string(),
        ));
    }

    // Extract SIZE — missing or unparseable → 0
    let size: u64 = values
        .get("SIZE")
        .map(|lines| last_non_empty(&lines).unwrap_or_default().trim().parse().unwrap_or(0))
        .unwrap_or(0);

    Ok(DescFields {
        name: name.unwrap(),
        version: version.unwrap(),
        size,
    })
}

/// Return the last non-empty (trimmed) line from a Vec<String>, or None if all empty.
fn last_non_empty(lines: &[String]) -> Option<String> {
    lines.iter().rev().find(|s| !s.trim().is_empty()).map(|s| s.trim().to_string())
}

pub fn parse_files(text: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut in_files = false;

    for line in text.lines() {
        if let Some(stripped) = line.strip_prefix('%') {
            if stripped.ends_with('%') {
                let header = stripped.strip_suffix('%').unwrap_or("");
                if header == "FILES" {
                    in_files = true;
                } else if in_files {
                    break;
                }
                continue;
            }
        }

        if in_files {
            let path = line.trim();
            if path.is_empty() {
                continue;
            }
            if path.ends_with('/') {
                continue;
            }
            paths.push(PathBuf::from(path));
        }
    }

    paths
}

/// Parse `pacman.conf` and return a minimal `PacmanConf`.
///
/// Parses `key = value` lines (with `#` comments). Extracts the `DBPath` key;
/// if the file is missing, does not exist, or lacks a valid `DBPath`, returns
/// the default dbpath `/var/lib/pacman` silently.
pub fn parse_pacman_conf(path: &Path) -> crate::error::Result<PacmanConf> {
    let default_db = PathBuf::from("/var/lib/pacman");

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(PacmanConf {
            dbpath: default_db,
        }),
    };

    let mut dbpath: Option<PathBuf> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip comments (lines starting with #) and empty lines
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }

        // Check for [section] headers — skip them
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            continue;
        }

        // Parse key = value
        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim();
            let value = trim_comment(trimmed[eq_pos + 1..].trim());

            if key.eq_ignore_ascii_case("dbpath") && !value.is_empty() {
                dbpath = Some(PathBuf::from(value));
            } else if key.eq_ignore_ascii_case("rootdir") && !value.is_empty() {
                // Just track it; not needed for our minimal usage yet
            }
        }
    }

    Ok(PacmanConf {
        dbpath: dbpath.unwrap_or(default_db),
    })
}

/// Strip a trailing inline comment (starting with `#`) from a config value.
/// Strips if the `#` is preceded by whitespace or appears at position 0.
fn trim_comment(s: &str) -> &str {
    match s.rfind('#') {
        Some(0) => "",
        Some(idx) => {
            if s[..idx].ends_with(char::is_whitespace) {
                s[..idx].trim_end()
            } else {
                s
            }
        }
        None => s,
    }
}

/// Check whether a package should be included per the filter.
fn matches_filter(name: &str, filter: &Filter) -> bool {
    let targets_match = if filter.targets.is_empty() {
        true
    } else {
        filter.targets.iter().any(|t| t == name)
    };
    let search_match = match &filter.search {
        None => true,
        Some(re) => re.is_match(name),
    };
    targets_match && search_match
}

/// Load packages from a local pacman database directory.
///
/// Walks `{dbpath}/local`, parses desc and files for each subdirectory,
/// applies the filter, and returns (entries, skipped_count, errors).
pub fn load_local_db(
    dbpath: &Path,
    filter: &Filter,
) -> crate::error::Result<(Vec<PackageEntry>, usize, Vec<String>)> {
    let local_dir = dbpath.join("local");
    let mut entries = Vec::new();
    let mut skipped = 0usize;
    let mut errors = Vec::new();

    let mut dirs = match std::fs::read_dir(&local_dir) {
        Ok(d) => d,
        Err(e) => return Err(crate::error::PkgduError::Io(e)),
    };

    for entry_result in dirs {
        let entry = match entry_result {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();

        // Skip non-directories
        if !path.is_dir() {
            continue;
        }

        let basename = path.file_name().map(|s| s.to_string_lossy().into_owned());

        // Skip ALPM_DB_VERSION file
        if let Some(ref name) = basename {
            if name == "ALPM_DB_VERSION" {
                continue;
            }
        }

        let desc_path = path.join("desc");
        let files_path = path.join("files");

        // Only process directories that have a desc file
        if !desc_path.is_file() {
            continue;
        }

        // Read and parse desc
        let desc_content = match std::fs::read_to_string(&desc_path) {
            Ok(c) => c,
            Err(_) => {
                skipped += 1;
                errors.push(format!("Failed to read desc for {}", basename.unwrap_or_default()));
                continue;
            }
        };

        let desc_fields = match parse_desc(&desc_content) {
            Ok(f) => f,
            Err(e) => {
                skipped += 1;
                errors.push(format!(
                    "Malformed desc for {}: {}",
                    basename.unwrap_or_default(),
                    e
                ));
                continue;
            }
        };

        // Read and parse files (missing = empty vec)
        let files = match std::fs::read_to_string(&files_path) {
            Ok(c) => parse_files(&c),
            Err(_) => Vec::new(),
        };

        // Apply filter
        if !matches_filter(&desc_fields.name, filter) {
            continue;
        }

        entries.push(PackageEntry {
            name: desc_fields.name,
            version: desc_fields.version,
            metadata_size: desc_fields.size,
            files,
        });
    }

    Ok((entries, skipped, errors))
}

