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
