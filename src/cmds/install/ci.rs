// `zap ci`, wipe .zap then a frozen production-only install (docker/CI builds)

use anyhow::Result;

pub const NAME: &str = "ci";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "clean install, production deps only (docker/CI builds; --json)";
pub const ABOUT: &str = "clean production install from the lockfile: no manifest re-resolution and
no lifecycle scripts. fails when package.json and .zap-lock.json disagree,
re-run `zap install` to re-lock first. --json suppresses the human output and
prints the install report document instead.";
pub const EXAMPLE: &str = "  $ zap ci --json
  production-only install for CI, the report as a json document";

pub fn run(args: &[String]) -> Result<i32> {
    let json = args.iter().any(|a| a == "--json");
    let ci_root = crate::workspace::install_root_for(&crate::util::find_root()?);
    let _ = std::fs::remove_dir_all(ci_root.join(".zap"));
    if !json {
        println!("{}", crate::util::green("clean install"));
    }
    let mut iargs = vec!["--production".to_string(), "--frozen-lockfile".to_string()];
    if json {
        iargs.push("--json".to_string());
    }
    crate::cmds::install_in(&ci_root, &iargs, None)
}
