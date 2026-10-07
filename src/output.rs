use crate::config::Config;
use crate::human_size::{format_size, UnitSpec};

use crate::scan::{PackageResult, ScanReport};

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

/// Cell styling applied when color is enabled.
#[derive(Clone, Copy)]
enum Style {
    Plain,
    Bold,
    Cyan,
    Green,
}

impl Style {
    fn apply(self, s: &str, has_color: bool) -> String {
        if !has_color {
            return s.to_string();
        }
        use owo_colors::OwoColorize;
        match self {
            Style::Plain => s.to_string(),
            Style::Bold => s.bold().to_string(),
            Style::Cyan => s.cyan().to_string(),
            Style::Green => s.green().to_string(),
        }
    }
}

#[derive(Clone)]
struct Column {
    title: &'static str,
    width: usize,
    right_align: bool,
}

impl Column {
    fn pad(&self, value: &str, has_color: bool, style: Style) -> String {
        let styled = style.apply(value, has_color);
        let w = UnicodeWidthStr::width(strip_ansi(&styled).as_str());
        let fill = " ".repeat(self.width.saturating_sub(w));
        if self.right_align {
            format!("{}{}", fill, styled)
        } else {
            format!("{}{}", styled, fill)
        }
    }
}

/// Percentage of a grand total, ncdu-style. `<0.1%` for small non-zero shares.
fn format_pct(value: u64, total: u64) -> String {
    if total == 0 {
        return "N/A".to_string();
    }
    let pct = value as f64 / total as f64 * 100.0;
    if value > 0 && pct < 0.1 {
        "<0.1%".to_string()
    } else {
        format!("{:.1}%", pct)
    }
}

/// Sum of compressed sizes over packages that have one.
fn sum_compressed(pkgs: &[PackageResult]) -> u64 {
    pkgs.iter().filter_map(|p| p.btrfs_compressed).sum()
}

/// Compression ratio for a subtotal, as compressed/apparent * 100.
fn subtotal_ratio(compressed: u64, apparent: u64) -> Option<f64> {
    if compressed > 0 && apparent > 0 {
        Some(compressed as f64 / apparent as f64 * 100.0)
    } else {
        None
    }
}

/// A single table row (a package, or a SHOWN/TOTAL subtotal).
struct Row {
    name: String,
    size: u64,
    compressed: Option<u64>,
    files: u64,
    ratio: Option<f64>,
}

fn header_row(cols: &[Column], has_color: bool) -> String {
    cols.iter()
        .map(|c| c.pad(c.title, has_color, Style::Bold))
        .collect::<Vec<_>>()
        .join("  ")
}

fn render_row(cols: &[Column], cells: &[(String, Style)], has_color: bool) -> String {
    cols.iter()
        .zip(cells.iter())
        .map(|(c, (text, style))| c.pad(text, has_color, *style))
        .collect::<Vec<_>>()
        .join("  ")
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

pub fn render_table(report: &ScanReport, cfg: &Config) -> String {
    let packages = &report.packages;
    let has_color = color_enabled(cfg);
    let use_apparent = cfg.apparent_size;
    let size_title = if use_apparent { "APPARENT" } else { "REAL" };
    // Show btrfs columns whenever any scanned package has compressed data
    // (not just the ones currently shown after -n truncation).
    let btrfs_active = cfg.btrfs && report.total_compressed > 0;

    let size_of = |p: &PackageResult| {
        if use_apparent {
            p.apparent_size
        } else {
            p.real_size
        }
    };
    let grand_total = if use_apparent {
        report.total_apparent
    } else {
        report.total_real
    };

    let mut cols = vec![
        Column {
            title: "PACKAGE",
            width: 0,
            right_align: false,
        },
        Column {
            title: size_title,
            width: 0,
            right_align: true,
        },
        Column {
            title: "PCT",
            width: 0,
            right_align: true,
        },
    ];
    if btrfs_active {
        cols.push(Column {
            title: "COMPRESSED",
            width: 0,
            right_align: true,
        });
    }
    if cfg.files {
        cols.push(Column {
            title: "FILES",
            width: 0,
            right_align: true,
        });
    }
    if btrfs_active {
        cols.push(Column {
            title: "RATIO",
            width: 0,
            right_align: true,
        });
    }
    for col in cols.iter_mut() {
        col.width = UnicodeWidthStr::width(col.title);
    }

    if packages.is_empty() {
        return header_row(&cols, has_color);
    }

    // Package rows.
    let mut rows: Vec<Row> = packages
        .iter()
        .map(|p| Row {
            name: p.name.clone(),
            size: size_of(p),
            compressed: p.btrfs_compressed,
            files: p.file_count,
            ratio: p.btrfs_ratio_percent(),
        })
        .collect();

    // Optional subtotal and grand-total rows.
    if cfg.total {
        if packages.len() < report.total_packages {
            let shown_apparent: u64 = packages.iter().map(|p| p.apparent_size).sum();
            let shown_compressed = sum_compressed(packages);
            rows.push(Row {
                name: format!("SHOWN (top {})", packages.len()),
                size: rows.iter().map(|r| r.size).sum(),
                compressed: if btrfs_active {
                    Some(shown_compressed)
                } else {
                    None
                },
                files: packages.iter().map(|p| p.file_count).sum(),
                ratio: subtotal_ratio(shown_compressed, shown_apparent),
            });
        }
        rows.push(Row {
            name: "TOTAL".to_string(),
            size: grand_total,
            compressed: if btrfs_active {
                Some(report.total_compressed)
            } else {
                None
            },
            files: report.total_files,
            ratio: subtotal_ratio(report.total_compressed, report.total_apparent),
        });
    }

    let cells_for = |row: &Row| -> Vec<(String, Style)> {
        let mut cells = vec![
            (row.name.clone(), Style::Plain),
            (format_field_size(row.size, cfg.humansize), Style::Cyan),
            (format_pct(row.size, grand_total), Style::Plain),
        ];
        if btrfs_active {
            cells.push((
                row.compressed
                    .map(|c| format_field_size(c, cfg.humansize))
                    .unwrap_or_else(|| "N/A".to_string()),
                Style::Cyan,
            ));
        }
        if cfg.files {
            cells.push((row.files.to_string(), Style::Plain));
        }
        if btrfs_active {
            cells.push((
                row.ratio
                    .map(|r| format!("{:.0}%", r))
                    .unwrap_or_else(|| "N/A".to_string()),
                Style::Green,
            ));
        }
        cells
    };

    // Column widths from the actual cell contents.
    for row in &rows {
        for (i, (text, _)) in cells_for(row).iter().enumerate() {
            let w = UnicodeWidthStr::width(text.as_str());
            if w > cols[i].width {
                cols[i].width = w;
            }
        }
    }

    let mut out = vec![header_row(&cols, has_color)];
    for row in &rows {
        out.push(render_row(&cols, &cells_for(row), has_color));
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

    fn report_of(pkgs: Vec<PackageResult>) -> ScanReport {
        let total_packages = pkgs.len();
        let total_real = pkgs.iter().map(|p| p.real_size).sum();
        let total_apparent = pkgs.iter().map(|p| p.apparent_size).sum();
        let total_files = pkgs.iter().map(|p| p.file_count).sum();
        let total_compressed = pkgs.iter().filter_map(|p| p.btrfs_compressed).sum();
        ScanReport {
            packages: pkgs,
            total_packages,
            total_real,
            total_apparent,
            total_files,
            total_compressed,
            ..Default::default()
        }
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
        let result = render_table(&report_of(pkgs), &cfg);
        assert!(result.contains("PACKAGE"));
        assert!(result.contains("REAL"));
        assert!(!result.contains("FILES"));
    }

    #[test]
    fn test_default_mode_no_format() {
        let pkgs = sample_packages();
        let cfg = make_config(false);
        let result = render_table(&report_of(pkgs), &cfg);
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
        let result = render_table(&report_of(pkgs), &cfg);
        assert!(result.contains("FILES"));
    }

    #[test]
    fn test_btrfs_columns_enabled() {
        let pkgs = sample_packages();
        let cfg = make_config(true);
        let result = render_table(&report_of(pkgs), &cfg);
        assert!(result.contains("COMPRESSED"));
        assert!(result.contains("RATIO"));
        assert!(!result.contains("FILES"));
    }

    #[test]
    fn test_btrfs_with_files_flag() {
        let pkgs = sample_packages();
        let mut cfg = make_config(true);
        cfg.files = true;
        let result = render_table(&report_of(pkgs), &cfg);
        assert!(result.contains("COMPRESSED"));
        assert!(result.contains("RATIO"));
        assert!(result.contains("FILES"));
    }

    #[test]
    fn test_pct_column_relative_to_grand_total() {
        let report = report_of(sample_packages());
        let cfg = make_config(false);
        let result = render_table(&report, &cfg);
        assert!(result.contains("PCT"));
        // zlib real 2_697_614_592 of total 3_610_814_592 ~= 74.7%
        assert!(result.contains("74.7%"), "got:\n{result}");
    }

    #[test]
    fn test_total_row_shown_and_grand_total_when_truncated() {
        let mut report = report_of(sample_packages());
        report.packages.truncate(2); // simulate -n 2
        let mut cfg = make_config(false);
        cfg.total = true;
        let result = render_table(&report, &cfg);
        assert!(result.contains("SHOWN (top 2)"), "got:\n{result}");
        assert!(result.contains("TOTAL"));
    }

    #[test]
    fn test_total_row_only_grand_total_when_not_truncated() {
        let report = report_of(sample_packages());
        let mut cfg = make_config(false);
        cfg.total = true;
        let result = render_table(&report, &cfg);
        assert!(!result.contains("SHOWN"));
        assert!(result.contains("TOTAL"));
    }
}
