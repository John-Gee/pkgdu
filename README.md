# pkgdu — Real Disk Usage per Package for Arch Linux

[![License: GPL-3.0](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70+-orange.svg)](https://www.rust-lang.org)

`pkgdu` reports the **real on-disk size** of each installed Arch Linux package,
complementing `expac` with actual filesystem measurements instead of metadata.

```
$ pkgdu -H auto --files
PACKAGE                    REAL      FILES
linux-firmware        1.2 GiB       2841
libreoffice-fresh   823.4 MiB        892
gcc                 412.7 MiB       1043
```

## Why This Exists

`expac -Q '%m'` reports the **PKGBUILD-declared install size** from pacman's
local database. This number is:

1. A rough estimate from the package maintainer — often wrong
2. Always the uncompressed size, regardless of btrfs/zfs compression
3. Doesn't account for actual block allocation (sparse files, block rounding)
4. Doesn't reflect runtime modifications (pacnew merges, user edits, deleted files)

`pkgdu` answers: **how much disk does each package actually use right now?**

## Installation

```bash
cargo build --release
sudo install -Dm755 target/release/pkgdu /usr/bin/pkgdu
```

AUR package will be published separately.

## Usage

```
pkgdu [OPTIONS] [POSITIONAL]...
```

**Format string detection:** If the first positional argument contains a `%`
character, it is treated as the format string; otherwise, all positional
arguments are treated as package targets (using default rich table output).

### Examples

```bash
# Default: top 20 packages by real disk usage
pkgdu

# Top 10 by real size, human-readable
pkgdu -n 10 -H auto

# All packages, apparent size
pkgdu -n 0 -H Mi --sort apparent

# Show metadata vs real size side by side
pkgdu -H auto "%n\t%p\t%m"

# Search for python packages
pkgdu -s 'python' -H auto

# Specific packages with custom format
pkgdu -H Mi -d $'\t' $'%n\t%m\t%a\t%f' linux glibc gcc

# btrfs compressed sizes (requires root)
sudo pkgdu --btrfs -H auto

# Pipe-friendly (no color, raw format)
pkgdu -H Mi "%n\t%m" | sort -k2 -rn | head -5

# Using --root for testing/debugging
pkgdu --root /mnt/arch --dbpath /mnt/arch/var/lib/pacman -H auto
```

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `[POSITIONAL]...` | (table mode) | Format string or package names |
| `-H <unit>` | `raw` (format) / `auto` (table) | Human-size: `B`, `K`/`Ki`, `M`/`Mi`, `G`/`Gi`, `T`/`Ti`, `auto`, `auto-si` |
| `-s <regex>` | — | Search packages by regex on name |
| `--sort <field>` | `real` | Sort by: `name` (ascending A-Z), `real`/`apparent`/`files` (descending), `ratio` (ascending, best compression first) |
| `-n, --limit <N>` | 20 | Show only top N packages (0 = unlimited) |
| `--btrfs` | off | Enable btrfs compressed size tokens (`%z`, `%r`) |
| `--root <path>` | `/` | Set filesystem root prefix |
| `--dbpath <path>` | — | Override pacman local DB path (absolute) |
| `-d <delim>` | `\n` | Delimiter between packages in format mode |
| `-v, --verbose` | off | Show warnings for missing files, permission errors |
| `--no-color` | auto | Disable color output (also honors `NO_COLOR`) |
| `--apparent-size` | — | Show apparent file size instead of disk blocks |
| `--total` | — | Show total row at the bottom of table |
| `--files` | — | Show file count column |

### Format Tokens

| Token | Field | Source |
|-------|-------|--------|
| `%n` | Package name | `desc` file `%NAME%` |
| `%v` | Package version | `desc` file `%VERSION%` |
| `%m` | Real disk usage | `stat() st_blocks * 512` |
| `%a` | Apparent size | `stat() st_size` |
| `%f` | File count | regular files successfully stat'd |
| `%p` | Metadata size | `desc` file `%SIZE%` |
| `%z` | btrfs compressed | btrfs extent ioctls (requires `--btrfs`) |
| `%r` | btrfs ratio | `(compressed / apparent) * 100` |
| `%%` | Literal `%` | — |

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Usage error (invalid args, missing paths) |
| 2 | Partial failure (unreadable files, malformed packages, or unavailable btrfs data) |

## Architecture

```
pkgdu/
├── Cargo.toml
├── LICENSE
├── README.md
├── docs/
│   ├── PLAN.md          # Design decisions and data flow
│   └── TASKS.md         # Implementation tasks
└── src/
    ├── main.rs          # CLI (clap), dispatch, output rendering
    ├── error.rs         # PkgduError enum
    ├── config.rs        # Config struct, validation
    ├── pacman.rs        # Parse pacman.conf, local DB, file manifests
    ├── scan.rs          # Parallel stat() scanner (rayon)
    ├── format.rs        # Format string tokenizer and renderer
    ├── human_size.rs    # Byte → human-readable formatting
    ├── output.rs        # Color, table formatting
    └── btrfs.rs         # btrfs compressed sizes
```

## Performance

- Full system scan (~1500 packages, ~300K files): **< 2 seconds** on NVMe
- Single package: **< 10 ms**
- Built with LTO for optimal release performance

## License

This project is licensed under the GNU General Public License v3.0 — see the
[LICENSE](LICENSE) file for details.
