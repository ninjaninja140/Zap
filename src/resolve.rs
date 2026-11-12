use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::http::Registry;
use crate::model::{LockMember, LockPackage, Packument, Tree};

/// what required a package: the project root, a workspace member, another package, or zap-managed type deps (@types/*, typescript)
#[derive(Clone)]
enum Parent {
    Root,
    Member(usize),
    Pkg(String),
    Types,
}

/// the npm os name for a rust OS name (std::env::consts::OS vocabulary)
fn os_name(os: &str) -> &str {
    match os {
        "windows" => "win32",
        "macos" => "darwin",
        "solaris" | "illumos" => "sunos",
        other => other,
    }
}

/// the npm cpu name for a rust arch name (std::env::consts::ARCH vocabulary)
fn arch_name(arch: &str) -> &str {
    match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        "powerpc64" => "ppc64",
        "loongarch64" => "loong64",
        other => other,
    }
}

/// the npm (os, cpu) names of this host
pub fn host_platform() -> (&'static str, &'static str) {
    (os_name(std::env::consts::OS), arch_name(std::env::consts::ARCH))
}

/// does a package's platform metadata accept this host? missing or empty
/// lists match anything (the common case); a declared list matches when ANY
/// entry equals the host name. os/cpu may also arrive as a single string
/// (pure, tested)
fn platform_accepts(os: Option<&Vec<String>>, cpu: Option<&Vec<String>>, host_os: &str, host_cpu: &str) -> bool {
    let ok = |l: Option<&Vec<String>>, host: &str| match l {
        None => true,
        Some(l) if l.is_empty() => true,
        Some(l) => l.iter().any(|x| x == host),
    };
    ok(os, host_os) && ok(cpu, host_cpu)
}

/// a package version's declared platforms: the os/cpu manifest fields, as
/// json arrays or single strings; None = not declared
fn declared_platforms(vi: &crate::model::VersionInfo) -> (Option<Vec<String>>, Option<Vec<String>>) {
    let list = |v: Option<&serde_json::Value>| match v {
        Some(serde_json::Value::Array(a)) => {
            Some(a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<String>>())
        }
        Some(serde_json::Value::String(s)) => Some(vec![s.clone()]),
        _ => None,
    };
    (list(vi.extra.get("os")), list(vi.extra.get("cpu")))
}

/// BFS dependency resolution from the root package.json + workspace members + type deps.
/// Wave-based so each packument is fetched at most once per install, fetched in parallel per wave.
/// Workspace members never hit the registry, they resolve to "ws:<name>" references.
/// Returns the tree plus a report of which direct deps ship their own types.
pub async fn resolve(
    reg: &Registry,
    root_deps: &BTreeMap<String, String>,
    members: &mut [LockMember],
    member_reqs: &[BTreeMap<String, String>],
    type_reqs: &BTreeMap<String, String>,
    pins: &BTreeMap<String, String>,
) -> Result<(Tree, BTreeMap<String, bool>, usize, Vec<(String, String, String)>)> {
    let reg = reg.clone();
    let member_names: HashSet<String> = members.iter().map(|m| m.name.clone()).collect();
    let mut packages: BTreeMap<String, LockPackage> = BTreeMap::new();
    let mut root_map: BTreeMap<String, String> = BTreeMap::new();
    let mut types_map: BTreeMap<String, String> = BTreeMap::new();
    let mut direct_types: BTreeMap<String, bool> = BTreeMap::new();
    let mut fetched: HashMap<String, Arc<Packument>> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut skipped_platform: usize = 0; // platform-mismatched packages dropped
    let mut peers: Vec<(String, String, String)> = Vec::new(); // (pkg_id, peer, range)
    let mut wave: Vec<(Parent, String, String)> = Vec::new();
    for (n, r) in root_deps {
        wave.push((Parent::Root, n.clone(), r.clone()));
    }
    for (i, reqs) in member_reqs.iter().enumerate() {
        for (n, r) in reqs {
            wave.push((Parent::Member(i), n.clone(), r.clone()));
        }
    }
    for (n, r) in type_reqs {
        wave.push((Parent::Types, n.clone(), r.clone()));
    }

    while !wave.is_empty() {
        // fetch metadata for every unseen non-member package in this wave, in parallel
        let mut to_fetch: HashSet<String> = HashSet::new();
        for (_, n, _) in &wave {
            if !member_names.contains(n) && !seen.contains(n) {
                to_fetch.insert(n.clone());
            }
        }
        if !to_fetch.is_empty() {
            let mut set = tokio::task::JoinSet::new();
            for n in to_fetch {
                let reg = reg.clone();
                set.spawn(async move {
                    let p = reg.packument(&n).await?;
                    Ok::<(String, Arc<Packument>), anyhow::Error>((n, p))
                });
            }
            while let Some(joined) = set.join_next().await {
                let (n, p) = joined.context("metadata fetch task panicked")??;
                fetched.insert(n, p);
            }
        }

        // pin each requirement to an exact version; enqueue the new package's own deps
        let mut next: Vec<(Parent, String, String)> = Vec::new();
        for (parent, name, range) in wave.drain(..) {
            // workspace members always win over the registry (npm rule)
            let dep_id = if member_names.contains(&name) {
                format!("ws:{}", name)
            } else {
                seen.insert(name.clone());
                let pack = fetched
                    .get(&name)
                    .with_context(|| format!("no metadata for `{}`", name))?
                    .clone();
                let ver = match pins.get(&name) {
                    Some(pv) if pack.versions.contains_key(pv) => pv.clone(),
                    _ => crate::util::pick_version(&name, &pack, &range).with_context(|| {
                        match &parent {
                            Parent::Root => format!("resolving root dependency `{}` ({})", name, range),
                            Parent::Member(i) => {
                                format!("`{}` requires `{}` ({})", members[*i].name, name, range)
                            }
                            Parent::Pkg(id) => format!("`{}` requires `{}` ({})", id, name, range),
                            Parent::Types => format!("type dependency `{}` ({})", name, range),
                        }
                    })?,
                };
                let id = format!("{}@{}", name, ver);
                if !packages.contains_key(&id) {
                    let vi = pack
                        .versions
                        .get(&ver)
                        .with_context(|| format!("registry has no version {} of `{}`", ver, name))?;
                    // platform gate: a package whose manifest declares os/cpu
                    // that exclude this host never enters the tree, no
                    // download, no lockfile entry, no transitive walk. applies
                    // to optionalDependencies variants AND packages that
                    // declare platform fields directly (typescript 7's
                    // per-platform natives ride in as plain dependencies)
                    let (host_os, host_cpu) = host_platform();
                    let (os_decl, cpu_decl) = declared_platforms(vi);
                    if !platform_accepts(os_decl.as_ref(), cpu_decl.as_ref(), host_os, host_cpu) {
                        // npm hard-errors EBADPLATFORM on direct deps; we warn
                        // and keep going, transitive skips stay silent
                        skipped_platform += 1;
                        if !matches!(parent, Parent::Pkg(_) | Parent::Types) {
                            eprintln!(
                                "{} `{}` {}@{} declares os {:?} cpu {:?}, not for {}:{}, skipping",
                                crate::util::yellow("warning:"),
                                name,
                                name,
                                ver,
                                os_decl.unwrap_or_default(),
                                cpu_decl.unwrap_or_default(),
                                host_os,
                                host_cpu
                            );
                        }
                        continue;
                    }
                    let dist = vi
                        .dist
                        .clone()
                        .with_context(|| format!("no tarball info for {}@{}", name, ver))?;
                    packages.insert(
                        id.clone(),
                        LockPackage {
                            integrity: dist.integrity.clone().unwrap_or_default(),
                            tarball: dist.tarball.clone(),
                            deps: BTreeMap::new(),
                            bin: vi
                                .bin
                                .as_ref()
                                .map(|b| crate::model::normalize_bin(&name, b))
                                .unwrap_or_default(),
                        },
                    );
                    // peer-dependency report: recorded while the metadata is
                    // in hand (no extra registry calls); the install flow
                    // checks the built tree afterwards, peers never
                    // auto-install, they're advisory
                    if let Some(peer_map) = vi.extra.get("peerDependencies").and_then(|v| v.as_object()) {
                        for (pn, pr) in peer_map {
                            if let Some(pr) = pr.as_str() {
                                peers.push((id.clone(), pn.clone(), pr.to_string()));
                            }
                        }
                    }
                    for (cn, cr) in &vi.dependencies {
                        next.push((Parent::Pkg(id.clone()), cn.clone(), cr.clone()));
                    }
                    // npm optionalDependencies: resolved like deps, gated by
                    // the same platform check (esbuild's per-platform binaries)
                    if let Some(opt) = vi.extra.get("optionalDependencies").and_then(|v| v.as_object()) {
                        for (cn, cr) in opt {
                            if let Some(cr) = cr.as_str() {
                                next.push((Parent::Pkg(id.clone()), cn.clone(), cr.to_string()));
                            }
                        }
                    }
                }
                // direct dep type report (root + members, registry deps only)
                if !matches!(parent, Parent::Pkg(_) | Parent::Types) {
                    let vi = pack.versions.get(&ver);
                    let has_own = vi.map(|v| v.types.is_some() || v.typings.is_some()).unwrap_or(true);
                    direct_types.entry(name.clone()).or_insert(has_own);
                }
                id
            };
            match parent {
                Parent::Root => {
                    root_map.insert(name.clone(), dep_id);
                }
                Parent::Member(i) => {
                    members[i].deps.insert(name.clone(), dep_id);
                }
                Parent::Types => {
                    types_map.insert(name.clone(), dep_id);
                }
                Parent::Pkg(id) => {
                    if let Some(p) = packages.get_mut(&id) {
                        p.deps.insert(name.clone(), dep_id);
                    }
                }
            }
        }
        wave = next;
    }

    // fallback pool: one "highest version" entry per package name, used when a package
    // requires something it didn't declare (keeps un-listed deps and computed requires alive);
    // disabled entirely in strict mode (the map simply stays empty)
    let mut fallback: BTreeMap<String, String> = BTreeMap::new();
    for id in packages.keys() {
        let (name, ver) = id
            .rsplit_once('@')
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .unwrap_or_else(|| (id.clone(), String::new()));
        let higher = match fallback.get(&name) {
            Some(existing) => {
                let existing_ver = existing.rsplit_once('@').map(|x| x.1).unwrap_or(existing.as_str());
                crate::util::version_lt(existing_ver, &ver)
            }
            None => true,
        };
        if higher {
            fallback.insert(name, id.clone());
        }
    }

    Ok((
        Tree {
            root: root_map,
            packages,
            fallback,
            members: members.to_vec(),
            types: types_map,
        },
        direct_types,
        skipped_platform,
        peers,
    ))
}

/// peer-dependency warnings against the built tree: a peer provided by ANY
/// tree package satisfies the consumer relationship (npm's consumer-chain
/// strictness is not replicated, shortcut; zap's pnp model is flat). pure:
/// takes (pkg_id, peer, range) triples + an id→version map, returns the
/// warning lines. unparseable ranges (workspace:*, urls) skip silently; a
/// package's peers pointing at itself skip; output caps at 10
pub fn peer_warnings(peers: &[(String, String, String)], provided: &BTreeMap<String, String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (pkg, peer, range) in peers {
        // self-reference: nothing useful to say
        let pkg_name = pkg.rsplit_once('@').map(|x| x.0).unwrap_or(pkg.as_str());
        if pkg_name == peer {
            continue;
        }
        // identical (pkg, peer) pairs: one warning, however often seen
        if !seen.insert((pkg.clone(), peer.clone())) {
            continue;
        }
        let provided_version = provided
            .iter()
            .filter(|(id, _)| id.rsplit_once('@').map(|(n, _)| n == peer).unwrap_or(false))
            // the representative version: the highest, zap's fallback pool
            // is what require() hands back for an undeclared peer
            .max_by_key(|(_, v)| semver::Version::parse(v).ok())
            .map(|(_, v)| v.clone());
        match provided_version {
            None => out.push(format!(
                "{} {} lists {} as a peer-dependency, it is recommended you install this package",
                crate::util::yellow("warning:"),
                pkg,
                peer
            )),
            Some(v) => {
                // weird ranges (workspace:*, git, urls) skip silently
                let Ok(req) = semver::VersionReq::parse(range) else { continue };
                let Ok(vers) = semver::Version::parse(&v) else { continue };
                if !req.matches(&vers) {
                    out.push(format!(
                        "{} {} lists {} as a peer-dependency, but your project provides {}, it is recommended you upgrade/downgrade this package in case of support errors",
                        crate::util::yellow("warning:"),
                        pkg,
                        peer,
                        v
                    ));
                }
            }
        }
    }
    // a pathological tree shouldn't spam 200 lines
    const MAX: usize = 10;
    if out.len() > MAX {
        let extra = out.len() - MAX;
        out.truncate(MAX);
        out.push(format!("… and {} more peer-dependency warnings", extra));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// env-touching tests serialize: ZAP_HOME leaks process-wide
    
    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zap-plat-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn os_and_arch_names_map_to_npm_vocabulary() {
        assert_eq!(os_name("windows"), "win32");
        assert_eq!(os_name("macos"), "darwin");
        assert_eq!(os_name("linux"), "linux");
        assert_eq!(os_name("solaris"), "sunos");
        assert_eq!(os_name("illumos"), "sunos");
        assert_eq!(os_name("freebsd"), "freebsd"); // already npm vocabulary
        assert_eq!(arch_name("x86_64"), "x64");
        assert_eq!(arch_name("aarch64"), "arm64");
        assert_eq!(arch_name("x86"), "ia32");
        assert_eq!(arch_name("powerpc64"), "ppc64");
        assert_eq!(arch_name("loongarch64"), "loong64");
        assert_eq!(arch_name("riscv64"), "riscv64"); // identity fallback
    }

    #[test]
    fn platform_accepts_empty_missing_and_multi_value_lists() {
        let (host_os, host_cpu) = ("win32", "x64");
        // missing fields: the common case, always accepted
        assert!(platform_accepts(None, None, host_os, host_cpu));
        // empty lists: declared but unconstrained
        let empty = vec![];
        assert!(platform_accepts(Some(&empty), None, host_os, host_cpu));
        assert!(platform_accepts(None, Some(&empty), host_os, host_cpu));
        // matching single + multi-value lists
        let win = vec!["win32".to_string()];
        let x64 = vec!["x64".to_string()];
        assert!(platform_accepts(Some(&win), Some(&x64), host_os, host_cpu));
        let multi = vec!["darwin".to_string(), "win32".to_string()];
        assert!(platform_accepts(Some(&multi), Some(&x64), host_os, host_cpu));
        // mismatches: os, cpu, both
        let linux = vec!["linux".to_string()];
        let arm64 = vec!["arm64".to_string()];
        assert!(!platform_accepts(Some(&linux), None, host_os, host_cpu));
        assert!(!platform_accepts(None, Some(&arm64), host_os, host_cpu));
        assert!(!platform_accepts(Some(&linux), Some(&arm64), host_os, host_cpu));
    }

    #[test]
    fn peer_warnings_cover_the_matrix() {
        let provided = BTreeMap::from([
            ("react@16.8.0".to_string(), "16.8.0".to_string()),
            ("react@17.0.2".to_string(), "17.0.2".to_string()),
            ("react@17.3.7-ts.1".to_string(), "17.3.7-ts.1".to_string()),
        ]);
        // missing peer: recommend installing
        let w = peer_warnings(&[("fakeui@1.0.0".into(), "vue".into(), "^3.0.0".into())], &provided);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("fakeui@1.0.0") && w[0].contains("vue"));
        assert!(w[0].contains("it is recommended you install this package"));
        // present but too old: recommend upgrading (16.8.0 vs ^17)
        let only_old = BTreeMap::from([("react@16.8.0".to_string(), "16.8.0".to_string())]);
        let w = peer_warnings(&[("fakeui@1.0.0".into(), "react".into(), "^17.0.0".into())], &only_old);
        assert!(w[0].contains("provides 16.8.0"), "warning 2 names the provided version");
        // satisfying peer: silence
        let sat = BTreeMap::from([("react@17.0.2".to_string(), "17.0.2".to_string())]);
        let w = peer_warnings(&[("fakeui@1.0.0".into(), "react".into(), "^17.0.0".into())], &sat);
        assert!(w.is_empty(), "satisfying peer is quiet");
        // several candidates: the highest is the representative (what the
        // fallback pool hands the consumer), a prerelease on top shadows a
        // satisfying stable and the warning names it honestly
        let w = peer_warnings(&[("fakeui@1.0.0".into(), "react".into(), "^17.0.0".into())], &provided);
        assert!(w[0].contains("provides 17.3.7-ts.1"), "the highest candidate is the messenger");
        // exact-with-prerelease: the req names the pre, the exact version matches
        let pre = BTreeMap::from([("react@17.3.7-ts.1".to_string(), "17.3.7-ts.1".to_string())]);
        let w = peer_warnings(
            &[("fakeui@1.0.0".into(), "react".into(), "17.3.7-ts.1".into())],
            &pre,
        );
        assert!(w.is_empty(), "exact prerelease req matches that exact version");
        // a caret with a prerelease: a NEWER pre of the SAME triple satisfies
        // (semver ordering), a different triple with a pre does not
        let w = peer_warnings(
            &[("fakeui@1.0.0".into(), "react".into(), "^17.3.7-ts.1".into())],
            &BTreeMap::from([("react@17.3.7-ts.2".to_string(), "17.3.7-ts.2".to_string())]),
        );
        assert!(w.is_empty(), "ts.2 > ts.1 in the same triple satisfies");
        let w = peer_warnings(
            &[("fakeui@1.0.0".into(), "react".into(), "^17.3.7-ts.1".into())],
            &BTreeMap::from([("react@17.3.8-ts.1".to_string(), "17.3.8-ts.1".to_string())]),
        );
        assert_eq!(w.len(), 1, "a pre on a different triple has no matching comparator");
        // unparseable range: silent skip
        let w = peer_warnings(
            &[("fakeui@1.0.0".into(), "react".into(), "workspace:*".into())],
            &provided,
        );
        assert!(w.is_empty());
        // self-reference: skipped
        let w = peer_warnings(&[("react@16.8.0".into(), "react".into(), "^16".into())], &provided);
        assert!(w.is_empty());
    }

    #[test]
    fn peer_warnings_dedupe_and_cap() {
        let trip = ("fakeui@1.0.0".to_string(), "react".to_string(), "^17.0.0".to_string());
        // the same (pkg, peer) twice: one warning
        let w = peer_warnings(&[trip.clone(), trip.clone()], &BTreeMap::new());
        assert_eq!(w.len(), 1, "deduped");
        // >10 distinct peers: first 10 + a count line
        let many: Vec<(String, String, String)> = (0..15)
            .map(|i| (format!("pkg{}@1.0.0", i), format!("peer{}", i), "*".to_string()))
            .collect();
        let w = peer_warnings(&many, &BTreeMap::new());
        assert_eq!(w.len(), 11, "10 warnings + the overflow line");
        assert!(w[10].contains("… and 5 more peer-dependency warnings"));
    }

    #[test]
    fn resolve_records_peer_triples_offline() {
        // fakeui@1.0.0 declares react ^17 as a peer; the seeded cache answers
        // the packument; the resolve records the triple, no extra fetches
        let _env = crate::cmds::env_test_lock();
        let home = tempdir("home");
        std::env::set_var("ZAP_HOME", &home);
        let cache = crate::util::meta_cache().unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        let seed = |name: &str, pack: serde_json::Value| {
            let key = crate::util::sha256_hex(name.as_bytes());
            let wrapped = serde_json::json!({
                "t": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                "body": pack,
            });
            std::fs::write(cache.join(format!("{}.json", key)), wrapped.to_string()).unwrap();
        };
        seed(
            "fakeui",
            serde_json::json!({
                "name": "fakeui",
                "versions": { "1.0.0": { "version": "1.0.0", "peerDependencies": { "react": "^17.0.0" }, "dist": { "tarball": "https://registry.npmjs.org/x/-/x-1.0.0.tgz" } } },
                "dist-tags": { "latest": "1.0.0" },
            }),
        );
        let reg = crate::http::Registry::new(false, false, false, 24, None).unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let root_deps = BTreeMap::from([("fakeui".to_string(), "^1.0.0".to_string())]);
        let (tree, _, _, peers) = rt
            .block_on(crate::resolve::resolve(&reg, &root_deps, &mut [], &[], &BTreeMap::new(), &BTreeMap::new()))
            .unwrap();
        std::env::remove_var("ZAP_HOME");
        assert_eq!(peers, vec![("fakeui@1.0.0".to_string(), "react".to_string(), "^17.0.0".to_string())]);
        assert!(tree.packages.contains_key("fakeui@1.0.0"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn resolve_skips_platform_mismatched_optionals_offline() {
        // a fake optional-dependency family: pkgplat's variants are os-gated;
        // the linux variant drags a transitive dep that must vanish with it.
        // packuments come from the seeded meta-cache: zero network
        let _env = crate::cmds::env_test_lock();
        let home = tempdir("home");
        std::env::set_var("ZAP_HOME", &home);
        let cache = crate::util::meta_cache().unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        let seed = |name: &str, pack: serde_json::Value| {
            let key = crate::util::sha256_hex(name.as_bytes());
            let wrapped = serde_json::json!({
                "t": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
                "body": pack,
            });
            std::fs::write(cache.join(format!("{}.json", key)), wrapped.to_string()).unwrap();
        };
        let host = host_platform(); // before the seeds: the matching variant is built for THIS host
        seed(
            "pkgplat",
            serde_json::json!({
                "name": "pkgplat",
                "versions": { "1.0.0": { "version": "1.0.0", "optionalDependencies": { "pkgplat-win": "^1.0.0", "pkgplat-linux": "^1.0.0", "pkgplat-host": "^1.0.0" }, "dist": { "tarball": "https://registry.npmjs.org/pkgplat/-/pkgplat-1.0.0.tgz" } } },
                "dist-tags": { "latest": "1.0.0" },
            }),
        );
        seed(
            "pkgplat-win",
            serde_json::json!({
                "name": "pkgplat-win",
                "versions": { "1.0.0": { "version": "1.0.0", "os": ["win32"], "cpu": ["x64"], "dist": { "tarball": "https://registry.npmjs.org/x/-/x-1.0.0.tgz" } } },
                "dist-tags": { "latest": "1.0.0" },
            }),
        );
        seed(
            "pkgplat-linux",
            serde_json::json!({
                "name": "pkgplat-linux",
                "versions": { "1.0.0": { "version": "1.0.0", "os": ["linux"], "cpu": ["x64"], "dependencies": { "linux-only-helper": "^1.0.0" }, "dist": { "tarball": "https://registry.npmjs.org/x/-/x-1.0.0.tgz" } } },
                "dist-tags": { "latest": "1.0.0" },
            }),
        );
        seed(
            "linux-only-helper",
            serde_json::json!({
                "name": "linux-only-helper",
                "versions": { "1.0.0": { "version": "1.0.0", "dist": { "tarball": "https://registry.npmjs.org/x/-/x-1.0.0.tgz" } } },
                "dist-tags": { "latest": "1.0.0" },
            }),
        );
        seed(
            "pkgplat-host",
            serde_json::json!({
                "name": "pkgplat-host",
                "versions": { "1.0.0": { "version": "1.0.0", "os": [host.0], "cpu": [host.1], "dependencies": { "host-helper": "^1.0.0" }, "dist": { "tarball": "https://registry.npmjs.org/x/-/x-1.0.0.tgz" } } },
                "dist-tags": { "latest": "1.0.0" },
            }),
        );
        seed(
            "host-helper",
            serde_json::json!({
                "name": "host-helper",
                "versions": { "1.0.0": { "version": "1.0.0", "dist": { "tarball": "https://registry.npmjs.org/x/-/x-1.0.0.tgz" } } },
                "dist-tags": { "latest": "1.0.0" },
            }),
        );
        seed(
            "pkgplat-meta",
            serde_json::json!({
                "name": "pkgplat-meta",
                "versions": { "1.0.0": { "version": "1.0.0", "os": "linux", "cpu": ["x64"], "dist": { "tarball": "https://registry.npmjs.org/x/-/x-1.0.0.tgz" } } },
                "dist-tags": { "latest": "1.0.0" },
            }),
        );

        let reg = crate::http::Registry::new(false, false, false, 24, None).unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let root_deps = BTreeMap::from([("pkgplat".to_string(), "^1.0.0".to_string())]);
        let (tree, _, skipped, _) = rt
            .block_on(crate::resolve::resolve(&reg, &root_deps, &mut [], &[], &BTreeMap::new(), &BTreeMap::new()))
            .unwrap();
        std::env::remove_var("ZAP_HOME");

        // the matching variant resolves on EVERY host (built from
        // host_platform()); the two fixed variants vanish unless the host is
        // their platform, so the skip count is 1 on win/linux, 2 on darwin
        assert!(tree.packages.contains_key("pkgplat-host@1.0.0"), "matching variant in the tree");
        for foreign in ["pkgplat-win@1.0.0", "pkgplat-linux@1.0.0"] {
            let is_os = (foreign.starts_with("pkgplat-win") && host.0 == "win32")
                || (foreign.starts_with("pkgplat-linux") && host.0 == "linux");
            if is_os {
                assert!(tree.packages.contains_key(foreign), "own-host variant present: {}", foreign);
            } else {
                assert!(!tree.packages.contains_key(foreign), "foreign variant skipped: {}", foreign);
            }
        }
        // the matching variant's transitive rides with it, on every host
        assert!(tree.packages.contains_key("host-helper@1.0.0"), "host transitive rides");
        // the linux-only helper hangs off pkgplat-linux: present iff linux host
        assert_eq!(
            tree.packages.contains_key("linux-only-helper@1.0.0"),
            host.0 == "linux",
            "linux transitive rides only on linux"
        );
        if host.0 == "darwin" {
            assert_eq!(skipped, 2, "both foreign variants skip on darwin");
        } else {
            assert_eq!(skipped, 1, "one platform skip counted");
        }
        // the resolved pkgplat's dep map references only the matching variant;
        // the root map still points at pkgplat itself
        assert_eq!(tree.root.get("pkgplat").map(|s| s.as_str()), Some("pkgplat@1.0.0"));
        let plat_deps = &tree.packages["pkgplat@1.0.0"].deps;
        assert!(
            plat_deps.values().any(|id| id == "pkgplat-host@1.0.0"),
            "matching variant referenced: {:?}",
            plat_deps
        );
        // foreign variants referenced only when the host is their platform
        for foreign in ["pkgplat-win", "pkgplat-linux"] {
            let referenced = plat_deps.values().any(|id| id.starts_with(foreign));
            let is_os = (foreign == "pkgplat-win" && host.0 == "win32")
                || (foreign == "pkgplat-linux" && host.0 == "linux");
            assert_eq!(referenced, is_os, "{} reference vs host {}", foreign, host.0);
        }
        // a direct dep declaring a single-string os field is gated the same
        // way when reached directly
        let root_deps = BTreeMap::from([("pkgplat-meta".to_string(), "^1.0.0".to_string())]);
        let (tree2, _, skipped2, _) = rt
            .block_on(crate::resolve::resolve(&reg, &root_deps, &mut [], &[], &BTreeMap::new(), &BTreeMap::new()))
            .unwrap_or_else(|e| panic!("resolve with seeded cache failed: {}", e));
        let _ = tree2;
        if host.0 != "linux" {
            assert_eq!(skipped2, 1, "direct-dep platform mismatch warns + skips");
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}
