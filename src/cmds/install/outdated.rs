// `zap outdated`, show deps that are behind the registry (exit 1 when any);
// --interactive (-i) opens a raw-mode picker and upgrades the chosen packages

use anyhow::{Context, Result};
use std::path::Path;

use crate::config::Config;
use crate::util::{dim, green, red, yellow};
use crate::workspace::install_root_for;

#[path = "_sel.rs"]
mod sel;

// re-exports so upgrade.rs can share the picker machinery without including
// _sel.rs twice (which would double-compile the module and fork its types)
pub use sel::{console_available, pick_version, sort_versions};

pub const NAME: &str = "outdated";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "show deps behind the registry (exit 1 if any); -i picks targets (keep / latest-of-major / latest)";
pub const ABOUT: &str = "shows every locked dep that's behind the registry with its severity
(outdated / major behind) and exits 1 when anything is stale, so it drops
straight into CI. -i opens a raw-mode selector: each dep gets a three-way
target, keep the current version, bump to the latest of its major, or jump
to the absolute latest.";
pub const EXAMPLE: &str = "  $ zap outdated -i
  pick per-dep targets (current / latest-of-major / latest) and apply";

// one checked dep: identity + what the registry said
#[derive(Clone)]
struct Row {
    source: String,
    name: String,
    current: String,
    wanted: String,
    latest_major: String,
    latest: String,
}

pub fn run(args: &[String]) -> Result<i32> {
    let ui = args.iter().any(|a| a == "--interactive" || a == "-i");
    let ctx = crate::util::find_root()?;
    let root = install_root_for(&ctx);
    let cfg = Config::load(&root);
    if ui {
        // the interactive flow is shared with `zap upgrade --interactive`
        // (no names); it re-reads the lockfile itself. the heading block is
        // built from the canonical name + the args as typed (run() only
        // sees the args after the command name)
        let mut full = vec![NAME.to_string()];
        full.extend(args.iter().cloned());
        return interactive_upgrade(&root, &cfg, &crate::heading_block(NAME, &full.join(" ")));
    }
    let lock = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow::anyhow!("no .zap-lock.json, run `zap install` first"))?;
    let rows = collect(&root, &lock, &cfg)?;
    if rows.is_empty() {
        println!("{}", dim("no dependencies to check"));
        return Ok(0);
    }

    let width = rows.iter().map(|r| r.name.len()).max().unwrap_or(6).max("package".len());
    println!("  {:<w$}  {:<10} {:<10} {:<10} {:<10} {}", "package", "current", "wanted", "major", "latest", "source", w = width);
    let mut any_outdated = false;
    for r in &rows {
        let status = if r.latest.is_empty() {
            dim("unknown")
        } else if r.current == r.latest {
            green("up to date")
        } else {
            any_outdated = true;
            let major_behind = match (semver::Version::parse(&r.current), semver::Version::parse(&r.latest)) {
                (Ok(c), Ok(l)) => c.major < l.major,
                _ => false,
            };
            if major_behind { red("major behind") } else { yellow("outdated") }
        };
        println!(
            "  {:<w$}  {:<10} {:<10} {:<10} {:<10} {} {}",
            r.name,
            r.current,
            r.wanted,
            r.latest_major,
            r.latest,
            dim(if r.source.is_empty() { "root" } else { &r.source }),
            status,
            w = width
        );
    }
    Ok(if any_outdated { 1 } else { 0 })
}

/// the three-target all-packages flow, shared by `zap outdated -i` and
/// `zap upgrade --interactive` (no names). it lives in this module because
/// _sel.rs is #[path]-included HERE; including it from upgrade.rs too would
/// double-compile the module and fork its types
pub fn interactive_upgrade(root: &Path, cfg: &Config, heading: &str) -> Result<i32> {
    let lock = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow::anyhow!("no .zap-lock.json, run `zap install` first"))?;
    let rows = collect(root, &lock, cfg)?;
    if rows.is_empty() {
        println!("{}", dim("no dependencies to check"));
        return Ok(0);
    }
    interactive(root, cfg, &rows, heading)
}

/// the --interactive flow: pick a target per outdated root dep in the
/// selector, then bump them through the normal paths
fn interactive(root: &Path, cfg: &Config, rows: &[Row], heading: &str) -> Result<i32> {
    let is_outdated = |r: &Row| !r.latest.is_empty() && r.latest != "?" && r.latest != r.current;
    // only root-manifest deps can be bumped from here: workspace links are
    // upgraded in their member project, so they stay out of the list
    let upgradable: Vec<Row> = rows
        .iter()
        .filter(|r| is_outdated(r) && (r.source.is_empty() || r.source == "dev"))
        .cloned()
        .collect();
    let ws_skipped = rows.iter().filter(|r| is_outdated(r) && !(r.source.is_empty() || r.source == "dev")).count();
    if upgradable.is_empty() {
        if ws_skipped > 0 {
            println!(
                "{}",
                dim(format!("{} workspace dep(s) outdated, upgrade them in their member project", ws_skipped))
            );
        } else {
            println!("{}", dim("all dependencies are up to date"));
        }
        return Ok(0);
    }
    if ws_skipped > 0 {
        println!(
            "{}",
            dim(format!("{} workspace dep(s) not shown (upgrade them in their member project)", ws_skipped))
        );
    }

    let targets = match sel::select(&upgradable, heading)? {
        Some(t) => t,
        None => return Ok(0), // q / esc / ctrl+c / enter with everything kept
    };

    // group the picks: latest-of-major is written straight into the manifest
    // (constrained range, one shared install pass), absolute-latest goes
    // through the normal upgrade path exactly as before
    let mut major: Vec<&Row> = Vec::new();
    let mut prod: Vec<String> = Vec::new();
    let mut dev: Vec<String> = Vec::new();
    for (i, r) in upgradable.iter().enumerate() {
        match targets[i] {
            sel::Target::Latest => {
                if r.source == "dev" {
                    dev.push(r.name.clone());
                } else {
                    prod.push(r.name.clone());
                }
            }
            // a major pick that wouldn't change anything (no stable release
            // shares the current major, or it IS the current version) counts
            // as keep
            sel::Target::Major if !r.latest_major.is_empty() && r.latest_major != r.current => major.push(r),
            _ => {}
        }
    }

    // major picks: record `^<latest-of-major>` in the manifest the same way
    // `zap add` records specs (respecting save-exact), then re-resolve via
    // the shared install entry, no resolution logic forked here
    if !major.is_empty() {
        let picks: Vec<(String, String, bool)> = major
            .iter()
            .map(|r| (r.name.clone(), r.latest_major.clone(), r.source == "dev"))
            .collect();
        record_ranges(root, cfg, &picks)?;
    }

    // add --dev is a global flag, so split by manifest section and run one
    // upgrade per section (a dev dep bumped plain would land in dependencies
    // and keep its stale devDependencies entry)
    let mut code = 0;
    if !prod.is_empty() {
        println!("{}", dim(format!("upgrading {} package(s): {}", prod.len(), prod.join(", "))));
        let args: Vec<String> = prod.iter().map(|n| format!("{}@latest", n)).collect();
        code = super::upgrade::run(&args)?;
    }
    if !dev.is_empty() && code == 0 {
        println!("{}", dim(format!("upgrading {} dev package(s): {}", dev.len(), dev.join(", "))));
        let mut args = vec!["--dev".to_string()];
        args.extend(dev.iter().map(|n| format!("{}@latest", n)));
        code = super::upgrade::run(&args)?;
    }
    // major-only picks: the upgrade calls above already installed everything
    // when latest picks exist; otherwise one shared install pass here
    if !major.is_empty() && prod.is_empty() && dev.is_empty() && code == 0 {
        code = crate::cmds::install_in(root, &[], None)?;
    }

    // per-package summary, applied or kept
    for (i, r) in upgradable.iter().enumerate() {
        match targets[i] {
            sel::Target::Latest => println!("{} {} {} → {}", green("✓"), r.name, r.current, r.latest),
            sel::Target::Major if !r.latest_major.is_empty() && r.latest_major != r.current => {
                println!("{} {} {} → {}", green("✓"), r.name, r.current, r.latest_major)
            }
            _ => println!("{} {}: kept", dim("-"), dim(&r.name)),
        }
    }
    Ok(code)
}

/// every locked dep (root manifest + workspace members) with wanted/latest
/// filled in from the registry; failed lookups become ("?", "?") with a
/// warning on stderr, the same rows the table has always shown
fn collect(root: &Path, lock: &crate::model::Lockfile, cfg: &Config) -> Result<Vec<Row>> {
    let rows = scan(root, lock)?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let reg = crate::http::Registry::new(
        false,
        false,
        cfg.prefer_offline,
        cfg.cache_ttl_hours,
        cfg.registry.as_deref(),
    )?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let results: Vec<Result<(usize, String, String, String)>> = rt.block_on(async {
        let mut set = tokio::task::JoinSet::new();
        for (i, (_, name, current, range)) in rows.iter().enumerate() {
            let reg = reg.clone();
            let name = name.clone();
            let current = current.clone();
            let range = range.clone();
            set.spawn(async move {
                let pack = reg.packument(&name).await?;
                let latest = pack.dist_tags.latest.clone().unwrap_or_default();
                let wanted = crate::util::pick_version(&name, &pack, &range).unwrap_or_default();
                let major = latest_major_from(&current, &pack);
                Ok((i, wanted, major, latest))
            });
        }
        let mut out = Vec::new();
        while let Some(j) = set.join_next().await {
            out.push(j.context("metadata task panicked")?);
        }
        Ok::<Vec<Result<(usize, String, String, String)>>, anyhow::Error>(out)
    })?;

    let mut filled: Vec<Option<(String, String, String)>> = vec![None; rows.len()];
    for r in results {
        match r {
            Ok((i, wanted, major, latest)) => filled[i] = Some((wanted, major, latest)),
            Err(e) => eprintln!("{} {}", yellow("warning:"), e),
        }
    }
    Ok(rows
        .into_iter()
        .enumerate()
        .map(|(i, (source, name, current, _range))| {
            let (wanted, major, latest) = filled[i]
                .clone()
                .unwrap_or_else(|| ("?".to_string(), "?".to_string(), "?".to_string()));
            Row { source, name, current, wanted, latest_major: major, latest }
        })
        .collect())
}

/// the newest stable registry version sharing `current`'s major; empty when
/// the current version isn't parseable or no stable release shares the major.
/// reads the same packument the wanted/latest pass already fetched
fn latest_major_from(current: &str, pack: &crate::model::Packument) -> String {
    let Ok(cur) = semver::Version::parse(current) else {
        return String::new();
    };
    pack.versions
        .keys()
        .filter_map(|v| semver::Version::parse(v).ok())
        .filter(|v| v.major == cur.major && v.pre.is_empty())
        .max()
        .map(|v| v.to_string())
        .unwrap_or_default()
}

/// record `^<version>` (or the exact version under save-exact) for every
/// (name, version, dev) pick in its manifest section, the way `zap add`
/// records specs; one read-modify-write for the whole batch. shared by the
/// major-bump path here and the version picker in upgrade.rs
pub(crate) fn record_ranges(root: &Path, cfg: &Config, picks: &[(String, String, bool)]) -> Result<()> {
    if picks.is_empty() {
        return Ok(());
    }
    let mut v = crate::util::read_json(&root.join("package.json"))?;
    let obj = v.as_object_mut().ok_or_else(|| anyhow::anyhow!("package.json root must be an object"))?;
    for (name, version, dev) in picks {
        let key = if *dev { "devDependencies" } else { "dependencies" };
        let deps = obj.entry(key.to_string()).or_insert_with(|| serde_json::json!({}));
        let dmap = deps.as_object_mut().ok_or_else(|| anyhow::anyhow!("{} must be an object", key))?;
        let range = if cfg.save_exact { version.clone() } else { format!("^{}", version) };
        dmap.insert(name.clone(), serde_json::json!(range));
    }
    std::fs::write(root.join("package.json"), format!("{}\n", serde_json::to_string_pretty(&v)?))?;
    Ok(())
}

/// the dep rows before the registry pass: (source, name, current version,
/// requested range), root manifest first, then workspace members
fn scan(root: &Path, lock: &crate::model::Lockfile) -> Result<Vec<(String, String, String, String)>> {
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
    for m in crate::workspace::discover(root, &patterns)? {
        for (n, r) in m.deps.iter().chain(m.dev.iter()) {
            if let Some(id) = lock
                .workspaces
                .iter()
                .find(|lm| lm.name == m.name)
                .and_then(|lm| lm.deps.get(n))
            {
                if id.strip_prefix("ws:").is_some() {
                    continue; // workspace link, always "latest"
                }
                let cur = id.rsplit_once('@').map(|x| x.1.to_string()).unwrap_or_default();
                rows.push((m.name.clone(), n.clone(), cur, r.clone()));
            }
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_collects_root_dev_and_workspace_rows() {        let dir = std::env::temp_dir().join(format!("zap-outdated-scan-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("packages/w")).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"root","workspaces":["packages/*"],"dependencies":{"ms":"^2.0.0"},"devDependencies":{"left-pad":"^1.0.0"}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("packages/w/package.json"), r#"{"name":"w","dependencies":{"ms":"^2.0.0"}}"#).unwrap();
        std::fs::write(
            dir.join(".zap-lock.json"),
            r#"{"version":2,"rootHash":"x","root":{"ms":"ms@2.1.3","left-pad":"left-pad@1.3.0"},"packages":{},"workspaces":[{"name":"w","dir":"packages/w","deps":{"ms":"ms@2.1.3"}}]}"#,
        )
        .unwrap();
        let lock = crate::lockfile::read(&dir.join(".zap-lock.json")).unwrap().unwrap();
        let rows = scan(&dir, &lock).unwrap();
        assert_eq!(
            rows[0],
            ("".to_string(), "ms".to_string(), "2.1.3".to_string(), "^2.0.0".to_string())
        );
        assert_eq!(rows[1].0, "dev");
        assert_eq!(rows[1].1, "left-pad");
        assert_eq!(
            rows[2],
            ("w".to_string(), "ms".to_string(), "2.1.3".to_string(), "^2.0.0".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn packument(versions: &[&str]) -> crate::model::Packument {
        let versions: serde_json::Map<String, serde_json::Value> = versions
            .iter()
            .map(|v| (v.to_string(), serde_json::json!({ "version": v })))
            .collect();
        serde_json::from_value(serde_json::json!({ "versions": versions, "dist-tags": {} })).unwrap()
    }

    #[test]
    fn latest_major_picks_the_newest_stable_in_the_current_major() {
        let pack = packument(&["4.0.0", "4.1.2", "4.2.0-beta.1", "5.1.0", "6.0.1"]);
        assert_eq!(latest_major_from("4.0.3", &pack), "4.1.2");
        assert_eq!(latest_major_from("4.1.2", &pack), "4.1.2"); // already at it
        assert_eq!(latest_major_from("6.0.0", &pack), "6.0.1");
        // prerelease-only major: no stable release shares it
        assert_eq!(latest_major_from("9.0.0", &pack), "");
        // unparseable current (dist-tag pins, weird ranges)
        assert_eq!(latest_major_from("next", &pack), "");
        // same version listed twice collapses in the map; max() stays total
        let dup = packument(&["2.0.0", "2.0.0", "2.1.0"]);
        assert_eq!(latest_major_from("2.0.0", &dup), "2.1.0");
    }
}
