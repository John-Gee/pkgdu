use std::path::PathBuf;

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

    /// Sort by field: name (ascending), real/apparent/files/ratio (descending)
    #[arg(long, default_value = "real")]
    pub sort: String,

    /// Show only top N packages (0 = unlimited)
    #[arg(short = 'n', default_value_t = 20)]
    pub limit: usize,

    /// Enable btrfs compressed size tokens (%z, %r)
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
}

impl RawArgs {
    /// Parse a humansize string into UnitSpec.
    fn parse_humansize(s: &str) -> std::result::Result<UnitSpec, String> {
        match s {
            "B" => Ok(UnitSpec::Raw),
            "K" | "Ki" => Ok(UnitSpec::Ki),
            "M" | "Mi" => Ok(UnitSpec::Mi),
            "G" | "Gi" => Ok(UnitSpec::Gi),
            "T" | "Ti" => Ok(UnitSpec::Ti),
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
        // `--sort ratio` is always rejected in Phase 1
        let sort = Self::parse_sort(&self.sort).map_err(PkgduError::Config)?;
        if matches!(sort, SortField::Ratio) {
            return Err(PkgduError::Config(
                "--sort ratio requires --btrfs (not yet supported in Phase 1)".to_string(),
            ));
        }

        // 4. --- Root existence check ---
        if !self.root.exists() {
            return Err(PkgduError::Config(format!(
                "root path does not exist: {}",
                self.root.display()
            )));
        }

        // 5. --- DBPath resolution ---
        let dbpath = if let Some(ref db) = self.dbpath {
            if !db.is_absolute() {
                return Err(PkgduError::Config(
                    "--dbpath must be an absolute path".to_string(),
                ));
            }
            db.clone()
        } else {
            // Default: read from pacman.conf (via --root/etc/pacman.conf) or /var/lib/pacman
            let conf_path = self.root.join("etc/pacman.conf");
            if conf_path.exists() {
                match parse_pacman_conf(&conf_path) {
                    Ok(conf) => conf.dbpath.clone(),
                    Err(_) => PathBuf::from("/var/lib/pacman"),
                }
            } else {
                PathBuf::from("/var/lib/pacman")
            }
        };

        // 6. --- dbpath existence check ---
        if !dbpath.exists() {
            return Err(PkgduError::Config(format!(
                "dbpath does not exist: {}",
                dbpath.display()
            )));
        }

        // 7. --- Limit default ---
        let limit = match (in_format_mode, self.limit) {
            (true, 0) => None, // format mode + 0 → unlimited
            (true, n) if !targets.is_empty() && n > 20 => Some(n), // explicit in format + targets
            (true, _) => None, // format mode default: unlimited
            (false, 0) => None, // table mode + 0 → unlimited
            (false, _) => Some(self.limit), // table mode default
        };

        // Humansize
        let humansize = if let Some(ref hs) = self.humansize {
            Some(Self::parse_humansize(hs).map_err(PkgduError::Config)?)
        } else {
            None
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
    fn test_config_from_raw_args_sort_ratio_without_btrfs_error() {
        let raw = <RawArgs as clap::Parser>::parse_from(&["pkgdu", "--sort", "ratio"]);
        let result = raw.into_config();
        assert!(result.is_err());
    }

    #[test]
    fn test_root_nonexistent_error() {
        // Put --root BEFORE positionals so trailing_var_arg doesn't swallow it
        let raw =
            <RawArgs as clap::Parser>::parse_from(&["pkgdu", "--root", "/nonexistent_path_xyz"]);
        let result = raw.into_config();
        assert!(result.is_err());
    }

    #[test]
    fn test_dbpath_relative_error() {
        // Put --dbpath BEFORE positionals so trailing_var_arg doesn't swallow it
        let raw = <RawArgs as clap::Parser>::parse_from(&["pkgdu", "--dbpath", "relative/path"]);
        let result = raw.into_config();
        assert!(result.is_err());
    }

    #[test]
    fn test_is_format_string_with_non_format() {
        assert!(!RawArgs::is_format_string("libgcrypt"));
    }

    #[test]
    fn test_targets_when_no_format_string() {
        let raw = <RawArgs as clap::Parser>::parse_from(&["pkgdu", "glibc", "firefox"]);
        let config = raw.into_config().unwrap();
        assert_eq!(
            config.targets,
            vec!["glibc".to_string(), "firefox".to_string()]
        );
        assert!(config.format.is_none());
    }

    #[test]
    fn test_format_mode_sets_no_limit() {
        let raw = <RawArgs as clap::Parser>::parse_from(&["pkgdu", "%n\t%m"]); // format mode: unlimited by default
        let config = raw.into_config().unwrap();
        assert!(config.format.is_some());
        assert!(config.limit.is_none()); // format mode has no limit by default
    }
}
