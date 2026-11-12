use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

use crate::model::Lockfile;

// hash of the dependency sections of every manifest, locks re-resolution to manifest changes (root + all workspace members)
pub fn root_hash(
    deps: &BTreeMap<String, String>,
    dev: &BTreeMap<String, String>,
    members: &[(String, &BTreeMap<String, String>, &BTreeMap<String, String>)],
) -> String {
    let payload = serde_json::json!({
        "dependencies": deps,
        "devDependencies": dev,
        "workspaces": members
            .iter()
            .map(|(name, d, dv)| serde_json::json!({
                "name": name,
                "dependencies": d,
                "devDependencies": dv,
            }))
            .collect::<Vec<_>>(),
    });
    crate::util::sha256_hex(payload.to_string().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn root_hash_detects_manifest_drift() {
        let deps = BTreeMap::from([("ms".to_string(), "^2.0.0".to_string())]);
        let dev = BTreeMap::new();
        let members = vec![];
        let base = root_hash(&deps, &dev, &members);
        // unchanged manifest: same hash (the cached-tree skip)
        assert_eq!(root_hash(&deps, &dev, &members), base);
        // hand-added root dep: drift
        let more = BTreeMap::from([
            ("ms".to_string(), "^2.0.0".to_string()),
            ("chalk".to_string(), "^4.0.0".to_string()),
        ]);
        assert_ne!(root_hash(&more, &dev, &members), base);
        // hand-removed root dep: drift
        assert_ne!(root_hash(&BTreeMap::new(), &dev, &members), base);
        // a workspace member's dep change: drift too
        let mdeps = BTreeMap::from([("ms".to_string(), "^1.0.0".to_string())]);
        let empty2 = BTreeMap::new();
        let with_member = vec![("w".to_string(), &mdeps, &empty2)];
        assert_ne!(root_hash(&deps, &dev, &with_member), base);
        // a member added: drift
        let empty3 = BTreeMap::new();
        let with_new_member = vec![("w2".to_string(), &empty3, &empty3)];
        assert_ne!(root_hash(&deps, &dev, &with_new_member), base);
    }
}

pub fn read(lock_path: &Path) -> Result<Option<Lockfile>> {
    if !lock_path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(lock_path)?;
    match serde_json::from_str::<Lockfile>(&text) {
        Ok(l) => Ok(Some(l)),
        Err(_) => Ok(None), // corrupt/unrecognized lockfile, ignore and re-resolve
    }
}

pub fn write(root: &Path, lock: &Lockfile) -> Result<()> {
    std::fs::write(root.join(".zap-lock.json"), serde_json::to_vec_pretty(lock)?)?;
    Ok(())
}
