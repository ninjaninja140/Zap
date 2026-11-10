// `zap doctor`, thin wrapper over the devtools library

pub const NAME: &str = "doctor";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "sanity checks: lockfile sync, runtime, ts, store";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::devtools::doctor_cmd(args)
}
