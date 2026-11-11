// `zap logout`, thin wrapper over the publish library

pub const NAME: &str = "logout";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "remove stored credentials for this registry";
pub const ABOUT: &str = "removes the stored credential for the active registry.";
pub const EXAMPLE: &str = "  $ zap logout
  drop the stored token";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::publish::logout_cmd(args)
}
