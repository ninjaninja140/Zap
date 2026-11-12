// `zap remove <pkg>...`, drop deps from the manifest then reinstall

use anyhow::{anyhow, bail, Result};

use crate::cmds::install_in;
use crate::util::green;
use crate::workspace::install_root_for;

pub const NAME: &str = "remove";
pub const ALIASES: &[&str] = &["rm", "uninstall"];
pub const USAGE: &str = "remove dependencies (-g for global installs)";
pub const ABOUT: &str = "drops named deps from package.json (dev deps from their own section),
unlinks them from the tree and re-installs. -g removes global installs: the
package dir under ~/.zap/global and its shims in ~/.zap/bin. aliases: rm,
uninstall.";
pub const EXAMPLE: &str = "  $ zap remove chalk
  remove chalk from the manifest and prune the tree
  $ zap remove -g typescript
  drop the global typescript install and its tsc shim";

pub fn run(args: &[String]) -> Result<i32> {
    let global = args.iter().any(|a| a == "--global" || a == "-g");
    if global {
        // global removal never touches a project manifest
        return crate::cmds::global_remove(args);
    }
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() {
        bail!("usage: zap remove <package> ...");
    }
    let ctx = crate::util::find_root()?;
    let install_root = install_root_for(&ctx);
    let pkg_path = ctx.join("package.json");
    let mut v = crate::util::read_json(&pkg_path)?;
    let obj = v.as_object_mut().ok_or_else(|| anyhow!("package.json root must be an object"))?;
    let mut removed = 0usize;
    for key in ["dependencies", "devDependencies"] {
        if let Some(d) = obj.get_mut(key).and_then(|x| x.as_object_mut()) {
            for n in &names {
                if d.remove(n).is_some() {
                    removed += 1;
                }
            }
        }
    }
    if removed == 0 {
        bail!("`{}` not found in dependencies", names.join(", "));
    }
    std::fs::write(&pkg_path, format!("{}\n", serde_json::to_string_pretty(&v)?))?;
    println!("{} {}", green("removed"), names.join(", "));
    install_in(&install_root, &[], None)
}
