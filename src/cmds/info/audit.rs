// `zap audit`, thin wrapper over the devtools library

pub const NAME: &str = "audit";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "check installed packages for known advisories";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::devtools::audit_cmd(args)
}
