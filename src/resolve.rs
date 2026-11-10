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
) -> Result<(Tree, BTreeMap<String, bool>)> {
    let reg = reg.clone();
    let member_names: HashSet<String> = members.iter().map(|m| m.name.clone()).collect();
    let mut packages: BTreeMap<String, LockPackage> = BTreeMap::new();
    let mut root_map: BTreeMap<String, String> = BTreeMap::new();
    let mut types_map: BTreeMap<String, String> = BTreeMap::new();
    let mut direct_types: BTreeMap<String, bool> = BTreeMap::new();
    let mut fetched: HashMap<String, Arc<Packument>> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
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
                let ver = crate::util::pick_version(&name, &pack, &range).with_context(|| {
                    match &parent {
                        Parent::Root => format!("resolving root dependency `{}` ({})", name, range),
                        Parent::Member(i) => {
                            format!("`{}` requires `{}` ({})", members[*i].name, name, range)
                        }
                        Parent::Pkg(id) => format!("`{}` requires `{}` ({})", id, name, range),
                        Parent::Types => format!("type dependency `{}` ({})", name, range),
                    }
                })?;
                let id = format!("{}@{}", name, ver);
                if !packages.contains_key(&id) {
                    let vi = pack
                        .versions
                        .get(&ver)
                        .with_context(|| format!("registry has no version {} of `{}`", ver, name))?;
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
                    for (cn, cr) in &vi.dependencies {
                        next.push((Parent::Pkg(id.clone()), cn.clone(), cr.clone()));
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
    ))
}
