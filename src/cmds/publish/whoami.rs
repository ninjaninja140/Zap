// `zap whoami`, thin wrapper over the publish library

pub const NAME: &str = "whoami";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "print the authenticated registry user";
pub const ABOUT: &str = "prints the user the active registry associates with the stored token,
the quickest auth check before a publish.";
pub const EXAMPLE: &str = "  $ zap whoami
  verify the stored credentials resolve to a user";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::publish::whoami_cmd(args)
}
