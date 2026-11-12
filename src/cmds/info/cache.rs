// `zap cache clean`, wipe the global store, metadata cache and dlx cache

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::util::green;

pub const NAME: &str = "cache";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "wipe the global store, metadata cache and dlx cache";
pub const ABOUT: &str = "wipes the global content-addressable store, the registry metadata
cache and the dlx cache, the reset button when a corrupted artifact makes
installs misbehave. logs each target's size before deleting.";
pub const EXAMPLE: &str = "  $ zap cache clean
  drop all local caches; the next install refetches";

/// the wipe targets with their stats, computed before any deletion
fn cache_targets(dirs: &[(&str, &Path)]) -> Vec<(String, PathBuf, u64, u64)> {
    dirs.iter()
        .map(|(label, path)| {
            let (files, bytes) = crate::cmds::dir_stats(path);
            ((*label).to_string(), path.to_path_buf(), files, bytes)
        })
        .collect()
}

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

            // the target log: sizes before deletion, always printed (a log,
            // not a bar, piped runs keep it)
            let targets = cache_targets(&[
                ("store", &store),
                ("metadata cache", &meta),
                ("dlx cache", &dlx),
            ]);
            let mut freed_files = 0u64;
            let mut freed_bytes = 0u64;
            for (label, path, files, bytes) in &targets {
                println!(
                    "  {} {}: {} ({}, {} files)",
                    crate::util::dim("removing"),
                    label,
                    path.display(),
                    crate::util::human(*bytes),
                    files
                );
                freed_files += files;
                freed_bytes += bytes;
            }

            // the store is the big one: per-entry removal under the bar (one
            // giant remove_dir_all would make the bar meaningless)
            let entries: Vec<PathBuf> = std::fs::read_dir(&store)
                .map(|rd| rd.flatten().map(|e| e.path()).collect())
                .unwrap_or_default();
            if !entries.is_empty() {
                let progress = crate::cmds::progress::Progress::start(0);
                progress.stage("removing store");
                progress.bump_jobs(0, entries.len() as u64, 0);
                for (k, e) in entries.iter().enumerate() {
                    let _ = std::fs::remove_dir_all(e);
                    progress.bump_jobs(k as u64 + 1, entries.len() as u64, 0);
                }
                progress.finish();
                // the root goes too (today's end-state: the whole store gone)
                let _ = std::fs::remove_dir(&store);
            }
            // metadata + dlx are small: quick wipes after the bar
            let _ = std::fs::remove_dir_all(&meta);
            let _ = std::fs::remove_dir_all(&dlx);

            println!("{} store, metadata cache and dlx cache cleared", green("done"));
            println!("  {} freed {} ({} files)", green("✓"), crate::util::human(freed_bytes), freed_files);
            Ok(0)
        }
        _ => bail!("usage: zap cache clean"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// env-touching tests serialize: ZAP_HOME leaks process-wide
    
    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zap-cache-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cache_targets_lists_sizes_and_files() {
        let _env = crate::cmds::env_test_lock();
        let home = tempdir("home");
        std::env::set_var("ZAP_HOME", &home);
        // store: two package entries, two files with known sizes
        let store = crate::util::store_dir().unwrap();
        std::fs::create_dir_all(store.join("a@1.0.0")).unwrap();
        std::fs::write(store.join("a@1.0.0").join("f.txt"), "12345").unwrap();
        std::fs::create_dir_all(store.join("b@2.0.0").join("sub")).unwrap();
        std::fs::write(store.join("b@2.0.0").join("sub").join("g.txt"), "1234567").unwrap();
        // meta + dlx: one file each
        let meta = crate::util::meta_cache().unwrap();
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("m.json"), "123").unwrap();
        let dlx = crate::util::zap_home().unwrap().join("dlx");
        std::fs::create_dir_all(&dlx).unwrap();
        std::fs::write(dlx.join("d.txt"), "12").unwrap();

        let targets = cache_targets(&[
            ("store", &store),
            ("metadata cache", &meta),
            ("dlx cache", &dlx),
            ("missing", &home.join("nope")),
        ]);
        assert_eq!(targets.len(), 4);
        let (label, path, files, bytes) = &targets[0];
        assert_eq!((label.as_str(), *files, *bytes), ("store", 2, 12));
        assert_eq!(path.file_name().unwrap(), "store");
        assert_eq!((targets[1].0.as_str(), targets[1].2, targets[1].3), ("metadata cache", 1, 3));
        assert_eq!((targets[2].0.as_str(), targets[2].2, targets[2].3), ("dlx cache", 1, 2));
        // a missing dir stats as zeros, still listed
        assert_eq!((targets[3].0.as_str(), targets[3].2, targets[3].3), ("missing", 0, 0));
        std::env::remove_var("ZAP_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn clean_wipes_everything_and_frees_the_logged_sizes() {
        let _env = crate::cmds::env_test_lock();
        let home = tempdir("clean");
        std::env::set_var("ZAP_HOME", &home);
        let store = crate::util::store_dir().unwrap();
        std::fs::create_dir_all(store.join("a@1.0.0").join("d")).unwrap();
        std::fs::write(store.join("a@1.0.0").join("d").join("f.txt"), "12345").unwrap();
        let meta = crate::util::meta_cache().unwrap();
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("m.json"), "123").unwrap();
        let dlx = crate::util::zap_home().unwrap().join("dlx");
        std::fs::create_dir_all(&dlx).unwrap();
        std::fs::write(dlx.join("d.txt"), "12").unwrap();

        // the accounting the log lines will show: 3 files, 10 bytes
        let targets = cache_targets(&[
            ("store", &store),
            ("metadata cache", &meta),
            ("dlx cache", &dlx),
        ]);
        let freed: (u64, u64) = targets.iter().fold((0, 0), |a, t| (a.0 + t.2, a.1 + t.3));
        assert_eq!(freed, (3, 10), "pre-deletion accounting");

        let code = run(&["clean".to_string()]).unwrap();
        assert_eq!(code, 0);
        // everything wiped
        assert!(!store.exists());
        assert!(!meta.exists());
        assert!(!dlx.exists());
        // second run: zeros + trivial bar + exit 0
        let code = run(&["clean".to_string()]).unwrap();
        assert_eq!(code, 0);
        std::env::remove_var("ZAP_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
