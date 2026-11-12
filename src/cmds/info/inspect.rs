// `zap inspect <pkg>[@range]`: one package version's dependencies, optional
// and peer dependencies, description and release history, straight from the
// registry metadata cache. remote-data command: no find_root anywhere, it
// works outside any project (like dlx)

use anyhow::{anyhow, Result};

use crate::model::Packument;

pub const NAME: &str = "inspect";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "deps, peers, description and releases for one package version";
pub const ABOUT: &str = "shows a package's resolved version with its description, dependencies,
optional and peer dependencies and their ranges, plus the release
history (total, latest, a few recent stables). reads the same registry
metadata cache installs and `zap view` use, so offline works when
cached, and it runs outside any project.";
pub const EXAMPLE: &str = "  $ zap inspect chalk@4.1.2
  what does chalk@4.1.2 pull in, peers included";

pub fn run(args: &[String]) -> Result<i32> {
    let spec = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| anyhow!("usage: zap inspect <package>[@version]"))?;
    let (name, range) = crate::util::split_spec(spec);
    // the fetch path is exactly `zap view`'s: config contributes the registry,
    // cache-ttl and prefer-offline settings, the metadata cache does the rest
    let dir = std::env::current_dir()?;
    let cfg = crate::config::Config::load(&dir);
    let reg = crate::http::Registry::new(
        false,
        false,
        cfg.prefer_offline,
        cfg.cache_ttl_hours,
        cfg.registry.as_deref(),
    )?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let pack = rt.block_on(reg.packument(&name))?;
    let version = pick_inspect_version(&name, &pack, &range)?;
    print!("{}", render(&name, &pack, &version));
    Ok(0)
}

/// which version to inspect: an explicit range goes through the normal
/// resolver; the bare name (or `latest`) resolves to the latest stable, the
/// `latest` dist-tag when it's a stable release, else the highest stable. a
/// prerelease tag never wins the default; name one explicitly to inspect it
pub fn pick_inspect_version(name: &str, pack: &Packument, range: &str) -> Result<String> {
    if !range.is_empty() && range != "latest" {
        return crate::util::pick_version(name, pack, range);
    }
    if let Some(l) = &pack.dist_tags.latest {
        let stable = semver::Version::parse(l).map(|v| v.pre.is_empty()).unwrap_or(true);
        if stable && pack.versions.contains_key(l) {
            return Ok(l.clone());
        }
    }
    let mut stables: Vec<semver::Version> = pack
        .versions
        .keys()
        .filter_map(|k| semver::Version::parse(k).ok())
        .filter(|v| v.pre.is_empty())
        .collect();
    stables.sort();
    stables
        .pop()
        .map(|v| v.to_string())
        .ok_or_else(|| anyhow!("no stable release of `{}`", name))
}

/// the rendered block, pure so it's testable offline: packument + resolved
/// version in, the report out
pub fn render(name: &str, pack: &Packument, version: &str) -> String {
    use crate::util::{bold, dim, green};
    let vi = pack.versions.get(version);
    // optional/peer maps ride the flattened manifest extra; only string
    // ranges are renderable
    let extra_map = |key: &str| -> Vec<(String, String)> {
        vi.and_then(|v| v.extra.get(key))
            .and_then(|x| x.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    };
    let deps: Vec<(String, String)> = vi
        .map(|v| v.dependencies.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    let optional = extra_map("optionalDependencies");
    let peers = extra_map("peerDependencies");

    let mut out = String::new();
    // header: name@version, description (the dash part skips when the
    // version carries no description)
    match vi.and_then(|v| v.extra.get("description")).and_then(|d| d.as_str()) {
        Some(d) => out.push_str(&format!("{} - {}\n", green(format!("{}@{}", name, version)), dim(d))),
        None => out.push_str(&format!("{}\n", green(format!("{}@{}", name, version)))),
    }
    // the three sections share one name column so rows line up down the block
    let width = deps.iter().chain(&optional).chain(&peers).map(|(n, _)| n.len()).max().unwrap_or(0);
    let mut section = |title: &str, rows: &[(String, String)]| {
        out.push_str(&format!("\n{}\n", bold(title)));
        if rows.is_empty() {
            out.push_str(&format!("  {}\n", dim("(none)")));
            return;
        }
        for (n, r) in rows {
            out.push_str(&format!("  {} {} {}\n", n, " ".repeat(width - n.len()), dim(r)));
        }
    };
    section("dependencies:", &deps);
    section("optionalDependencies:", &optional);
    section("peerDependencies:", &peers);

    // release history: total + the latest tag + a few recent stables (the
    // newest stables below the latest tag, prereleases stay out)
    let mut stables: Vec<semver::Version> = pack
        .versions
        .keys()
        .filter_map(|k| semver::Version::parse(k).ok())
        .filter(|v| v.pre.is_empty())
        .collect();
    stables.sort();
    let latest_v = pack.dist_tags.latest.as_deref().and_then(|l| semver::Version::parse(l).ok());
    let recent: Vec<&semver::Version> =
        stables.iter().rev().filter(|v| latest_v.as_ref() != Some(*v)).collect();
    out.push_str(&format!(
        "\nversions: {} published, latest {}",
        pack.versions.len(),
        pack.dist_tags.latest.as_deref().unwrap_or("?")
    ));
    if !recent.is_empty() {
        let shown: Vec<String> = recent.iter().take(3).map(|v| v.to_string()).collect();
        let more = if recent.len() > 3 { "…" } else { "" };
        out.push_str(&format!(" (also recent: {}{})", shown.join(", "), more));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(v: serde_json::Value) -> Packument {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn render_shows_deps_peers_description_and_history() {
        let p = pack(serde_json::json!({
            "versions": {
                "4.1.2": {
                    "version": "4.1.2",
                    "description": "Terminal string styling done right",
                    "dependencies": { "ansi-styles": "^4.1.0", "supports-color": "^7.1.0" },
                    "optionalDependencies": { "fsevents": "^2.3.2" },
                    "peerDependencies": { "react": ">=16" },
                    "dist": { "tarball": "https://reg.invalid/chalk-4.1.2.tgz" },
                },
                "4.1.1": { "version": "4.1.1" },
                "4.1.0": { "version": "4.1.0" },
                "4.0.0": { "version": "4.0.0" },
                "3.9.9": { "version": "3.9.9" },
                "5.0.0-alpha.1": { "version": "5.0.0-alpha.1" },
            },
            "dist-tags": { "latest": "4.1.2" },
        }));
        let out = render("chalk", &p, "4.1.2");
        assert!(out.contains("chalk@4.1.2"), "header: {}", out);
        assert!(out.contains("- ") && out.contains("Terminal string styling done right"), "description: {}", out);
        for piece in ["ansi-styles", "^4.1.0", "supports-color", "^7.1.0"] {
            assert!(out.contains(piece), "dep row piece {}: {}", piece, out);
        }
        assert!(out.contains("optionalDependencies:") && out.contains("fsevents"), "{}", out);
        assert!(out.contains("peerDependencies:") && out.contains("react") && out.contains(">=16"), "{}", out);
        assert!(out.contains("versions: 6 published, latest 4.1.2"), "{}", out);
        // recent stables exclude the latest tag itself, prereleases stay out
        assert!(out.contains("(also recent: 4.1.1, 4.1.0, 4.0.0…)"), "{}", out);
        assert!(!out.contains("5.0.0-alpha.1"), "{}", out);
    }

    #[test]
    fn render_handles_missing_fields() {
        let p = pack(serde_json::json!({
            "versions": { "1.0.0": { "version": "1.0.0" } },
            "dist-tags": { "latest": "1.0.0" },
        }));
        let out = render("lonely", &p, "1.0.0");
        let head = out.lines().next().unwrap();
        assert!(head.contains("lonely@1.0.0") && !head.contains("- "), "bare header without a description: {:?}", head);
        assert!(!out.contains("- "), "no dash part when the description is missing");
        assert_eq!(out.matches("(none)").count(), 3, "all three sections render (none)");
        assert!(out.contains("versions: 1 published, latest 1.0.0"));
        assert!(!out.contains("also recent"), "nothing older to list: {}", out);
    }

    #[test]
    fn bare_name_resolves_to_the_latest_stable() {
        let p = pack(serde_json::json!({
            "versions": {
                "1.0.0": { "version": "1.0.0" },
                "1.9.0": { "version": "1.9.0" },
                "2.0.0-beta.1": { "version": "2.0.0-beta.1" },
            },
            "dist-tags": { "latest": "2.0.0-beta.1" },
        }));
        // a prerelease `latest` tag never wins the bare-name default
        assert_eq!(pick_inspect_version("pkg", &p, "").unwrap(), "1.9.0");
        assert_eq!(pick_inspect_version("pkg", &p, "latest").unwrap(), "1.9.0");
        // naming a prerelease explicitly is allowed
        assert_eq!(pick_inspect_version("pkg", &p, "2.0.0-beta.1").unwrap(), "2.0.0-beta.1");
        // ranges go through the normal resolver
        assert_eq!(pick_inspect_version("pkg", &p, "^1.0.0").unwrap(), "1.9.0");
        assert!(pick_inspect_version("pkg", &p, "999.0.0").is_err(), "unsatisfiable range errors");

        // a stable `latest` tag wins even when a higher stable exists
        let p2 = pack(serde_json::json!({
            "versions": { "1.0.0": { "version": "1.0.0" }, "2.0.0": { "version": "2.0.0" } },
            "dist-tags": { "latest": "1.0.0" },
        }));
        assert_eq!(pick_inspect_version("pkg", &p2, "").unwrap(), "1.0.0");

        // nothing resolvable: a clear error
        let p3 = pack(serde_json::json!({ "versions": {}, "dist-tags": {} }));
        assert!(pick_inspect_version("pkg", &p3, "").is_err());
    }
}
