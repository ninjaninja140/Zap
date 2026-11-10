// `zap pack`, thin wrapper over the publish library

pub const NAME: &str = "pack";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "build an npm tarball from package.json";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::publish::pack_cmd(args)
}
