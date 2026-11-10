use anyhow::{anyhow, bail, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::util::{dim, green, yellow};

/// detected package managers + the version pins extracted from their lockfiles
/// (name -> exact version)
#[derive(Default)]
pub struct Migration {
    pub managers: Vec<String>,
    pub pins: BTreeMap<String, String>,
    pub artifacts: Vec<PathBuf>,
    pub lockfile_version: Option<u64>,
}

impl Migration {
    pub fn summary(&self) -> String {
        let mut s = self.managers.join(" + ");
        if !s.is_empty() {
            s.push_str(&format!(", {} version pins", self.pins.len()));
        }
        if let Some(v) = self.lockfile_version {
            s.push_str(&format!(" (lockfile v{})", v));
        }
        s
    }
}

/// scan the project for another package manager's lockfiles and extract pins.
/// priority: pnpm, yarn, bun, npm, deno, the first manager that pins a package wins.
pub fn detect(root: &Path) -> Result<Option<Migration>> {
    let mut m = Migration::default();
    let mut managers: Vec<String> = Vec::new();
    let mut artifacts: Vec<PathBuf> = Vec::new();

    let pnpm_lock = root.join("pnpm-lock.yaml");
    if pnpm_lock.is_file() {
        pins_from_pnpm_lock(&pnpm_lock, &mut m.pins)?;
        managers.push("pnpm".to_string());
        artifacts.push(pnpm_lock);
    }

    let yarn_lock = root.join("yarn.lock");
    if yarn_lock.is_file() {
        let text = std::fs::read_to_string(&yarn_lock)?;
        let berry = text.contains("__metadata:");
        pins_from_yarn_lock(&text, &mut m.pins)?;
        managers.push(if berry { "yarn (berry)".to_string() } else { "yarn (classic)".to_string() });
        artifacts.push(yarn_lock);
    }

    let bun_lock = root.join("bun.lock");
    if bun_lock.is_file() {
        pins_from_bun_lock(&bun_lock, &mut m.pins)?;
        managers.push("bun".to_string());
        artifacts.push(bun_lock);
    } else {
        let lockb = root.join("bun.lockb");
        if lockb.is_file() {
            managers.push("bun (binary lock, resolving fresh)".to_string());
            artifacts.push(lockb);
        }
    }

    let npm_lock = root.join("package-lock.json");
    if npm_lock.is_file() {
        pins_from_package_lock(&npm_lock, &mut m.pins)?;
        managers.push("npm".to_string());
        artifacts.push(npm_lock);
    }

    let deno_lock = root.join("deno.lock");
    if deno_lock.is_file() {
        let v = parse_json(&deno_lock)?;
        m.lockfile_version = v.get("version").and_then(|x| x.as_u64());
        pins_from_deno_lock(&v, &mut m.pins);
        managers.push("deno".to_string());
        artifacts.push(deno_lock);
    }

    m.managers = managers;
    m.artifacts = artifacts;
    m.artifacts.push(root.join("node_modules"));
    m.artifacts.push(root.join(".yarn"));
    m.artifacts.push(root.join(".pnp.cjs"));
    m.artifacts.push(root.join(".pnp.mjs"));
    m.artifacts.push(root.join(".pnp-resolve.mjs"));

    if m.managers.is_empty() {
        return Ok(None);
    }
    Ok(Some(m))
}

/// pin a name -> exact version, only when the version looks like real semver
fn pin(out: &mut BTreeMap<String, String>, name: &str, ver: &str) {
    if name.is_empty() || ver.is_empty() {
        return;
    }
    if semver::Version::parse(ver).is_err() {
        return;
    }
    out.entry(name.to_string()).or_insert_with(|| ver.to_string());
}

/// "name@range" -> pin(name, ver); handles scoped names and npm: aliases
fn pin_from_spec(out: &mut BTreeMap<String, String>, spec: &str, ver: &str) {
    let ver = ver.strip_prefix("npm:").unwrap_or(ver);
    let Some((name, _)) = spec.rsplit_once('@') else { return };
    let name = name.split("@npm:").next().unwrap_or(name);
    pin(out, name, ver);
}

// parsers

fn pins_from_package_lock(path: &Path, out: &mut BTreeMap<String, String>) -> Result<()> {
    let v: serde_json::Value = parse_json(path)?;
    if let Some(packages) = v.get("packages").and_then(|p| p.as_object()) {
        for (key, entry) in packages {
            let Some(ver) = entry.get("version").and_then(|x| x.as_str()) else { continue };
            let mut rel = key.as_str();
            while let Some((_, tail)) = rel.split_once("node_modules/") {
                rel = tail;
            }
            let name = pkg_name_from_rel(rel);
            pin(out, &name, ver);
        }
    } else if let Some(deps) = v.get("dependencies").and_then(|d| d.as_object()) {
        for (name, entry) in deps {
            if let Some(ver) = entry.get("version").and_then(|x| x.as_str()) {
                pin(out, name, ver);
            }
        }
    }
    Ok(())
}

fn pkg_name_from_rel(rel: &str) -> String {
    let segs: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    if segs.first().map(|s| s.starts_with('@')).unwrap_or(false) && segs.len() >= 2 {
        segs[..2].join("/")
    } else {
        segs.last().map(|s| s.to_string()).unwrap_or_default()
    }
}

fn pins_from_yarn_lock(text: &str, out: &mut BTreeMap<String, String>) -> Result<()> {
    let mut current: Vec<String> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') && t.ends_with(':') {
            let key = t.trim_end_matches(':').trim_matches('"');
            current = key.split(", ").map(|s| s.trim().trim_matches('"').to_string()).collect();
            continue;
        }
        if let Some(v) = t.strip_prefix("version ") {
            let ver = v.trim().trim_matches('"');
            for spec in &current {
                pin_from_spec(out, spec, ver);
            }
        }
        if let Some(r) = t.strip_prefix("resolution ") {
            let res = r.trim().trim_matches('"').trim_start_matches(": ");
            if let Some((_, ver)) = res.rsplit_once('@') {
                for spec in &current {
                    pin_from_spec(out, spec, ver);
                }
            }
        }
    }
    Ok(())
}

fn pins_from_pnpm_lock(path: &Path, out: &mut BTreeMap<String, String>) -> Result<()> {
    let text = std::fs::read_to_string(path)?;
    for line in text.lines() {
        let Some(stripped) = line.trim_start().strip_prefix('/') else { continue };
        let key = stripped.trim_end_matches(':').trim_matches('"');
        let key = key.split('(').next().unwrap_or(key);
        let Some((name, ver)) = key.rsplit_once('@') else { continue };
        let name = name.replace('+', "/");
        pin(out, &name, ver);
    }
    Ok(())
}

fn pins_from_bun_lock(path: &Path, out: &mut BTreeMap<String, String>) -> Result<()> {
    let text = std::fs::read_to_string(path)?;
    let clean = crate::config::strip_jsonc(&text);
    let v: serde_json::Value = serde_json::from_str(&clean)
        .map_err(|e| anyhow!("parsing {}: {}", path.display(), e))?;
    if let Some(packages) = v.get("packages").and_then(|p| p.as_object()) {
        for (key, _) in packages {
            let Some((name, ver)) = key.rsplit_once('@') else { continue };
            pin(out, name, ver);
        }
    }
    Ok(())
}

fn pins_from_deno_lock(v: &serde_json::Value, out: &mut BTreeMap<String, String>) {
    if let Some(specs) = v.get("npm").and_then(|n| n.get("specifiers")).and_then(|s| s.as_object()) {
        for (_, resolved) in specs {
            // resolved is "name@version", split before pinning
            if let Some(spec) = resolved.as_str() {
                if let Some((_, ver)) = spec.rsplit_once('@') {
                    pin_from_spec(out, spec, ver);
                }
            }
        }
    }
}

fn parse_json(path: &Path) -> Result<serde_json::Value> {
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| anyhow!("parsing {}: {}", path.display(), e))
}

// deno synthesis

/// deno.json-only projects: pull npm: imports into a package.json so zap can install.
/// handles deno.json and deno.jsonc, and maps the deno 2 `workspace` field to
/// package.json workspaces. uses `.zaprc.yaml` for zap config.
fn synthesize_from_deno(root: &Path) -> Result<()> {
    let deno_json = root.join("deno.json");
    let deno_jsonc = root.join("deno.jsonc");
    let config: serde_json::Value = if deno_json.is_file() {
        parse_json(&deno_json)?
    } else if deno_jsonc.is_file() {
        let clean = crate::config::strip_jsonc(&std::fs::read_to_string(&deno_jsonc)?);
        serde_json::from_str(&clean).map_err(|e| anyhow!("parsing {}: {}", deno_jsonc.display(), e))?
    } else {
        return Ok(());
    };

    let imports = config
        .get("imports")
        .and_then(|x| x.as_object())
        .ok_or_else(|| anyhow!("deno config has no imports, nothing to migrate"))?;
    let mut deps = serde_json::Map::new();
    for (key, val) in imports {
        let Some(spec) = val.as_str().and_then(|s| s.strip_prefix("npm:")) else {
            continue;
        };
        let range = spec.rsplit_once('@').map(|(_, r)| r.to_string()).unwrap_or_else(|| spec.to_string());
        deps.insert(key.clone(), serde_json::json!(range));
    }
    if deps.is_empty() {
        return Ok(());
    }

    let mut pkg = serde_json::json!({
        "name": root.file_name().map(|s| s.to_string_lossy()).unwrap_or_default(),
        "version": "1.0.0",
        "private": true,
        "dependencies": deps,
    });

    // deno 2 workspace field -> package.json workspaces
    if let Some(ws) = config.get("workspace").and_then(|x| x.as_array()) {
        let ws_paths: Vec<String> = ws
            .iter()
            .filter_map(|w| w.as_str().map(|s| s.to_string()))
            .collect();
        if !ws_paths.is_empty() {
            if let Some(obj) = pkg.as_object_mut() {
                obj.insert("workspaces".into(), serde_json::json!(ws_paths));
            }
        }
    }

    std::fs::write(root.join("package.json"), format!("{}\n", serde_json::to_string_pretty(&pkg)?))?;
    println!(
        "{} synthesized package.json with {} npm dependencies from {}",
        green("+"),
        deps.len(),
        root.join("deno.json").display()
    );
    Ok(())
}

fn touch_gitignore(root: &Path) -> Result<()> {
    let gi = root.join(".gitignore");
    if !gi.is_file() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&gi)?;
    let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    for want in [".zap/", "node_modules/"] {
        if !lines.iter().any(|l| l.trim() == want) {
            lines.push(want.to_string());
        }
    }
    std::fs::write(&gi, format!("{}\n", lines.join("\n")))?;
    Ok(())
}

// command

/// `zap migrate [--dry-run] [--yes]`, convert an npm/yarn/pnpm/bun/deno project to zap, reusing its locked versions so nothing upgrades behind your back
pub fn migrate_cmd(args: &[String]) -> Result<i32> {
    let dry = args.iter().any(|a| a == "--dry-run");
    let yes = args.iter().any(|a| a == "--yes" || a == "-y");
    let ctx = crate::util::find_root().unwrap_or_else(|_| std::env::current_dir().unwrap());
    let root = crate::workspace::install_root_for(&ctx);

    if root.join(".zap-lock.json").is_file() {
        bail!("this is already a zap project (.zap-lock.json exists)");
    }

    let Some(mig) = detect(&root)? else {
        println!("{} no other package manager's lockfile found (looked for npm, yarn, pnpm, bun, deno)", yellow("nothing:"));
        return Ok(0);
    };
    println!("{} {}", green("detected"), mig.summary());

    // deno-only project: synthesize a package.json from deno.json/deno.jsonc imports
    if !root.join("package.json").is_file() {
        synthesize_from_deno(&root)?;
    }

    if mig.pins.is_empty() {
        println!("{}", yellow("no version pins available, installing fresh from package.json"));
    } else {
        println!("  {} version pins from the old lockfile, nothing will upgrade", mig.pins.len());
    }

    if dry {
        let mut shown = 0;
        for (name, ver) in &mig.pins {
            if shown >= 15 {
                println!("  {}", dim(format!("... and {} more", mig.pins.len() - shown)));
                break;
            }
            println!("  {}@{}", name, ver);
            shown += 1;
        }
        println!("{}", yellow("dry run, nothing installed or removed"));
        return Ok(0);
    }

    crate::cmds::install_in(&root, &[], Some(&mig.pins))?;

    // cleanup: old lockfiles, node_modules, yarn/pnp state
    let existing: Vec<&PathBuf> = mig.artifacts.iter().filter(|p| p.exists()).collect();
    if !existing.is_empty() {
        println!("{} old artifacts:", yellow("found"));
        for p in &existing {
            println!("  {}", p.display());
        }
        let confirmed = yes || {
            print!("remove them? [y/N] ");
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
                false
            } else {
                matches!(line.trim(), "y" | "Y" | "yes")
            }
        };
        if confirmed {
            for p in &existing {
                let ok = if p.is_dir() {
                    std::fs::remove_dir_all(p)
                } else {
                    std::fs::remove_file(p)
                };
                if ok.is_err() {
                    println!("{} could not remove {}", yellow("warning:"), p.display());
                }
            }
            touch_gitignore(&root)?;
            println!("{} old artifacts removed", green("-"));
        } else {
            println!("{}", dim("kept old artifacts, delete them manually whenever"));
        }
    }
    println!("{} migrated to zap, verify with `zap exec node <entry>` or `zap tsc`", green("done"));
    Ok(0)
}