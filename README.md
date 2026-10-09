# pkgdu — Real Disk Usage per Package for Arch Linux

[![License: GPL-3.0](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)

`pkgdu` reports the **real on-disk size** of each installed Arch Linux package.
`expac -Q '%m'` reports pacman's *declared* install size from package metadata;
`pkgdu` measures what is actually allocated — block rounding, compression,
hardlinks, and post-install changes included.

```
$ pkgdu -H auto rocm-llvm miopen-hip rocblas
PACKAGE          REAL      %
rocm-llvm     6.8 GiB  78.1%
miopen-hip    1.4 GiB  16.6%
rocblas     475.1 MiB   5.3%
```

`%` is each package's share of the total size of all matching packages.
For the same three packages, `expac -Q '%n %m'` reports 7364364057,
2447557128 and 1388086875 bytes — `rocblas` uses barely a third of the space
its metadata claims, because the ROCm packages share hardlinked libraries.

A full scan of ~2500 packages (~600k files) takes under a second warm on NVMe.

## Installation

`pkgdu` is packaged for Arch Linux; build it with the bundled `PKGBUILD`:

```bash
git clone https://github.com/John-Gee/pkgdu
cd pkgdu
makepkg -si
```

It's also available on the AUR as `pkgdu`.

## Usage

```
pkgdu [OPTIONS] [POSITIONAL]...
```

If the first positional argument contains a `%`, it's treated as the format
string; otherwise all positionals are package targets and the rich table is
printed.

### Examples

```bash
# Top 20 packages by real disk usage
pkgdu

# All packages, apparent size, human-readable
pkgdu -n 0 -H auto --apparent-size

# Metadata vs real size side by side
pkgdu -H auto "%n\t%p\t%m"

# Expand the biggest packages into file/dir trees
pkgdu --tree --depth 3 --min-percent 1

# btrfs on-disk sizes (requires root)
sudo pkgdu --btrfs -H auto

# Pipe-friendly: no color, raw format
pkgdu -H Mi "%n\t%m" | sort -k2 -rn | head -5

# Scan a chroot
pkgdu --root /mnt/arch --dbpath /mnt/arch/var/lib/pacman -H auto
```

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `[POSITIONAL]...` | (table mode) | Format string or package names |
| `-H <unit>` | `raw` (format) / `auto` (table) | Human-size: `B`, `K`/`Ki`, `M`/`Mi`, `G`/`Gi`, `T`/`Ti`, `auto`, `auto-si` |
| `-s <regex>` | — | Search packages by regex on name |
| `--sort <field>` | `real` | Sort by: `name` (ascending A-Z), `real` (alias `size`)/`apparent`/`files` (descending), `ratio` (ascending, best compression first; requires `--btrfs`) |
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
| `--min-percent <P>` | 0.1 | Hide tree entries below P% of their parent (requires `--tree`) |
| `-h, --help` | — | Print help |

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

## License

This project is licensed under the GNU General Public License v3.0 — see the
[LICENSE](LICENSE) file for details.
