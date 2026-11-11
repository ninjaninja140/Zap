// package-hash allowlist gating lifecycle scripts (.zap-allow.json at the
// project root); dependency lifecycle scripts only run when the package-id
// AND its recorded integrity hash match an entry, commit-friendly project
// state, deliberately not part of the lockfile (resolution rewrites that)

use anyhow::Context;
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub type AllowList = BTreeMap<String, String>;

/// the gate decision for one package's lifecycle scripts
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Gate {
    Allowed,
    NotListed,
    HashMismatch,
}

/// read the project's allowlist; a missing file is an empty allowlist
pub fn load(root: &Path) -> Result<AllowList> {
    let path = root.join(".zap-allow.json");
    if !path.is_file() {
        return Ok(AllowList::new());
    }
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("parsing {} (fix the json or delete the file)", path.display()))
}

/// write the allowlist sorted (btree map order)
pub fn save(root: &Path, list: &AllowList) -> Result<()> {
    let path = root.join(".zap-allow.json");
    let body = serde_json::to_string_pretty(list).context("serializing allowlist")?;
    std::fs::write(&path, format!("{}\n", body))
        .with_context(|| format!("writing {}", path.display()))
}

/// gate one package: listed in the allowlist AND its recorded integrity
/// (from the lockfile/tree) string-matches the entry; a package that left
/// the tree makes its entry inert (nothing iterates it)
pub fn gate(list: &AllowList, id: &str, recorded: Option<&str>) -> Gate {
    match list.get(id) {
        None => Gate::NotListed,
        Some(expected) => match recorded {
            Some(actual) if expected == actual => Gate::Allowed,
            _ => Gate::HashMismatch,
        },
    }
}

/// the dependency-script decision, pure: trust-all (`lifecycle-allow-all`)
/// runs everything and ignores the allowlist entirely; otherwise gate()
pub fn dep_gate(allow_all: bool, list: &AllowList, id: &str, recorded: Option<&str>) -> Gate {
    if allow_all {
        return Gate::Allowed;
    }
    gate(list, id, recorded)
}

/// `zap allow --all`: trust-all on, writes `lifecycle-allow-all: true`
/// through the config system's project set path and deletes the (now
/// pointless) .zap-allow.json; returns (rc file written, file removed?)
pub fn trust_all(root: &Path) -> Result<(PathBuf, bool)> {
    let rc = crate::config::set_project_key(root, "lifecycle-allow-all", "true")?;
    let file = root.join(".zap-allow.json");
    let removed = if file.is_file() {
        std::fs::remove_file(&file).with_context(|| format!("removing {}", file.display()))?;
        true
    } else {
        false
    };
    Ok((rc, removed))
}

/// `zap allow --remove --all`: trust-all off, written as `false` through the
/// same config path (there is no key-removal path and hand-rolling one would
/// break the don't-edit-rc-files-by-hand rule); any .zap-allow.json is left
/// alone and regains its meaning
pub fn untrust_all(root: &Path) -> Result<PathBuf> {
    crate::config::set_project_key(root, "lifecycle-allow-all", "false")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zap-allow-test-{}", tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_is_empty_allowlist() {
        let root = temp_root("missing");
        let list = load(&root).unwrap();
        assert!(list.is_empty());
        assert_eq!(gate(&list, "ms@2.1.3", Some("sha512-x")), Gate::NotListed);
    }

    #[test]
    fn round_trip_sorted_save() {
        let root = temp_root("round");
        let mut list = AllowList::new();
        list.insert("z@1".into(), "sha512-b".into());
        list.insert("a@1".into(), "sha512-a".into());
        save(&root, &list).unwrap();
        let text = std::fs::read_to_string(root.join(".zap-allow.json")).unwrap();
        // btree order: a sorts before z, file is valid json
        assert!(text.find("\"a@1\"").unwrap() < text.find("\"z@1\"").unwrap());
        let back = load(&root).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back["a@1"], "sha512-a");
    }

    #[test]
    fn malformed_json_is_a_clear_error() {
        let root = temp_root("bad");
        std::fs::write(root.join(".zap-allow.json"), "{ not json").unwrap();
        let err = format!("{}", load(&root).unwrap_err());
        assert!(err.contains(".zap-allow.json"), "error names the file: {}", err);
    }

    #[test]
    fn gate_decisions() {
        let mut list = AllowList::new();
        list.insert("ms@2.1.3".into(), "sha512-good".into());
        assert_eq!(gate(&list, "ms@2.1.3", Some("sha512-good")), Gate::Allowed);
        assert_eq!(gate(&list, "ms@2.1.3", Some("sha512-evil")), Gate::HashMismatch);
        assert_eq!(gate(&list, "ms@2.1.3", None), Gate::HashMismatch);
        assert_eq!(gate(&list, "other@1", Some("sha512-good")), Gate::NotListed);
        // an entry for a package no longer in the tree is inert: nothing
        // iterates it, and other ids gate independently
        list.insert("gone@1".into(), "sha512-x".into());
        assert_eq!(gate(&list, "other@1", None), Gate::NotListed);
        assert_eq!(gate(&list, "gone@1", Some("sha512-x")), Gate::Allowed);
    }

    #[test]
    fn trust_all_round_trip_via_config_path() {
        let root = temp_root("trustall");
        // no rc file yet: the set path creates .zaprc.yaml
        let rc = untrust_all(&root).unwrap();
        assert!(rc.ends_with(".zaprc.yaml"));
        assert_eq!(crate::config::Config::load(&root).lifecycle_allow_all, false);
        let rc = trust_all(&root).unwrap().0;
        assert!(rc.ends_with(".zaprc.yaml"));
        assert!(rc.is_file());
        let text = std::fs::read_to_string(&rc).unwrap();
        assert!(text.contains("lifecycle-allow-all: true"));
        assert_eq!(crate::config::Config::load(&root).lifecycle_allow_all, true);
        // clear writes false through the same path, flag reads off again
        untrust_all(&root).unwrap();
        assert_eq!(crate::config::Config::load(&root).lifecycle_allow_all, false);
    }

    #[test]
    fn trust_all_removes_the_allow_file() {
        let root = temp_root("trustdelete");
        std::fs::write(root.join(".zap-allow.json"), r#"{"ms@2.1.3":"sha512-x"}"#).unwrap();
        let (rc, removed) = trust_all(&root).unwrap();
        assert!(removed && !root.join(".zap-allow.json").exists());
        assert!(rc.is_file());
        // second run: idempotent, nothing to remove
        let (_, removed) = trust_all(&root).unwrap();
        assert!(!removed);
    }

    #[test]
    fn dep_gate_honors_trust_all() {
        let list = AllowList::new();
        // trust-all: everything allowed even when the allowlist would block
        assert_eq!(dep_gate(true, &list, "any@1", None), Gate::Allowed);
        assert_eq!(dep_gate(true, &list, "any@1", Some("sha512-whatever")), Gate::Allowed);
        // without the flag, the plain allowlist gate applies
        assert_eq!(dep_gate(false, &list, "any@1", None), Gate::NotListed);
        let mut list = AllowList::new();
        list.insert("a@1".into(), "sha512-good".into());
        assert_eq!(dep_gate(false, &list, "a@1", Some("sha512-bad")), Gate::HashMismatch);
    }
}
