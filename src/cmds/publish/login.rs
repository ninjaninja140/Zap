// `zap login`, thin wrapper over the publish library

pub const NAME: &str = "login";
pub const ALIASES: &[&str] = &["adduser"];
pub const USAGE: &str = "browser login (npm web-auth flow) or paste a token";
pub const ABOUT: &str = "authenticates against the active registry: a browser-based npm
web-auth flow by default, or `--token <t>` to paste a token directly. the
credential is stored per-registry for publish/whoami.";
pub const EXAMPLE: &str = "  $ zap login
  open the browser flow and store the resulting token";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::publish::login_cmd(args)
}
