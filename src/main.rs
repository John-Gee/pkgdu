mod btrfs;
mod config;
mod error;
mod format;
mod human_size;
mod output;
mod pacman;
mod scan;

use config::{Config, RawArgs};

fn color_enabled(cfg: &Config) -> bool {
    !cfg.no_color && std::env::var_os("NO_COLOR").is_none()
}

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

    let use_color = color_enabled(&cfg);

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
    if cfg.verbose {
        for warn in &report.warnings {
            eprintln!("{}", warn);
        }
    }

    // Render output to stdout
    let result = if let Some(ref fmt) = cfg.format {
        report
            .packages
            .iter()
            .map(|pkg| fmt.render(pkg, &cfg))
            .collect::<Vec<_>>()
            .join(&cfg.delim)
    } else {
        output::render_table(&report.packages, &cfg)
    };
    println!("{}", result);

    // Exit codes
    if report.skipped_packages + report.permission_errors > 0 {
        std::process::exit(2);
    }
}
