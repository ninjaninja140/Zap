use anyhow::{anyhow, bail, Context, Result};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::{Config, Linker};
use crate::model::{LockMember, LockPackage, Lockfile, Tree};
use crate::util::{cyan, dim, green, red, yellow};
use crate::workspace::{install_root_for, Member};

// install

pub fn install(args: &[String]) -> Result<i32> {
    let ctx = crate::util::find_root()?;
    install_in(&install_root_for(&ctx), args)
}

pub fn install_in(root: &Path, args: &[String]) -> Result<i32> {
    let force = args.iter().any(|a| a == "--force" || a == "-f");
    let offline = args.iter().any(|a| a == "--offline");
    let refresh = args.iter().any(|a| a == "--refresh");
    let frozen = args.iter().any(|a| a == "--frozen-lockfile" || a == "--frozen");
    let cfg = Config::load(root);
    let started = std::time::Instant::now();

    let manifest = crate::util::read_json(&root.join("package.json"))?;
    let (deps, dev) = read_deps_from(&manifest);
    let patterns = crate::workspace::patterns_from(&manifest);
    let members_raw = crate::workspace::discover(root, &patterns)?;
    let mut members: Vec<LockMember> = members_raw
        .iter()
        .map(|m| LockMember {
            name: m.name.clone(),
            dir: m.dir_rel.clone(),
            deps: BTreeMap::new(),
        })
        .collect();
    let member_reqs: Vec<BTreeMap<String, String>> = members_raw
        .iter()
        .map(|m: &Member| {
            let mut all = m.deps.clone();
            for (k, v) in &m.dev {
                all.insert(k.clone(), v.clone());
            }
            all
        })
        .collect();
    let hash_members: Vec<(String, &BTreeMap<String, String>, &BTreeMap<String, String>)> =
        members_raw.iter().map(|m| (m.name.clone(), &m.deps, &m.dev)).collect();

    if deps.is_empty() && dev.is_empty() && members.is_empty() {
        println!("{}", dim("nothing to install (no dependencies)"));
        return Ok(0);
    }
    let reg = crate::http::Registry::new(
        offline,
        refresh,
        cfg.prefer_offline,
        cfg.cache_ttl_hours,
        cfg.registry.as_deref(),
    )?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // ts-first: tsconfig anywhere (or a pinned version) => manage @types/* + typescript.
    // a user-installed typescript dependency always wins over the zap-managed one.
    let user_ts = deps.contains_key("typescript")
        || dev.contains_key("typescript")
        || member_reqs.iter().any(|m| m.contains_key("typescript"));
    let ts_on = root.join("tsconfig.json").is_file()
        || members_raw
            .iter()
            .any(|m| root.join(&m.dir_rel).join("tsconfig.json").is_file())
        || cfg.typescript.is_some();

    // reuse the lockfile when every manifest is unchanged
    let hash = crate::lockfile::root_hash(&deps, &dev, &hash_members);
    let mut cached: Option<Tree> = None;
    if !force {
        if let Some(lock) = crate::lockfile::read(&root.join(".zap-lock.json"))? {
            // type deps are derived from tsconfig detection + the pinned ts version +
            // whether the user manages typescript themselves, all outside the manifests
            let ts_ok = if !ts_on {
                !lock.types.contains_key("typescript")
            } else {
                match (lock.types.get("typescript"), cfg.typescript.as_deref()) {
                    (Some(id), Some(pin)) => id.starts_with(&format!("typescript@{}", pin)),
                    (Some(_), None) => true,
                    (None, _) => user_ts, // user manages it, no managed entry is correct
                }
            };
            if lock.root_hash == hash && ts_ok {
                cached = Some(Tree::from(&lock));
            }
        }
    }
    let tree = match cached {
        Some(t) => t,
        None => {
            if frozen {
                bail!("--frozen-lockfile: manifests changed but the lockfile wasn't updated, run `zap install` locally first");
            }
            let mut all = deps.clone();
            for (k, v) in &dev {
                all.insert(k.clone(), v.clone());
            }
            let empty = BTreeMap::new();
            let (t1, direct_types) =
                rt.block_on(crate::resolve::resolve(&reg, &all, &mut members, &member_reqs, &empty))?;
            if ts_on {
                // probe @types/<dep> for every direct dep that doesn't ship its own types
                let mut type_reqs: BTreeMap<String, String> = BTreeMap::new();
                for (name, has_own) in &direct_types {
                    if *has_own || name.starts_with("@types/") {
                        continue;
                    }
                    let types_pkg = format!("@types/{}", name);
                    if let Some(pack) = rt.block_on(reg.try_packument(&types_pkg))? {
                        if crate::util::pick_version(&types_pkg, &pack, "*").is_ok() {
                            type_reqs.insert(types_pkg, "*".to_string());
                        }
                    }
                }
                // typescript itself, pinned by config or latest, skipped when the
                // user manages their own typescript dependency
                if !user_ts {
                    type_reqs.insert(
                        "typescript".to_string(),
                        cfg.typescript.clone().unwrap_or_else(|| "*".to_string()),
                    );
                }
                let (t2, _) = rt.block_on(crate::resolve::resolve(
                    &reg,
                    &all,
                    &mut members,
                    &member_reqs,
                    &type_reqs,
                ))?;
                crate::lockfile::write(root, &t2.clone().into_lockfile(hash))?;
                t2
            } else {
                crate::lockfile::write(root, &t1.clone().into_lockfile(hash))?;
                t1
            }
        }
    };

    rt.block_on(materialize(root, &tree, &reg, cfg.concurrency))?;
    match cfg.linker {
        Linker::Pnp => {
            crate::pnp::write_runtime(root, &tree, cfg.strict)?;
            let hook = crate::pnp::node_options(root, Linker::Pnp);
            crate::pnp::write_shims(root, &tree, hook.as_deref())?;
        }
        Linker::NodeModules => {
            crate::pnp::remove_runtime(root);
            crate::linker::link_node_modules(root, &tree)?;
            crate::pnp::write_shims(root, &tree, None)?;
        }
    }
    if ts_on {
        let ts_id = ts_package_id(&tree);
        if let Some(ts_id) = ts_id {
            let managed = tree.types.contains_key("typescript");
            let ts_dir = root.join(".zap").join("v").join(crate::util::pkg_dir_name(&ts_id));
            let hook = crate::pnp::node_options(root, cfg.linker);
            if cfg.linker == Linker::Pnp {
                // tsc resolves modules itself (NODE_PATH only works on ts <= 5), give compilers AND editors real resolution via junctions
                crate::linker::link_typecheck_dir(root, &tree).context("linking ts typecheck dir")?;
                if cfg.editor_node_modules {
                    crate::linker::link_node_modules(root, &tree).context("linking node_modules")?;
                }
            }
            crate::linker::write_ts_shims(root, &ts_dir, hook.as_deref())
                .context("writing ts shims")?;
            println!(
                "  {} typescript {} ready (tsc/tsserver in .zap/bin, {})",
                green("ts"),
                ts_id.rsplit_once('@').map(|x| x.1).unwrap_or(""),
                if managed { "zap-managed" } else { "user-managed" }
            );
        }
    }
    if cfg.lifecycle {
        run_lifecycle(root, &tree, &cfg)?;
    }

    let total = tree.packages.len();
    let secs = started.elapsed().as_secs_f64();
    println!(
        "{} {} packages in {:.2}s ({})",
        green("done"),
        total,
        secs,
        if cfg.linker == Linker::Pnp { "pnp" } else { "node-modules" }
    );
    Ok(0)
}

/// download missing packages into the global store, then hardlink everything into .zap/v
async fn materialize(
    root: &Path,
    tree: &Tree,
    reg: &crate::http::Registry,
    concurrency: usize,
) -> Result<(u64, u64, u64)> {
    let started = std::time::Instant::now();

    // wipe the previous virtual store + shims for a clean state
    let vroot = root.join(".zap").join("v");
    let _ = std::fs::remove_dir_all(&vroot);
    std::fs::create_dir_all(&vroot)?;
    let _ = std::fs::remove_dir_all(root.join(".zap").join("bin"));

    let mut jobs: Vec<(String, LockPackage)> = Vec::new();
    for (id, pkg) in &tree.packages {
        let store_dir = crate::store::store_pkg_dir(id)?;
        if !store_dir.join(".zap-ok").exists() {
            jobs.push((id.clone(), pkg.clone()));
        }
    }
    let mut fetched_bytes: u64 = 0;
    let mut fetched_count: u64 = 0;
    if !jobs.is_empty() {
        let sem = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
        let reg = reg.clone();
        let mut set = tokio::task::JoinSet::new();
        for (id, pkg) in jobs {
            let reg = reg.clone();
            let sem = sem.clone();
            set.spawn(async move {
                let _permit = sem.acquire_owned().await.context("semaphore closed")?;
                let (_dir, bytes) = crate::store::ensure_in_store(&reg, &id, &pkg).await?;
                Ok::<(String, u64), anyhow::Error>((id, bytes))
            });
        }
        while let Some(joined) = set.join_next().await {
            let (_id, bytes) = joined.context("download task panicked")??;
            fetched_bytes += bytes;
            if bytes > 0 {
                fetched_count += 1;
            }
        }
    }

    let mut linked_files = 0u64;
    for id in tree.packages.keys() {
        let from = crate::store::store_pkg_dir(id)?;
        let to = vroot.join(crate::util::pkg_dir_name(id));
        linked_files += crate::store::link_tree(&from, &to)? as u64;
    }
    println!(
        "  {} {} ({}) · {} from local store · {} files linked in {:.2}s",
        green("fetched"),
        fetched_count,
        crate::util::human(fetched_bytes),
        tree.packages.len() as u64 - fetched_count,
        linked_files,
        started.elapsed().as_secs_f64()
    );
    Ok((fetched_count, fetched_bytes, linked_files))
}

/// opt-in lifecycle scripts (config: `lifecycle: true`), never run by default
fn run_lifecycle(root: &Path, tree: &Tree, cfg: &Config) -> Result<()> {
    let vroot = root.join(".zap").join("v");
    let mut ran = 0usize;
    for (id, _pkg) in &tree.packages {
        let dir = vroot.join(crate::util::pkg_dir_name(id));
        let Ok(v) = crate::util::read_json(&dir.join("package.json")) else {
            continue;
        };
        let Some(scripts) = v.get("scripts").and_then(|x| x.as_object()) else {
            continue;
        };
        for stage in ["preinstall", "install", "postinstall"] {
            let Some(script) = scripts.get(stage).and_then(|x| x.as_str()) else {
                continue;
            };
            let mut c = crate::util::shell_command(script);
            c.current_dir(&dir);
            crate::pnp::apply_env(&mut c, root, cfg.linker);
            c.env("npm_lifecycle_event", stage);
            c.env("INIT_CWD", dir.to_string_lossy().to_string());
            match c.status() {
                Ok(s) if s.success() => ran += 1,
                Ok(s) => eprintln!(
                    "{} lifecycle `{}` in {} exited with {}",
                    yellow("warning:"),
                    stage,
                    id,
                    s.code().unwrap_or(1)
                ),
                Err(e) => eprintln!(
                    "{} lifecycle `{}` in {} failed: {}",
                    yellow("warning:"),
                    stage,
                    id,
                    e
                ),
            }
        }
    }
    if ran > 0 {
        println!("  {} {} lifecycle scripts", green("ran"), ran);
    }
    Ok(())
}

// manifest helpers

pub fn read_deps(root: &Path) -> Result<(BTreeMap<String, String>, BTreeMap<String, String>)> {
    Ok(read_deps_from(&crate::util::read_json(&root.join("package.json"))?))
}

fn read_deps_from(v: &serde_json::Value) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let get = |key: &str| -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        if let Some(m) = v.get(key).and_then(|x| x.as_object()) {
            for (k, val) in m {
                if let Some(s) = val.as_str() {
                    out.insert(k.clone(), s.to_string());
                }
            }
        }
        out
    };
    (get("dependencies"), get("devDependencies"))
}

// add / remove command logic

pub fn add(args: &[String]) -> Result<i32> {
    let is_dev = args.iter().any(|a| a == "--dev" || a == "-D");
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
    install_in(&install_root, &[])
}

pub fn remove(args: &[String]) -> Result<i32> {
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() {
        bail!("usage: zap remove <package> ...");
    }
    let ctx = crate::util::find_root()?;
    let install_root = install_root_for(&ctx);
    let pkg_path = ctx.join("package.json");
    let mut v = crate::util::read_json(&pkg_path)?;
    let obj = v.as_object_mut().ok_or_else(|| anyhow!("package.json root must be an object"))?;
    let mut removed = 0usize;
    for key in ["dependencies", "devDependencies"] {
        if let Some(d) = obj.get_mut(key).and_then(|x| x.as_object_mut()) {
            for n in &names {
                if d.remove(n).is_some() {
                    removed += 1;
                }
            }
        }
    }
    if removed == 0 {
        bail!("`{}` not found in dependencies", names.join(", "));
    }
    std::fs::write(&pkg_path, format!("{}\n", serde_json::to_string_pretty(&v)?))?;
    println!("{} {}", green("removed"), names.join(", "));
    install_in(&install_root, &[])
}

// run / exec / dlx

pub fn run_script(args: &[String]) -> Result<i32> {
    let ctx = crate::util::find_root()?;
    let install_root = install_root_for(&ctx);
    let cfg = Config::load(&ctx);
    ensure_runtime_ready(&install_root, &cfg)?;
    if args.is_empty() {
        let v = crate::util::read_json(&ctx.join("package.json"))?;
        println!("available scripts:");
        if let Some(s) = v.get("scripts").and_then(|x| x.as_object()) {
            for (k, val) in s {
                println!("  {}, {}", k, val.as_str().unwrap_or(""));
            }
        }
        return Ok(0);
    }
    let v = crate::util::read_json(&ctx.join("package.json"))?;
    let script = v
        .get("scripts")
        .and_then(|s| s.get(&args[0]))
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("script `{}` not found in package.json", args[0]))?;
    let extra = args[1..].join(" ");
    let line = if extra.is_empty() { script.to_string() } else { format!("{} {}", script, extra) };
    spawn_shell(&ctx, &install_root, &line, &cfg)
}

pub fn exec(args: &[String]) -> Result<i32> {
    let ctx = crate::util::find_root()?;
    let install_root = install_root_for(&ctx);
    let cfg = Config::load(&ctx);
    ensure_runtime_ready(&install_root, &cfg)?;
    if args.is_empty() {
        bail!("usage: zap exec <command> [args...]");
    }
    // spawn directly (no shell) so quoting stays sane
    let mut c = std::process::Command::new(&args[0]);
    c.args(&args[1..]).current_dir(&ctx);
    crate::pnp::apply_env(&mut c, &install_root, cfg.linker);
    let status = c.status().with_context(|| format!("running `{}`", args[0]))?;
    Ok(status.code().unwrap_or(1))
}

fn ensure_runtime_ready(install_root: &Path, cfg: &Config) -> Result<()> {
    match cfg.linker {
        Linker::Pnp => {
            if !install_root.join(".pnp.cjs").is_file() {
                bail!("no pnp runtime here, run `zap install` first");
            }
        }
        Linker::NodeModules => {
            if !install_root.join("node_modules").is_dir() {
                bail!("no node_modules here, run `zap install` first");
            }
        }
    }
    Ok(())
}

fn spawn_shell(cwd: &Path, env_root: &Path, line: &str, cfg: &Config) -> Result<i32> {
    let mut c = crate::util::shell_command(line);
    c.current_dir(cwd);
    crate::pnp::apply_env(&mut c, env_root, cfg.linker);
    let status = c.status().with_context(|| format!("running `{}`", line))?;
    Ok(status.code().unwrap_or(1))
}

pub fn dlx(args: &[String]) -> Result<i32> {
    let (name, range, passthrough) = parse_x_args(args)?;
    dlx_run(&name, range, passthrough)
}

fn parse_x_args(args: &[String]) -> Result<(String, String, Vec<String>)> {
    let spec = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow!("usage: zap x <package|bin>[@version] [args...]"))?
        .clone();
    let passthrough: Vec<String> = args.iter().filter(|a| a.as_str() != spec).cloned().collect();
    let (name, range) = crate::util::split_spec(&spec);
    Ok((name, range, passthrough))
}

/// `zap x <bin>`, run a bin that's already installed in this
/// project (deps, dev deps, workspace members, type deps); fall back to dlx fetch
pub fn x(args: &[String]) -> Result<i32> {
    let (name, range, passthrough) = parse_x_args(args)?;
    if range.is_empty() {
        if let Some((bin_file, install_root)) = find_local_bin(&name)? {
            let cfg = Config::load(&install_root);
            let mut c = std::process::Command::new("node");
            c.arg(bin_file).args(&passthrough).current_dir(&install_root);
            crate::pnp::apply_env(&mut c, &install_root, cfg.linker);
            let status = c.status().with_context(|| format!("running {}", name))?;
            return Ok(status.code().unwrap_or(1));
        }
    }
    dlx_run(&name, range, passthrough)
}

/// look up a bin across the installed tree: registry packages, type deps, members
fn find_local_bin(name: &str) -> Result<Option<(PathBuf, PathBuf)>> {
    let ctx = crate::util::find_root()?;
    let install_root = install_root_for(&ctx);
    let Some(lock) = crate::lockfile::read(&install_root.join(".zap-lock.json"))? else {
        return Ok(None);
    };
    let tree = Tree::from(&lock);
    let vroot = install_root.join(".zap").join("v");
    for (id, pkg) in &tree.packages {
        if let Some(rel) = pkg.bin.get(name) {
            return Ok(Some((
                vroot.join(crate::util::pkg_dir_name(id)).join(rel.trim_start_matches("./")),
                install_root,
            )));
        }
    }
    for m in &tree.members {
        let member = install_root.join(&m.dir);
        let Ok(v) = crate::util::read_json(&member.join("package.json")) else {
            continue;
        };
        let Some(bin) = v.get("bin") else { continue };
        let normalized = crate::model::normalize_bin(
            v.get("name").and_then(|x| x.as_str()).unwrap_or(""),
            bin,
        );
        if let Some(rel) = normalized.get(name) {
            return Ok(Some((member.join(rel.trim_start_matches("./")), install_root)));
        }
    }
    Ok(None)
}

/// `zap create <template> [args...]`, runs create-<template>'s bin like npm create
pub fn create(args: &[String]) -> Result<i32> {
    let template = args
        .first()
        .ok_or_else(|| anyhow!("usage: zap create <template> [args...], e.g. zap create vite my-app"))?
        .clone();
    let passthrough: Vec<String> = args.iter().skip(1).cloned().collect();
    let (name, range) = crate::util::split_spec(&template);
    dlx_run(&format!("create-{}", name), range, passthrough)
}

fn dlx_run(name: &str, range: String, passthrough: Vec<String>) -> Result<i32> {
    let name = name.to_string();
    let key = if range.is_empty() {
        crate::util::slug(&name)
    } else {
        format!("{}-{}", crate::util::slug(&name), &crate::util::sha256_hex(range.as_bytes())[..8])
    };
    let dir = crate::util::zap_home()?.join("dlx").join(key);

    if !dir.join(".pnp.json").is_file() {
        std::fs::create_dir_all(&dir)?;
        let mut deps = serde_json::Map::new();
        deps.insert(
            name.clone(),
            serde_json::Value::String(if range.is_empty() { "*".to_string() } else { range.clone() }),
        );
        let pkg = serde_json::json!({ "name": "zap-dlx", "private": true, "dependencies": deps });
        std::fs::write(dir.join("package.json"), format!("{}\n", serde_json::to_string_pretty(&pkg)?))?;
        install_in(&dir, &[])?;
    }
    let lock = crate::lockfile::read(&dir.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow!("dlx cache is broken, run `zap cache clean` and retry"))?;
    let short = name.rsplit('/').next().unwrap_or(&name).to_string();
    let entry = lock
        .packages
        .iter()
        .find(|(id, _)| id.rsplit_once('@').map(|(n, _)| n == name.as_str()).unwrap_or(false));
    let (id, pkg) = entry.ok_or_else(|| anyhow!("`{}` was not installed", name))?;
    let (bin_name, rel) = pkg
        .bin
        .get(&short)
        .map(|rel| (short.clone(), rel.clone()))
        .or_else(|| pkg.bin.iter().next().map(|(k, v)| (k.clone(), v.clone())))
        .ok_or_else(|| anyhow!("`{}` has no bin, nothing to run", name))?;
    let bin_file = dir
        .join(".zap")
        .join("v")
        .join(crate::util::pkg_dir_name(id))
        .join(rel.trim_start_matches("./"));
    let linker = if dir.join(".pnp.cjs").is_file() { Linker::Pnp } else { Linker::NodeModules };
    let mut c = std::process::Command::new("node");
    c.arg(bin_file).args(&passthrough).current_dir(std::env::current_dir()?);
    crate::pnp::apply_env(&mut c, &dir, linker);
    let status = c.status().with_context(|| format!("running {}", bin_name))?;
    Ok(status.code().unwrap_or(1))
}

/// find the active typescript package id: zap-managed first, then user-installed
/// (root or any workspace member dependency)
pub fn ts_package_id(tree: &Tree) -> Option<String> {
    tree.types
        .get("typescript")
        .cloned()
        .or_else(|| tree.root.get("typescript").cloned())
        .or_else(|| tree.members.iter().find_map(|m| m.deps.get("typescript").cloned()))
}

/// run the zap-managed typescript compiler with types resolution wired up
pub fn tsc(args: &[String]) -> Result<i32> {
    let ctx = crate::util::find_root()?;
    let install_root = install_root_for(&ctx);
    let cfg = Config::load(&ctx);
    ensure_runtime_ready(&install_root, &cfg)?;
    let lock = crate::lockfile::read(&install_root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow!("no .zap-lock.json, run `zap install` first"))?;
    let tree = Tree::from(&lock);
    let ts_id = ts_package_id(&tree)
        .ok_or_else(|| anyhow!("typescript is not managed here, add a tsconfig.json or set `typescript: <version>` in .zaprc, then `zap install`"))?;
    if cfg.linker == Linker::Pnp {
        crate::linker::link_typecheck_dir(&install_root, &tree)?;
    }
    let pkg = tree.packages.get(ts_id.as_str());
    let rel = pkg
        .and_then(|p| p.bin.get("tsc"))
        .cloned()
        .unwrap_or_else(|| "bin/tsc".to_string());
    let bin_file = install_root
        .join(".zap")
        .join("v")
        .join(crate::util::pkg_dir_name(&ts_id))
        .join(rel.trim_start_matches("./"));
    let mut c = std::process::Command::new("node");
    c.arg(bin_file).args(args).current_dir(&ctx);
    crate::pnp::apply_env(&mut c, &install_root, cfg.linker);
    if cfg.linker == Linker::Pnp {
        c.env(
            "NODE_PATH",
            install_root.join(".zap").join("ts-nm").display().to_string(),
        );
    }
    let status = c.status().with_context(|| "running tsc")?;
    Ok(status.code().unwrap_or(1))
}

/// npm-style version bump: `zap version [major|minor|patch|x.y.z]`
pub fn version_cmd(args: &[String]) -> Result<i32> {
    let kind = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(|s| s.as_str())
        .unwrap_or("patch");
    let ctx = crate::util::find_root()?;
    let path = ctx.join("package.json");
    let mut v = crate::util::read_json(&path)?;
    let cur = v
        .get("version")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("package.json has no version"))?
        .to_string();
    let mut ver = semver::Version::parse(&cur).with_context(|| format!("bad version `{}`", cur))?;
    match kind {
        "major" => {
            ver.major += 1;
            ver.minor = 0;
            ver.patch = 0;
            ver.pre = semver::Prerelease::EMPTY;
            ver.build = semver::BuildMetadata::EMPTY;
        }
        "minor" => {
            ver.minor += 1;
            ver.patch = 0;
            ver.pre = semver::Prerelease::EMPTY;
        }
        "patch" => {
            ver.patch += 1;
            ver.pre = semver::Prerelease::EMPTY;
        }
        other => {
            ver = semver::Version::parse(other)
                .with_context(|| format!("bad version `{}` (use major|minor|patch|x.y.z)", other))?;
        }
    }
    let new = ver.to_string();
    if let Some(obj) = v.as_object_mut() {
        obj.insert("version".into(), serde_json::json!(new));
    }
    std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&v)?))?;
    println!("{} {} -> {}", green("version"), cur, new);
    Ok(0)
}

// inspect / misc

pub fn ls(args: &[String]) -> Result<i32> {
    let all = args.iter().any(|a| a == "--all" || a == "-a");
    let ctx = crate::util::find_root()?;
    let root = install_root_for(&ctx);
    let lock = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow!("no .zap-lock.json, run `zap install` first"))?;
    let (deps, dev) = read_deps(&root)?;
    println!(
        "{}",
        dim(format!("{} ({} packages installed)", root.display(), lock.packages.len()))
    );
    for (n, r) in deps.iter().chain(dev.iter()) {
        match lock.root.get(n) {
            Some(i) => println!("  {} {}", n, i.rsplit_once('@').map(|x| x.1).unwrap_or("")),
            None => println!("  {} {} {}", n, r, yellow("(not installed, run `zap install`)")),
        }
    }
    if !lock.workspaces.is_empty() {
        println!("{}", dim("workspace members:"));
        for m in &lock.workspaces {
            println!("  {} {} {}", dim("[ws]"), m.name, dim(&m.dir));
        }
    }
    if all {
        println!();
        println!("{}", dim("full dependency tree:"));
        for id in lock.packages.keys() {
            println!("  {}", id);
        }
    }
    Ok(0)
}

pub fn why(args: &[String]) -> Result<i32> {
    let target = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow!("usage: zap why <package>"))?
        .clone();
    let ctx = crate::util::find_root()?;
    let root = install_root_for(&ctx);
    let lock: Lockfile = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow!("no .zap-lock.json, run `zap install` first"))?;
    let mut printed = 0usize;
    let mut visited: HashSet<String> = HashSet::new();
    for dep_id in lock.root.values() {
        walk(&lock, dep_id, &mut Vec::new(), &target, &mut printed, &mut visited);
    }
    for m in &lock.workspaces {
        for dep_id in m.deps.values() {
            walk(&lock, dep_id, &mut Vec::new(), &target, &mut printed, &mut visited);
        }
    }
    if printed == 0 {
        println!("{}", yellow(format!("`{}` is not in the dependency tree", target)));
    }
    Ok(0)
}

fn walk(
    lock: &Lockfile,
    id: &str,
    chain: &mut Vec<String>,
    target: &str,
    printed: &mut usize,
    visited: &mut HashSet<String>,
) {
    let name = id.rsplit_once('@').map(|x| x.0).unwrap_or(id);
    chain.push(id.to_string());
    if name == target {
        println!("  {}", chain.join(" -> "));
        *printed += 1;
        chain.pop();
        return;
    }
    if !visited.insert(id.to_string()) {
        chain.pop();
        return;
    }
    if let Some(p) = lock.packages.get(id) {
        for dep_id in p.deps.values() {
            walk(lock, dep_id, chain, target, printed, visited);
        }
    }
    chain.pop();
}

/// show which deps are behind the registry (exit 1 when something is outdated)
pub fn outdated(args: &[String]) -> Result<i32> {
    let _ = args;
    let ctx = crate::util::find_root()?;
    let root = install_root_for(&ctx);
    let lock = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow!("no .zap-lock.json, run `zap install` first"))?;
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

/// no args: re-resolve everything within existing ranges (== install --force).
/// with args: like `add`, bumping packages to their latest version.
pub fn upgrade(args: &[String]) -> Result<i32> {
    if args.is_empty() {
        println!("{}", dim("re-resolving all dependencies within their ranges..."));
        return install(&["--force".to_string()]);
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
    add(&bumped)
}

/// store inspection: `zap store` / `zap store path` / `zap store size`
pub fn store(args: &[String]) -> Result<i32> {
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

fn dir_stats(dir: &Path) -> (u64, u64) {
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

/// print the path of an installed package's bin, or list them with no arg
pub fn bin(args: &[String]) -> Result<i32> {
    let ctx = crate::util::find_root()?;
    let root = install_root_for(&ctx);
    let lock = crate::lockfile::read(&root.join(".zap-lock.json"))?
        .ok_or_else(|| anyhow!("no .zap-lock.json, run `zap install` first"))?;
    let vroot = root.join(".zap").join("v");
    let target = args.iter().find(|a| !a.starts_with('-')).cloned();
    let mut found = 0usize;
    for (id, pkg) in &lock.packages {
        for (bin_name, rel) in &pkg.bin {
            let Some(t) = &target else {
                println!("  {} -> {}", bin_name, vroot.join(crate::util::pkg_dir_name(id)).join(rel).display());
                found += 1;
                continue;
            };
            if bin_name == t {
                println!("{}", vroot.join(crate::util::pkg_dir_name(id)).join(rel).display());
                return Ok(0);
            }
            let _ = &found;
        }
    }
    if target.is_some() {
        bail!("no bin named `{}` in the dependency tree", target.unwrap());
    }
    if found == 0 {
        println!("{}", dim("no bins installed"));
    }
    Ok(0)
}

pub fn init(_args: &[String]) -> Result<i32> {
    let root = std::env::current_dir()?;
    let p = root.join("package.json");
    if p.is_file() {
        bail!("package.json already exists here");
    }
    let name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase().replace(' ', "-"))
        .unwrap_or_else(|| "project".to_string());
    let v = serde_json::json!({ "name": name, "version": "1.0.0", "private": true });
    std::fs::write(&p, format!("{}\n", serde_json::to_string_pretty(&v)?))?;
    println!("{} package.json", green("created"));
    println!("next: {}", cyan("zap add <pkg>"));
    Ok(0)
}

pub fn cache(args: &[String]) -> Result<i32> {
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
