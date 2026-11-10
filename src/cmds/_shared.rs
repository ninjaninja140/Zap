// shared helpers backing the command files; not part of the registry (the
// `_` prefix keeps build.rs from treating this as a command)

use anyhow::{anyhow, bail, Context, Result};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::{Config, Linker};
use crate::model::{LockMember, LockPackage, Lockfile, Tree};
use crate::util::{cyan, dim, green, yellow};
use crate::workspace::{install_root_for, Member};

// install

pub fn install_in(root: &Path, args: &[String], pins: Option<&BTreeMap<String, String>>) -> Result<i32> {
    let force = args.iter().any(|a| a == "--force" || a == "-f");
    let offline = args.iter().any(|a| a == "--offline");
    let refresh = args.iter().any(|a| a == "--refresh");
    let frozen = args.iter().any(|a| a == "--frozen-lockfile" || a == "--frozen");
    let production = args.iter().any(|a| a == "--production" || a == "-P" || a == "--prod" || a == "--omit=dev");
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
    // skipped in production mode, you don't need the compiler in a docker image
    let user_ts = deps.contains_key("typescript")
        || dev.contains_key("typescript")
        || member_reqs.iter().any(|m| m.contains_key("typescript"));
    let ts_on = !production && (root.join("tsconfig.json").is_file()
        || members_raw
            .iter()
            .any(|m| root.join(&m.dir_rel).join("tsconfig.json").is_file())
        || cfg.typescript.is_some());

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
    if !production {
        for (k, v) in &dev {
            all.insert(k.clone(), v.clone());
        }
    }
    let empty = BTreeMap::new();
    let pins_map = pins.unwrap_or(&empty);
    let (t1, direct_types) =
        rt.block_on(crate::resolve::resolve(&reg, &all, &mut members, &member_reqs, &empty, &pins_map))?;
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
                    &pins_map,
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
        // npm order: root preinstall first, dependency lifecycles, then the
        // root's own install/postinstall; workspace members get nothing
        let mut ran = run_root_lifecycle(root, &["preinstall"], &cfg);
        ran += run_lifecycle(root, &tree, &cfg);
        ran += run_root_lifecycle(root, &["install", "postinstall"], &cfg);
        if ran > 0 {
            println!("  {} {} lifecycle scripts", green("ran"), ran);
        }
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

/// opt-in dependency lifecycle scripts (config: `lifecycle: true`), never run
/// by default; returns how many ran
fn run_lifecycle(root: &Path, tree: &Tree, cfg: &Config) -> usize {
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
            if run_lifecycle_stage(root, &dir, id, stage, script, cfg) {
                ran += 1;
            }
        }
    }
    ran
}

/// the root project's own install lifecycle hooks, same opt-in gate as deps
fn run_root_lifecycle(root: &Path, stages: &[&str], cfg: &Config) -> usize {
    let Ok(v) = crate::util::read_json(&root.join("package.json")) else {
        return 0;
    };
    let Some(scripts) = v.get("scripts").and_then(|x| x.as_object()) else {
        return 0;
    };
    let mut ran = 0usize;
    for stage in stages {
        if let Some(script) = scripts.get(*stage).and_then(|x| x.as_str()) {
            if run_lifecycle_stage(root, root, "root", stage, script, cfg) {
                ran += 1;
            }
        }
    }
    ran
}

/// run one lifecycle stage in `dir`, npm env vars set; `what` names the owner
/// in warnings ("root" or the package id)
fn run_lifecycle_stage(root: &Path, dir: &Path, what: &str, stage: &str, script: &str, cfg: &Config) -> bool {
    let mut c = crate::util::shell_command(script);
    c.current_dir(dir);
    crate::pnp::apply_env(&mut c, root, cfg.linker);
    c.env("npm_lifecycle_event", stage);
    c.env("INIT_CWD", dir.to_string_lossy().to_string());
    let ok = match c.status() {
        Ok(s) if s.success() => true,
        Ok(s) => {
            eprintln!(
                "{} lifecycle `{}` in {} exited with {}",
                yellow("warning:"),
                stage,
                what,
                s.code().unwrap_or(1)
            );
            false
        }
        Err(e) => {
            eprintln!("{} lifecycle `{}` in {} failed: {}", yellow("warning:"), stage, what, e);
            false
        }
    };
    ok
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

// run / exec / dlx helpers

pub fn ensure_runtime_ready(install_root: &Path, cfg: &Config) -> Result<()> {
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

pub fn spawn_shell(cwd: &Path, env_root: &Path, line: &str, cfg: &Config) -> Result<i32> {
    let mut c = crate::util::shell_command(line);
    c.current_dir(cwd);
    crate::pnp::apply_env(&mut c, env_root, cfg.linker);
    let status = c.status().with_context(|| format!("running `{}`", line))?;
    Ok(status.code().unwrap_or(1))
}

pub fn parse_x_args(args: &[String]) -> Result<(String, String, Vec<String>)> {
    let spec = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow!("usage: zap x <package|bin>[@version] [args...]"))?
        .clone();
    let passthrough: Vec<String> = args.iter().filter(|a| a.as_str() != spec).cloned().collect();
    let (name, range) = crate::util::split_spec(&spec);
    Ok((name, range, passthrough))
}

/// look up a bin across the installed tree: registry packages, type deps, members
pub fn find_local_bin(name: &str) -> Result<Option<(PathBuf, PathBuf)>> {
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

pub fn dlx_run(name: &str, range: String, passthrough: Vec<String>) -> Result<i32> {
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
        install_in(&dir, &[], None)?;
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

// project scan

pub fn walk(
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

pub fn dir_stats(dir: &Path) -> (u64, u64) {
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

// zap skills

pub const SKILLS_START: &str = "<!-- zap:start -->";
pub const SKILLS_END: &str = "<!-- zap:end -->";

pub fn has_zap_block(content: &str) -> bool {
    content.contains(SKILLS_START)
}

/// inject or replace the zap skills block in a markdown file
pub fn inject_skills(path: &Path, block: &str) -> Result<()> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    if let Some(start) = content.find(SKILLS_START) {
        if let Some(end_rel) = content[start..].find(SKILLS_END) {
            let end = start + end_rel + SKILLS_END.len();
            let new = format!("{}{}\n{}\n{}", &content[..start], block, "", &content[end..].trim_start_matches('\n'));
            std::fs::write(path, new)?;
            return Ok(());
        }
    }
    let sep = if content.is_empty() || content.ends_with('\n') { "" } else { "\n" };
    std::fs::write(path, format!("{}{}{}\n", content, sep, block))?;
    Ok(())
}

pub fn strip_skills(path: &Path) -> Result<bool> {
    let content = std::fs::read_to_string(path)?;
    let Some(start) = content.find(SKILLS_START) else { return Ok(false) };
    let Some(end_rel) = content[start..].find(SKILLS_END) else { return Ok(false) };
    let end = start + end_rel + SKILLS_END.len();
    let new = format!("{}{}", &content[..start], content[end..].trim_start_matches('\n'));
    std::fs::write(path, new)?;
    Ok(true)
}

// project scan (--project reference search)

/// code file extensions to scan for --project references
const CODE_EXTS: [&str; 12] = ["js", "mjs", "cjs", "ts", "tsx", "jsx", "mts", "cts", "vue", "svelte", "astro", "mdx"];

fn is_code_file(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| CODE_EXTS.contains(&e))
        .unwrap_or(false)
}

/// does a source line reference `name` as a module specifier?
/// matches `"name"`, `'name'`, `"name/sub"`, `'name/sub'`, not `"name-other"` or `./name`
fn line_has_ref(line: &str, name: &str) -> bool {
    for q in ['"', '\''] {
        if line.contains(&format!("{}{}{}", q, name, q))
            || line.contains(&format!("{}{}/", q, name))
        {
            return true;
        }
    }
    false
}

/// `zap why --project <pkg>`, scan every source file in the project (root +
/// workspace members) for references to the package
pub fn why_project(root: &Path, target: &str) -> Result<i32> {
    let ts_on = root.join("tsconfig.json").is_file();
    let mut refs: Vec<(String, usize, String)> = Vec::new();

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            if path.is_dir() {
                let skip = matches!(
                    name.as_str(),
                    "node_modules" | ".zap" | ".git" | ".yarn" | "target" | "dist" | "out" | "build"
                ) || (name == "dist" && ts_on);
                if !skip {
                    stack.push(path);
                }
            } else if is_code_file(&name) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    for (i, line) in content.lines().enumerate() {
                        if line_has_ref(line, target) {
                            let rel = path
                                .strip_prefix(root)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .replace('\\', "/");
                            refs.push((rel, i + 1, line.trim().to_string()));
                        }
                    }
                }
            }
        }
    }

    // exclude metadata files from the results
    let refs: Vec<_> = refs
        .into_iter()
        .filter(|(p, _, _)| {
            let fname = Path::new(p)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            fname != "package.json" && fname != ".zap-lock.json"
        })
        .collect();

    if refs.is_empty() {
        println!(
            "{} no references to `{}` found in project source files",
            yellow("nothing:"),
            target
        );
        return Ok(1);
    }
    println!(
        "{} {} reference{} to `{}` in project source files:",
        green("found"),
        refs.len(),
        if refs.len() == 1 { "" } else { "s" },
        target
    );
    for (path, line, content) in &refs {
        let shown: String = {
            let c: String = content.chars().take(100).collect();
            if content.chars().count() > 100 {
                format!("{}…", c)
            } else {
                c
            }
        };
        println!("  {}:{} {}", cyan(path), line, dim(shown));
    }
    Ok(0)
}
