// `zap allow`: review and approve packages whose lifecycle scripts may run
// (the allowlist is .zap-allow.json, gated at install by `lifecycle: true`;
// `--all` switches to trust-all via the lifecycle-allow-all config key)

use anyhow::{anyhow, bail, Result};
use std::path::Path;

pub const NAME: &str = "allow";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str =
    "allowlist packages whose lifecycle scripts may run (lifecycle: true); --all trusts everything, --remove --all reverts";

pub fn run(args: &[String]) -> Result<i32> {
    let root = crate::util::find_root()?;
    let cfg = crate::config::Config::load(&root);
    let has_all = args.iter().any(|a| a == "--all");
    let has_remove = args.iter().any(|a| a == "--remove");
    let pkgs: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--all" && a.as_str() != "--remove")
        .cloned()
        .collect();

    if has_all && has_remove {
        // trust-all off; package names make no sense here
        if !pkgs.is_empty() {
            bail!("package names don't combine with --all, use `zap allow --remove <pkg>` for entries");
        }
        return trust_all_off(&root);
    }
    if has_all {
        if !pkgs.is_empty() {
            bail!("package names don't combine with --all, list them without the flag");
        }
        return trust_all_on(&root);
    }
    if has_remove {
        if pkgs.is_empty() {
            bail!("usage: zap allow --remove <pkg>... (or --remove --all to turn allow-all off)");
        }
        let mut list = crate::allow::load(&root)?;
        return remove(&root, &mut list, &pkgs);
    }
    if pkgs.is_empty() {
        return list_mode(&root, &cfg);
    }
    let mut list = crate::allow::load(&root)?;
    add(&root, &mut list, &pkgs)
}

/// --all: trust every package — the rc key goes through the config system's
/// own project set path, the (now pointless) allowlist file is deleted
fn trust_all_on(root: &Path) -> Result<i32> {
    let (rc, removed) = crate::allow::trust_all(root)?;
    println!(
        "{} allow-all on: {} in {} — .zap-allow.json: {}",
        crate::util::green("✓"),
        crate::util::cyan("lifecycle-allow-all"),
        rc.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
        if removed { crate::util::yellow("deleted") } else { crate::util::dim("none present") }
    );
    Ok(0)
}

/// --remove --all: trust-all off; any allowlist file is left alone and
/// regains its meaning
fn trust_all_off(root: &Path) -> Result<i32> {
    let rc = crate::allow::untrust_all(root)?;
    println!(
        "{} allow-all off: {} = false in {} (an existing .zap-allow.json regains meaning)",
        crate::util::green("✓"),
        crate::util::cyan("lifecycle-allow-all"),
        rc.file_name().and_then(|s| s.to_str()).unwrap_or("?")
    );
    Ok(0)
}

/// no args: allow-all state first, then allowlisted packages and anything in
/// the current resolution that has lifecycle scripts but isn't allowlisted
fn list_mode(root: &Path, cfg: &crate::config::Config) -> Result<i32> {
    if cfg.lifecycle_allow_all {
        let rc = crate::config::find_rc_file(root)
            .map(|p| p.file_name().and_then(|s| s.to_str()).unwrap_or("rc file").to_string())
            .unwrap_or_else(|| "rc file".to_string());
        println!("{} (lifecycle-allow-all in {})", crate::util::green("allow all: on"), rc);
        if root.join(".zap-allow.json").is_file() {
            println!("  {} .zap-allow.json exists but is ignored while allow-all is on", crate::util::dim("note:"));
        }
        return Ok(0);
    }
    let list = crate::allow::load(root)?;
    println!("allowlisted:");
    if list.is_empty() {
        println!("  {}", crate::util::dim("none"));
    }
    for (id, hash) in &list {
        println!("  {} {}", id, crate::util::dim(hash_prefix(hash)));
    }
    let mut pending: Vec<(String, &str)> = Vec::new();
    if let Some(lock) = crate::lockfile::read(&root.join(".zap-lock.json"))? {
        let vroot = root.join(".zap").join("v");
        for id in lock.packages.keys() {
            let Ok(v) = crate::util::read_json(&vroot.join(crate::util::pkg_dir_name(id)).join("package.json"))
            else {
                continue;
            };
            let has_scripts = ["preinstall", "install", "postinstall"]
                .iter()
                .any(|s| v.get("scripts").and_then(|x| x.as_object()).map(|o| o.contains_key(*s)).unwrap_or(false));
            if !has_scripts {
                continue;
            }
            match crate::allow::gate(&list, id, lock.packages.get(id).map(|p| p.integrity.as_str())) {
                crate::allow::Gate::Allowed => {}
                crate::allow::Gate::NotListed => pending.push((id.clone(), "")),
                crate::allow::Gate::HashMismatch => pending.push((id.clone(), " (hash changed)")),
            }
        }
    }
    println!("pending:");
    if pending.is_empty() {
        println!("  {}", crate::util::dim("none"));
    } else {
        for (id, note) in pending {
            println!("  {}{}", crate::util::cyan(id), crate::util::yellow(note));
        }
        println!("  {} {} to allow them", crate::util::dim("run"), crate::util::cyan("`zap allow <pkg>`"));
    }
    Ok(0)
}

/// approve: resolve each name (short name or full id) against the current
/// lockfile and record the recorded integrity; idempotent
fn add(root: &Path, list: &mut crate::allow::AllowList, pkgs: &[String]) -> Result<i32> {
    let lock = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow!("no lockfile here yet, run {} first", crate::util::cyan("`zap install`")))?;
    let mut added = 0usize;
    for arg in pkgs {
        let matches: Vec<(&String, &crate::model::LockPackage)> = lock
            .packages
            .iter()
            .filter(|(id, _)| id.as_str() == arg || id_name(id) == arg)
            .collect();
        if matches.is_empty() {
            return Err(anyhow!("`{}` is not resolved yet, run {} first", arg, crate::util::cyan("`zap install`")));
        }
        for (id, pkg) in matches {
            list.insert(id.clone(), pkg.integrity.clone());
            added += 1;
        }
    }
    crate::allow::save(root, list)?;
    println!("{} {} entr{} ({} total allowlisted)", crate::util::green("allowed"), added, if added == 1 { "y" } else { "ies" }, list.len());
    Ok(0)
}

/// revoke: accept short names or full ids; every arg must match something
fn remove(root: &Path, list: &mut crate::allow::AllowList, pkgs: &[String]) -> Result<i32> {
    let mut removed = 0usize;
    for arg in pkgs {
        let victims: Vec<String> = list
            .keys()
            .filter(|id| id.as_str() == arg || id_name(id) == arg)
            .cloned()
            .collect();
        if victims.is_empty() {
            return Err(anyhow!("`{}` is not allowlisted", arg));
        }
        for v in victims {
            list.remove(&v);
            removed += 1;
        }
    }
    crate::allow::save(root, list)?;
    println!(
        "{} {} entr{} ({} total allowlisted)",
        crate::util::yellow("removed"),
        removed,
        if removed == 1 { "y" } else { "ies" },
        list.len()
    );
    Ok(0)
}

/// "ms@2.1.3" -> "ms", "@scope/pkg@1.0.0" -> "@scope/pkg"
fn id_name(id: &str) -> &str {
    match id.rsplit_once('@') {
        Some((name, _)) if !name.is_empty() => name,
        _ => id,
    }
}

/// shortened hash for display
fn hash_prefix(h: &str) -> String {
    let s: String = h.chars().take(18).collect();
    if h.chars().count() > 18 {
        format!("{}…", s)
    } else {
        s
    }
}
