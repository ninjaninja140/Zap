// `zap why <pkg>`, dependency chains; --project scans source files for refs

use anyhow::{anyhow, Result};
use std::collections::HashSet;

use crate::cmds::{walk, why_project};
use crate::model::Lockfile;
use crate::util::yellow;
use crate::workspace::install_root_for;

pub const NAME: &str = "why";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "show dependency chains for a package (--project scans source files)";
pub const ABOUT: &str = "shows every chain that pulls a package into the tree, root dep,
transitive or workspace link. --project additionally greps your source files
for imports so you see what actually references it.";
pub const EXAMPLE: &str = "  $ zap why chalk
  print the dependency chains ending at chalk";

pub fn run(args: &[String]) -> Result<i32> {
    let project = args.iter().any(|a| a == "--project");
    let target = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow!("usage: zap why [--project] <package>"))?
        .clone();
    let root = install_root_for(&crate::util::find_root()?);

    if project {
        return why_project(&root, &target);
    }
    let ctx = crate::util::find_root()?;
    let root = install_root_for(&ctx);
    let lock: Lockfile = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow!("no .zap-lock.json, run `zap install` first"))?;
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
