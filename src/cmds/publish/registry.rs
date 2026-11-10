// `zap registry`, thin wrapper over the publish library

pub const NAME: &str = "registry";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "switch registries (private -> public flow)";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::publish::registry_cmd(args)
}
