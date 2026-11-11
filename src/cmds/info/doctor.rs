// `zap doctor`, thin wrapper over the devtools library

pub const NAME: &str = "doctor";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "sanity checks: lockfile sync, runtime, ts, store";
pub const ABOUT: &str = "runs sanity checks over the project: lockfile sync, runtime
resolution, typescript setup and store health, the first stop when
something feels off.";
pub const EXAMPLE: &str = "  $ zap doctor
  check lockfile, runtime, ts and store in one pass";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::devtools::doctor_cmd(args)
}
