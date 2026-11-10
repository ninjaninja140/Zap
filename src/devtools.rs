use anyhow::{anyhow, bail, Context, Result};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::config::Config;
use crate::model::Tree;
use crate::util::{cyan, dim, green, red, yellow};
use crate::workspace::install_root_for;

fn load_tree() -> Result<(std::path::PathBuf, Tree)> {
    let ctx = crate::util::find_root()?;
    let root = install_root_for(&ctx);
    let lock = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow!("no .zap-lock.json, run `zap install` first"))?;
    Ok((root, Tree::from(&lock)))
}

fn blocking_client(secs: u64) -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(secs))
        .build()?)
}

// info / view

/// `zap view|info <pkg> [field]`, registry metadata, cached like installs
pub fn info_cmd(args: &[String]) -> Result<i32> {
    let (name, field) = {
        let positional: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
        match positional.len() {
            0 => bail!("usage: zap view <package> [field]"),
            1 => (positional[0].clone(), None),
            _ => (positional[0].clone(), Some(positional[1].clone())),
        }
    };
    let dir = std::env::current_dir()?;
    let cfg = Config::load(&dir);
    let reg = crate::http::Registry::new(
        false,
        false,
        cfg.prefer_offline,
        cfg.cache_ttl_hours,
        cfg.registry.as_deref(),
    )?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let pack = rt.block_on(reg.packument(&name))?;

    if let Some(field) = field {
        let value = match field.as_str() {
            "dist-tags" => serde_json::to_value(&pack.dist_tags)?,
            "versions" => serde_json::json!(pack.versions.keys().collect::<Vec<_>>()),
            _ => {
                let latest = pack
                    .dist_tags
                    .latest
                    .clone()
                    .and_then(|l| pack.versions.get(&l))
                    .ok_or_else(|| anyhow!("no latest version for `{}`", name))?;
                // fields of the latest version manifest
                let raw: serde_json::Value = serde_json::to_value(latest)?;
                raw.get(&field).cloned().unwrap_or(serde_json::Value::Null)
            }
        };
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(0);
    }

    let latest = pack
        .dist_tags
        .latest
        .clone()
        .and_then(|l| pack.versions.get(&l))
        .ok_or_else(|| anyhow!("no latest version for `{}`", name))?;
    println!("{}@{}", green(&name), latest.version);
    if let Some(d) = latest.extra.get("description").and_then(|v| v.as_str()) {
        println!("  {}", dim(d));
    }
    println!("  versions: {} total, latest {}", pack.versions.len(), pack.dist_tags.latest.as_deref().unwrap_or("?"));
    if !pack.dist_tags.other.is_empty() {
        let tags: Vec<String> = pack
            .dist_tags
            .other
            .iter()
            .map(|(k, v)| format!("{}={}", k, value_str(v)))
            .collect();
        println!("  dist-tags: {}", tags.join(", "));
    }
    if !latest.dependencies.is_empty() {
        println!("  dependencies:");
        for (k, v) in &latest.dependencies {
            println!("    {} {}", k, dim(v));
        }
    }
    if let Some(lic) = latest.extra.get("license").and_then(|v| v.as_str()) {
        println!("  license: {}", lic);
    }
    if let Some(home) = latest.extra.get("homepage").and_then(|v| v.as_str()) {
        println!("  home: {}", cyan(home));
    }
    Ok(0)
}

fn value_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// licenses

/// `zap licenses`, license summary of everything installed (yarn v1 classic)
pub fn licenses_cmd(_args: &[String]) -> Result<i32> {
    let (root, tree) = load_tree()?;
    let vroot = root.join(".zap").join("v");
    let mut by_license: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut missing = 0usize;
    for id in tree.packages.keys() {
        let pkg_json = vroot.join(crate::util::pkg_dir_name(id)).join("package.json");
        let license = crate::util::read_json(&pkg_json)
            .ok()
            .and_then(|v| license_of(&v))
            .unwrap_or_else(|| "UNKNOWN".to_string());
        if license == "UNKNOWN" {
            missing += 1;
        }
        by_license.entry(license).or_default().push(id.to_string());
    }
    if by_license.is_empty() {
        println!("{}", dim("nothing installed"));
        return Ok(0);
    }
    for (license, mut pkgs) in by_license {
        pkgs.sort();
        println!("{} ({} package{})", cyan(&license), pkgs.len(), if pkgs.len() == 1 { "" } else { "s" });
        for p in &pkgs {
            println!("  {}", p);
        }
    }
    if missing > 0 {
        println!("{}", yellow(format!("{} package(s) declare no license", missing)));
    }
    Ok(0)
}

fn license_of(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.get("license").and_then(|x| x.as_str()) {
        return Some(s.to_string());
    }
    // legacy `licenses: [{type: "MIT"}]`
    if let Some(arr) = v.get("licenses").and_then(|x| x.as_array()) {
        let parts: Vec<String> = arr
            .iter()
            .filter_map(|l| l.get("type").and_then(|t| t.as_str()).map(|s| s.to_string()))
            .collect();
        if !parts.is_empty() {
            return Some(parts.join(", "));
        }
    }
    None
}

// audit

/// `zap audit`, bulk advisory check against the registry (npm 7+ endpoint)
pub fn audit_cmd(_args: &[String]) -> Result<i32> {
    let (root, tree) = load_tree()?;
    let cfg = Config::load(&root);
    let base = cfg
        .registry
        .clone()
        .or_else(|| std::env::var("ZAP_REGISTRY").ok())
        .unwrap_or_else(|| "https://registry.npmjs.org".to_string())
        .trim_end_matches('/')
        .to_string();

    // strip prerelease/build suffixes the advisory db may not match? npm sends exact ids
    let mut query = serde_json::Map::new();
    for id in tree.packages.keys() {
        if id.starts_with("ws:") {
            continue;
        }
        query.insert(id.clone(), serde_json::json!([]));
    }
    if query.is_empty() {
        println!("{}", dim("nothing installed"));
        return Ok(0);
    }

    let client = blocking_client(60)?;
    let resp = client
        .post(format!("{}/-/npm/v1/security/advisories/bulk", base))
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .body(serde_json::to_string(&serde_json::Value::Object(query))?)
        .send()?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        bail!("audit failed ({}): {}", status, text);
    }
    let advisories: BTreeMap<String, serde_json::Value> = serde_json::from_str(&text)
        .with_context(|| "could not parse audit response")?;

    let mut hits = 0usize;
    let mut by_severity: BTreeMap<&str, usize> = BTreeMap::new();
    let mut lines: Vec<String> = Vec::new();
    for (pkg, list) in &advisories {
        for a in list.as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
            hits += 1;
            let severity = a.get("severity").and_then(|s| s.as_str()).unwrap_or("unknown");
            *by_severity.entry(severity).or_default() += 1;
            let title = a.get("title").and_then(|t| t.as_str()).unwrap_or("");
            let url = a.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let colored = match severity {
                "critical" | "high" => red(severity),
                "moderate" => yellow(severity),
                _ => dim(severity),
            };
            lines.push(format!("  {} {}, {} {}", colored, pkg, title, dim(url)));
        }
    }
    if hits == 0 {
        println!("{} no known vulnerabilities in {} packages", green("ok"), tree.packages.len());
        return Ok(0);
    }
    let summary: Vec<String> = by_severity
        .iter()
        .map(|(s, n)| format!("{} {}", n, s))
        .collect();
    println!("{} vulnerabilities across {} advisories:", yellow("found"), hits);
    for l in &lines {
        println!("{}", l);
    }
    println!("{}", dim(summary.join(" · ")));
    Ok(1)
}

// doctor

/// `zap doctor`, sanity checks for a zap project
pub fn doctor_cmd(_args: &[String]) -> Result<i32> {
    let ctx = crate::util::find_root()?;
    let root = install_root_for(&ctx);
    let cfg = Config::load(&root);
    let mut failures = 0usize;

    let mut check = |ok: bool, ok_msg: &str, bad_msg: &str| {
        if ok {
            println!("  {} {}", green("ok"), ok_msg);
        } else {
            failures += 1;
            println!("  {} {}", red("fail"), bad_msg);
        }
    };

    // manifests vs lockfile
    let manifest = crate::util::read_json(&root.join("package.json"))?;
    let (deps, dev) = crate::cmds::read_deps(&root)?;
    let patterns = crate::workspace::patterns_from(&manifest);
    let members_raw = crate::workspace::discover(&root, &patterns)?;
    let hash = crate::lockfile::root_hash(&deps, &dev, &members_raw.iter().map(|m| (m.name.clone(), &m.deps, &m.dev)).collect::<Vec<_>>());
    let lock = crate::lockfile::read(&root.join(".zap-lock.json"))?;
    match &lock {
        Some(l) if l.root_hash == hash => check(true, "lockfile is in sync with manifests", ""),
        Some(_) => check(false, "", "lockfile is out of sync with manifests, run `zap install`"),
        None => check(false, "", "no .zap-lock.json, run `zap install`"),
    }

    // runtime present for the configured linker
    match cfg.linker {
        crate::config::Linker::Pnp => {
            check(
                root.join(".pnp.cjs").is_file(),
                "pnp runtime present",
                "pnp runtime missing, run `zap install`",
            );
        }
        crate::config::Linker::NodeModules => {
            check(
                root.join("node_modules").is_dir(),
                "node_modules present",
                "node_modules missing, run `zap install`",
            );
        }
    }

    // node_modules junctions still valid
    let nm_paths = root.join(".zap").join("nm-paths.json");
    if let Ok(text) = std::fs::read_to_string(&nm_paths) {
        if let Ok(links) = serde_json::from_str::<Vec<String>>(&text) {
            let broken = links
                .iter()
                .filter(|l| {
                    let p = std::path::PathBuf::from(l);
                    std::fs::read_link(&p).is_ok() && !p.exists()
                })
                .count();
            if broken == 0 {
                println!("  {} {} junctions intact", green("ok"), links.len());
            } else {
                failures += 1;
                println!("  {} {} broken junctions, run `zap install`", red("fail"), broken);
            }
        }
    }

    // typescript story
    let ts_on = root.join("tsconfig.json").is_file()
        || members_raw.iter().any(|m| root.join(&m.dir_rel).join("tsconfig.json").is_file())
        || cfg.typescript.is_some();
    if ts_on {
        if let Some(lock) = &lock {
            let tree = Tree::from(lock);
            let user_ts = deps.contains_key("typescript")
                || dev.contains_key("typescript")
                || members_raw
                    .iter()
                    .any(|m| m.deps.contains_key("typescript") || m.dev.contains_key("typescript"));
            match (crate::cmds::ts_package_id(&tree), user_ts) {
                (Some(id), true) => {
                    if tree.types.contains_key("typescript") {
                        failures += 1;
                        println!("  {} both zap-managed and user typescript present, remove one", red("fail"));
                    } else {
                        println!("  {} typescript {} (user-managed)", green("ok"), id);
                    }
                }
                (Some(id), false) => println!("  {} typescript {} (zap-managed)", green("ok"), id),
                (None, _) => {
                    failures += 1;
                    println!("  {} tsconfig detected but typescript not installed, run `zap install`", red("fail"));
                }
            }
        }
    }

    // toolchain
    let node_ver = std::process::Command::new("node")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());
    match node_ver {
        Some(v) => println!("  {} node {}", green("ok"), v),
        None => {
            failures += 1;
            println!("  {} node not found on PATH", red("fail"));
        }
    }

    // store
    let store = match &cfg.store_dir {
        Some(d) => d.clone(),
        None => crate::util::store_dir()?,
    };
    let (files, bytes) = store_stats(&store);
    println!("  {} store: {} files, {} ({})", green("ok"), files, crate::util::human(bytes), store.display());

    if failures > 0 {
        println!("{}", red(format!("{} problem(s) found", failures)));
        Ok(1)
    } else {
        println!("{}", green("all checks passed"));
        Ok(0)
    }
}

fn store_stats(dir: &std::path::Path) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(md) = e.metadata() {
                    files += 1;
                    bytes += md.len();
                }
            }
        }
    }
    (files, bytes)
}
