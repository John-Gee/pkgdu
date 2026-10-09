use pkgdu::config::{self, RawArgs};
use pkgdu::output;
use pkgdu::scan;

fn eprint_error(msg: &str, color: bool) {
    if color {
        use owo_colors::OwoColorize;
        eprintln!("{}", msg.red());
    } else {
        eprintln!("{}", msg);
    }
}

fn main() {
    let raw_args = {
        use clap::Parser;
        RawArgs::parse()
    };

    let cfg = match raw_args.into_config() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    let use_color = output::stderr_color_enabled(&cfg);

    // Reject ratio sort if --btrfs is not set
    if cfg.sort == config::SortField::Ratio && !cfg.btrfs {
        eprint_error("--sort ratio requires --btrfs", use_color);
        std::process::exit(1);
    }

    let report = match scan::scan_packages(&cfg) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    // Print diagnostics to stderr
    for err in &report.errors {
        eprint_error(err, use_color);
    }
    for target in &report.missing_targets {
        eprint_error(&format!("no such package: {}", target), use_color);
    }
    if cfg.verbose {
        for warn in &report.warnings {
            eprintln!("{}", warn);
        }
    }

    // Render output to stdout. Nothing matched means nothing to print — avoid
    // emitting a bare header.
    if !report.packages.is_empty() {
        let result = if let Some(ref fmt) = cfg.format {
            report
                .packages
                .iter()
                .map(|pkg| fmt.render(pkg, &cfg))
                .collect::<Vec<_>>()
                .join(&cfg.delim)
        } else if cfg.depth.is_some() {
            output::render_tree(&report, &cfg)
        } else {
            output::render_table(&report, &cfg)
        };
        println!("{}", result);
    }

    // Exit codes: 2 when a package or file could not be fully scanned, or some
    // (but not all) requested targets were unknown; 1 when nothing matched at
    // all (unknown targets, an empty search, or an empty database).
    if report.skipped_packages + report.permission_errors > 0 || !report.errors.is_empty() {
        std::process::exit(2);
    }
    if !report.missing_targets.is_empty() && !report.packages.is_empty() {
        std::process::exit(2);
    }
    if report.packages.is_empty() {
        std::process::exit(1);
    }
}
