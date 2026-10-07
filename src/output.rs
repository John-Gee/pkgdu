use crate::config::Config;
use crate::human_size::{format_size, UnitSpec};

use crate::scan::PackageResult;

use std::io::IsTerminal;
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
                if (0x40..=0x7E).contains(&b) || b == 0x3F {
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

    fn padded_styled<C>(&self, value: impl std::fmt::Display, has_color: bool, style: C) -> String
    where
        C: Fn(String) -> String,
    {
        let s = format!("{}", value);
        let colored = if has_color { style(s) } else { s };
        let w = UnicodeWidthStr::width(strip_ansi(&colored).as_str());
        if w < self.width {
            format!("{}{}", colored, " ".repeat(self.width - w))
        } else {
            colored
        }
    }
}

/// Whether the user has permitted colored output at all (independent of stream).
fn colors_requested(no_color: bool) -> bool {
    !no_color && std::env::var_os("NO_COLOR").is_none()
}

/// Color for the stdout table: enabled only when stdout is a terminal.
pub fn color_enabled(cfg: &Config) -> bool {
    colors_requested(cfg.no_color) && std::io::stdout().is_terminal()
}

/// Color for stderr diagnostics: enabled only when stderr is a terminal.
pub fn stderr_color_enabled(cfg: &Config) -> bool {
    colors_requested(cfg.no_color) && std::io::stderr().is_terminal()
}

/// Format a size value into human-readable string using optional UnitSpec override.
/// `None` means raw bytes.
fn format_field_size(bytes: u64, humansize: Option<UnitSpec>) -> String {
    format_size(bytes, humansize.unwrap_or(UnitSpec::Raw))
}

/// Render the full scan report as a rich table string.
pub fn render_table(packages: &[PackageResult], cfg: &Config) -> String {
    let has_color = color_enabled(cfg);

    let size_title = if cfg.apparent_size {
        "APPARENT"
    } else {
        "REAL"
    };

    // Only show btrfs columns when data is actually present
    let btrfs_active = cfg.btrfs && packages.iter().any(|p| p.btrfs_compressed.is_some());

    let mut cols = vec![
        Column {
            title: "PACKAGE",
            width: 8,
        },
        Column {
            title: size_title,
            width: 12,
        },
    ];

    if btrfs_active {
        cols.push(Column {
            title: "COMPRESSED",
            width: 12,
        });
    }

    let files_idx = if cfg.files {
        let idx = cols.len();
        cols.push(Column {
            title: "FILES",
            width: 7,
        });
        Some(idx)
    } else {
        None
    };

    if btrfs_active {
        cols.push(Column {
            title: "RATIO",
            width: 8,
        });
    }

    let mut max_name = packages.iter().fold(0usize, |m, p| {
        let w = UnicodeWidthStr::width(p.name.as_str());
        if w > m {
            w
        } else {
            m
        }
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
            if w > m {
                w
            } else {
                m
            }
        });
        cols[1].width = std::cmp::max(title_w, data_w) + 1;
    }

    if btrfs_active {
        {
            let idx = 2;
            let title_w = UnicodeWidthStr::width(cols[idx].title);
            let data_w = packages.iter().fold(0usize, |m, p| {
                let val = p
                    .btrfs_compressed
                    .map(|bytes| format_field_size(bytes, cfg.humansize))
                    .unwrap_or_else(|| "N/A".to_string());
                let w = UnicodeWidthStr::width(val.as_str());
                if w > m {
                    w
                } else {
                    m
                }
            });
            cols[idx].width = std::cmp::max(title_w, data_w) + 1;
        }

        let ratio_idx = cols.len() - 1;
        {
            let title_w = UnicodeWidthStr::width(cols[ratio_idx].title);
            let data_w = packages.iter().fold(0usize, |m, p| {
                let val = match p.btrfs_compressed {
                    Some(s) if s > 0 => format!("{:.0}%", s as f64 / p.real_size as f64 * 100.0),
                    _ => "N/A".to_string(),
                };
                let w = UnicodeWidthStr::width(val.as_str());
                if w > m {
                    w
                } else {
                    m
                }
            });
            cols[ratio_idx].width = std::cmp::max(title_w, data_w) + 1;
        }
    }

    if let Some(idx) = files_idx {
        let title_w = UnicodeWidthStr::width(cols[idx].title);
        let data_w = packages.iter().fold(0usize, |m, p| {
            let val = format!("{}", p.file_count);
            let w = UnicodeWidthStr::width(val.as_str());
            if w > m {
                w
            } else {
                m
            }
        });
        cols[idx].width = std::cmp::max(title_w, data_w) + 1;
    }

    // Build header row (colorized if TTY)
    let header = cols
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if Some(i) == files_idx {
                let visible_w = UnicodeWidthStr::width(c.title);
                if has_color {
                    use owo_colors::OwoColorize;
                    let colored = c.title.to_string().bold().to_string();
                    if visible_w < c.width {
                        format!("{}{}", " ".repeat(c.width - visible_w), colored)
                    } else {
                        colored
                    }
                } else {
                    format!("{:>width$}", c.title, width = c.width)
                }
            } else if has_color {
                use owo_colors::OwoColorize;
                c.padded_styled(c.title, true, |s| s.bold().to_string())
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
        let size_val = if cfg.apparent_size {
            pkg.apparent_size
        } else {
            pkg.real_size
        };
        let size_hf = format_field_size(size_val, cfg.humansize);

        if !btrfs_active {
            if cfg.files {
                let file_col = format!("{:>width$}", pkg.file_count, width = cols[2].width);
                out.push(format!(
                    "{}  {}  {}",
                    cols[0].padded(&pkg.name),
                    cols[1].padded_styled(&size_hf, has_color, |s| {
                        use owo_colors::OwoColorize;
                        s.cyan().to_string()
                    }),
                    file_col,
                ));
            } else {
                out.push(format!(
                    "{}  {}",
                    cols[0].padded(&pkg.name),
                    cols[1].padded_styled(&size_hf, has_color, |s| {
                        use owo_colors::OwoColorize;
                        s.cyan().to_string()
                    }),
                ));
            }
        } else {
            let btrfs_compressed = pkg
                .btrfs_compressed
                .map(|bytes| format_field_size(bytes, cfg.humansize))
                .unwrap_or_else(|| "N/A".to_string());
            let ratio_str = match pkg.btrfs_compressed {
                Some(s) if s > 0 => format!("{:.0}%", s as f64 / pkg.real_size as f64 * 100.0),
                _ => "N/A".to_string(),
            };

            let ratio_colored = if has_color {
                use owo_colors::OwoColorize;
                ratio_str.green().to_string()
            } else {
                ratio_str
            };

            if cfg.files {
                let file_col = format!("{:>width$}", pkg.file_count, width = cols[3].width);
                out.push(format!(
                    "{}  {}  {}  {}  {}",
                    cols[0].padded(&pkg.name),
                    cols[1].padded_styled(&size_hf, has_color, |s| {
                        use owo_colors::OwoColorize;
                        s.cyan().to_string()
                    }),
                    cols[2].padded_styled(&btrfs_compressed, has_color, |s| {
                        use owo_colors::OwoColorize;
                        s.cyan().to_string()
                    }),
                    file_col,
                    cols.last().unwrap().padded(&ratio_colored)
                ));
            } else {
                out.push(format!(
                    "{}  {}  {}  {}",
                    cols[0].padded(&pkg.name),
                    cols[1].padded_styled(&size_hf, has_color, |s| {
                        use owo_colors::OwoColorize;
                        s.cyan().to_string()
                    }),
                    cols[2].padded_styled(&btrfs_compressed, has_color, |s| {
                        use owo_colors::OwoColorize;
                        s.cyan().to_string()
                    }),
                    cols.last().unwrap().padded(&ratio_colored)
                ));
            }
        }
    }

    // Append total row if requested
    if cfg.total {
        let total_real: u64 = packages.iter().map(|p| p.real_size).sum();
        let total_apparent: u64 = packages.iter().map(|p| p.apparent_size).sum();
        let total_files: u64 = packages.iter().map(|p| p.file_count).sum();
        let is_apparent = cfg.apparent_size;
        let total_size = if is_apparent {
            total_apparent
        } else {
            total_real
        };
        let size_hf = format_field_size(total_size, cfg.humansize);

        if !btrfs_active {
            if cfg.files {
                let file_col = format!("{:>width$}", total_files, width = cols[2].width);
                out.push(format!(
                    "{}  {}  {}",
                    cols[0].padded("TOTAL"),
                    cols[1].padded_styled(&size_hf, has_color, |s| {
                        use owo_colors::OwoColorize;
                        s.cyan().to_string()
                    }),
                    file_col,
                ));
            } else {
                out.push(format!(
                    "{}  {}",
                    cols[0].padded("TOTAL"),
                    cols[1].padded_styled(&size_hf, has_color, |s| {
                        use owo_colors::OwoColorize;
                        s.cyan().to_string()
                    }),
                ));
            }
        } else {
            let total_compressed: u64 = packages.iter().filter_map(|p| p.btrfs_compressed).sum();
            let comp_hf = format_field_size(total_compressed, cfg.humansize);
            let ratio_str = if total_compressed > 0 {
                format!(
                    "{:.0}%",
                    total_compressed as f64 / total_real as f64 * 100.0
                )
            } else {
                "N/A".to_string()
            };
            let ratio_colored = if has_color {
                use owo_colors::OwoColorize;
                ratio_str.green().to_string()
            } else {
                ratio_str
            };
            if cfg.files {
                let file_col = format!("{:>width$}", total_files, width = cols[3].width);
                out.push(format!(
                    "{}  {}  {}  {}  {}",
                    cols[0].padded("TOTAL"),
                    cols[1].padded_styled(&size_hf, has_color, |s| {
                        use owo_colors::OwoColorize;
                        s.cyan().to_string()
                    }),
                    cols[2].padded_styled(&comp_hf, has_color, |s| {
                        use owo_colors::OwoColorize;
                        s.cyan().to_string()
                    }),
                    file_col,
                    cols.last().unwrap().padded(&ratio_colored)
                ));
            } else {
                out.push(format!(
                    "{}  {}  {}  {}",
                    cols[0].padded("TOTAL"),
                    cols[1].padded_styled(&size_hf, has_color, |s| {
                        use owo_colors::OwoColorize;
                        s.cyan().to_string()
                    }),
                    cols[2].padded_styled(&comp_hf, has_color, |s| {
                        use owo_colors::OwoColorize;
                        s.cyan().to_string()
                    }),
                    cols.last().unwrap().padded(&ratio_colored)
                ));
            }
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
                btrfs_compressed: Some(621_504_000),
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
            total: false,
            files: false,
        }
    }

    #[test]
    fn test_color_disabled_by_no_color_flag() {
        let mut cfg = make_config(false);
        cfg.no_color = true;
        assert!(!color_enabled(&cfg));
    }

    #[test]
    fn test_color_enabled_false_when_stdout_not_a_tty() {
        // Under `cargo test` stdout is captured (a pipe), so color must be off.
        let cfg = make_config(false);
        assert!(!color_enabled(&cfg));
    }

    #[test]
    fn test_empty_search_result() {
        let pkgs = vec![];
        let cfg = make_config(false);
        let result = render_table(&pkgs, &cfg);
        assert!(result.contains("PACKAGE"));
        assert!(result.contains("REAL"));
        assert!(!result.contains("FILES"));
    }

    #[test]
    fn test_default_mode_no_format() {
        let pkgs = sample_packages();
        let cfg = make_config(false);
        let result = render_table(&pkgs, &cfg);
        assert!(result.contains("PACKAGE"));
        assert!(result.contains("REAL"));
        assert!(!result.contains("FILES"));
        assert!(result.contains("zlib"));
        assert!(result.contains("firefox"));
        assert!(result.contains("bash"));
    }

    #[test]
    fn test_files_column_when_flag_set() {
        let pkgs = sample_packages();
        let mut cfg = make_config(false);
        cfg.files = true;
        let result = render_table(&pkgs, &cfg);
        assert!(result.contains("FILES"));
    }

    #[test]
    fn test_btrfs_columns_enabled() {
        let pkgs = sample_packages();
        let cfg = make_config(true);
        let result = render_table(&pkgs, &cfg);
        assert!(result.contains("COMPRESSED"));
        assert!(result.contains("RATIO"));
        assert!(!result.contains("FILES"));
    }

    #[test]
    fn test_btrfs_with_files_flag() {
        let pkgs = sample_packages();
        let mut cfg = make_config(true);
        cfg.files = true;
        let result = render_table(&pkgs, &cfg);
        assert!(result.contains("COMPRESSED"));
        assert!(result.contains("RATIO"));
        assert!(result.contains("FILES"));
    }
}
