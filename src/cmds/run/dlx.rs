// `zap dlx <pkg> [args]`, force a one-off run of a package binary

use anyhow::Result;

use crate::cmds::{dlx_run, parse_x_args};

pub const NAME: &str = "dlx";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "force one-off run of a package binary";
pub const ABOUT: &str = "always fetches the package fresh into a one-off cache and runs its
default bin, use it when you want the latest version regardless of what's
installed locally.";
pub const EXAMPLE: &str = "  $ zap dlx cowsay mooin'
  fetch cowsay on the fly and run it";

pub fn run(args: &[String]) -> Result<i32> {
    let (name, range, passthrough) = parse_x_args(args)?;
    dlx_run(&name, range, passthrough)
}
