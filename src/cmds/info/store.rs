// `zap store [path|size]`, inspect the global content-addressable store

use anyhow::{bail, Result};

use crate::cmds::dir_stats;
use crate::config::Config;

pub const NAME: &str = "store";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "inspect the global content-addressable store";
pub const ABOUT: &str = "inspects the global content-addressable store: `zap store size` sums
the disk usage, `zap store path <name>` prints where a package's content
lives.";
pub const EXAMPLE: &str = "  $ zap store size
  show how much disk the global store holds";

pub fn run(args: &[String]) -> Result<i32> {
    let cfg = Config::load(&std::env::current_dir()?);
    let dir = match &cfg.store_dir {
        Some(d) => d.clone(),
        None => crate::util::store_dir()?,
    };
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "path" => {
            println!("{}", dir.display());
            Ok(0)
        }
        "size" | "" => {
            let (files, bytes) = dir_stats(&dir);
            match sub {
                "size" => {
                    println!("{}", crate::util::human(bytes));
                    Ok(0)
                }
                _ => {
                    println!(
                        "{}, {} files, {}",
                        dir.display(),
                        files,
                        crate::util::human(bytes)
                    );
                    Ok(0)
                }
            }
        }
        other => bail!("usage: zap store [path|size] (unknown subcommand `{}`)", other),
    }
}
