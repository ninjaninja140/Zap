// `zap login`, thin wrapper over the publish library

pub const NAME: &str = "login";
pub const ALIASES: &[&str] = &["adduser"];
pub const USAGE: &str = "browser login (npm web-auth flow) or paste a token";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::publish::login_cmd(args)
}
