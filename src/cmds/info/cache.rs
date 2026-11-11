// `zap cache clean`, wipe the global store, metadata cache and dlx cache

use anyhow::{bail, Result};

use crate::config::Config;
use crate::util::green;

pub const NAME: &str = "cache";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "wipe the global store, metadata cache and dlx cache";
pub const ABOUT: &str = "wipes the global content-addressable store, the registry metadata
cache and the dlx cache, the reset button when a corrupted artifact makes
installs misbehave.";
pub const EXAMPLE: &str = "  $ zap cache clean
  drop all local caches; the next install refetches";

pub fn run(args: &[String]) -> Result<i32> {
    match args.first().map(|s| s.as_str()) {
        Some("clean") | Some("clear") => {
            let cfg = Config::load(&std::env::current_dir()?);
            let store = match &cfg.store_dir {
                Some(d) => d.clone(),
                None => crate::util::store_dir()?,
            };
            let meta = crate::util::meta_cache()?;
            let dlx = crate::util::zap_home()?.join("dlx");
            for d in [store, meta, dlx] {
                let _ = std::fs::remove_dir_all(d);
            }
            println!("{} store, metadata cache and dlx cache cleared", green("done"));
            Ok(0)
        }
        _ => bail!("usage: zap cache clean"),
    }
}
