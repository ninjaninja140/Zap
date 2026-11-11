// `zap publish`, thin wrapper over the publish library

pub const NAME: &str = "publish";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "publish a package (auth via ZAP_TOKEN / `token` in .zaprc); --tag t, --access a, --otp c, --registry name|url, --dry-run";
pub const ABOUT: &str = "publishes the package to the registry with sha512 integrity. auth comes
from `zap login`, ZAP_TOKEN/NPM_TOKEN or `token:` in .zaprc; --tag, --access,
--otp and --registry override the defaults, --dry-run goes through the whole
flow without uploading.";
pub const EXAMPLE: &str = "  $ zap publish --tag next --otp 123456
  publish under the `next` dist-tag with a one-time password";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::publish::publish_cmd(args)
}
