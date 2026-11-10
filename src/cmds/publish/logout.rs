// `zap logout`, thin wrapper over the publish library

pub const NAME: &str = "logout";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "remove stored credentials for this registry";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::publish::logout_cmd(args)
}
