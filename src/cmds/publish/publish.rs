// `zap publish`, thin wrapper over the publish library

pub const NAME: &str = "publish";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "publish a package (auth via ZAP_TOKEN / `token` in .zaprc); --tag t, --access a, --otp c, --registry name|url, --dry-run";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::publish::publish_cmd(args)
}
