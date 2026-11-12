// `zap add <pkg>[@range]...`, resolve specs, record them, then install

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;

use crate::cmds::install_in;
use crate::config::Config;
use crate::util::green;
use crate::workspace::install_root_for;

pub const NAME: &str = "add";
pub const ALIASES: &[&str] = &["a"];
pub const USAGE: &str = "add dependencies (supports --dev, -g global; workspace deps auto-link)";
pub const ABOUT: &str = "adds deps to package.json and installs them in the same pass. specs are
`name`, `name@range` or `name@latest`; packages that are workspace members
link as `workspace:*`. --dev (or -D) records into devDependencies. -g
(--global) installs into ~/.zap/global with the bins shimmed into ~/.zap/bin,
no project needed.";
pub const EXAMPLE: &str = "  $ zap add chalk --dev
  add chalk to devDependencies and install it
  $ zap add -g typescript
  install typescript globally, tsc shimmed for PATH-wide use";

pub fn run(args: &[String]) -> Result<i32> {
    let is_dev = args.iter().any(|a| a == "--dev" || a == "-D");
    let global = args.iter().any(|a| a == "--global" || a == "-g");
    if global {
        // global installs run outside any project: find_root must not run
        return crate::cmds::global_add(args);
    }
    let specs: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .cloned()
        .collect();
    if specs.is_empty() {
        bail!("usage: zap add <package>[@range] ...");
    }
    let ctx = crate::util::find_root()?;
    let install_root = install_root_for(&ctx);
    let cfg = Config::load(&ctx);
    let reg = crate::http::Registry::new(
        false,
        false,
        cfg.prefer_offline,
        cfg.cache_ttl_hours,
        cfg.registry.as_deref(),
    )?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;

    let mut picks: Vec<(String, String)> = rt.block_on(async {
        let mut set = tokio::task::JoinSet::new();
        for spec in specs {
            let reg = reg.clone();
            set.spawn(async move {
                let (name, range) = crate::util::split_spec(&spec);
                let pack = reg.packument(&name).await?;
                let version =
                    match crate::util::pick_version(&name, &pack, if range.is_empty() { "*" } else { &range }) {
                        Ok(v) => v,
                        Err(_) => {
                            // maybe the range is a dist-tag like `next` or `canary`
                            let tag = pack.dist_tags.other.get(&range).and_then(|v| v.as_str());
                            match tag {
                                Some(t) if pack.versions.contains_key(t) => t.to_string(),
                                _ => return Err(anyhow!("no version of `{}` matches `{}`", name, range)),
                            }
                        }
                    };
                Ok((name, version))
            });
        }
        let mut out = Vec::new();
        while let Some(j) = set.join_next().await {
            out.push(j.context("metadata task panicked")??);
        }
        Ok::<Vec<(String, String)>, anyhow::Error>(out)
    })?;
    picks.sort();

    // deps on workspace members are recorded as "workspace:*" (npm-like style)
    let member_names: HashSet<String> = {
        let manifest = crate::util::read_json(&install_root.join("package.json"))?;
        let patterns = crate::workspace::patterns_from(&manifest);
        crate::workspace::discover(&install_root, &patterns)?
            .into_iter()
            .map(|m| m.name)
            .collect()
    };

    let pkg_path = ctx.join("package.json");
    let mut v = crate::util::read_json(&pkg_path)?;
    let obj = v.as_object_mut().ok_or_else(|| anyhow!("package.json root must be an object"))?;
    let key = if is_dev { "devDependencies" } else { "dependencies" };
    let deps = obj.entry(key.to_string()).or_insert_with(|| serde_json::json!({}));
    let dmap = deps.as_object_mut().ok_or_else(|| anyhow!("{} must be an object", key))?;
    for (name, ver) in &picks {
        let range = if member_names.contains(name) {
            "workspace:*".to_string()
        } else if cfg.save_exact {
            ver.clone()
        } else {
            format!("^{}", ver)
        };
        dmap.insert(name.clone(), serde_json::json!(range));
        println!("{} {}@{}", green("+"), name, range);
    }
    std::fs::write(&pkg_path, format!("{}\n", serde_json::to_string_pretty(&v)?))?;
    install_in(&install_root, &[], None)
}
