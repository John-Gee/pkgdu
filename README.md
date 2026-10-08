# pkgdu — Real Disk Usage per Package for Arch Linux

[![License: GPL-3.0](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70+-orange.svg)](https://www.rust-lang.org)

`pkgdu` reports the **real on-disk size** of each installed Arch Linux package,
complementing `expac` with actual filesystem measurements instead of metadata.

```
$ pkgdu -H auto --files
PACKAGE                    REAL     PCT   FILES
linux-firmware        1.2 GiB    14.8%    2841
libreoffice-fresh   823.4 MiB     7.6%     892
gcc                 412.7 MiB     3.1%    1043
```

`PCT` is each package's share of the total real (or apparent) size of all
matching packages.

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

# btrfs on-disk sizes (requires root)
sudo pkgdu --btrfs -H auto
# a full --btrfs scan sweeps the filesystem's extents once; naming packages
# reads only those files

# Expand the biggest packages into their file/dir trees
pkgdu --tree --depth 3 --min-percent 1

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
| `--btrfs` | off | Enable btrfs on-disk size tokens (`%z`, `%r`) |
| `--root <path>` | `/` | Set filesystem root prefix |
| `--dbpath <path>` | — | Override pacman local DB path (absolute) |
| `-d <delim>` | `\n` | Delimiter between packages in format mode |
| `-v, --verbose` | off | Show warnings for missing files, permission errors |
| `--no-color` | auto | Disable color output (also honors `NO_COLOR`) |
| `--apparent-size` | — | Show apparent file size instead of disk blocks |
| `--total` | — | Show a TOTAL row; when `-n` truncates the list, also a SHOWN subtotal |
| `--files` | — | Show file count column |
| `--tree` | off | Expand each shown package into a file/dir tree (default depth 1) |
| `--depth <N>` | 1 (with `--tree`) | Tree depth in levels; implies `--tree` |
| `--breadth <K>` | 5 | Show at most K children per tree directory (0 = no limit) |
| `--min-percent <P>` | 0.1 with `--tree` | Hide tree entries below P% of their parent |

### Format Tokens

| Token | Field | Source |
|-------|-------|--------|
| `%n` | Package name | `desc` file `%NAME%` |
| `%v` | Package version | `desc` file `%VERSION%` |
| `%m` | Real disk usage | `stat() st_blocks * 512`; hardlinked inodes counted once |
| `%a` | Apparent size | `stat() st_size`; hardlinked inodes counted once |
| `%f` | File count | unique regular files (hardlinked inodes counted once) |
| `%p` | Metadata size | `desc` file `%SIZE%` |
| `%z` | btrfs on-disk usage | all file extents via btrfs ioctls, compressed or not (requires `--btrfs` + root) |
| `%r` | btrfs ratio | `(%z / %a) * 100`; 100% = uncompressed |
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
    └── btrfs.rs         # btrfs on-disk sizes
```

## Performance

- Full system scan (~1500 packages, ~300K files): **< 2 seconds** on NVMe
- Single package: **< 10 ms**
- Built with LTO for optimal release performance

## License

This project is licensed under the GNU General Public License v3.0 — see the
[LICENSE](LICENSE) file for details.
