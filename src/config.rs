use std::path::{Path, PathBuf};

use crate::error::{PkgduError, Result};
use crate::format::FormatString;
use crate::human_size::UnitSpec;
use crate::pacman::parse_pacman_conf;
use regex::Regex;

/// Sorting field for --sort.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SortField {
    Name,
    Real,
    Apparent,
    Files,
    Ratio,
}

/// Raw CLI arguments — built by clap.
#[derive(clap::Parser, Debug)]
#[command(name = "pkgdu", about = "Real disk usage per package for Arch Linux")]
pub struct RawArgs {
    /// Format string (printf-style) or package names
    #[arg(trailing_var_arg = true)]
    pub positional: Vec<String>,

    /// Human-size unit (B, K, Ki, M, Mi, G, Gi, T, Ti, auto, auto-si)
    #[arg(short = 'H')]
    pub humansize: Option<String>,

    /// Search packages by regex on name
    #[arg(short = 's')]
    pub search: Option<String>,

    /// Sort by field: name (ascending), real/apparent/files (descending), ratio (ascending)
    #[arg(long, default_value = "real")]
    pub sort: String,

    /// Show only top N packages (0 = unlimited)
    #[arg(short = 'n', long)]
    pub limit: Option<usize>,

    /// Enable btrfs on-disk size tokens (%z, %r)
    #[arg(long)]
    pub btrfs: bool,

    /// Set filesystem root prefix
    #[arg(long, default_value = "/")]
    pub root: PathBuf,

    /// Override pacman local DB path (absolute)
    #[arg(long)]
    pub dbpath: Option<PathBuf>,

    /// Delimiter between packages in format mode
    #[arg(short = 'd', default_value = "\n")]
    pub delim: String,

    /// Show warnings for missing files, permission errors
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Disable color output
    #[arg(long)]
    pub no_color: bool,

    /// Show apparent file size instead of disk blocks (du --apparent-size)
    #[arg(long)]
    pub apparent_size: bool,

    /// Show total row at the bottom of table output
    #[arg(long)]
    pub total: bool,

    /// Show file count column
    #[arg(long)]
    pub files: bool,
}

/// Final CLI configuration — built from RawArgs + validation.
pub struct Config {
    pub root: PathBuf,
    pub dbpath: PathBuf,
    pub targets: Vec<String>,
    pub search: Option<Regex>,
    pub sort: SortField,
    pub limit: Option<usize>,
    pub btrfs: bool,
    pub verbose: bool,
    pub format: Option<FormatString>,
    pub humansize: Option<UnitSpec>,
    pub delim: String,
    pub no_color: bool,
    pub apparent_size: bool,
    pub total: bool,
    pub files: bool,
}

/// Rebase an absolute path from the target system onto `root`, e.g.
/// `/var/lib/pacman` with root `/mnt/arch` -> `/mnt/arch/var/lib/pacman`.
/// With root `/` the path is returned unchanged.
fn rebase_under_root(root: &Path, path: &Path) -> PathBuf {
    match path.strip_prefix("/") {
        Ok(rel) => root.join(rel),
        Err(_) => root.join(path),
    }
}

impl RawArgs {
    /// Parse a humansize string into UnitSpec.
    fn parse_humansize(s: &str) -> std::result::Result<UnitSpec, String> {
        match s {
            "B" => Ok(UnitSpec::Raw),
            "K" => Ok(UnitSpec::K),
            "M" => Ok(UnitSpec::M),
            "G" => Ok(UnitSpec::G),
            "T" => Ok(UnitSpec::T),
            "Ki" => Ok(UnitSpec::Ki),
            "Mi" => Ok(UnitSpec::Mi),
            "Gi" => Ok(UnitSpec::Gi),
            "Ti" => Ok(UnitSpec::Ti),
            "auto" => Ok(UnitSpec::Auto),
            "auto-si" => Ok(UnitSpec::AutoSi),
            _ => Err(format!("invalid -H value: {}", s)),
        }
    }

    /// Parse a sort field string into SortField.
    fn parse_sort(s: &str) -> std::result::Result<SortField, String> {
        match s {
            "name" => Ok(SortField::Name),
            "real" | "size" => Ok(SortField::Real),
            "apparent" => Ok(SortField::Apparent),
            "files" => Ok(SortField::Files),
            "ratio" => Ok(SortField::Ratio),
            _ => Err(format!("invalid --sort value: {}", s)),
        }
    }

    /// Detect if the first positional argument is a format string
    /// (contains '%' characters).
    fn is_format_string(arg: &str) -> bool {
        arg.contains('%')
    }

    /// Convert RawArgs to Config with validation.
    pub fn into_config(self) -> Result<Config> {
        // 1. --- Format string detection ---
        // clap puts all positional args in `positional`. The first element containing '%'
        // is the format string; remaining elements are package targets.
        let format_str = self.positional.first().and_then(|s| {
            if Self::is_format_string(s) {
                Some(s.as_str())
            } else {
                None
            }
        });

        let targets = if format_str.is_some() {
            self.positional[1..].to_vec()
        } else {
            self.positional.clone()
        };

        let format = match format_str {
            Some(s) => Some(FormatString::parse(s).map_err(PkgduError::Config)?),
            None => None,
        };

        let in_format_mode = format.is_some();

        // 2. --- Search regex ---
        let search = if let Some(ref s) = self.search {
            Some(
                Regex::new(s)
                    .map_err(|e| PkgduError::Config(format!("search regex error: {}", e)))?,
            )
        } else {
            None
        };

        // 3. --- Sort field ---
        let sort = Self::parse_sort(&self.sort).map_err(PkgduError::Config)?;

        // 4. --- Root existence check ---
        if !self.root.exists() {
            return Err(PkgduError::Config(format!(
                "root path does not exist: {}",
                self.root.display()
            )));
        }

        // 5. --- DBPath resolution ---
        // --dbpath is absolute and used as-is (not rebased under --root, per
        // the documented contract). Otherwise the target's DBPath (from
        // {root}/etc/pacman.conf, default /var/lib/pacman) is rebased under
        // --root so that scanning a chroot reads that chroot's database.
        let dbpath = if let Some(ref db) = self.dbpath {
            if !db.is_absolute() {
                return Err(PkgduError::Config(
                    "--dbpath must be an absolute path".to_string(),
                ));
            }
            db.clone()
        } else {
            let configured = self
                .root
                .join("etc/pacman.conf")
                .exists()
                .then(|| parse_pacman_conf(&self.root.join("etc/pacman.conf")).ok())
                .flatten()
                .map(|conf| conf.dbpath)
                .unwrap_or_else(|| PathBuf::from("/var/lib/pacman"));
            rebase_under_root(&self.root, &configured)
        };

        // 6. --- dbpath existence check ---
        if !dbpath.exists() {
            return Err(PkgduError::Config(format!(
                "dbpath does not exist: {}",
                dbpath.display()
            )));
        }

        // 7. --- Limit default ---
        // Explicit -n always wins (0 => unlimited). Without -n, table mode
        // defaults to 20; format mode defaults to unlimited.
        let limit = match self.limit {
            Some(0) => None,
            Some(n) => Some(n),
            None if in_format_mode => None,
            None => Some(20),
        };

        // Human-size unit. The default depends on the mode: raw bytes for the
        // machine-facing format string, auto for the human-facing table.
        let humansize = match self.humansize {
            Some(ref hs) => Some(Self::parse_humansize(hs).map_err(PkgduError::Config)?),
            None if in_format_mode => None,
            None => Some(UnitSpec::Auto),
        };

        Ok(Config {
            root: self.root,
            dbpath,
            targets,
            search,
            sort,
            limit,
            btrfs: self.btrfs,
            verbose: self.verbose,
            format,
            humansize,
            delim: self.delim,
            no_color: self.no_color,
            apparent_size: self.apparent_size,
            total: self.total,
            files: self.files,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_HUMAN_SIZE_VALUES: &[&str] = &[
        "B", "K", "Ki", "M", "Mi", "G", "Gi", "T", "Ti", "auto", "auto-si",
    ];

    #[test]
    fn test_parse_humansize_valid_values() {
        for val in VALID_HUMAN_SIZE_VALUES {
            let result = RawArgs::parse_humansize(val);
            assert!(
                result.is_ok(),
                "expected valid UnitSpec for '{}', got: {:?}",
                val,
                result.err()
            );
        }
    }

    #[test]
    fn test_parse_humansize_invalid_value() {
        let result = RawArgs::parse_humansize("invalid_xyz");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_humansize_si_and_iec_are_distinct() {
        assert!(matches!(
            RawArgs::parse_humansize("K").unwrap(),
            UnitSpec::K
        ));
        assert!(matches!(
            RawArgs::parse_humansize("Ki").unwrap(),
            UnitSpec::Ki
        ));
        assert!(matches!(
            RawArgs::parse_humansize("M").unwrap(),
            UnitSpec::M
        ));
        assert!(matches!(
            RawArgs::parse_humansize("Mi").unwrap(),
            UnitSpec::Mi
        ));
    }

    #[test]
    fn test_format_mode_default_unit_is_raw() {
        let raw = <RawArgs as clap::Parser>::parse_from(["pkgdu", "%m"]);
        let config = raw.into_config().unwrap();
        assert!(config.humansize.is_none());
    }

    #[test]
    fn test_table_mode_default_unit_is_auto() {
        let raw = <RawArgs as clap::Parser>::parse_from(["pkgdu"]);
        let config = raw.into_config().unwrap();
        assert!(matches!(config.humansize, Some(UnitSpec::Auto)));
    }

    #[test]
    fn test_sort_field_name() {
        assert_eq!(RawArgs::parse_sort("name").unwrap(), SortField::Name);
    }

    #[test]
    fn test_sort_field_real() {
        assert_eq!(RawArgs::parse_sort("real").unwrap(), SortField::Real);
        assert_eq!(RawArgs::parse_sort("size").unwrap(), SortField::Real);
    }

    #[test]
    fn test_sort_field_apparent() {
        assert_eq!(
            RawArgs::parse_sort("apparent").unwrap(),
            SortField::Apparent
        );
    }

    #[test]
    fn test_sort_field_files() {
        assert_eq!(RawArgs::parse_sort("files").unwrap(), SortField::Files);
    }

    #[test]
    fn test_is_format_string_positive() {
        assert!(RawArgs::is_format_string("%n\t%m"));
    }

    #[test]
    fn test_is_format_string_negative() {
        assert!(!RawArgs::is_format_string("glibc"));
    }

    #[test]
    fn test_into_config_sort_ratio_with_btrfs_ok() {
        let raw = <RawArgs as clap::Parser>::parse_from(["pkgdu", "--sort", "ratio", "--btrfs"]);
        let config = raw.into_config().unwrap();
        assert_eq!(config.sort, SortField::Ratio);
        assert!(config.btrfs);
    }

    #[test]
    fn test_root_nonexistent_error() {
        // Put --root BEFORE positionals so trailing_var_arg doesn't swallow it
        let raw =
            <RawArgs as clap::Parser>::parse_from(["pkgdu", "--root", "/nonexistent_path_xyz"]);
        let result = raw.into_config();
        assert!(result.is_err());
    }

    #[test]
    fn test_dbpath_relative_error() {
        // Put --dbpath BEFORE positionals so trailing_var_arg doesn't swallow it
        let raw = <RawArgs as clap::Parser>::parse_from(["pkgdu", "--dbpath", "relative/path"]);
        let result = raw.into_config();
        assert!(result.is_err());
    }

    #[test]
    fn test_rebase_under_root() {
        assert_eq!(
            rebase_under_root(Path::new("/"), Path::new("/var/lib/pacman")),
            PathBuf::from("/var/lib/pacman")
        );
        assert_eq!(
            rebase_under_root(Path::new("/mnt/arch"), Path::new("/var/lib/pacman")),
            PathBuf::from("/mnt/arch/var/lib/pacman")
        );
    }

    #[test]
    fn test_is_format_string_with_non_format() {
        assert!(!RawArgs::is_format_string("libgcrypt"));
    }

    #[test]
    fn test_targets_when_no_format_string() {
        let raw = <RawArgs as clap::Parser>::parse_from(["pkgdu", "glibc", "firefox"]);
        let config = raw.into_config().unwrap();
        assert_eq!(
            config.targets,
            vec!["glibc".to_string(), "firefox".to_string()]
        );
        assert!(config.format.is_none());
    }

    #[test]
    fn test_format_mode_sets_no_limit() {
        let raw = <RawArgs as clap::Parser>::parse_from(["pkgdu", "%n\t%m"]); // format mode: unlimited by default
        let config = raw.into_config().unwrap();
        assert!(config.format.is_some());
        assert!(config.limit.is_none()); // format mode has no limit by default
    }

    #[test]
    fn test_explicit_limit_in_format_mode() {
        let raw = <RawArgs as clap::Parser>::parse_from(["pkgdu", "-n", "2", "%n"]);
        let config = raw.into_config().unwrap();
        assert_eq!(config.limit, Some(2));
    }

    #[test]
    fn test_limit_zero_is_unlimited() {
        let raw = <RawArgs as clap::Parser>::parse_from(["pkgdu", "-n", "0"]);
        let config = raw.into_config().unwrap();
        assert_eq!(config.limit, None);
    }

    #[test]
    fn test_table_mode_default_limit() {
        let raw = <RawArgs as clap::Parser>::parse_from(["pkgdu"]);
        let config = raw.into_config().unwrap();
        assert_eq!(config.limit, Some(20));
    }
}
