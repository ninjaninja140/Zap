// `zap tsc [args]`, run the zap-managed typescript compiler with types wired up

use anyhow::{anyhow, Context, Result};

use crate::cmds::{ensure_runtime_ready, ts_package_id};
use crate::config::{Config, Linker};
use crate::workspace::install_root_for;

pub const NAME: &str = "tsc";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "run the zap-managed typescript compiler";

pub fn run(args: &[String]) -> Result<i32> {
    let ctx = crate::util::find_root()?;
    let install_root = install_root_for(&ctx);
    let cfg = Config::load(&ctx);
    ensure_runtime_ready(&install_root, &cfg)?;
    let lock = crate::lockfile::read(&install_root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow!("no .zap-lock.json, run `zap install` first"))?;
    let tree = crate::model::Tree::from(&lock);
    let ts_id = ts_package_id(&tree)
        .ok_or_else(|| anyhow!("typescript is not managed here, add a tsconfig.json or set `typescript: <version>` in .zaprc, then `zap install`"))?;
    if cfg.linker == Linker::Pnp {
        crate::linker::link_typecheck_dir(&install_root, &tree)?;
    }
    let pkg = tree.packages.get(ts_id.as_str());
    let rel = pkg
        .and_then(|p| p.bin.get("tsc"))
        .cloned()
        .unwrap_or_else(|| "bin/tsc".to_string());
    let bin_file = install_root
        .join(".zap")
        .join("v")
        .join(crate::util::pkg_dir_name(&ts_id))
        .join(rel.trim_start_matches("./"));
    let mut c = std::process::Command::new("node");
    c.arg(bin_file).args(args).current_dir(&ctx);
    crate::pnp::apply_env(&mut c, &install_root, cfg.linker);
    if cfg.linker == Linker::Pnp {
        c.env(
            "NODE_PATH",
            install_root.join(".zap").join("ts-nm").display().to_string(),
        );
    }
    let status = c.status().with_context(|| "running tsc")?;
    Ok(status.code().unwrap_or(1))
}
