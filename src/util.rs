use anyhow::{bail, Context, Result};
use base64::Engine;
use sha2::{Digest, Sha256, Sha512};
use std::path::PathBuf;

pub fn green(s: impl AsRef<str>) -> String {
    format!("\x1b[32m{}\x1b[0m", s.as_ref())
}
pub fn yellow(s: impl AsRef<str>) -> String {
    format!("\x1b[33m{}\x1b[0m", s.as_ref())
}
pub fn red(s: impl AsRef<str>) -> String {
    format!("\x1b[31m{}\x1b[0m", s.as_ref())
}
pub fn cyan(s: impl AsRef<str>) -> String {
    format!("\x1b[36m{}\x1b[0m", s.as_ref())
}
pub fn dim(s: impl AsRef<str>) -> String {
    format!("\x1b[2m{}\x1b[0m", s.as_ref())
}

pub fn home() -> Result<PathBuf> {
    // ZAP_HOME overrides everything (CI caches, docker cache mounts)
    if let Ok(h) = std::env::var("ZAP_HOME") {
        return Ok(PathBuf::from(h));
    }
    #[cfg(windows)]
    let base = std::env::var("USERPROFILE").context("USERPROFILE not set")?;
    #[cfg(not(windows))]
    let base = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(base))
}
/// everything zap persists (store, metadata cache, credentials, registries, dlx)
pub fn zap_home() -> Result<PathBuf> {
    Ok(home()?.join(".zap"))
}

/// a shell command running one line: cmd on windows, sh elsewhere
pub fn shell_command(line: &str) -> std::process::Command {
    #[cfg(windows)]
    {
        let mut c = std::process::Command::new("cmd");
        c.args(["/d", "/s", "/c", line]);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(line);
        c
    }
}
pub fn store_dir() -> Result<PathBuf> {
    Ok(zap_home()?.join("store"))
}
pub fn meta_cache() -> Result<PathBuf> {
    Ok(zap_home()?.join("meta-cache"))
}

pub fn find_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("package.json").is_file() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("not inside a project (no package.json in any parent directory), run `zap init` first");
        }
    }
}

pub fn read_json(path: &std::path::Path) -> Result<serde_json::Value> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn slug(name: &str) -> String {
    name.trim_start_matches('@').replace('/', "__")
}

/// directory name inside the virtual store for a package id like "@a/b@1.2.3" -> "a__b@1.2.3"
pub fn pkg_dir_name(id: &str) -> String {
    match id.rsplit_once('@') {
        Some((name, ver)) => format!("{}@{}", slug(name), ver),
        None => id.to_string(),
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// verify npm-style integrity strings ("sha512-<base64>"); unknown algorithms pass
pub fn verify_integrity(data: &[u8], integrity: &str) -> bool {
    let Some(b64) = integrity.strip_prefix("sha512-") else {
        return true;
    };
    let Ok(want) = base64::engine::general_purpose::STANDARD.decode(b64) else {
        return false;
    };
    want == Sha512::digest(data).to_vec()
}

/// "pkg@^1.2.3" -> ("pkg", "^1.2.3"); handles scoped names
pub fn split_spec(s: &str) -> (String, String) {
    let bytes = s.as_bytes();
    let mut at = None;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'@' && i > 0 {
            at = Some(i);
        }
    }
    match at {
        Some(i) => (s[..i].to_string(), s[i + 1..].to_string()),
        None => (s.to_string(), String::new()),
    }
}

/// translate x-wildcards into semver-crate wildcards ("1.x" -> "1.*")
pub fn normalize_range(range: &str) -> String {
    let t = range.trim();
    if t.is_empty() || t == "*" || t == "latest" {
        return "*".to_string();
    }
    t.split('.')
        .map(|p| if p == "x" || p == "X" { "*".to_string() } else { p.to_string() })
        .collect::<Vec<_>>()
        .join(".")
}

pub fn version_lt(a: &str, b: &str) -> bool {
    match (semver::Version::parse(a), semver::Version::parse(b)) {
        (Ok(x), Ok(y)) => x < y,
        _ => a < b,
    }
}

pub fn human(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0usize;
    while v >= 1024.0 && u < 3 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.1} {}", v, units[u])
    }
}

/// pick the best version of a package for an npm range; tries each `||` alternative
pub fn pick_version(name: &str, pack: &crate::model::Packument, range: &str) -> Result<String> {
    let range = normalize_range(range);
    for alt in range.split("||") {
        if let Some(v) = pick_single(pack, alt.trim()) {
            return Ok(v);
        }
    }
    if range == "*" {
        if let Some(latest) = &pack.dist_tags.latest {
            if pack.versions.contains_key(latest) {
                return Ok(latest.clone());
            }
        }
    }
    bail!("no version of `{}` satisfies `{}`", name, range)
}

fn pick_single(pack: &crate::model::Packument, range: &str) -> Option<String> {
    if range.is_empty() || range == "*" {
        if let Some(latest) = &pack.dist_tags.latest {
            if pack.versions.contains_key(latest) {
                return Some(latest.clone());
            }
        }
        let req = semver::VersionReq::parse("*").ok()?;
        return pick_by_req(pack, &req, "*");
    }
    // hyphen ranges like "1.2.3 - 2.0" (approximate translation)
    if range.contains(" - ") {
        if let Some(req) = hyphen_req(range) {
            return pick_by_req(pack, &req, range);
        }
    }
    // npm space-separated ANDs (">=1 <2") -> comma-separated for the semver crate
    let norm = space_commas(range);
    if let Ok(req) = semver::VersionReq::parse(&norm) {
        return pick_by_req(pack, &req, range);
    }
    // exact version literal
    if pack.versions.contains_key(range) {
        return Some(range.to_string());
    }
    None
}

/// ">= 2.1.2 < 3.0.0" -> ">=2.1.2, <3.0.0" (the semver crate wants comma-separated ANDs),
/// with npm comparator semantics applied per group
fn space_commas(range: &str) -> String {
    const OPS: [&str; 8] = [">=", "<=", "~>", ">", "<", "=", "^", "~"];
    let tokens: Vec<&str> = range.split_whitespace().collect();
    let mut groups: Vec<String> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        let group = if OPS.contains(&t) && i + 1 < tokens.len() {
            i += 2;
            format!("{}{}", t, tokens[i - 1])
        } else {
            i += 1;
            t.to_string()
        };
        groups.push(norm_cmp(&group, &OPS));
    }
    groups.join(", ")
}

/// give one comparator npm semantics: "~>" -> "~", bare "1.2.3" -> "= 1.2.3",
/// bare partials "1"/"1.2" -> ranges, wildcards pass through
fn norm_cmp(t: &str, ops: &[&str; 8]) -> String {
    if t == "*" {
        return t.to_string();
    }
    if ops.iter().any(|op| t.starts_with(op)) {
        if let Some(rest) = t.strip_prefix("~>") {
            return format!("~{}", rest);
        }
        return t.to_string();
    }
    let parts: Vec<&str> = t.split('.').collect();
    if parts.iter().any(|p| *p == "*" || *p == "x" || *p == "X") {
        return t.to_string(); // crate handles "1.*" / "1.2.*" like npm
    }
    let nums: Option<Vec<u64>> = parts.iter().map(|p| p.parse().ok()).collect();
    match (&nums, parts.len()) {
        (Some(v), 1) => format!(">= {}, < {}.0.0", t, v[0] + 1),
        (Some(v), 2) => format!(">= {}, < {}.{}.0", t, v[0], v[1] + 1),
        _ => format!("= {}", t), // "1.2.3", "1.2.3-beta.1", or anything odd: exact
    }
}

fn pick_by_req(pack: &crate::model::Packument, req: &semver::VersionReq, raw: &str) -> Option<String> {
    let mut versions: Vec<semver::Version> = pack
        .versions
        .values()
        .filter_map(|v| semver::Version::parse(&v.version).ok())
        .collect();
    versions.sort();
    let has_pre = raw.contains('-');
    let mut matching: Vec<semver::Version> = versions
        .iter()
        .filter(|v| req.matches(v) && (has_pre || v.pre.is_empty()))
        .cloned()
        .collect();
    if matching.is_empty() {
        // last resort: allow prereleases rather than fail outright
        matching = versions.into_iter().filter(|v| req.matches(v)).collect();
    }
    matching.pop().map(|v| v.to_string())
}

fn hyphen_req(range: &str) -> Option<semver::VersionReq> {
    let (lo, hi) = range.split_once(" - ")?;
    let lo = lo.trim();
    let hi = hi.trim();
    let parts: Vec<&str> = hi.split('.').collect();
    let upper = match parts.len() {
        1 => format!("<{}.0.0", parts[0]),
        2 => format!(
            "<{}.{}.0",
            parts[0],
            parts[1].parse::<u64>().ok().map(|n| n + 1).unwrap_or(0)
        ),
        _ => format!("<= {}", hi),
    };
    semver::VersionReq::parse(&format!(">= {}, {}", lo, upper)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Packument, VersionInfo};

    fn pack(versions: &[&str], latest: &str) -> Packument {
        let mut versions_map = std::collections::BTreeMap::new();
        for v in versions {
            versions_map.insert(
                v.to_string(),
                VersionInfo {
                    version: v.to_string(),
                    dist: None,
                    dependencies: Default::default(),
                    bin: None,
                    types: None,
                    typings: None,
                    extra: Default::default(),
                },
            );
        }
        Packument {
            versions: versions_map,
            dist_tags: crate::model::DistTags {
                latest: Some(latest.to_string()),
                other: Default::default(),
            },
        }
    }

    #[test]
    fn ranges() {
        let p = pack(&["1.0.0", "1.2.3", "1.9.0", "2.0.0", "2.1.2"], "2.1.2");
        assert_eq!(pick_version("x", &p, "^1.0.0").unwrap(), "1.9.0");
        assert_eq!(pick_version("x", &p, "~1.2.0").unwrap(), "1.2.3");
        assert_eq!(pick_version("x", &p, "*").unwrap(), "2.1.2");
        assert_eq!(pick_version("x", &p, ">= 2.0.0 < 2.1.0").unwrap(), "2.0.0");
        assert_eq!(pick_version("x", &p, "1.x").unwrap(), "1.9.0");
        // npm semantics: first || alternative that matches wins
        assert_eq!(pick_version("x", &p, "1.2.3 || ^2.0.0").unwrap(), "1.2.3");
        assert_eq!(pick_version("x", &p, "^2.0.0 || 1.2.3").unwrap(), "2.1.2");
        // bare versions are exact in npm (the semver crate defaults to caret!)
        assert_eq!(pick_version("x", &p, "1.2.3").unwrap(), "1.2.3");
        assert_eq!(pick_version("x", &p, "1").unwrap(), "1.9.0");
        assert_eq!(pick_version("x", &p, "1.2").unwrap(), "1.2.3");
        assert_eq!(pick_version("x", &p, "1.0.0 - 1.2.3").unwrap(), "1.2.3");
        assert!(pick_version("x", &p, ">=3.0.0").is_err());
        // dist-tag latest wins for bare "*"
        let p2 = pack(&["1.0.0", "2.0.0-rc.1"], "1.0.0");
        assert_eq!(pick_version("x", &p2, "*").unwrap(), "1.0.0");
    }

    #[test]
    fn specs_and_slugs() {
        assert_eq!(split_spec("chalk"), ("chalk".into(), "".into()));
        assert_eq!(split_spec("chalk@^5"), ("chalk".into(), "^5".into()));
        assert_eq!(split_spec("@a/b@1.2"), ("@a/b".into(), "1.2".into()));
        assert_eq!(split_spec("@a/b"), ("@a/b".into(), "".into()));
        assert_eq!(pkg_dir_name("@a/b@1.2.3"), "a__b@1.2.3");
        assert_eq!(pkg_dir_name("chalk@5.6.2"), "chalk@5.6.2");
    }

    #[test]
    fn integrity() {
        let data = b"hello world";
        // sha512("hello world") base64
        let ok = "sha512-MJ7MSJwS1utMxA9QyQLytNDtd+5RGnx6m808qG1M2G+YndNbxf9JlnDaNCVbRbDP2DDoH2Bdz33FVC6TrpzXbw==";
        assert!(verify_integrity(data, ok));
        assert!(!verify_integrity(data, "sha512-AAAA="));
        assert!(verify_integrity(data, "md5-whatever")); // unknown algo passes
    }
}
