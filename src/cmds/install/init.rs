// `zap init`, minimal package.json bootstrap

use anyhow::{bail, Result};
use crate::util::{cyan, green};

pub const NAME: &str = "init";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "create a package.json";

pub fn run(_args: &[String]) -> Result<i32> {
    let root = std::env::current_dir()?;
    let p = root.join("package.json");
    if p.is_file() {
        bail!("package.json already exists here");
    }
    let name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase().replace(' ', "-"))
        .unwrap_or_else(|| "project".to_string());
    let v = serde_json::json!({ "name": name, "version": "1.0.0", "private": true });
    std::fs::write(&p, format!("{}\n", serde_json::to_string_pretty(&v)?))?;
    println!("{} package.json", green("created"));
    println!("next: {}", cyan("zap add <pkg>"));
    Ok(0)
}
