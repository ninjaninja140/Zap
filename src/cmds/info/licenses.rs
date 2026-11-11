// `zap licenses`, thin wrapper over the devtools library

pub const NAME: &str = "licenses";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "license summary of installed packages";
pub const ABOUT: &str = "summarizes the licenses of every installed package so you can check
redistribution terms before publishing.";
pub const EXAMPLE: &str = "  $ zap licenses
  license summary of the installed tree";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::devtools::licenses_cmd(args)
}
