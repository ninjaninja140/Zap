// `zap config [-g] [<key> [value]]`, thin wrapper over the config library
// (src/config.rs stays as the core Config infrastructure)

pub const NAME: &str = "config";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "read or set config (global with -g; no rc = new .zaprc.yaml)";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::config::config_cmd(args)
}
