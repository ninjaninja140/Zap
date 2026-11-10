use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Deserialize, Serialize, Clone)]
pub struct DistInfo {
    pub tarball: String,
    #[serde(default)]
    pub integrity: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct VersionInfo {
    pub version: String,
    #[serde(default)]
    pub dist: Option<DistInfo>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    #[serde(default)]
    pub bin: Option<serde_json::Value>,
    #[serde(default)]
    pub types: Option<String>,
    #[serde(default)]
    pub typings: Option<String>,
    /// every other manifest field (description, homepage, license, engines, ...)
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize, Serialize, Clone, Default)]
pub struct DistTags {
    #[serde(default)]
    pub latest: Option<String>,
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize, Clone)]
pub struct Packument {
    #[serde(default)]
    pub versions: BTreeMap<String, VersionInfo>,
    #[serde(rename = "dist-tags", default)]
    pub dist_tags: DistTags,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct LockPackage {
    pub integrity: String,
    pub tarball: String,
    #[serde(default)]
    pub deps: BTreeMap<String, String>,
    #[serde(default)]
    pub bin: BTreeMap<String, String>,
}

/// a workspace member as recorded in the tree/lockfile; deps may reference other
/// members as "ws:<name>"
#[derive(Serialize, Deserialize, Clone)]
pub struct LockMember {
    pub name: String,
    pub dir: String,
    #[serde(default)]
    pub deps: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Lockfile {
    pub version: u32,
    #[serde(rename = "rootHash")]
    pub root_hash: String,
    pub root: BTreeMap<String, String>,
    #[serde(default)]
    pub packages: BTreeMap<String, LockPackage>,
    #[serde(default)]
    pub fallback: BTreeMap<String, String>,
    #[serde(default)]
    pub workspaces: Vec<LockMember>,
    #[serde(default)]
    pub types: BTreeMap<String, String>,
}

/// resolved dependency tree (same shape as the lockfile, without the root hash)
#[derive(Clone)]
pub struct Tree {
    pub root: BTreeMap<String, String>,
    pub packages: BTreeMap<String, LockPackage>,
    pub fallback: BTreeMap<String, String>,
    pub members: Vec<LockMember>,
    /// zap-managed type dependencies (@types/*, pinned typescript), not in package.json
    pub types: BTreeMap<String, String>,
}

impl Tree {
    pub fn into_lockfile(self, root_hash: String) -> Lockfile {
        Lockfile {
            version: 2,
            root_hash,
            root: self.root,
            packages: self.packages,
            fallback: self.fallback,
            workspaces: self.members,
            types: self.types,
        }
    }
}

impl From<&Lockfile> for Tree {
    fn from(l: &Lockfile) -> Self {
        Tree {
            root: l.root.clone(),
            packages: l.packages.clone(),
            fallback: l.fallback.clone(),
            members: l.workspaces.clone(),
            types: l.types.clone(),
        }
    }
}

/// normalize a package.json `bin` field (string or object form) into name -> relative path
pub fn normalize_bin(pkg_name: &str, bin: &serde_json::Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    match bin {
        serde_json::Value::String(s) => {
            let short = pkg_name.rsplit('/').next().unwrap_or(pkg_name);
            out.insert(short.to_string(), s.clone());
        }
        serde_json::Value::Object(m) => {
            for (k, v) in m {
                if let Some(s) = v.as_str() {
                    out.insert(k.clone(), s.to_string());
                }
            }
        }
        _ => {}
    }
    out
}
