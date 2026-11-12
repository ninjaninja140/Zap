// `zap dlx <pkg> [args]`, force a one-off run of a package binary

use anyhow::Result;

use crate::cmds::{dlx_run, parse_x_args};

pub const NAME: &str = "dlx";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "force one-off run of a package binary";
pub const ABOUT: &str = "runs a package's bin from a one-off cache under ~/.zap/dlx, works
from any cwd, no project needed. a range pins the version; without one the
cached install is reused.";
pub const EXAMPLE: &str = "  $ zap dlx cowsay mooin'
  fetch cowsay on the fly and run it, project or not";

pub fn run(args: &[String]) -> Result<i32> {
    let (name, range, passthrough) = parse_x_args(args)?;
    dlx_run(&name, range, passthrough)
}
