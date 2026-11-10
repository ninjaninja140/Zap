use anyhow::Result;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::util::read_json;

/// one workspace member package
#[derive(Clone)]
pub struct Member {
    pub name: String,
    /// directory relative to the workspace root, forward slashes
    pub dir_rel: String,
    pub deps: BTreeMap<String, String>,
    pub dev: BTreeMap<String, String>,
}

const BANNED: [&str; 4] = ["node_modules", ".zap", ".git", "target"];

/// expand workspace glob patterns ("packages/*", "apps/**", "core") into members
pub fn discover(root: &Path, patterns: &[String]) -> Result<Vec<Member>> {
    let mut dirs: HashSet<PathBuf> = HashSet::new();
    for pattern in patterns {
        let parts: Vec<&str> = pattern
            .trim()
            .split('/')
            .filter(|p| !p.is_empty())
            .collect();
        if parts.is_empty() {
            continue;
        }
        expand(root, &parts, &mut dirs);
    }
    let mut members = Vec::new();
    for dir in dirs {
        let Ok(v) = read_json(&dir.join("package.json")) else {
            continue;
        };
        let name = v
            .get("name")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                dir.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
        let deps = dep_map(&v, "dependencies");
        let dev = dep_map(&v, "devDependencies");
        let dir_rel = dir
            .strip_prefix(root)
            .unwrap_or(&dir)
            .to_string_lossy()
            .replace('\\', "/");
        members.push(Member {
            name,
            dir_rel,
            deps,
            dev,
        });
    }
    members.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(members)
}

fn dep_map(v: &serde_json::Value, key: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(m) = v.get(key).and_then(|x| x.as_object()) {
        for (k, val) in m {
            if let Some(s) = val.as_str() {
                out.insert(k.clone(), s.to_string());
            }
        }
    }
    out
}

fn expand(dir: &Path, parts: &[&str], out: &mut HashSet<PathBuf>) {
    if parts.is_empty() {
        if dir.join("package.json").is_file() {
            out.insert(dir.to_path_buf());
        }
        return;
    }
    let part = parts[0];
    let rest = &parts[1..];
    if part == "**" {
        // zero or more directories
        expand(dir, rest, out);
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let name = e.file_name();
                if BANNED.iter().any(|b| name == *b) {
                    continue;
                }
                if e.path().is_dir() {
                    expand(&e.path(), parts, out);
                }
            }
        }
        return;
    }
    if part == "*" {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let name = e.file_name();
                if BANNED.iter().any(|b| name == *b) {
                    continue;
                }
                if e.path().is_dir() {
                    expand(&e.path(), rest, out);
                }
            }
        }
        return;
    }
    let sub = dir.join(part);
    if sub.is_dir() {
        expand(&sub, rest, out);
    }
}

/// read the `workspaces` globs from a package.json, if any
pub fn patterns_from(v: &serde_json::Value) -> Vec<String> {
    match v.get("workspaces") {
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        Some(serde_json::Value::Object(o)) => o
            .get("packages")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// from a context directory (nearest package.json), find the workspace root that
/// governs installs: the nearest ancestor (including ctx itself) whose manifest
/// declares workspaces. falls back to ctx itself.
pub fn install_root_for(ctx: &Path) -> PathBuf {
    let mut dir = ctx.to_path_buf();
    loop {
        if dir.join("package.json").is_file() {
            if let Ok(v) = read_json(&dir.join("package.json")) {
                if !patterns_from(&v).is_empty() {
                    return dir;
                }
            }
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => return ctx.to_path_buf(),
        }
    }
}
