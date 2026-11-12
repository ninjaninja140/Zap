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

// global installs: one isolated project dir per package under ~/.zap/global,
// its bins shimmed into ~/.zap/bin for PATH-wide use

/// the global area: one tiny isolated project per globally installed package
pub fn global_root() -> Result<PathBuf> {
    Ok(crate::util::zap_home()?.join("global"))
}

/// the shared user-level shim dir for global package bins (~/.zap/bin)
pub fn global_bin_dir() -> Result<PathBuf> {
    Ok(crate::util::zap_home()?.join("bin"))
}

/// `zap add -g <pkg>[@range]...`: write an isolated manifest per package and
/// run the normal install machinery on it, no cross-package version
/// conflicts, ever. the package's bins shim into ~/.zap/bin (the shims embed
/// absolute paths + the pnp hook, they work from any cwd). works outside any
/// project; global dirs use default config on purpose (no rc files in
/// ~/.zap/global, globals aren't projects)
pub fn global_add(args: &[String]) -> Result<i32> {
    let specs: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if specs.is_empty() {
        bail!("usage: zap add -g <package>[@range] ...");
    }
    let groot = global_root()?;
    std::fs::create_dir_all(&groot)?;
    let bin_dir = global_bin_dir()?;
    std::fs::create_dir_all(&bin_dir)?;
    let mut new_shims = 0usize;
    let mut code = 0;
    for spec in specs {
        let (name, range) = crate::util::split_spec(&spec);
        let dir = groot.join(&name);
        std::fs::create_dir_all(&dir)?;
        // re-add with a different range = re-install: the manifest rewrite +
        // re-resolve handles the upgrade; the isolated dir keeps it clean
        let dep_range = if range.is_empty() { "*" } else { &range };
        let manifest = serde_json::json!({
            "name": &name,
            "version": "0.0.0",
            "private": true,
            "dependencies": { &name: dep_range },
        });
        std::fs::write(dir.join("package.json"), format!("{}\n", serde_json::to_string_pretty(&manifest)?))?;
        code = install_in(&dir, &[], None)?;
        if code != 0 {
            return Ok(code);
        }
        let cfg = Config::load(&dir);
        let hook = crate::pnp::node_options(&dir, cfg.linker);
        // the installed package's own bins, from its lock entry
        let lock = crate::lockfile::read(&dir.join(".zap-lock.json"))?
            .ok_or_else(|| anyhow!("no lockfile after global install of `{}`", name))?;
        let own_bins: Vec<(String, String)> = lock
            .root
            .get(&name)
            .and_then(|id| lock.packages.get(id))
            .map(|p| p.bin.iter().map(|(b, r)| (b.clone(), r.clone())).collect())
            .unwrap_or_default();
        if own_bins.is_empty() {
            println!("{} {} (no bins to shim)", dim("installed"), name);
            continue;
        }
        for (bin_name, rel) in &own_bins {
            let shim_path = bin_dir.join(crate::pnp::shim_file_name(bin_name));
            let is_new = !shim_path.exists();
            if !is_new {
                // collision: another global owns this bin name, last install wins
                let old = std::fs::read_to_string(&shim_path).unwrap_or_default();
                let mine = dir.to_string_lossy().to_string();
                if !old.contains(&mine) {
                    println!(
                        "{} bin {} was already shimmed by another global package, last install wins",
                        crate::util::yellow("note:"),
                        bin_name
                    );
                }
            }
            let bin_file = dir
                .join(".zap")
                .join("v")
                .join(crate::util::pkg_dir_name(lock.root.get(&name).map(|s| s.as_str()).unwrap_or("")))
                .join(rel.trim_start_matches("./"));
            crate::pnp::write_shim(&bin_dir, bin_name, &bin_file, hook.as_deref(), &[])?;
            if is_new {
                new_shims += 1;
            }
        }
        println!("{} {}@{}", green("+"), name, lock.root.get(&name).and_then(|id| id.rsplit_once('@')).map(|x| x.1).unwrap_or("?"));
    }
    if new_shims > 0 {
        println!(
            "{}",
            dim(format!(
                "{} new shim(s) in {}, add it to PATH to use them everywhere",
                new_shims,
                bin_dir.display()
            ))
        );
    }
    Ok(code)
}

/// `zap remove -g <name>...`: drop the isolated dir + that package's shims
/// from ~/.zap/bin. the shims embed the package's absolute install path, so
/// they're identified by a content scan, no shim ledger needed (shortcut:
/// scan instead of tracking a registry file; ~a dozen files at worst)
pub fn global_remove(args: &[String]) -> Result<i32> {
    let names: Vec<String> = args.iter().filter(|a| !a.starts_with('-')).cloned().collect();
    if names.is_empty() {
        bail!("usage: zap remove -g <package> ...");
    }
    let groot = global_root()?;
    let bin_dir = global_bin_dir()?;
    for name in &names {
        let dir = groot.join(name);
        if !dir.is_dir() {
            bail!("`{}` is not installed globally", name);
        }
        let prefix = dir.to_string_lossy().to_string();
        if let Ok(rd) = std::fs::read_dir(&bin_dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if let Ok(content) = std::fs::read_to_string(&p) {
                    if content.contains(&prefix) {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
        }
        std::fs::remove_dir_all(&dir)?;
        println!("{} {}", green("-"), name);
    }
    Ok(0)
}

/// `zap ls -g`: globally installed packages with their resolved versions
pub fn global_ls() -> Result<i32> {
    let groot = global_root()?;
    let mut found = 0usize;
    if let Ok(rd) = std::fs::read_dir(&groot) {
        for e in rd.flatten() {
            let dir = e.path();
            if !dir.is_dir() {
                continue;
            }
            let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let ver = crate::lockfile::read(&dir.join(".zap-lock.json"))
                .ok()
                .flatten()
                .and_then(|l| l.root.values().next().cloned())
                .and_then(|id| id.rsplit_once('@').map(|x| x.1.to_string()));
            println!("  {} {}", name, crate::util::dim(&ver.unwrap_or_else(|| "?".to_string())));
            found += 1;
        }
    }
    if found == 0 {
        println!("{}", crate::util::dim("no global packages"));
    }
    Ok(0)
}

// install

/// structured result of one install pass (json mode's document); serde
/// camelCase so the ts side reads it without translation
#[derive(serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FetchStats {
    pub count: u64,
    pub bytes: u64,
    pub from_store: u64,
}

#[derive(serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleStats {
    pub ran: u32,
    pub blocked: u32,
}

#[derive(serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InstallReport {
    /// (pkg_id, manifest section that pulled it in): newly in the tree
    pub added: Vec<(String, String)>,
    /// ids gone vs the previous lockfile
    pub removed: Vec<String>,
    /// (new id, old version, new version)
    pub changed: Vec<(String, String, String)>,
    pub fetched: FetchStats,
    pub linked_files: u64,
    pub skipped_platform: usize,
    /// the rendered peer-dependency warning lines (same text the human mode prints)
    pub peers: Vec<String>,
    pub lifecycle: LifecycleStats,
    pub done_packages: usize,
    pub seconds: f64,
}

pub fn install_in(root: &Path, args: &[String], pins: Option<&BTreeMap<String, String>>) -> Result<i32> {
    let json = args.iter().any(|a| a == "--json");
    let (code, report) = install_in_core(root, args, pins, json)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(code)
}

/// the core: `json` gates every human print (inert bar, no summary lines) and
/// collects the structured report instead, the document is dumped by
/// install_in so stdout carries only the json
pub(crate) fn install_in_core(root: &Path, args: &[String], pins: Option<&BTreeMap<String, String>>, json: bool) -> Result<(i32, InstallReport)> {
    let force = args.iter().any(|a| a == "--force" || a == "-f");
    let offline = args.iter().any(|a| a == "--offline");
    let refresh = args.iter().any(|a| a == "--refresh");
    let frozen = args.iter().any(|a| a == "--frozen-lockfile" || a == "--frozen");
    let production = args.iter().any(|a| a == "--production" || a == "-P" || a == "--prod" || a == "--omit=dev");
    let cfg = Config::load(root);
    let started = std::time::Instant::now();
    // the previous lockfile, for the tree-level diff (captured before resolve
    // rewrites it; None on first install)
    let prev_lock = crate::lockfile::read(&root.join(".zap-lock.json"))?;
    let mut report = InstallReport::default();
    let progress = Arc::new(if json {
        // json mode: never a bar
        crate::cmds::progress::Progress::start_with(false, cfg.concurrency.min(8))
    } else {
        // human mode: tty-probed exactly as before
        crate::cmds::progress::Progress::start(cfg.concurrency.min(8))
    });

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
        // nothing to install, but when a previous install left state behind
        // (lockfile, vroot, junctions, pnp artifacts), run the flow anyway so
        // the removal tears all of it down; the early return is for pristine
        // script-only projects
        let has_state = root.join(".zap-lock.json").is_file()
            || root.join(".zap").is_dir()
            || root.join("node_modules").exists();
        if !has_state {
            report.seconds = started.elapsed().as_secs_f64();
            if !json {
                println!("{}", dim("nothing to install (no dependencies)"));
            }
            return Ok((0, report));
        }
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
    let mut skipped = 0usize; // platform-mismatched packages the resolve dropped
    let mut peers: Vec<(String, String, String)> = Vec::new(); // peer-dependency report
    let tree = match cached {
        Some(t) => t,
        None => {
            if frozen {
                bail!("--frozen-lockfile: manifests changed but the lockfile wasn't updated, run `zap install` locally first");
            }
            progress.stage("resolving");
    let mut all = deps.clone();
    if !production {
        for (k, v) in &dev {
            all.insert(k.clone(), v.clone());
        }
    }
    let empty = BTreeMap::new();
    let pins_map = pins.unwrap_or(&empty);
    let (t1, direct_types, sk1, p1) =
        rt.block_on(crate::resolve::resolve(&reg, &all, &mut members, &member_reqs, &empty, pins_map))?;
    skipped = sk1;
    peers = p1;
            if ts_on {
                // probe @types/<dep> for every direct dep that doesn't ship its own types
                progress.stage("typescript");
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
                let (t2, _, sk2, p2) = rt.block_on(crate::resolve::resolve(
                    &reg,
                    &all,
                    &mut members,
                    &member_reqs,
                    &type_reqs,
                    pins_map,
                ))?;
                skipped = sk2;
                peers = p2;
                crate::lockfile::write(root, &t2.clone().into_lockfile(hash))?;
                t2
            } else {
                crate::lockfile::write(root, &t1.clone().into_lockfile(hash))?;
                t1
            }
        }
    };

    let (fetched_bytes, fetched_count, linked_files, mat_secs) = rt.block_on(materialize(
        root,
        &tree,
        &reg,
        cfg.concurrency,
        progress.clone(),
        Arc::new(direct_set(&tree)),
    ))?;
    // the bar clears (thread joined) before any summary line prints
    progress.finish();
    report.skipped_platform = skipped;
    if skipped > 0 && !json {
        let (ho, hc) = crate::resolve::host_platform();
        println!(
            "{}",
            dim(format!("skipped {} platform-mismatched package(s) (host is {}:{})", skipped, ho, hc))
        );
    }
    // peer-dependency advisories: warn + recommend, never auto-install; the
    // rendered lines are the report's peers (same text either mode)
    let provided: BTreeMap<String, String> =
        tree.packages.keys().map(|id| (id.clone(), id.rsplit_once('@').map(|x| x.1.to_string()).unwrap_or_default())).collect();
    let peer_lines = crate::resolve::peer_warnings(&peers, &provided);
    report.peers = peer_lines.clone();
    if !json {
        for w in peer_lines {
            println!("{}", w);
        }
    }
    report.fetched = FetchStats {
        count: fetched_count,
        bytes: fetched_bytes,
        from_store: tree.packages.len() as u64 - fetched_count,
    };
    report.linked_files = linked_files;
    if !json {
        println!(
            "  {} {} ({}) · {} from local store · {} files linked in {:.2}s",
            green("fetched"),
            fetched_count,
            crate::util::human(fetched_bytes),
            tree.packages.len() as u64 - fetched_count,
            linked_files,
            mat_secs
        );
    }
    // mirror-staleness fix: the tarball's package.json is the authoritative
    // description (a proxy's cached packument can be stale); refresh the
    // meta-cache on drift, one dim note when something actually changed
    let refreshed = refresh_descriptions(root, &tree);
    if !json && !refreshed.is_empty() {
        println!(
            "  {} refreshed cached description for {}",
            dim("note:"),
            dim(refreshed.join(", "))
        );
    }
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
            if !json {
                println!(
                    "  {} typescript {} ready (tsc/tsserver in .zap/bin, {})",
                    green("ts"),
                    ts_id.rsplit_once('@').map(|x| x.1).unwrap_or(""),
                    if managed { "zap-managed" } else { "user-managed" }
                );
            }
        }
    }
    if cfg.lifecycle {
        // npm order: root preinstall first, dependency lifecycles, then the
        // root's own install/postinstall; workspace members get nothing.
        // dep scripts are allowlist-gated (.zap-allow.json); the root's own
        // are trusted user code, gated by `lifecycle: true` alone
        let allow = crate::allow::load(root)?;
        let mut ran = run_root_lifecycle(root, &["preinstall"], &cfg, json);
        let (dep_ran, blocked) = run_lifecycle(root, &tree, &allow, &cfg, json);
        ran += dep_ran;
        ran += run_root_lifecycle(root, &["install", "postinstall"], &cfg, json);
        report.lifecycle = LifecycleStats {
            ran: ran as u32,
            blocked: blocked as u32,
        };
        if !json && ran > 0 {
            println!("  {} {} lifecycle scripts", green("ran"), ran);
        }
        if !json && blocked > 0 {
            println!(
                "  {} {} package{} lifecycle scripts but {} not allowlisted, {} to review",
                yellow("note:"),
                blocked,
                if blocked == 1 { " has" } else { "s have" },
                if blocked == 1 { "is" } else { "are" },
                cyan("`zap allow`")
            );
        }
    }

    report.done_packages = tree.packages.len();
    report.seconds = started.elapsed().as_secs_f64();
    if !json {
        println!(
            "{} {} packages in {:.2}s ({})",
            green("done"),
            tree.packages.len(),
            report.seconds,
            if cfg.linker == Linker::Pnp { "pnp" } else { "node-modules" }
        );
    }
    // tree-level delta vs the previous lockfile (the manifest edits live in
    // the caller's own output; the doc carries what's in the tree now)
    let (added, removed, changed) = lock_diff(prev_lock.as_ref(), &tree, &deps, &dev, &member_reqs);
    report.added = added;
    report.removed = removed;
    report.changed = changed;
    Ok((0, report))
}

/// the previous-vs-new tree delta: added ids with the manifest section that
/// pulls them in, removed ids, and same-name version changes
fn lock_diff(
    prev: Option<&crate::model::Lockfile>,
    tree: &Tree,
    deps: &BTreeMap<String, String>,
    dev: &BTreeMap<String, String>,
    member_reqs: &[BTreeMap<String, String>],
) -> (Vec<(String, String)>, Vec<String>, Vec<(String, String, String)>) {
    let prev_versions: BTreeMap<String, String> = prev
        .map(|l| {
            Tree::from(l)
                .packages
                .keys()
                .filter_map(|id| id.rsplit_once('@').map(|x| (x.0.to_string(), x.1.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let section_of = |name: &str| {
        if deps.contains_key(name) {
            "dependencies"
        } else if dev.contains_key(name) {
            "devDependencies"
        } else if member_reqs.iter().any(|m| m.contains_key(name)) {
            "workspace"
        } else {
            "transitive"
        }
    };
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for id in tree.packages.keys() {
        let Some((name, ver)) = id.rsplit_once('@') else { continue };
        match prev_versions.get(name) {
            None => added.push((id.clone(), section_of(name).to_string())),
            Some(old) if old != ver => changed.push((id.clone(), old.clone(), ver.to_string())),
            _ => {}
        }
    }
    for (name, ver) in &prev_versions {
        if !tree.packages.keys().any(|id| id.rsplit_once('@').map(|x| x.0 == name).unwrap_or(false)) {
            removed.push(format!("{name}@{ver}"));
        }
    }
    (added, removed, changed)
}

/// download missing packages into the global store, then hardlink everything into .zap/v
async fn materialize(
    root: &Path,
    tree: &Tree,
    reg: &crate::http::Registry,
    concurrency: usize,
    progress: Arc<crate::cmds::progress::Progress>,
    direct: Arc<HashSet<String>>,
) -> Result<(u64, u64, u64, f64)> {
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
        progress.stage("downloading");
        let total = jobs.len() as u64;
        progress.bump_jobs(0, total, 0);
        let sem = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
        let reg = reg.clone();
        let mut set = tokio::task::JoinSet::new();
        for (id, pkg) in jobs {
    let reg = reg.clone();
            let sem = sem.clone();
    let progress = progress.clone();
    let direct = direct.clone();
            set.spawn(async move {
                let _permit = sem.acquire_owned().await.context("semaphore closed")?;
                // a lane only for the user's declared surface (root + member
                // manifests); transitive downloads ride the aggregate bar
                let guard = crate::cmds::progress::lane_for(&id, &direct, &progress);
                let hook: Option<crate::cmds::progress::LaneHook> = guard.as_ref().map(|g| {
                    let h = g.hook();
                    std::sync::Arc::new(h) as crate::cmds::progress::LaneHook
                });
                let (_dir, bytes) =
                    crate::store::ensure_in_store(&reg, &id, &pkg, hook.as_deref()).await?;
                // queue the persistent `✓ downloaded` line before the slot frees
                if let Some(g) = &guard {
                    g.complete();
                }
                drop(guard); // the lane idles for the next queued job
                Ok::<(String, u64), anyhow::Error>((id, bytes))
            });
        }        while let Some(joined) = set.join_next().await {
            let (_id, bytes) = joined.context("download task panicked")??;
            fetched_bytes += bytes;
            if bytes > 0 {
                fetched_count += 1;
            }
            progress.bump_jobs(fetched_count, total, fetched_bytes);
        }
    }

    progress.stage("linking");
    let link_total = tree.packages.len() as u64;
    progress.bump_jobs(0, link_total, 0);
    let linked_files = link_all(&vroot, tree, concurrency, &progress).await?;
    // stats for install_in's summary line (printed after the bar clears there)
    Ok((fetched_bytes, fetched_count, linked_files, started.elapsed().as_secs_f64()))
}

/// description drift check: for every materialized package, the tarball's
/// package.json description wins over the cached packument's (proxy mirrors
/// go stale); returns the ids whose cache entry was rewritten. runs on every
/// install at any version, the read is local fs, the write only on drift
fn refresh_descriptions(root: &Path, tree: &Tree) -> Vec<String> {
    let vroot = root.join(".zap").join("v");
    let mut updated = Vec::new();
    for id in tree.packages.keys() {
        let Some((name, ver)) = id.rsplit_once('@') else { continue };
        let Ok(v) = crate::util::read_json(&vroot.join(crate::util::pkg_dir_name(id)).join("package.json")) else {
            continue;
        };
        let Some(desc) = v.get("description").and_then(|d| d.as_str()) else { continue };
        if crate::http::Registry::cache_update_description(name, ver, desc).unwrap_or(false) {
            updated.push(id.clone());
        }
    }
    updated
}

/// the user's declared surface: every id resolved for the root manifest
/// (deps + dev, both land in the root map) plus every workspace member's
/// resolved deps. transitive/internal packages (and zap-managed type deps)
/// stay out: their downloads ride the aggregate bar lane-less
fn direct_set(tree: &Tree) -> HashSet<String> {
    tree.root
        .values()
        .chain(tree.members.iter().flat_map(|m| m.deps.values()))
        .cloned()
        .collect()
}

/// link every package's store tree into the project's virtual store, in
/// parallel: per-package spawn_blocking tasks under the same semaphore as
/// the downloads (link_tree is sync FS work). packages link into independent
/// target dirs, so the tasks share nothing; returns the exact file count
async fn link_all(
    vroot: &Path,
    tree: &Tree,
    concurrency: usize,
    progress: &crate::cmds::progress::Progress,
) -> Result<u64> {
    let link_total = tree.packages.len() as u64;
    progress.bump_jobs(0, link_total, 0);
    let sem = Arc::new(tokio::sync::Semaphore::new(concurrency.max(1)));
    let mut set = tokio::task::JoinSet::new();
    for id in tree.packages.keys() {
        let from = crate::store::store_pkg_dir(id)?;
        let to = vroot.join(crate::util::pkg_dir_name(id));
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.context("semaphore closed")?;
            let n = tokio::task::spawn_blocking(move || crate::store::link_tree(&from, &to))
                .await
                .context("link task panicked")??;
            Ok::<u32, anyhow::Error>(n)
        });
    }
    let mut linked_files: u64 = 0;
    let mut link_done: u64 = 0;
    // fail-fast: the first error surfaces when its task drains, like downloads
    while let Some(joined) = set.join_next().await {
        let n = joined.context("link task panicked")??;
        linked_files += n as u64;
        link_done += 1;
        progress.bump_jobs(link_done, link_total, 0);
    }
    Ok(linked_files)
}

/// opt-in dependency lifecycle scripts (config: `lifecycle: true`), gated by
/// the package-hash allowlist, never run by default; returns (ran, blocked)
/// where blocked counts packages with lifecycle scripts that the gate skipped
fn run_lifecycle(root: &Path, tree: &Tree, allow: &crate::allow::AllowList, cfg: &Config, quiet: bool) -> (usize, usize) {
    let vroot = root.join(".zap").join("v");
    let mut ran = 0usize;
    let mut blocked = 0usize;
    for (id, pkg) in &tree.packages {
        let dir = vroot.join(crate::util::pkg_dir_name(id));
        let Ok(v) = crate::util::read_json(&dir.join("package.json")) else {
            continue;
        };
        let Some(scripts) = v.get("scripts").and_then(|x| x.as_object()) else {
            continue;
        };
        let has_scripts = ["preinstall", "install", "postinstall"].iter().any(|s| scripts.contains_key(*s));
        if !has_scripts {
            continue;
        }
        // the tree carries the lockfile's recorded integrity per package id;
        // trust-all (lifecycle-allow-all) bypasses the allowlist entirely
        if crate::allow::dep_gate(cfg.lifecycle_allow_all, allow, id, Some(&pkg.integrity)) != crate::allow::Gate::Allowed {
            blocked += 1;
            continue;
        }
        for stage in ["preinstall", "install", "postinstall"] {
            let Some(script) = scripts.get(stage).and_then(|x| x.as_str()) else {
                continue;
            };
            if run_lifecycle_stage(root, &dir, id, stage, script, cfg, quiet) {
                ran += 1;
            }
        }
    }
    (ran, blocked)
}

/// the root project's own install lifecycle hooks, same opt-in gate as deps
fn run_root_lifecycle(root: &Path, stages: &[&str], cfg: &Config, quiet: bool) -> usize {
    let Ok(v) = crate::util::read_json(&root.join("package.json")) else {
        return 0;
    };
    let Some(scripts) = v.get("scripts").and_then(|x| x.as_object()) else {
        return 0;
    };
    let mut ran = 0usize;
    for stage in stages {
        if let Some(script) = scripts.get(*stage).and_then(|x| x.as_str()) {
            if run_lifecycle_stage(root, root, "root", stage, script, cfg, quiet) {
                ran += 1;
            }
        }
    }
    ran
}

/// run one lifecycle stage in `dir`, npm env vars set; `what` names the owner
/// in warnings ("root" or the package id). quiet (json mode) captures the
/// output instead of inheriting stdio so the document stays alone on stdout
fn run_lifecycle_stage(root: &Path, dir: &Path, what: &str, stage: &str, script: &str, cfg: &Config, quiet: bool) -> bool {
    let mut c = crate::util::shell_command(script);
    c.current_dir(dir);
    crate::pnp::apply_env(&mut c, root, cfg.linker);
    c.env("npm_lifecycle_event", stage);
    c.env("INIT_CWD", dir.to_string_lossy().to_string());
    let ok = if quiet {
        match c.output() {
            Ok(o) if o.status.success() => true,
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                eprintln!(
                    "{} lifecycle `{}` in {} exited with {}{}",
                    yellow("warning:"),
                    stage,
                    what,
                    o.status.code().unwrap_or(1),
                    if err.trim().is_empty() { String::new() } else { format!(": {}", err.trim()) }
                );
                false
            }
            Err(e) => {
                eprintln!("{} lifecycle `{}` in {} failed: {}", yellow("warning:"), stage, what, e);
                false
            }
        }
    } else {
        match c.status() {
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
    // outside any project there's no local tree: None lets the caller fall
    // back to dlx instead of erroring (npx-style remote run from anywhere)
    let Ok(ctx) = crate::util::find_root() else {
        return Ok(None);
    };
    find_local_bin_in(&install_root_for(&ctx), name)
}

/// the lookup against one install root (testable without cwd games)
pub fn find_local_bin_in(install_root: &Path, name: &str) -> Result<Option<(PathBuf, PathBuf)>> {
    let Some(lock) = crate::lockfile::read(&install_root.join(".zap-lock.json"))? else {
        return Ok(None);
    };
    let tree = Tree::from(&lock);
    let vroot = install_root.join(".zap").join("v");
    for (id, pkg) in &tree.packages {
        if let Some(rel) = pkg.bin.get(name) {
            return Ok(Some((
                vroot.join(crate::util::pkg_dir_name(id)).join(rel.trim_start_matches("./")),
                install_root.to_path_buf(),
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
            return Ok(Some((member.join(rel.trim_start_matches("./")), install_root.to_path_buf())));
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

/// test-only: one process-wide lock around ZAP_HOME env mutation, cargo
/// runs test threads in parallel and the env leaks process-wide; poison-
/// healing so one panic doesn't cascade
#[cfg(test)]
pub(crate) fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
    L.lock().unwrap_or_else(|p| p.into_inner())
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
            let new = format!("{}{}\n{}\n{}", &content[..start], block, "", content[end..].trim_start_matches('\n'));
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

// gitignore

/// zap runtime entries a project's .gitignore should carry (mirrors zap's own
/// repo .gitignore). `.zap-lock.json` is deliberately absent, lockfiles are
/// committed for reproducibility
const RUNTIME_IGNORES: [&str; 6] = [".zap/", "node_modules/", ".pnp.cjs", ".pnp.json", ".pnp.mjs", ".pnp-resolve.mjs"];

/// make sure the project's .gitignore carries the zap runtime entries:
/// appends only what's missing under a `# zap runtime` block; creates the
/// file when a git repo exists (walking up), no-op otherwise so non-git
/// dirs don't get littered. Ok(Some(state)) / Ok(None) for callers to note
pub fn ensure_gitignore(root: &Path) -> Result<Option<&'static str>> {
    let path = root.join(".gitignore");
    let exists = path.is_file();
    if !exists && !git_repo_above(root) {
        return Ok(None);
    }
    let content = if exists { std::fs::read_to_string(&path)? } else { String::new() };
    // trim-compare so `.zap` and `.zap/` both count as present
    let have: Vec<String> = content
        .lines()
        .map(|l| l.trim().trim_end_matches('/').to_string())
        .collect();
    let missing: Vec<&str> = RUNTIME_IGNORES
        .iter()
        .copied()
        .filter(|e| !have.contains(&e.trim_end_matches('/').to_string()))
        .collect();
    if missing.is_empty() {
        return Ok(None);
    }
    let mut out = content;
    if !out.is_empty() {
        // sane trailing-newline state: append from a fresh line, blank line
        // between the user's content and the zap block
        if !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str("# zap runtime\n");
    } else {
        out.push_str("# zap runtime state\n");
    }
    for entry in &missing {
        out.push_str(entry);
        out.push('\n');
    }
    std::fs::write(&path, out)?;
    Ok(if exists { Some("updated") } else { Some("created") })
}

/// the one-line note callers print when ensure_gitignore did something
pub fn print_gitignore_note(state: &str) {
    if state == "created" {
        println!("  {} .gitignore created", green("✓"));
    } else {
        println!("  {} .gitignore updated (zap runtime entries)", green("✓"));
    }
}

/// is any ancestor of root (root itself included) a git repo? `.git` may be a
/// dir or a file (worktrees/submodules); same walk style as find_root
fn git_repo_above(root: &Path) -> bool {
    let mut dir = Some(root.to_path_buf());
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return true;
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// env-touching tests serialize here: ZAP_HOME leaks process-wide and
    /// cargo runs tests in parallel threads
    
    // direct-set builder: root deps + every member's deps, ids resolved
    // global installs (offline: seeded meta-cache + store)

    /// an offline home + a bin-bearing fake package: bin map with two entries
    fn seed_global_pkg(name: &str, bins: &[(&str, &str)]) -> PathBuf {
        let home = tempdir("ghome");
        std::env::set_var("ZAP_HOME", &home);
        let cache = crate::util::meta_cache().unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        let ver = "1.0.0";
        let mut bin_map = serde_json::Map::new();
        let mut store_files: Vec<(String, String)> = Vec::new();
        for (bin, rel) in bins {
            bin_map.insert(bin.to_string(), serde_json::json!(rel));
            store_files.push((rel.to_string(), format!("console.log('{} bin');", bin)));
        }
        let pack = serde_json::json!({
            "name": name,
            "versions": { ver: { "version": ver, "bin": bin_map, "dist": { "tarball": format!("https://reg.invalid/{}-{}.tgz", name, ver) } } },
            "dist-tags": { "latest": ver },
        });
        let key = crate::util::sha256_hex(name.as_bytes());
        let wrapped = serde_json::json!({
            "t": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
            "body": pack,
        });
        std::fs::write(cache.join(format!("{}.json", key)), wrapped.to_string()).unwrap();
        let dir = crate::store::store_pkg_dir(&format!("{}@{}", name, ver)).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), format!("{{\"name\":\"{}\",\"version\":\"{}\",\"bin\":{}}}", name, ver, serde_json::to_string(&bin_map).unwrap())).unwrap();
        for (rel, content) in store_files {
            let p = dir.join(rel.trim_start_matches("./"));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        std::fs::write(dir.join(".zap-ok"), "ok").unwrap();
        home
    }

    #[test]
    fn global_add_ls_remove_roundtrip_offline() {
        let _env = crate::cmds::env_test_lock();
        let home = seed_global_pkg("fakebin", &[("fakebin", "./cli.js")]);
        let g = crate::cmds::global_root().unwrap();
        let bin_dir = crate::cmds::global_bin_dir().unwrap();

        // global add works with NO project context: run() never calls find_root
        let code = crate::cmds::global_add(&["-g".to_string(), "fakebin".to_string()]).unwrap();
        assert_eq!(code, 0);
        let dir = g.join("fakebin");
        assert!(dir.join(".zap-lock.json").is_file(), "its own isolated install ran");
        assert!(dir.join(".pnp.cjs").is_file(), "pnp runtime in the global dir");
        // the shim landed in ~/.zap/bin with an absolute path + the pnp hook
        let shim = bin_dir.join(crate::pnp::shim_file_name("fakebin"));
        assert!(shim.is_file(), "shim written");
        let content = std::fs::read_to_string(&shim).unwrap();
        let cli = dir.join(".zap").join("v").join("fakebin@1.0.0").join("cli.js");
        assert!(content.contains(&cli.display().to_string()), "absolute bin path embedded");
        assert!(content.contains("NODE_OPTIONS"), "pnp hook embedded (default linker)");
        assert!(cli.is_file(), "the bin landed in the global's vroot");

        // ls -g: the global root holds the package (the version comes from its lock)
        let lock = crate::lockfile::read(&dir.join(".zap-lock.json")).unwrap().unwrap();
        assert_eq!(lock.root.get("fakebin").map(|s| s.as_str()), Some("fakebin@1.0.0"));
        crate::cmds::global_ls().unwrap();

        // remove -g: the dir AND the shim go; a second remove errors
        let code = crate::cmds::global_remove(&["-g".to_string(), "fakebin".to_string()]).unwrap();
        assert_eq!(code, 0);
        assert!(!dir.exists(), "global dir removed");
        assert!(!shim.exists(), "shim removed by the content scan");
        assert!(crate::cmds::global_remove(&["-g".to_string(), "fakebin".to_string()]).is_err(), "removing a non-global errors");
        std::env::remove_var("ZAP_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn global_multi_bin_package_shims_every_bin() {
        let _env = crate::cmds::env_test_lock();
        let home = seed_global_pkg("multibin", &[("alfa", "./a.js"), ("beta", "./b.js")]);
        let bin_dir = crate::cmds::global_bin_dir().unwrap();
        crate::cmds::global_add(&["-g".to_string(), "multibin".to_string()]).unwrap();
        for (bin, script) in [("alfa", "a.js"), ("beta", "b.js")] {
            let shim = bin_dir.join(crate::pnp::shim_file_name(bin));
            assert!(
                shim.is_file(),
                "bin {} shimmed; bin_dir = {:?}",
                bin,
                std::fs::read_dir(&bin_dir).map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect::<Vec<_>>()).unwrap_or_default()
            );
            let content = std::fs::read_to_string(&shim).unwrap();
            assert!(
                content.contains(script),
                "shim {} points at its own script {}: {:?}",
                bin,
                script,
                content.replace('\r', "")
            );
        }
        let _ = std::fs::remove_dir_all(crate::cmds::global_root().unwrap());
        std::env::remove_var("ZAP_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn direct_set_covers_root_and_members_only() {
            let tree = Tree {
                root: BTreeMap::from([
                    ("ms".to_string(), "ms@2.1.3".to_string()),
                    ("chalk".to_string(), "chalk@4.1.2".to_string()),
                ]),
                packages: BTreeMap::new(),
                fallback: BTreeMap::new(),
                members: vec![LockMember {
                    name: "w".to_string(),
                    dir: "packages/w".to_string(),
                    deps: BTreeMap::from([("left-pad".to_string(), "left-pad@1.3.0".to_string())]),
                }],
                types: BTreeMap::from([("typescript".to_string(), "typescript@5.9.3".to_string())]),
            };
            let s = direct_set(&tree);
    assert!(s.contains("ms@2.1.3") && s.contains("chalk@4.1.2"), "root deps direct");
    assert!(s.contains("left-pad@1.3.0"), "member deps direct");
    assert!(!s.contains("typescript@5.9.3"), "zap-managed types stay lane-less");
    assert_eq!(s.len(), 3);
        }

    #[test]
    fn lane_claims_filter_to_the_direct_set() {
            // an ACTIVE bar (probe bypassed): only ids in the direct set claim
    let progress = Arc::new(crate::cmds::progress::Progress::start_with(true, 4));
    let direct: HashSet<String> = HashSet::from(["top@1.0.0".to_string()]);
    let g1 = crate::cmds::progress::lane_for("top@1.0.0", &direct, &progress);
    assert!(g1.is_some(), "a direct dep claims a lane");
    assert!(crate::cmds::progress::lane_for("mid@1.0.0", &direct, &progress).is_none(), "transitive rides lane-less");
    assert!(crate::cmds::progress::lane_for("leaf@1.0.0", &direct, &progress).is_none());
            g1.unwrap().complete();
            // the completed line persists in the queue
    assert_eq!(progress.lane_history(), vec!["top@1.0.0".to_string()]);
            progress.finish();
        }

    #[test]
    fn lanes_offline_flow_keeps_the_aggregate_exact() {
            // seeded top->mid->leaf chain, fully store-seeded (offline): the
            // aggregate covers all 3, no download jobs (all store hits) so no
            // lanes, the wiring must not choke on that
    let _env = crate::cmds::env_test_lock();
    let home = tempdir("home");
    std::env::set_var("ZAP_HOME", &home);
    let proj = tempdir("proj");
    let cache = crate::util::meta_cache().unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    let seed = |name: &str, ver: &str, deps: &[(&str, &str)]| {
                let dmap: serde_json::Map<String, serde_json::Value> = deps
                    .iter()
                    .map(|(n, r)| (n.to_string(), serde_json::json!(r)))
                    .collect();
                let pack = serde_json::json!({
                    "name": name,
                    "versions": { ver: { "version": ver, "dependencies": dmap, "dist": { "tarball": format!("https://reg.invalid/{}-{}.tgz", name, ver) } } },
                    "dist-tags": { "latest": ver },
                });
                let key = crate::util::sha256_hex(name.as_bytes());
                let wrapped = serde_json::json!({
                    "t": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                    "body": pack,
                });
                std::fs::write(cache.join(format!("{}.json", key)), wrapped.to_string()).unwrap();
                let dir = crate::store::store_pkg_dir(&format!("{}@{}", name, ver)).unwrap();
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("package.json"), format!("{{\"name\":\"{}\",\"version\":\"{}\"}}", name, ver)).unwrap();
                std::fs::write(dir.join(".zap-ok"), "ok").unwrap();
            };
            seed("top", "1.0.0", &[("mid", "^1.0.0")]);
            seed("mid", "1.0.0", &[("leaf", "^1.0.0")]);
            seed("leaf", "1.0.0", &[]);
            std::fs::write(
                proj.join("package.json"),
                "{\"name\":\"lanes\",\"version\":\"1.0.0\",\"dependencies\":{\"top\":\"^1.0.0\"}}\n",
            )
            .unwrap();

    let reg = crate::http::Registry::new(false, false, false, 24, None).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let mut members: Vec<LockMember> = Vec::new();
    let root_deps = BTreeMap::from([("top".to_string(), "^1.0.0".to_string())]);
    let (tree, _, _, _) = rt
                .block_on(crate::resolve::resolve(&reg, &root_deps, &mut members, &[], &BTreeMap::new(), &BTreeMap::new()))
                .unwrap();
    assert_eq!(tree.packages.len(), 3, "top, mid, leaf all resolve");

    let progress = Arc::new(crate::cmds::progress::Progress::start_with(true, 4));
    let (fetched_bytes, fetched_count, linked, _) = rt
                .block_on(materialize(
                    &proj,
                    &tree,
                    &reg,
                    4,
                    progress.clone(),
                    Arc::new(direct_set(&tree)),
                ))
                .unwrap();
            progress.finish();
    std::env::remove_var("ZAP_HOME");

            // the aggregate covered the whole chain; store hits = no jobs
    assert_eq!(fetched_count, 0);
    assert_eq!(fetched_bytes, 0);
    assert_eq!(linked, 3, "three packages linked");
    assert!(progress.lane_history().is_empty(), "no downloads, no lanes");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&proj);
        }

    // dlx / x outside projects (offline: seeded dlx cache)

    #[test]
    fn find_local_bin_in_looks_up_the_tree_or_none() {
        let proj = tempdir("localbin");
        // no lockfile: None (the caller falls back to dlx)
        assert_eq!(find_local_bin_in(&proj, "whatever").unwrap(), None);
        // a seeded lock + vroot: Some((bin file, install root))
        let lock = crate::model::Lockfile {
            version: 2,
            root_hash: "x".to_string(),
            root: BTreeMap::from([("fakebin".to_string(), "fakebin@1.0.0".to_string())]),
            packages: BTreeMap::from([(
                "fakebin@1.0.0".to_string(),
                LockPackage {
                    integrity: String::new(),
                    tarball: String::new(),
                    deps: BTreeMap::new(),
                    bin: BTreeMap::from([("fakebin".to_string(), "./cli.js".to_string())]),
                },
            )]),
            fallback: BTreeMap::new(),
            workspaces: Vec::new(),
            types: BTreeMap::new(),
        };
        std::fs::write(proj.join(".zap-lock.json"), serde_json::to_vec_pretty(&lock).unwrap()).unwrap();
        std::fs::create_dir_all(proj.join(".zap").join("v").join("fakebin@1.0.0")).unwrap();
        std::fs::write(proj.join(".zap").join("v").join("fakebin@1.0.0").join("cli.js"), "ok").unwrap();
        let (bin_file, root) = find_local_bin_in(&proj, "fakebin").unwrap().unwrap();
        assert_eq!(root, proj);
        assert!(bin_file.ends_with("cli.js") && bin_file.is_file());
        // an unknown bin: None
        assert_eq!(find_local_bin_in(&proj, "nope").unwrap(), None);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn dlx_run_works_from_a_seeded_cache_offline() {
        // the dlx area is project-independent: seed its packument + the store,
        // then dlx_run installs into ~/.zap/dlx/<key> and runs the bin, the
        // test's cwd is the repo (a project!) and that's irrelevant to the path
        let _env = crate::cmds::env_test_lock();
        let home = tempdir("dlxhome");
        std::env::set_var("ZAP_HOME", &home);
        let cache = crate::util::meta_cache().unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        let ver = "1.0.0";
        let pack = serde_json::json!({
            "name": "fakebin",
            "versions": { ver: { "version": ver, "bin": { "fakebin": "./cli.js" }, "dist": { "tarball": "https://reg.invalid/fakebin-1.0.0.tgz" } } },
            "dist-tags": { "latest": ver },
        });
        let key = crate::util::sha256_hex("fakebin".as_bytes());
        let wrapped = serde_json::json!({
            "t": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
            "body": pack,
        });
        std::fs::write(cache.join(format!("{}.json", key)), wrapped.to_string()).unwrap();
        let dir = crate::store::store_pkg_dir(&format!("fakebin@{}", ver)).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), format!("{{\"name\":\"fakebin\",\"version\":\"{}\",\"bin\":{{\"fakebin\":\"./cli.js\"}}}}", ver)).unwrap();
        std::fs::write(dir.join("cli.js"), "console.log('fakebin ran');").unwrap();
        std::fs::write(dir.join(".zap-ok"), "ok").unwrap();

        // dlx installs the one-package manifest into the dlx area and runs it
        let code = dlx_run("fakebin", String::new(), vec![]).unwrap();
        assert_eq!(code, 0, "the bin ran under the dlx cache's pnp env");
        let dlx_dir = crate::util::zap_home().unwrap().join("dlx").join(crate::util::slug("fakebin"));
        assert!(dlx_dir.join(".pnp.json").is_file(), "the dlx cache holds its own pnp runtime");
        // a second run reuses the cache (the dir already built)
        let code = dlx_run("fakebin", String::new(), vec![]).unwrap();
        assert_eq!(code, 0);
        std::env::remove_var("ZAP_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn parallel_link_all_is_exact_and_complete() {
        let _env = crate::cmds::env_test_lock();
        let home = tempdir("home");
        std::env::set_var("ZAP_HOME", &home);
        let proj = tempdir("proj");
        let vroot = proj.join(".zap").join("v");

        // three fake packages in the store, nested files each
        let ids = ["fake-a@1.0.0", "fake-b@2.1.0", "@scope/pkg@0.2.3"];
        let mut packages = BTreeMap::new();
        for (i, id) in ids.iter().enumerate() {
            let dir = crate::store::store_pkg_dir(id).unwrap();
            for f in 0..(i + 1) {
                // nested dirs: a/b/file<k>.txt
                let p = dir.join("a").join("b").join(format!("file{}.txt", f));
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, format!("{}-{}", id, f)).unwrap();
            }
            packages.insert(
                id.to_string(),
                LockPackage {
                    integrity: String::new(),
                    tarball: String::new(),
                    deps: BTreeMap::new(),
                    bin: BTreeMap::new(),
                },
            );
        }
        let tree = Tree {
            root: BTreeMap::new(),
            packages,
            fallback: BTreeMap::new(),
            members: Vec::new(),
            types: BTreeMap::new(),
        };

        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let progress = Arc::new(crate::cmds::progress::Progress::start_with(false, 4));
        let linked = rt.block_on(link_all(&vroot, &tree, 4, &progress)).unwrap();
        std::env::remove_var("ZAP_HOME");

        // exact count: 1 + 2 + 3 files, and every file present with content
        assert_eq!(linked, 6, "per-package file counts sum exactly");
        for (i, id) in ids.iter().enumerate() {
            for f in 0..(i + 1) {
                let p = vroot
                    .join(crate::util::pkg_dir_name(id))
                    .join("a")
                    .join("b")
                    .join(format!("file{}.txt", f));
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                assert_eq!(text, format!("{}-{}", id, f), "{} landed with content", p.display());
            }
        }
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&proj);
    }

    // manifest <-> installation drift (offline: seeded meta-cache + store)

    /// a fake ZAP_HOME with the packuments in the meta-cache and the packages'
    /// store dirs pre-extracted (with .zap-ok markers), install runs with
    /// zero network
    fn offline_setup(packages: &[(&str, &str)]) -> (PathBuf, PathBuf) {
        let home = tempdir("home");
        std::env::set_var("ZAP_HOME", &home);
        let proj = tempdir("proj");
        // packuments: {"versions": {ver: {version, dist:{tarball}}}, "dist-tags": {latest: ver}}
        for (name, ver) in packages {
            let name = name.to_string();
            let ver = ver.to_string();
            let pack = serde_json::json!({
                "name": name,
                "versions": { ver.clone(): { "version": ver.clone(), "dist": { "tarball": format!("https://registry.npmjs.org/{}/-/{}-{}.tgz", name, name, ver) } } },
                "dist-tags": { "latest": ver },
            });
            let key = crate::util::sha256_hex(name.as_bytes());
            // mirror http.rs's envelope exactly: {"t": now, "body": pack}
            let wrapped = serde_json::json!({
                "t": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                "body": pack,
            });
    std::fs::create_dir_all(crate::util::meta_cache().unwrap()).unwrap();
            std::fs::write(
                crate::util::meta_cache().unwrap().join(format!("{}.json", key)),
                wrapped.to_string(),
            )
            .unwrap();
            // the store copy, extracted + ok-marked so ensure_in_store hits it
            let dir = crate::store::store_pkg_dir(&format!("{}@{}", name, ver)).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("package.json"), format!("{{\"name\":\"{}\",\"version\":\"{}\"}}", name, ver)).unwrap();
            std::fs::write(dir.join(".zap-ok"), "ok").unwrap();
        }
        (home, proj)
    }

    fn manifest_write(proj: &Path, deps: &[(&str, &str)]) {
        let m: serde_json::Map<String, serde_json::Value> = deps
            .iter()
            .map(|(n, r)| (n.to_string(), serde_json::json!(r)))
            .collect();
        std::fs::write(
            proj.join("package.json"),
            format!("{}\n", serde_json::to_string_pretty(&serde_json::json!({ "name": "drift", "version": "1.0.0", "dependencies": m })).unwrap()),
        )
        .unwrap();
    }

    #[test]
    fn hand_added_dep_gets_resolved_and_installed() {
        let _env = crate::cmds::env_test_lock();
        let (home, proj) = offline_setup(&[("ms", "2.1.3"), ("left-pad", "1.3.0")]);
        std::fs::write(proj.join(".zaprc.yaml"), "linker: node-modules\n").unwrap();
        manifest_write(&proj, &[("ms", "^2.0.0")]);
        install_in(&proj, &[], None).unwrap();

        // hand-add a dep (no lockfile knowledge of it) -> install picks it up
        manifest_write(&proj, &[("ms", "^2.0.0"), ("left-pad", "^1.0.0")]);
        install_in(&proj, &[], None).unwrap();

        let lock = crate::lockfile::read(&proj.join(".zap-lock.json")).unwrap().unwrap();
        assert!(lock.root.contains_key("left-pad"), "forward drift resolved into the lockfile");
        assert!(proj.join(".zap").join("v").join("left-pad@1.3.0").is_dir(), "materialized");
        assert!(proj.join("node_modules").join("left-pad").exists(), "junction created");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn hand_removed_dep_disappears_everywhere() {
        let _env = crate::cmds::env_test_lock();
        let (home, proj) = offline_setup(&[("ms", "2.1.3"), ("left-pad", "1.3.0")]);
        std::fs::write(proj.join(".zaprc.yaml"), "linker: node-modules\n").unwrap();
        manifest_write(&proj, &[("ms", "^2.0.0"), ("left-pad", "^1.0.0")]);
        install_in(&proj, &[], None).unwrap();
        assert!(proj.join("node_modules").join("left-pad").exists());

        // hand-remove -> install prunes lockfile, vroot and the junction
        manifest_write(&proj, &[("ms", "^2.0.0")]);
        install_in(&proj, &[], None).unwrap();

        let lock = crate::lockfile::read(&proj.join(".zap-lock.json")).unwrap().unwrap();
        assert!(!lock.root.contains_key("left-pad"), "gone from the lockfile");
        assert!(!proj.join(".zap").join("v").join("left-pad@1.3.0").exists(), "gone from .zap/v");
        assert!(!proj.join("node_modules").join("left-pad").exists(), "junction pruned");
        // no collateral: the surviving dep's junction still works
        assert!(proj.join("node_modules").join("ms").exists(), "survivor untouched");
        // and the pnp artifacts are gone with it (node-modules linker removed the runtime)
        assert!(!proj.join(".pnp.cjs").exists());
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn frozen_lockfile_rejects_drift_without_mutating() {
        let _env = crate::cmds::env_test_lock();
        let (home, proj) = offline_setup(&[("ms", "2.1.3"), ("left-pad", "1.3.0")]);
        manifest_write(&proj, &[("ms", "^2.0.0")]);
        install_in(&proj, &[], None).unwrap();

        manifest_write(&proj, &[("ms", "^2.0.0"), ("left-pad", "^1.0.0")]);
        let lock_before = std::fs::read(proj.join(".zap-lock.json")).unwrap();
        let v_before = std::fs::read_dir(proj.join(".zap").join("v")).unwrap().count();
        assert!(
            install_in(&proj, &["--frozen-lockfile".to_string()], None).is_err(),
            "frozen + drift = error, not a silent re-resolve"
        );
        assert_eq!(std::fs::read(proj.join(".zap-lock.json")).unwrap(), lock_before, "lock untouched");
        assert_eq!(std::fs::read_dir(proj.join(".zap").join("v")).unwrap().count(), v_before, "vroot untouched");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&proj);
    }

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zap-gitignore-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn appends_only_the_missing_entries() {
        let dir = tempdir("append");
        std::fs::write(dir.join(".gitignore"), "node_modules/\n# build\ndist/").unwrap();
        let state = ensure_gitignore(&dir).unwrap().unwrap();
        assert_eq!(state, "updated");
        let text = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert_eq!(text.matches("node_modules/").count(), 1, "existing entry not duplicated");
        assert!(text.contains("# build") && text.contains("dist/"), "unrelated content intact");
        assert!(text.contains("# zap runtime"));
        for e in [".zap/", ".pnp.cjs", ".pnp.json", ".pnp.mjs", ".pnp-resolve.mjs"] {
    assert!(text.contains(e), "{} appended", e);
        }
        // the no-trailing-newline source kept its content on its own line
        assert!(text.contains("dist/\n\n# zap runtime"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn complete_file_is_a_noop() {
        let dir = tempdir("noop");
        std::fs::write(
            dir.join(".gitignore"),
            ".zap\nnode_modules\n.pnp.cjs\n.pnp.json\n.pnp.mjs\n.pnp-resolve.mjs\n",
        )
        .unwrap();
        let before = std::fs::read(dir.join(".gitignore")).unwrap();
        assert_eq!(ensure_gitignore(&dir).unwrap(), None);
        assert_eq!(std::fs::read(dir.join(".gitignore")).unwrap(), before, "file untouched");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn creates_with_full_block_when_git_present() {
        let dir = tempdir("create");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        assert_eq!(ensure_gitignore(&dir).unwrap().unwrap(), "created");
        let text = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
        assert!(text.starts_with("# zap runtime state"));
        assert_eq!(text.lines().count(), 7, "header + all six entries");
        // idempotent: a second pass is a no-op
        assert_eq!(ensure_gitignore(&dir).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_non_git_dirs() {
        let dir = tempdir("nogit");
        assert_eq!(ensure_gitignore(&dir).unwrap(), None);
        assert!(!dir.join(".gitignore").exists(), "nothing created");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_walks_up_to_the_nearest_repo() {
        let base = tempdir("walk");
        let deep = base.join("a/b/proj");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(base.join(".git")).unwrap();
        assert_eq!(ensure_gitignore(&deep).unwrap().unwrap(), "created");
        assert!(deep.join(".gitignore").is_file());
        let _ = std::fs::remove_dir_all(&base);
    }

    // description refresh (offline: seeded meta-cache + store)

    #[test]
    fn install_refreshes_stale_cached_descriptions() {
        // a proxy mirror's cached packument carries a stale description; the
        // store tarball's package.json is authoritative: install must rewrite
        // the cache entry, and inspect must then render the real words
        let _env = crate::cmds::env_test_lock();
        let home = tempdir("desc");
        std::env::set_var("ZAP_HOME", &home);
        let proj = tempdir("descproj");
        let cache = crate::util::meta_cache().unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        let pack = serde_json::json!({
            "name": "staledesc",
            "versions": { "1.0.0": { "version": "1.0.0", "description": "stale mirror words", "dist": { "tarball": "https://reg.invalid/staledesc-1.0.0.tgz" } } },
            "dist-tags": { "latest": "1.0.0" },
        });
        let key = crate::util::sha256_hex("staledesc".as_bytes());
        let wrapped = serde_json::json!({
            "t": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
            "body": pack,
        });
        std::fs::write(cache.join(format!("{}.json", key)), wrapped.to_string()).unwrap();
        // the store copy (extracted + ok-marked) holds the right description
        let dir = crate::store::store_pkg_dir("staledesc@1.0.0").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            "{\"name\":\"staledesc\",\"version\":\"1.0.0\",\"description\":\"the real words\"}",
        )
        .unwrap();
        std::fs::write(dir.join(".zap-ok"), "ok").unwrap();
        std::fs::write(
            proj.join("package.json"),
            "{\"name\":\"drift\",\"version\":\"1.0.0\",\"dependencies\":{\"staledesc\":\"^1.0.0\"}}\n",
        )
        .unwrap();

        install_in(&proj, &[], None).unwrap();

        // the meta-cache entry now carries the tarball's description
        let text = std::fs::read_to_string(cache.join(format!("{}.json", key))).unwrap();
        assert!(text.contains("the real words"), "cache refreshed: {}", text);
        assert!(!text.contains("stale mirror words"), "stale words gone: {}", text);
        // and inspect renders the refreshed packument, offline, from the cache
        let reg = crate::http::Registry::new(false, false, true, 24, None).unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let pack = rt.block_on(reg.packument("staledesc")).unwrap();
        let out = crate::cmds::info::inspect::render("staledesc", &pack, "1.0.0");
        assert!(out.contains("staledesc@1.0.0"), "header: {}", out);
        assert!(out.contains("- ") && out.contains("the real words"), "refreshed description: {}", out);
        std::env::remove_var("ZAP_HOME");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&proj);
    }

    // install report (the json mode's document, collected via install_in_core)

    #[test]
    fn install_report_tracks_the_tree_delta() {
        let _env = crate::cmds::env_test_lock();
        let (home, proj) = offline_setup(&[("ms", "2.1.3"), ("left-pad", "1.3.0")]);
        std::fs::write(proj.join(".zaprc.yaml"), "linker: node-modules\n").unwrap();
        manifest_write(&proj, &[("ms", "^2.0.0")]);

        // first install: ms enters the tree from nothing
        let (_, r1) = install_in_core(&proj, &[], None, false).unwrap();
        assert_eq!(r1.added, vec![("ms@2.1.3".to_string(), "dependencies".to_string())]);
        assert!(r1.removed.is_empty() && r1.changed.is_empty());
        assert_eq!(r1.fetched.count, 0, "store-seeded: nothing fetched");
        assert_eq!(r1.fetched.from_store, 1);
        assert!(r1.linked_files > 0, "link_all's exact count carried");
        assert_eq!(r1.done_packages, 1);

        // hand-add left-pad: the second report sees only the new id
        manifest_write(&proj, &[("ms", "^2.0.0"), ("left-pad", "^1.0.0")]);
        let (_, r2) = install_in_core(&proj, &[], None, false).unwrap();
        assert_eq!(r2.added, vec![("left-pad@1.3.0".to_string(), "dependencies".to_string())]);
        assert_eq!(r2.fetched.count, 0);
        assert_eq!(r2.fetched.from_store, 2);
        assert_eq!(r2.done_packages, 2);

        // hand-remove left-pad: the removal lands in the report
        manifest_write(&proj, &[("ms", "^2.0.0")]);
        let (_, r3) = install_in_core(&proj, &[], None, false).unwrap();
        assert_eq!(r3.removed, vec!["left-pad@1.3.0".to_string()]);
        assert!(r3.added.is_empty() && r3.changed.is_empty());
        assert_eq!(r3.done_packages, 1);

        // no-op install: empty delta, tree intact
        let (_, r4) = install_in_core(&proj, &[], None, false).unwrap();
        assert!(r4.added.is_empty() && r4.removed.is_empty() && r4.changed.is_empty());
        assert_eq!(r4.done_packages, 1);
        assert_eq!(r4.lifecycle.ran, 0, "lifecycle not enabled in this fixture");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn install_json_mode_prints_only_the_document() {
        // the json flag inside install_in: the doc must be the ONLY stdout
        let _env = crate::cmds::env_test_lock();
        let (home, proj) = offline_setup(&[("ms", "2.1.3")]);
        std::fs::write(proj.join(".zaprc.yaml"), "linker: node-modules\n").unwrap();
        manifest_write(&proj, &[("ms", "^2.0.0")]);
        // main.rs's heading suppression isn't part of install_in; run() paths
        // get it there. here the core is called directly with the flag
        let code = install_in(&proj, &["--json".to_string()], None).unwrap();
        assert_eq!(code, 0);
        // report fields parse back out of a freshly built doc
        let (_, report) = install_in_core(&proj, &["--json".to_string()], None, true).unwrap();
        let doc = serde_json::to_value(&report).unwrap();
        assert!(doc["donePackages"].as_u64().is_some(), "camelCase doc: {}", doc);
        assert!(doc["fetched"]["fromStore"].as_u64().is_some());
        assert!(doc["lifecycle"]["ran"].as_u64().is_some());
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&proj);
    }
}
