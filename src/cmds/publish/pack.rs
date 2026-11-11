// `zap pack`, thin wrapper over the publish library

pub const NAME: &str = "pack";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "build an npm tarball from package.json";
pub const ABOUT: &str = "builds the npm tarball from package.json (files/bin/main honored,
readme normalized) and prints the name, version, size and path, handy to
inspect what would ship before publishing.";
pub const EXAMPLE: &str = "  $ zap pack
  create the tarball in the current directory";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::publish::pack_cmd(args)
}
