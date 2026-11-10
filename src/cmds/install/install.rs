// `zap install`, resolve + materialize from package.json into the pnp tree

use crate::cmds::install_in;
use crate::workspace::install_root_for;
use anyhow::Result;

pub const NAME: &str = "install";
pub const ALIASES: &[&str] = &["i"];
pub const USAGE: &str = "install everything from package.json (incl. workspaces; --offline, --force, --refresh, --frozen-lockfile for CI)";

pub fn run(args: &[String]) -> Result<i32> {
    let ctx = crate::util::find_root()?;
    install_in(&install_root_for(&ctx), args, None)
}
