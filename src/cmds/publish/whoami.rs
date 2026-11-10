// `zap whoami`, thin wrapper over the publish library

pub const NAME: &str = "whoami";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "print the authenticated registry user";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::publish::whoami_cmd(args)
}
