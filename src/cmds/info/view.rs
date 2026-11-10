// `zap view|info <pkg> [field]`, thin wrapper over the devtools library

pub const NAME: &str = "view";
pub const ALIASES: &[&str] = &["info"];
pub const USAGE: &str = "registry metadata for a package (cached)";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::devtools::info_cmd(args)
}
