// `zap install`, resolve + materialize from package.json into the pnp tree

use crate::cmds::install_in;
use crate::workspace::install_root_for;
use anyhow::Result;

pub const NAME: &str = "install";
pub const ALIASES: &[&str] = &["i"];
pub const USAGE: &str = "install everything from package.json (incl. workspaces; --offline, --force, --refresh, --frozen-lockfile for CI, --json)";
pub const ABOUT: &str = "resolves and links everything in package.json, workspace members included.
the pnp linker is the default (no node_modules); `linker: node-modules` in
.zaprc switches to junctions. tarballs are sha512-verified and hardlinked
from the global store when seen before, so repeat installs are near-instant.
dependency lifecycle scripts run only when `lifecycle: true` is set and the
package is approved via `zap allow`. --json suppresses the human output and
prints the install report document instead.";
pub const EXAMPLE: &str = "  $ zap install --frozen-lockfile
  clean production install for CI; lockfile must match package.json
  $ zap install --json
  the install report as a json document, for tools";

pub fn run(args: &[String]) -> Result<i32> {
    let ctx = crate::util::find_root()?;
    install_in(&install_root_for(&ctx), args, None)
}
