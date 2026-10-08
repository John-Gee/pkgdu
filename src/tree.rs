use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

/// A node in a package's file/directory tree.
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub children: Vec<TreeNode>,
}

/// A package's file tree, rooted after stripping the dominant path prefix.
#[derive(Debug, Clone)]
pub struct PackageTree {
    pub name: String,
    /// Package total (same metric as the sizes below).
    pub size: u64,
    /// Top-level nodes (children of the stripped prefix).
    pub nodes: Vec<TreeNode>,
}

/// A leading prefix is stripped when a single subtree holds at least this
/// fraction of the package, so `/usr/{lib,share,...}` shows `lib`, `share`, …
/// and `/opt/foo/...` shows `foo`'s internals rather than a lone `usr`/`opt`.
const DOMINANT_FRACTION: f64 = 0.9;

/// Build a package's file tree from manifest-relative paths and their sizes.
///
/// Files with size 0 (symlinks, hardlink duplicates, unreadable) are ignored.
/// The dominant leading prefix is stripped so the first displayed level is
/// meaningful.
pub fn build_package_tree(name: &str, total: u64, files: &[(PathBuf, u64)]) -> PackageTree {
    let entries: Vec<(Vec<String>, u64)> = files
        .iter()
        .filter(|(_, size)| *size > 0)
        .map(|(path, size)| (components(path), *size))
        .collect();

    let prefix = dominant_prefix(&entries);
    let mut root = Builder::default();
    for (comps, size) in &entries {
        let rel = if comps.starts_with(prefix.as_slice()) {
            &comps[prefix.len()..]
        } else {
            &comps[..]
        };
        root.insert(rel, *size);
    }

    PackageTree {
        name: name.to_string(),
        size: total,
        nodes: root.into_nodes(),
    }
}

/// Path components, ignoring the root/prefix/`.`/`..`.
fn components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// The longest leading component prefix that still covers at least
/// [`DOMINANT_FRACTION`] of the package's bytes. Capped so the file name is
/// always kept.
fn dominant_prefix(entries: &[(Vec<String>, u64)]) -> Vec<String> {
    let total: u64 = entries.iter().map(|(_, size)| *size).sum();
    if total == 0 {
        return Vec::new();
    }
    let min_len = entries.iter().map(|(c, _)| c.len()).min().unwrap_or(0);
    let max_k = min_len.saturating_sub(1); // keep at least the file name

    let mut best: Vec<String> = Vec::new();
    for k in 1..=max_k {
        let mut groups: HashMap<Vec<String>, u64> = HashMap::new();
        for (comps, size) in entries {
            *groups.entry(comps[..k].to_vec()).or_default() += size;
        }
        match groups.into_iter().max_by_key(|(_, size)| *size) {
            Some((prefix, size)) if (size as f64) / (total as f64) >= DOMINANT_FRACTION => {
                best = prefix;
            }
            _ => break,
        }
    }
    best
}

#[derive(Default)]
struct Builder {
    size: u64,
    children: BTreeMap<String, Builder>,
}

impl Builder {
    fn insert(&mut self, comps: &[String], size: u64) {
        self.size += size;
        if let Some((first, rest)) = comps.split_first() {
            self.children
                .entry(first.clone())
                .or_default()
                .insert(rest, size);
        }
    }

    fn into_nodes(self) -> Vec<TreeNode> {
        let mut nodes: Vec<TreeNode> = self
            .children
            .into_iter()
            .map(|(name, child)| {
                let is_dir = !child.children.is_empty();
                TreeNode {
                    name,
                    size: child.size,
                    is_dir,
                    children: child.into_nodes(),
                }
            })
            .collect();
        nodes.sort_by(|a, b| b.size.cmp(&a.size).then(a.name.cmp(&b.name)));
        nodes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[(&str, u64)]) -> Vec<(PathBuf, u64)> {
        paths.iter().map(|(p, s)| (PathBuf::from(p), *s)).collect()
    }

    fn names(nodes: &[TreeNode]) -> Vec<&str> {
        nodes.iter().map(|n| n.name.as_str()).collect()
    }

    #[test]
    fn test_strips_dominant_usr_prefix() {
        // usr holds 100% -> strip it; top level is lib/share.
        let tree = build_package_tree(
            "pkg",
            300,
            &files(&[("usr/lib/a.so", 100), ("usr/share/doc/b", 200)]),
        );
        assert_eq!(names(&tree.nodes), vec!["share", "lib"]); // sorted by size desc
        assert_eq!(tree.nodes[0].size, 200);
        assert!(tree.nodes[0].is_dir);
    }

    #[test]
    fn test_strips_dominant_prefix_with_stray_file() {
        // usr is 99% -> still stripped; the stray etc file stays top-level.
        let tree = build_package_tree(
            "pkg",
            300,
            &files(&[("usr/lib/a.so", 297), ("etc/conf", 3)]),
        );
        let mut got = names(&tree.nodes);
        got.sort_unstable();
        assert_eq!(got, vec!["etc", "lib"]);
    }

    #[test]
    fn test_single_root_dir_shows_internals() {
        // All under opt/foo -> strip it; internals shown.
        let tree = build_package_tree(
            "pkg",
            300,
            &files(&[("opt/foo/bin/x", 100), ("opt/foo/lib/y", 200)]),
        );
        assert_eq!(names(&tree.nodes), vec!["lib", "bin"]);
    }

    #[test]
    fn test_files_in_one_dir_become_leaves() {
        // All directly in usr/lib -> files are the top level.
        let tree = build_package_tree(
            "pkg",
            300,
            &files(&[("usr/lib/a.so", 100), ("usr/lib/b.so", 200)]),
        );
        assert_eq!(names(&tree.nodes), vec!["b.so", "a.so"]);
        assert!(!tree.nodes[0].is_dir);
    }

    #[test]
    fn test_split_top_level_dirs_kept() {
        // Neither usr nor etc dominates (33/67) -> both kept.
        let tree = build_package_tree("pkg", 300, &files(&[("usr/bin/x", 100), ("etc/conf", 200)]));
        assert_eq!(names(&tree.nodes), vec!["etc", "usr"]);
    }

    #[test]
    fn test_zero_sized_files_ignored() {
        let tree = build_package_tree(
            "pkg",
            100,
            &files(&[("usr/lib/a", 100), ("usr/lib/link", 0)]),
        );
        assert_eq!(names(&tree.nodes), vec!["a"]);
    }
}
