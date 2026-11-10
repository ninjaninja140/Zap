// `zap ci`, wipe .zap then a frozen production-only install (docker/CI builds)

use anyhow::Result;

pub const NAME: &str = "ci";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "clean install, production deps only (docker/CI builds)";

pub fn run(_args: &[String]) -> Result<i32> {
    let ci_root = crate::workspace::install_root_for(&crate::util::find_root()?);
    let _ = std::fs::remove_dir_all(ci_root.join(".zap"));
    println!("{}", crate::util::green("clean install"));
    crate::cmds::install_in(&ci_root, &["--production".to_string(), "--frozen-lockfile".to_string()], None)
}
