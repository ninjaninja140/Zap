// `zap why <pkg>`, dependency chains; --project scans source files for refs

use anyhow::{anyhow, Result};
use std::collections::HashSet;

use crate::cmds::{walk, why_project};
use crate::model::Lockfile;
use crate::util::yellow;
use crate::workspace::install_root_for;

pub const NAME: &str = "why";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "show dependency chains for a package (--project scans source files, --json)";
pub const ABOUT: &str = "shows every chain that pulls a package into the tree, root dep,
transitive or workspace link. --project additionally greps your source files
for imports so you see what actually references it. --json prints the chains
as arrays of {name, version} nodes (lockfile chains only, the --project
grep stays human-only).";
pub const EXAMPLE: &str = "  $ zap why chalk
  print the dependency chains ending at chalk
  $ zap why chalk --json
  the same chains as a json document, for tools";

pub fn run(args: &[String]) -> Result<i32> {
    let project = args.iter().any(|a| a == "--project");
    let json = args.iter().any(|a| a == "--json");
    let target = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow!("usage: zap why [--project] <package>"))?
        .clone();
    let root = install_root_for(&crate::util::find_root()?);

    if project && !json {
        return why_project(&root, &target);
    }
    let ctx = crate::util::find_root()?;
    let root = install_root_for(&ctx);
    let lock: Lockfile = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow!("no .zap-lock.json, run `zap install` first"))?;

    if json {
        let chains = collect_chains(&lock, &target);
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "pkg": target,
                "chains": chains
                    .iter()
                    .map(|c| c.iter().map(|id| id_node(id)).collect::<Vec<_>>())
                    .collect::<Vec<_>>(),
            }))?
        );
        return Ok(0);
    }

    let mut printed = 0usize;
    let mut visited: HashSet<String> = HashSet::new();
    for dep_id in lock.root.values() {
        walk(&lock, dep_id, &mut Vec::new(), &target, &mut printed, &mut visited);
    }
    for m in &lock.workspaces {
        for dep_id in m.deps.values() {
            walk(&lock, dep_id, &mut Vec::new(), &target, &mut printed, &mut visited);
        }
    }
    if printed == 0 {
        println!("{}", yellow(format!("`{}` is not in the dependency tree", target)));
    }
    Ok(0)
}

/// the --json chain walk: _shared's walk collecting instead of printing, one
/// shared visited set across root + workspace roots, exactly like the human
/// traversal
fn collect_chains(lock: &Lockfile, target: &str) -> Vec<Vec<String>> {
    let mut chains = Vec::new();
    let mut visited = HashSet::new();
    let mut chain = Vec::new();
    let mut roots: Vec<&String> = lock.root.values().collect();
    for m in &lock.workspaces {
        roots.extend(m.deps.values());
    }
    for dep_id in roots {
        walk_collect(lock, dep_id, &mut chain, target, &mut chains, &mut visited);
    }
    chains
}

fn walk_collect(
    lock: &Lockfile,
    id: &str,
    chain: &mut Vec<String>,
    target: &str,
    out: &mut Vec<Vec<String>>,
    visited: &mut HashSet<String>,
) {
    let name = id.rsplit_once('@').map(|x| x.0).unwrap_or(id);
    chain.push(id.to_string());
    if name == target {
        out.push(chain.clone());
        chain.pop();
        return;
    }
    if !visited.insert(id.to_string()) {
        chain.pop();
        return;
    }
    if let Some(p) = lock.packages.get(id) {
        for dep_id in p.deps.values() {
            walk_collect(lock, dep_id, chain, target, out, visited);
        }
    }
    chain.pop();
}

/// a chain entry id ("ms@2.1.3", scoped "@x/y@1.0.0", workspace link "ws:w")
/// as the json node object
fn id_node(id: &str) -> serde_json::Value {
    match id.strip_prefix("ws:") {
        Some(ws) => serde_json::json!({ "name": ws, "version": serde_json::Value::Null, "workspace": true }),
        None => match id.rsplit_once('@') {
            Some((n, v)) => serde_json::json!({ "name": n, "version": v }),
            None => serde_json::json!({ "name": id, "version": serde_json::Value::Null }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_with_transitive() -> Lockfile {
        serde_json::from_str(
            r#"{
            "version": 2, "rootHash": "x",
            "root": { "a": "chalk@5.3.0", "b": "ms@2.1.3" },
            "packages": {
                "chalk@5.3.0": { "integrity": "sha512-x", "tarball": "t", "deps": { "ansi-styles": "ansi-styles@6.2.1" } },
                "ansi-styles@6.2.1": { "integrity": "sha512-y", "tarball": "t", "deps": {} }
            }
        }"#,
        )
        .unwrap()
    }

    #[test]
    fn json_chains_mirror_the_human_walk() {
        let lock = lock_with_transitive();
        // direct hit
        let chains = collect_chains(&lock, "ms");
        assert_eq!(chains, vec![vec!["ms@2.1.3".to_string()]]);
        // transitive chain, root dep first
        let chains = collect_chains(&lock, "ansi-styles");
        assert_eq!(chains, vec![vec!["chalk@5.3.0".to_string(), "ansi-styles@6.2.1".to_string()]]);
        // not in the tree → empty (human path prints the note, json just empties)
        assert!(collect_chains(&lock, "nope").is_empty());
    }

    #[test]
    fn chain_nodes_parse_ids() {
        assert_eq!(id_node("ms@2.1.3"), serde_json::json!({ "name": "ms", "version": "2.1.3" }));
        assert_eq!(
            id_node("@scope/pkg@1.0.0"),
            serde_json::json!({ "name": "@scope/pkg", "version": "1.0.0" })
        );
        assert_eq!(
            id_node("ws:w"),
            serde_json::json!({ "name": "w", "version": serde_json::Value::Null, "workspace": true })
        );
    }

    #[test]
    fn why_json_document_shape() {
        let lock = lock_with_transitive();
        let chains = collect_chains(&lock, "ansi-styles");
        let doc = serde_json::json!({
            "pkg": "ansi-styles",
            "chains": chains.iter().map(|c| c.iter().map(|id| id_node(id)).collect::<Vec<_>>()).collect::<Vec<_>>(),
        });
        let parsed: serde_json::Value = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(parsed["pkg"], "ansi-styles");
        assert_eq!(parsed["chains"][0][0]["name"], "chalk");
        assert_eq!(parsed["chains"][0][0]["version"], "5.3.0");
        assert_eq!(parsed["chains"][0][1]["name"], "ansi-styles");
    }
}
