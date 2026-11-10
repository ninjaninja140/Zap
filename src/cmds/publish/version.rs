// `zap version`, npm-version-style bumping, advertised but not wired up yet

pub const NAME: &str = "version";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "bump package.json version (npm version)";

// shortcut: version bumping is not implemented, this stub only exists so the
// advertised command errors clearly instead of falling through to run_script;
// wire up when npm-version parity lands
pub fn run(_args: &[String]) -> anyhow::Result<i32> {
    anyhow::bail!("`zap version` is not implemented yet")
}
