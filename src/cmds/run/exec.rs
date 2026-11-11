// `zap exec <cmd> [args...]`, spawn directly (no shell) with the linker active

use anyhow::{bail, Context, Result};

use crate::cmds::ensure_runtime_ready;
use crate::config::Config;
use crate::workspace::install_root_for;

pub const NAME: &str = "exec";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "run any command with the linker active";
pub const ABOUT: &str = "spawns any command directly (no shell) with the active linker's
environment wired up, pnp resolve hooks or node_modules paths, so tools
that need to resolve the tree work outside scripts.";
pub const EXAMPLE: &str = "  $ zap exec jest --watch
  run jest with the project's dependency resolution active";

pub fn run(args: &[String]) -> Result<i32> {
    let ctx = crate::util::find_root()?;
    let install_root = install_root_for(&ctx);
    let cfg = Config::load(&ctx);
    ensure_runtime_ready(&install_root, &cfg)?;
    if args.is_empty() {
        bail!("usage: zap exec <command> [args...]");
    }
    // spawn directly (no shell) so quoting stays sane
    let mut c = std::process::Command::new(&args[0]);
    c.args(&args[1..]).current_dir(&ctx);
    crate::pnp::apply_env(&mut c, &install_root, cfg.linker);
    let status = c.status().with_context(|| format!("running `{}`", args[0]))?;
    Ok(status.code().unwrap_or(1))
}
