// `zap upgrade`: no args re-resolves within ranges; with args bumps to
// latest; --interactive opens pickers, per-package version lists with
// names, or the all-packages three-target flow without

use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::config::Config;
use crate::util::{dim, green};

pub const NAME: &str = "upgrade";
pub const ALIASES: &[&str] = &["up"];
pub const USAGE: &str = "no args: re-resolve within ranges; with args: bump to latest; -i pick versions per package (no args: all-packages targets)";
pub const ABOUT: &str = "no args re-resolves every dep within its manifest range (a cheap
refresh); with args, named packages are bumped to their absolute latest and
re-recorded. --interactive picks exact versions per named package, or opens
the three-target flow for the whole tree when no names are given.";
pub const EXAMPLE: &str = "  $ zap upgrade chalk
  bump chalk to the newest version on the registry";

pub fn run(args: &[String]) -> Result<i32> {
    let ui = args.iter().any(|a| a == "--interactive" || a == "-i");
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if ui {
        let ctx = crate::util::find_root()?;
        let root = crate::workspace::install_root_for(&ctx);
        let cfg = Config::load(&root);
        // heading from the canonical name + args as typed (run() only sees
        // the args after the command name)
        let mut full = vec![NAME.to_string()];
        full.extend(args.iter().cloned());
        let heading = crate::heading_block(&full);
        return if names.is_empty() {
            // the three-target all-packages flow lives in outdated.rs,
            // _sel.rs is #[path]-included there; including it here too would
            // double-compile the module and fork its types
            super::outdated::interactive_upgrade(&root, &cfg, &heading)
        } else {
            pick_versions(&root, &cfg, &names, &heading)
        };
    }
    if args.is_empty() {
        println!("{}", dim("re-resolving all dependencies within their ranges..."));
        return super::install::run(&["--force".to_string()]);
    }
    let bumped: Vec<String> = args
        .iter()
        .map(|a| {
            if a.starts_with('-') {
                return a.clone();
            }
            let (_, range) = crate::util::split_spec(a);
            if range.is_empty() {
                format!("{}@latest", a)
            } else {
                a.clone()
            }
        })
        .collect();
    super::add::run(&bumped)
}

/// the per-package version picker flow: validate every name against the
/// manifest (local), check for a console, fetch packuments through the same
/// registry path outdated's collect() uses, then open one picker per package
/// buffering the picks, any cancel aborts before a single write, and
/// finally record all ranges and run one shared install pass
fn pick_versions(root: &Path, cfg: &Config, names: &[String], heading: &str) -> Result<i32> {
    let manifest = crate::util::read_json(&root.join("package.json"))?;
    let mut wanted: Vec<(String, bool)> = Vec::new(); // (name, dev)
    let mut seen = std::collections::HashSet::new();
    for arg in names {
        let (name, _) = crate::util::split_spec(arg);
        if !seen.insert(name.clone()) {
            continue;
        }
        let in_deps = manifest.get("dependencies").and_then(|d| d.get(&name)).is_some();
        let in_dev = manifest.get("devDependencies").and_then(|d| d.get(&name)).is_some();
        if !in_deps && !in_dev {
            bail!("`{}` is not a dependency of this project", name);
        }
        wanted.push((name, !in_deps && in_dev));
    }
    if !super::outdated::console_available() {
        // piped stdin: bail before any network work
        bail!("interactive mode needs a terminal");
    }

    let reg = crate::http::Registry::new(
        false,
        false,
        cfg.prefer_offline,
        cfg.cache_ttl_hours,
        cfg.registry.as_deref(),
    )?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let packs: std::collections::BTreeMap<String, std::sync::Arc<crate::model::Packument>> = rt.block_on(async {
        let mut set = tokio::task::JoinSet::new();
        for (name, _) in &wanted {
            let reg = reg.clone();
            let name = name.clone();
            set.spawn(async move {
                let pack = reg.packument(&name).await?;
                Ok::<_, anyhow::Error>((name, pack))
            });
        }
        let mut out = Vec::new();
        while let Some(j) = set.join_next().await {
            // first ? for the join, second for the packument fetch itself
            out.push(j.context("metadata task panicked")??);
        }
        Ok::<_, anyhow::Error>(out.into_iter().collect())
    })?;

    // one picker per package, in the order the user named them; picks buffer
    // so a later cancel discards everything (fold_picks below)
    let mut picks: Vec<Option<String>> = Vec::new();
    for (name, _) in &wanted {
        let versions = super::outdated::sort_versions(packs[name].versions.keys().cloned().collect());
        if versions.is_empty() {
            bail!("no versions of `{}` on the registry", name);
        }
        picks.push(super::outdated::pick_version(name, &versions, heading)?);
    }
    let Some(picked) = fold_picks(picks) else {
        return Ok(0); // cancelled mid-pickers: nothing written
    };

    let lock = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow::anyhow!("no .zap-lock.json, run `zap install` first"))?;
    let mut records: Vec<(String, String, bool)> = Vec::new();
    let mut summary: Vec<(String, String, String)> = Vec::new(); // (name, old, chosen)
    for ((name, dev), chosen) in wanted.iter().zip(&picked) {
        let version: &str = chosen; // fold_picks guarantees a pick
        let old = lock
            .root
            .get(name)
            .and_then(|id| id.rsplit_once('@'))
            .map(|x| x.1.to_string())
            .unwrap_or_else(|| "?".to_string());
        records.push((name.clone(), version.to_string(), *dev));
        summary.push((name.clone(), old, version.to_string()));
    }
    super::outdated::record_ranges(root, cfg, &records)?;
    let code = crate::cmds::install_in(root, &[], None)?;
    for (name, old, chosen) in &summary {
        println!("{} {} {} → {}", green("✓"), name, old, chosen);
    }
    Ok(code)
}

/// one cancelled picker (None) aborts the whole batch: the fold yields none
/// unless every picker picked (pure, tested)
fn fold_picks(picks: Vec<Option<String>>) -> Option<Vec<String>> {
    if picks.iter().any(|p| p.is_none()) {
        return None;
    }
    Some(picks.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_cancelled_picker_aborts_the_batch() {
        assert_eq!(fold_picks(vec![]), Some(vec![]));
        assert_eq!(fold_picks(vec![Some("1.0.0".into())]), Some(vec!["1.0.0".into()]));
        // a cancel at any position discards every buffered pick
        assert_eq!(fold_picks(vec![Some("1.0.0".into()), None]), None);
        assert_eq!(fold_picks(vec![None, Some("1.0.0".into())]), None);
    }
}
