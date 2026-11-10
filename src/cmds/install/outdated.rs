// `zap outdated`, show deps that are behind the registry (exit 1 when any)

use anyhow::{Context, Result};

use crate::config::Config;
use crate::util::{dim, green, red, yellow};
use crate::workspace::install_root_for;

pub const NAME: &str = "outdated";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "show deps that are behind the registry (exit 1 if any)";

pub fn run(args: &[String]) -> Result<i32> {
    let _ = args;
    let ctx = crate::util::find_root()?;
    let root = install_root_for(&ctx);
    let lock = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow::anyhow!("no .zap-lock.json, run `zap install` first"))?;
    let cfg = Config::load(&root);

    // (source, name, current version, range)
    let mut rows: Vec<(String, String, String, String)> = Vec::new();
    let manifest = crate::util::read_json(&root.join("package.json"))?;
    for (key, label) in [("dependencies", ""), ("devDependencies", "dev")] {
        if let Some(m) = manifest.get(key).and_then(|x| x.as_object()) {
            for (n, r) in m {
                let range = r.as_str().unwrap_or("*").to_string();
                if let Some(id) = lock.root.get(n) {
                    let cur = id.rsplit_once('@').map(|x| x.1.to_string()).unwrap_or_default();
                    rows.push((label.to_string(), n.clone(), cur, range));
                }
            }
        }
    }
    let patterns = crate::workspace::patterns_from(&manifest);
    for m in crate::workspace::discover(&root, &patterns)? {
        for (n, r) in m.deps.iter().chain(m.dev.iter()) {
            if let Some(id) = lock
                .workspaces
                .iter()
                .find(|lm| lm.name == m.name)
                .and_then(|lm| lm.deps.get(n))
            {
            if let Some(_cur_id) = id.strip_prefix("ws:") {
                    continue; // workspace link, always "latest"
                }
                let cur = id.rsplit_once('@').map(|x| x.1.to_string()).unwrap_or_default();
                rows.push((m.name.clone(), n.clone(), cur, r.clone()));
            }
        }
    }
    if rows.is_empty() {
        println!("{}", dim("no dependencies to check"));
        return Ok(0);
    }

    let reg = crate::http::Registry::new(
        false,
        false,
        cfg.prefer_offline,
        cfg.cache_ttl_hours,
        cfg.registry.as_deref(),
    )?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let results: Vec<Result<(usize, String, String)>> = rt.block_on(async {
        let mut set = tokio::task::JoinSet::new();
        for (i, (_, name, _, range)) in rows.iter().enumerate() {
            let reg = reg.clone();
            let name = name.clone();
            let range = range.clone();
            set.spawn(async move {
                let pack = reg.packument(&name).await?;
                let latest = pack.dist_tags.latest.clone().unwrap_or_default();
                let wanted = crate::util::pick_version(&name, &pack, &range).unwrap_or_default();
                Ok((i, wanted, latest))
            });
        }
        let mut out = Vec::new();
        while let Some(j) = set.join_next().await {
            out.push(j.context("metadata task panicked")?);
        }
        Ok::<Vec<Result<(usize, String, String)>>, anyhow::Error>(out)
    })?;

    let width = rows.iter().map(|r| r.1.len()).max().unwrap_or(6).max(6);
    println!("  {:<w$}  {:<10} {:<10} {:<10} {}", "package", "current", "wanted", "latest", "source", w = width);
    let mut any_outdated = false;
    let mut filled: Vec<Option<(String, String)>> = vec![None; rows.len()];
    for r in results {
        match r {
            Ok((i, wanted, latest)) => filled[i] = Some((wanted, latest)),
            Err(e) => eprintln!("{} {}", yellow("warning:"), e),
        }
    }
    for (i, (source, name, current, range)) in rows.iter().enumerate() {
        let (wanted, latest) = match &filled[i] {
            Some(v) => v.clone(),
            None => ("?".to_string(), "?".to_string()),
        };
        let status = if latest.is_empty() {
            dim("unknown")
        } else if *current == latest {
            green("up to date")
        } else {
            any_outdated = true;
            let major_behind = match (semver::Version::parse(current), semver::Version::parse(&latest)) {
                (Ok(c), Ok(l)) => c.major < l.major,
                _ => false,
            };
            if major_behind { red("major behind") } else { yellow("outdated") }
        };
        println!(
            "  {:<w$}  {:<10} {:<10} {:<10} {} {}",
            name,
            current,
            wanted,
            latest,
            dim(if source.is_empty() { "root" } else { source }),
            status,
            w = width
        );
        let _ = range;
    }
    Ok(if any_outdated { 1 } else { 0 })
}
