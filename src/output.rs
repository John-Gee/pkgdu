use crate::config::Config;
use crate::human_size::{format_size, UnitSpec};

use crate::scan::PackageResult;

use unicode_width::UnicodeWidthStr;

/// Strip ANSI escape sequences from a string, returning only visible text.
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c as u8 == 0x1b {
            // Skip the CSI introducer byte '[' (0x5B) if present
            if chars.peek().copied() == Some('[') {
                chars.next();
            }
            // Skip remaining bytes until a final byte (0x40-0x7E) is found
            while let Some(next) = chars.peek().copied() {
                let b = next as u8;
                if (b >= 0x40 && b <= 0x7E) || b == 0x3F {
                    chars.next();
                    break;
                }
                chars.next();
            }
        } else {
            result.push(c);
        }
    }
    result
}

#[derive(Clone)]
pub struct Column {
    pub title: &'static str,
    pub width: usize,
}

impl Column {
    fn padded(&self, value: impl std::fmt::Display) -> String {
        let s = format!("{}", value);
        // Strip ANSI codes for accurate width measurement
        let visible = strip_ansi(&s);
        let w = UnicodeWidthStr::width(visible.as_str());
        if w < self.width {
            format!("{}{}", s, " ".repeat(self.width - w))
        } else {
            s
        }
    }

    fn padded_color(&self, value: impl std::fmt::Display, has_color: bool) -> String {
        if has_color {
            let s = format!("{}", value);
            use owo_colors::OwoColorize;
            let colored = s.bold().cyan().to_string();
            // Measure only visible characters (strip ANSI codes before width check)
            let w = UnicodeWidthStr::width(strip_ansi(&colored).as_str());
            if w < self.width {
                format!("{}{}", colored, " ".repeat(self.width - w))
            } else {
                colored
            }
        } else {
            self.padded(value)
        }
    }

}

/// Determine whether terminal output supports colors considering NO_COLOR and --no-color.
fn color_enabled(cfg: &Config) -> bool {
    if cfg.no_color || std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    true
}

/// Format a size value into human-readable string using optional UnitSpec override.
fn format_field_size(bytes: u64, humansize: Option<UnitSpec>) -> String {
    match humansize {
        Some(u) => format_size(bytes, u),
        None => format_size(bytes, UnitSpec::Auto),
    }
}

/// Render the full scan report as a rich table string.
pub fn render_table(packages: &[PackageResult], cfg: &Config) -> String {
    let has_color = color_enabled(cfg);

    use owo_colors::OwoColorize;

    let size_title = if cfg.apparent_size { "APPARENT" } else { "REAL" };

    let mut cols = vec![
        Column {
            title: "PACKAGE",
            width: 8,
        },
        Column {
            title: size_title,
            width: 12,
        },
        Column {
            title: "FILES",
            width: 7,
        },
    ];

    if cfg.btrfs {
        cols.insert(
            2,
            Column {
                title: "COMPRESSED",
                width: 12,
            },
        );
        cols.push(Column {
            title: "RATIO",
            width: 8,
        });
    }

    let file_count_idx = if cfg.btrfs { 3 } else { 2 };

    let mut max_name = packages.iter().fold(0usize, |m, p| {
        let w = UnicodeWidthStr::width(p.name.as_str());
        if w > m { w } else { m }
    }) + 1;
    if max_name < 8 {
        max_name = 8;
    }
    cols[0].width = max_name;

    {
        let title_w = UnicodeWidthStr::width(cols[1].title);
        let data_w = packages.iter().fold(0usize, |m, p| {
            let val = if cfg.apparent_size {
                format_field_size(p.apparent_size, cfg.humansize)
            } else {
                format_field_size(p.real_size, cfg.humansize)
            };
            let w = UnicodeWidthStr::width(val.as_str());
            if w > m { w } else { m }
        });
        cols[1].width = std::cmp::max(title_w, data_w) + 1;
    }

    {
        let title_w = UnicodeWidthStr::width(cols[file_count_idx].title);
        let data_w = packages.iter().fold(0usize, |m, p| {
            let val = format!("{}", p.file_count);
            let w = UnicodeWidthStr::width(val.as_str());
            if w > m { w } else { m }
        });
        cols[file_count_idx].width = std::cmp::max(title_w, data_w) + 1;
    }

    if cfg.btrfs {
        {
            let title_w = UnicodeWidthStr::width(cols[2].title);
            let data_w = packages.iter().fold(0usize, |m, p| {
                let val = p.btrfs_compressed.map(|bytes| format_field_size(bytes, cfg.humansize)).unwrap_or_else(|| "N/A".to_string());
                let w = UnicodeWidthStr::width(val.as_str());
                if w > m { w } else { m }
            });
            cols[2].width = std::cmp::max(title_w, data_w) + 1;
        }

        let ratio_idx = cols.len() - 1;
        {
            let title_w = UnicodeWidthStr::width(cols[ratio_idx].title);
            let data_w = packages.iter().fold(0usize, |m, _p| {
                let val = if cfg.sort == crate::config::SortField::Real || cfg.sort == crate::config::SortField::Files || cfg.sort == crate::config::SortField::Apparent {
                    "N/A".to_string()
                } else {
                    String::new()
                };
                let w = UnicodeWidthStr::width(val.as_str());
                if w > m { w } else { m }
            });
            cols[ratio_idx].width = std::cmp::max(title_w, data_w) + 1;
        }
    }

    // Build header row (colorized if TTY)
    let header = cols
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if i == file_count_idx {
                let visible_w = UnicodeWidthStr::width(c.title);
                if has_color {
                    use owo_colors::OwoColorize;
                    let colored = format!("{}", c.title).bold().cyan().to_string();
                    if visible_w < c.width {
                        format!("{}{}", " ".repeat(c.width - visible_w), colored)
                    } else {
                        colored
                    }
                } else {
                    format!("{:>width$}", c.title, width = c.width)
                }
            } else if has_color {
                c.padded_color(c.title, true)
            } else {
                c.padded(c.title)
            }
        })
        .collect::<Vec<_>>()
        .join("  ");

    if packages.is_empty() {
        return header;
    }

    let mut out = vec![header];

    for pkg in packages {
        let size_val = if cfg.apparent_size { pkg.apparent_size } else { pkg.real_size };
        let size_hf = format_field_size(size_val, cfg.humansize);

        if !cfg.btrfs {
            let f = format!("{:>width$}", pkg.file_count, width = cols[2].width);
            let file_cell = if has_color {
                f.bold().cyan().to_string()
            } else {
                f
            };
            out.push(format!(
                "{}  {}  {}",
                cols[0].padded_color(&pkg.name, has_color),
                cols[1].padded_color(&size_hf, has_color),
                file_cell,
            ));
        } else {
            let btrfs_compressed = pkg.btrfs_compressed.map(|bytes| format_field_size(bytes, cfg.humansize)).unwrap_or_else(|| "N/A".to_string());
            let ratio_str = if cfg.sort == crate::config::SortField::Real || cfg.sort == crate::config::SortField::Files || cfg.sort == crate::config::SortField::Apparent {
                "N/A".to_string()
            } else {
                String::new()
            };

            let f = format!("{:>width$}", pkg.file_count, width = cols[3].width);
            let file_cell = if has_color {
                f.bold().cyan().to_string()
            } else {
                f
            };
            out.push(format!(
                "{}  {}  {}  {}  {}",
                cols[0].padded_color(&pkg.name, has_color),
                cols[1].padded_color(&size_hf, has_color),
                cols[2].padded_color(&btrfs_compressed, has_color),
                file_cell,
                cols.last().unwrap().padded_color(ratio_str, has_color)
            ));
        }
    }

    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_packages() -> Vec<PackageResult> {
        vec![
            PackageResult {
                name: "zlib".to_string(),
                version: "1.0-1".into(),
                real_size: 2_697_614_592,
                apparent_size: 3_456_789_123,
                file_count: 150,
                metadata_size: 2_697_614_592,
                btrfs_compressed: None,
            },
            PackageResult {
                name: "firefox".to_string(),
                version: "120.0-1".into(),
                real_size: 863_200_000,
                apparent_size: 1_200_000_000,
                file_count: 400,
                metadata_size: 863_200_000,
                btrfs_compressed: None,
            },
            PackageResult {
                name: "bash".to_string(),
                version: "5.2-1".into(),
                real_size: 50_000_000,
                apparent_size: 80_000_000,
                file_count: 30,
                metadata_size: 50_000_000,
                btrfs_compressed: None,
            },
        ]
    }

    fn make_config(btrfs_flag: bool) -> Config {
        Config {
            root: std::path::PathBuf::from("/"),
            dbpath: std::path::PathBuf::from("/var/lib/pacman"),
            targets: vec![],
            search: None,
            sort: crate::config::SortField::Real,
            limit: Some(20),
            btrfs: btrfs_flag,
            verbose: false,
            format: None,
            humansize: None,
            delim: "\n".to_string(),
            no_color: false,
            apparent_size: false,
        }
    }

    #[test]
    fn test_color_enabled_no_override() {
        let cfg = make_config(false);
        let result = color_enabled(&cfg);
        assert!(result); // default true unless NO_COLOR set
    }

    #[test]
    fn test_empty_search_result() {
        let pkgs = vec![];
        let cfg = make_config(false);
        let result = render_table(&pkgs, &cfg);
        assert!(result.contains("PACKAGE"));
        assert!(result.contains("REAL"));
        assert!(result.contains("FILES"));
    }

    #[test]
    fn test_default_mode_no_format() {
        let pkgs = sample_packages();
        let cfg = make_config(false);
        let result = render_table(&pkgs, &cfg);
        assert!(result.contains("PACKAGE"));
        assert!(result.contains("REAL"));
        assert!(result.contains("FILES"));
        assert!(result.contains("zlib"));
        assert!(result.contains("firefox"));
        assert!(result.contains("bash"));
    }

    #[test]
    fn test_btrfs_columns_enabled() {
        let pkgs = sample_packages();
        let cfg = make_config(true);
        let result = render_table(&pkgs, &cfg);
        assert!(result.contains("COMPRESSED"));
        assert!(result.contains("RATIO"));
    }

}
