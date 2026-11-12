// `zap ls [--all]`, direct deps + workspace members, --all for the full tree

use anyhow::{anyhow, Result};

use crate::cmds::read_deps;
use crate::util::{dim, yellow};
use crate::workspace::install_root_for;

pub const NAME: &str = "ls";
pub const ALIASES: &[&str] = &["list"];
pub const USAGE: &str = "list dependencies and workspace members (-g for global installs)";
pub const ABOUT: &str = "lists the direct dependencies (prod + dev) and workspace members with
their resolved versions; --all expands to the full tree. -g lists the
globally installed packages instead.";
pub const EXAMPLE: &str = "  $ zap ls --all
  list the whole dependency tree, workspaces included
  $ zap ls -g
  list the global installs with their versions";

pub fn run(args: &[String]) -> Result<i32> {
    let all = args.iter().any(|a| a == "--all" || a == "-a");
    let global = args.iter().any(|a| a == "--global" || a == "-g");
    if global {
        // the global list needs no project
        return crate::cmds::global_ls();
    }
    let ctx = crate::util::find_root()?;
    let root = install_root_for(&ctx);
    let lock = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow!("no .zap-lock.json, run `zap install` first"))?;
    let (deps, dev) = read_deps(&root)?;
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
