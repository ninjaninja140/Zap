// `zap bin [name]`, print bin paths from the dependency tree

use anyhow::{anyhow, bail, Result};

use crate::util::dim;
use crate::workspace::install_root_for;

pub const NAME: &str = "bin";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "print bin paths from the dependency tree";

pub fn run(args: &[String]) -> Result<i32> {
    let ctx = crate::util::find_root()?;
    let root = install_root_for(&ctx);
    let lock = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow!("no .zap-lock.json, run `zap install` first"))?;
    let vroot = root.join(".zap").join("v");
    let target = args.iter().find(|a| !a.starts_with('-')).cloned();
    let mut found = 0usize;
    for (id, pkg) in &lock.packages {
        for (bin_name, rel) in &pkg.bin {
            let Some(t) = &target else {
                println!("  {} -> {}", bin_name, vroot.join(crate::util::pkg_dir_name(id)).join(rel).display());
                found += 1;
                continue;
            };
            if bin_name == t {
                println!("{}", vroot.join(crate::util::pkg_dir_name(id)).join(rel).display());
                return Ok(0);
            }
            let _ = &found;
        }
    }
    if target.is_some() {
        bail!("no bin named `{}` in the dependency tree", target.unwrap());
    }
    if found == 0 {
        println!("{}", dim("no bins installed"));
    }
    Ok(0)
}
