// `zap ls [--all] [--json] [-g]`, direct deps + workspace members

use anyhow::{anyhow, Result};
use std::collections::BTreeMap;
use std::path::Path;

use crate::cmds::read_deps;
use crate::model::Lockfile;
use crate::util::{dim, yellow};
use crate::workspace::install_root_for;

pub const NAME: &str = "ls";
pub const ALIASES: &[&str] = &["list"];
pub const USAGE: &str = "list dependencies and workspace members (--all full tree, -g global, --json)";
pub const ABOUT: &str = "lists the direct dependencies (prod + dev) and workspace members with
their resolved versions; --all expands to the full tree. -g lists the
globally installed packages instead. --json prints the listing as a json
document (workspace members and the full tree included; with -g, the
globals as {\"global\": [{name, version}]}).";
pub const EXAMPLE: &str = "  $ zap ls --all
  list the whole dependency tree, workspaces included
  $ zap ls --json
  the same listing as a json document, for tools";

pub fn run(args: &[String]) -> Result<i32> {
    let all = args.iter().any(|a| a == "--all" || a == "-a");
    let global = args.iter().any(|a| a == "--global" || a == "-g");
    let json = args.iter().any(|a| a == "--json");
    if global {
        if json {
            println!("{}", serde_json::to_string_pretty(&global_ls_json()?)?);
            return Ok(0);
        }
        // the global list needs no project
        return crate::cmds::global_ls();
    }
    let ctx = crate::util::find_root()?;
    let root = install_root_for(&ctx);
    let lock = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow!("no .zap-lock.json, run `zap install` first"))?;
    let (deps, dev) = read_deps(&root)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&ls_json(&root, &lock, &deps, &dev, all)?)?);
        return Ok(0);
    }
    println!(
        "{}",
        dim(format!("{} ({} packages installed)", root.display(), lock.packages.len()))
    );
    for (n, r) in deps.iter().chain(dev.iter()) {
        match lock.root.get(n) {
            Some(i) => println!("  {} {}", n, i.rsplit_once('@').map(|x| x.1).unwrap_or("")),
            None => println!("  {} {} {}", n, r, yellow("(not installed, run `zap install`)")),
        }
    }
    if !lock.workspaces.is_empty() {
        println!("{}", dim("workspace members:"));
        for m in &lock.workspaces {
            println!("  {} {} {}", dim("[ws]"), m.name, dim(&m.dir));
        }
    }
    if all {
        println!();
        println!("{}", dim("full dependency tree:"));
        for id in lock.packages.keys() {
            println!("  {}", id);
        }
    }
    Ok(0)
}

/// the --json mirror of the project listing: manifest identity + direct deps
/// with resolved versions (the requested range when not installed, the same
/// fallback the human list shows) + workspace members; --all adds the tree
fn ls_json(
    root: &Path,
    lock: &Lockfile,
    deps: &BTreeMap<String, String>,
    dev: &BTreeMap<String, String>,
    all: bool,
) -> Result<serde_json::Value> {
    let manifest = crate::util::read_json(&root.join("package.json"))?;
    let dep_map = |list: &BTreeMap<String, String>| {
        let mut m = serde_json::Map::new();
        for (n, r) in list {
            let v = match lock.root.get(n) {
                Some(i) => i.rsplit_once('@').map(|x| x.1.to_string()).unwrap_or_else(|| r.clone()),
                None => r.clone(),
            };
            m.insert(n.clone(), serde_json::json!(v));
        }
        serde_json::Value::Object(m)
    };
    let mut doc = serde_json::json!({
        "name": manifest.get("name").and_then(|x| x.as_str()).unwrap_or(""),
        "version": manifest.get("version").and_then(|x| x.as_str()).unwrap_or(""),
        "private": manifest.get("private").and_then(|x| x.as_bool()).unwrap_or(false),
        "dependencies": dep_map(deps),
        "devDependencies": dep_map(dev),
        "workspaceMembers": lock
            .workspaces
            .iter()
            .map(|m| serde_json::json!({ "name": m.name, "dir": m.dir }))
            .collect::<Vec<_>>(),
    });
    if all {
        doc.as_object_mut()
            .unwrap()
            .insert("tree".into(), serde_json::json!(lock.packages.keys().collect::<Vec<_>>()));
    }
    Ok(doc)
}

/// the -g --json listing: same global_root walk `zap ls -g` does, collected
/// instead of printed
fn global_ls_json() -> Result<serde_json::Value> {
    Ok(serde_json::json!({ "global": global_pkgs(&crate::cmds::global_root()?) }))
}

/// one entry per package dir under the global root, version resolved from
/// each package's own lockfile ("?" when it has none)
fn global_pkgs(groot: &Path) -> Vec<serde_json::Value> {
    let mut pkgs = Vec::new();
    if let Ok(rd) = std::fs::read_dir(groot) {
        for e in rd.flatten() {
            let dir = e.path();
            if !dir.is_dir() {
                continue;
            }
            let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let version = crate::lockfile::read(&dir.join(".zap-lock.json"))
                .ok()
                .flatten()
                .and_then(|l| l.root.values().next().cloned())
                .and_then(|id| id.rsplit_once('@').map(|x| x.1.to_string()))
                .unwrap_or_else(|| "?".to_string());
            pkgs.push(serde_json::json!({ "name": name, "version": version }));
        }
    }
    // deterministic output regardless of readdir order
    pkgs.sort_by(|a, b| a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or("")));
    pkgs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (std::path::PathBuf, Lockfile) {
        let dir = std::env::temp_dir().join(format!("zap-ls-json-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"root","private":true,"version":"1.2.3","dependencies":{"ms":"^2.0.0"},"devDependencies":{"left-pad":"^1.0.0"}}"#,
        )
        .unwrap();
        let lock: Lockfile = serde_json::from_str(
            r#"{"version":2,"rootHash":"x","root":{"ms":"ms@2.1.3","left-pad":"left-pad@1.3.0"},"packages":{},"workspaces":[{"name":"w","dir":"packages/w","deps":{}}]}"#,
        )
        .unwrap();
        (dir, lock)
    }

    #[test]
    fn ls_json_carries_the_listing() {
        let (dir, lock) = fixture();
        // ghost isn't in the lock → the not-installed fallback shows the range
        let deps = BTreeMap::from([("ms".to_string(), "^2.0.0".to_string()), ("ghost".to_string(), "^9.0.0".to_string())]);
        let dev = BTreeMap::from([("left-pad".to_string(), "^1.0.0".to_string())]);
        let doc = ls_json(&dir, &lock, &deps, &dev, false).unwrap();
        // parses round-trip and carries the key fields
        let text = serde_json::to_string(&doc).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["name"], "root");
        assert_eq!(parsed["version"], "1.2.3");
        assert_eq!(parsed["private"], true);
        assert_eq!(parsed["dependencies"]["ms"], "2.1.3"); // resolved, like the human list
        assert_eq!(parsed["dependencies"]["ghost"], "^9.0.0"); // not installed → range
        assert_eq!(parsed["devDependencies"]["left-pad"], "1.3.0"); // installed → resolved
        assert_eq!(parsed["workspaceMembers"][0]["name"], "w");
        assert!(parsed.get("tree").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ls_json_all_adds_the_tree() {
        let (dir, lock) = fixture();
        let doc = ls_json(&dir, &lock, &BTreeMap::new(), &BTreeMap::new(), true).unwrap();
        assert_eq!(doc["tree"].as_array().map(|a| a.len()), Some(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn global_pkgs_collects_global_installs() {
        let groot = std::env::temp_dir().join(format!("zap-ls-json-global-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&groot);
        let pkg = groot.join("chalk");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join(".zap-lock.json"),
            r#"{"version":2,"rootHash":"x","root":{"chalk":"chalk@5.3.0"},"packages":{}}"#,
        )
        .unwrap();
        // a dir with no lockfile → "?" version, still listed
        std::fs::create_dir_all(groot.join("bare")).unwrap();
        let pkgs = global_pkgs(&groot);
        let _ = std::fs::remove_dir_all(&groot);
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs[0]["name"], "bare"); // sorted by name
        assert_eq!(pkgs[0]["version"], "?");
        assert_eq!(pkgs[1]["name"], "chalk");
        assert_eq!(pkgs[1]["version"], "5.3.0");
    }
}
