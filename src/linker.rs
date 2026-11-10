use anyhow::Result;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command;
#[cfg(windows)]
use anyhow::bail;

use crate::model::Tree;

const NM_MANIFEST: &str = ".zap/nm-paths.json";

/// node-modules linker: pnpm-style layout. node_modules/<name> junctions point into
/// the project's .zap/v hardlink farm, and every package dir gets node_modules/<dep>
/// junctions to its own deps. no pnp hook needed, node's native resolution does the rest.
pub fn link_node_modules(root: &Path, tree: &Tree) -> Result<()> {
    let vroot = root.join(".zap").join("v");
    let mut links: Vec<(PathBuf, PathBuf)> = Vec::new();

    let target_of = |dep_id: &str| -> PathBuf {
        if let Some(name) = dep_id.strip_prefix("ws:") {
            let member = tree.members.iter().find(|m| m.name == name);
            match member {
                Some(m) => root.join(&m.dir),
                None => vroot.clone(), // broken; resolved below as error path is skipped
            }
        } else {
            vroot.join(crate::util::pkg_dir_name(dep_id))
        }
    };

    let add_link = |nm_base: &Path, name: &str, target: &Path, links: &mut Vec<(PathBuf, PathBuf)>| {
        let link = match name.split_once('/') {
            Some((scope, short)) if name.starts_with('@') => nm_base.join(scope).join(short),
            _ => nm_base.join(name),
        };
        links.push((link, target.to_path_buf()));
    };

    // root node_modules: direct deps + type deps + workspace member names
    for (name, id) in &tree.root {
        add_link(&root.join("node_modules"), name, &target_of(id), &mut links);
    }
    for (name, id) in &tree.types {
        add_link(&root.join("node_modules"), name, &target_of(id), &mut links);
    }
    for m in &tree.members {
        add_link(&root.join("node_modules"), &m.name, &root.join(&m.dir), &mut links);
    }

    // each member's node_modules: its own deps (registry + cross-member)
    for m in &tree.members {
        let base = root.join(&m.dir).join("node_modules");
        for (name, id) in &m.deps {
            add_link(&base, name, &target_of(id), &mut links);
        }
    }

    // each virtual package dir: node_modules/<dep> -> sibling virtual dir (pnpm layout)
    for (id, pkg) in &tree.packages {
        let base = vroot.join(crate::util::pkg_dir_name(id)).join("node_modules");
        for (name, dep_id) in &pkg.deps {
            add_link(&base, name, &target_of(dep_id), &mut links);
        }
    }

    reconcile(root, &links)?;

    // node_modules/.bin shims for everything reachable from a direct dep
    write_dot_bin(root, tree)?;

    Ok(())
}

/// remove junctions we created last run that are gone from the current plan,
/// then create all current junctions. never touches real directories.
fn reconcile(root: &Path, links: &[(PathBuf, PathBuf)]) -> Result<()> {
    let manifest = root.join(NM_MANIFEST);
    let previous: Vec<String> = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let current: std::collections::HashSet<String> =
        links.iter().map(|(l, _)| l.to_string_lossy().to_string()).collect();
    for p in &previous {
        let path = PathBuf::from(p);
        if !current.contains(p) && std::fs::read_link(&path).is_ok() {
            let _ = std::fs::remove_dir(path); // removes the link, never the target
        }
    }
    for (link, target) in links {
        ensure_junction(link, target)?;
    }
    std::fs::create_dir_all(root.join(".zap"))?;
    std::fs::write(&manifest, serde_json::to_string(&links.iter().map(|(l, _)| l.to_string_lossy()).collect::<Vec<_>>())?)?;
    Ok(())
}

/// junctions on windows, symlinks elsewhere, never touches real directories
fn ensure_junction(link: &Path, target: &Path) -> Result<()> {
    if std::fs::read_link(link).is_ok() {
        // stale link of ours: replace it
        let _ = std::fs::remove_dir(link);
    } else if link.is_dir() {
        // a real directory the user owns, leave it alone
        return Ok(());
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(windows)]
    {
        // cmd's mklink parses forward slashes as switches, backslashes only
        let link_s = link.to_string_lossy().replace('/', "\\");
        let target_s = target.to_string_lossy().replace('/', "\\");
        let out = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link_s)
            .arg(target_s)
            .output()?;
        if !out.status.success() {
            bail!(
                "junction {} -> {} failed: {}",
                link.display(),
                target.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).map_err(|e| {
            anyhow::anyhow!("symlink {} -> {} failed: {}", link.display(), target.display(), e)
        })?;
    }
    Ok(())
}

/// node_modules/.bin shims for bins of every direct dep (root + members)
fn write_dot_bin(root: &Path, tree: &Tree) -> Result<()> {
    let vroot = root.join(".zap").join("v");

    let mut bins: Vec<(String, PathBuf)> = Vec::new(); // (bin name, bin file)
    let add_pkg_bins = |id: &str, bins: &mut Vec<(String, PathBuf)>| {
        if let Some(pkg) = tree.packages.get(id) {
            let dir = vroot.join(crate::util::pkg_dir_name(id));
            for (bin_name, rel) in &pkg.bin {
                bins.push((bin_name.clone(), dir.join(rel.trim_start_matches("./"))));
            }
        }
    };

    let bin_dir = root.join("node_modules").join(".bin");
    let _ = std::fs::remove_dir_all(&bin_dir);
    std::fs::create_dir_all(&bin_dir)?;
    for (_, id) in &tree.root {
        if let Some(name) = id.strip_prefix("ws:") {
            // member bins live in the member dir; its package.json declares them
            if let Some(m) = tree.members.iter().find(|m| m.name == name) {
                add_member_bins(root, &m.dir, &bin_dir)?;
            }
            continue;
        }
        add_pkg_bins(id, &mut bins);
    }
    for (bin_name, bin_file) in &bins {
        crate::pnp::write_shim(&bin_dir, bin_name, bin_file, None, &[])?;
    }

    Ok(())
}

fn add_member_bins(root: &Path, dir_rel: &str, bin_dir: &Path) -> Result<()> {
    let member = root.join(dir_rel);
    let Ok(v) = crate::util::read_json(&member.join("package.json")) else {
        return Ok(());
    };
    let Some(bin) = v.get("bin") else { return Ok(()) };
    let normalized = crate::model::normalize_bin(
        v.get("name").and_then(|x| x.as_str()).unwrap_or(""),
        bin,
    );
    for (bin_name, rel) in &normalized {
        let bin_file = member.join(rel.trim_start_matches("./"));
        crate::pnp::write_shim(bin_dir, bin_name, &bin_file, None, &[])?;
    }
    Ok(())
}

/// flat junction dir (.zap/ts-nm) that NODE_PATH can point at, so `tsc` running under
/// the pnp linker can still resolve types (tsc has its own resolver, no node hook)
pub fn link_typecheck_dir(root: &Path, tree: &Tree) -> Result<PathBuf> {
    let dir = root.join(".zap").join("ts-nm");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let vroot = root.join(".zap").join("v");
    let mut links: Vec<(PathBuf, PathBuf)> = Vec::new();
    let add = |name: &str, id: &str, links: &mut Vec<(PathBuf, PathBuf)>| {
        let target = match id.strip_prefix("ws:") {
            Some(mname) => match tree.members.iter().find(|m| m.name == mname) {
                Some(m) => root.join(&m.dir),
                None => return,
            },
            None => vroot.join(crate::util::pkg_dir_name(id)),
        };
        let link = match name.split_once('/') {
            Some((scope, short)) if name.starts_with('@') => dir.join(scope).join(short),
            _ => dir.join(name),
        };
        links.push((link, target));
    };
    for (name, id) in &tree.root {
        add(name, id, &mut links);
    }
    for (name, id) in &tree.types {
        add(name, id, &mut links);
    }
    for m in &tree.members {
        add(&m.name.clone(), &format!("ws:{}", m.name), &mut links);
        for (name, id) in &m.deps {
            add(name, id, &mut links);
        }
    }
    for (link, target) in &links {
        ensure_junction(link, target)?;
    }
    Ok(dir)
}

/// .zap/bin shims for the typescript toolchain with type resolution wired in
/// (NODE_PATH for ts<=5, junction farm for editors)
pub fn write_ts_shims(root: &Path, ts_dir: &Path, hook: Option<&str>) -> Result<()> {
    let bin_dir = root.join(".zap").join("bin");
    std::fs::create_dir_all(&bin_dir)?;
    let Ok(v) = crate::util::read_json(&ts_dir.join("package.json")) else {
        return Ok(());
    };
    let Some(bin) = v.get("bin") else { return Ok(()) };
    let normalized = crate::model::normalize_bin("typescript", bin);
    let ts_nm = root.join(".zap").join("ts-nm").display().to_string();
    let extra: Vec<(&str, &str)> = if hook.is_some() {
        vec![("NODE_PATH", ts_nm.as_str())]
    } else {
        vec![]
    };
    for (bin_name, rel) in &normalized {
        let bin_file = ts_dir.join(rel.trim_start_matches("./"));
        crate::pnp::write_shim(&bin_dir, bin_name, &bin_file, hook, &extra)?;
    }
    Ok(())
}
