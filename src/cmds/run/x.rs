// `zap x <bin> [args]`, run an installed bin locally, npx-style dlx fallback

use anyhow::{Context, Result};

use crate::cmds::{dlx_run, find_local_bin, parse_x_args};
use crate::config::Config;

pub const NAME: &str = "x";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "run an installed bin locally, or fetch it (like npx)";
pub const ABOUT: &str = "runs a binary from the local dependency tree, falling back to a dlx
fetch when it isn't installed (npx style), the fallback works from any cwd,
no project needed. args after the bin name pass through.";
pub const EXAMPLE: &str = "  $ zap x eslint src/
  run the local eslint, fetching it only if it's missing";

pub fn run(args: &[String]) -> Result<i32> {
    let (name, range, passthrough) = parse_x_args(args)?;
    if range.is_empty() {
        if let Some((bin_file, install_root)) = find_local_bin(&name)? {
            let cfg = Config::load(&install_root);
            let mut c = std::process::Command::new("node");
            c.arg(bin_file).args(&passthrough).current_dir(&install_root);
            crate::pnp::apply_env(&mut c, &install_root, cfg.linker);
            let status = c.status().with_context(|| format!("running {}", name))?;
            return Ok(status.code().unwrap_or(1));
        }
    }
    dlx_run(&name, range, passthrough)
}
