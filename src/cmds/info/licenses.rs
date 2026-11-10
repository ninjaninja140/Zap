// `zap licenses`, thin wrapper over the devtools library

pub const NAME: &str = "licenses";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "license summary of installed packages";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::devtools::licenses_cmd(args)
}
