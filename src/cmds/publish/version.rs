// `zap version`, version bumping for package.json (+ changelog entry, git commit/tag)

use anyhow::{anyhow, bail, Context, Result};
use semver::Version;
use std::path::Path;

use crate::util::green;

pub const NAME: &str = "version";
pub const ALIASES: &[&str] = &["bump"];
pub const USAGE: &str = "bump package.json version (major|minor|patch|<x.y.z>; --no-git-tag-version, -m <msg>)";
pub const ABOUT: &str = "npm-version-style bump of package.json: major|minor|patch or an
explicit x.y.z. writes a changelog entry and a git commit + tag by default;
--no-git-tag-version skips the git side, -m <msg> overrides the message.
alias: bump.";
pub const EXAMPLE: &str = "  $ zap version patch
  1.2.3 -> 1.2.4, changelog entry, commit and tag";

/// pure version math: major/minor/patch bumps strip prerelease+build metadata;
/// anything else is parsed as an exact version (prerelease/build allowed there)
fn next_version(cur: &Version, spec: &str) -> Result<Version> {
    match spec {
        "major" | "minor" | "patch" => {
            let mut v = cur.clone();
            // shortcut: npm's prerelease-re.increment dance is out of scope, bumps go straight to a release
            v.pre = semver::Prerelease::EMPTY;
            v.build = semver::BuildMetadata::EMPTY;
            match spec {
                "major" => {
                    v.major += 1;
                    v.minor = 0;
                    v.patch = 0;
                }
                "minor" => {
                    v.minor += 1;
                    v.patch = 0;
                }
                _ => v.patch += 1,
            }
            Ok(v)
        }
        _ => Version::parse(spec).map_err(|_| anyhow!("`{}` is not a valid semver version", spec)),
    }
}

/// today as YYYY-MM-DD (std-only, utc civil-date conversion)
fn today() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        / 86400;
    // howard hinnant's days-from-civil inverse
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// run git in the project dir, surfacing stderr on failure
fn git(ctx: &Path, args: &[&str]) -> Result<()> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(ctx)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<i32> {
    // parse: one positional (or none = print current), --no-git-tag-version, -m/--message <msg>
    let mut spec: Option<String> = None;
    let mut message: Option<String> = None;
    let mut no_git = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-git-tag-version" => no_git = true,
            "-m" | "--message" => {
                i += 1;
                message = Some(
                    args.get(i)
                        .context("missing value after -m/--message")?
                        .clone(),
                );
            }
            a if a.starts_with('-') => bail!("unknown flag `{}`, see zap help version", a),
            a => {
                if spec.is_some() {
                    bail!("usage: zap version [major|minor|patch|<x.y.z>] [--no-git-tag-version] [-m <msg>]");
                }
                spec = Some(a.to_string());
            }
        }
        i += 1;
    }

    let ctx = crate::util::find_root()?;
    let pkg_path = ctx.join("package.json");
    let mut v = crate::util::read_json(&pkg_path)?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| anyhow!("package.json root must be an object"))?;

    // no args: just print the current version
    let Some(spec) = spec else {
        let cur = obj
            .get("version")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow!("package.json has no version field"))?;
        println!("{}", cur);
        return Ok(0);
    };

    let cur_str = obj
        .get("version")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("package.json has no version field"))?;
    let cur = Version::parse(cur_str)
        .map_err(|e| anyhow!("package.json version `{}` is not valid semver: {}", cur_str, e))?;
    let next = next_version(&cur, &spec)?;
    if next == cur {
        bail!("version is already {}", cur);
    }

    // insert on an existing key keeps its position, so key order survives
    obj.insert("version".into(), serde_json::json!(next.to_string()));
    std::fs::write(&pkg_path, format!("{}\n", serde_json::to_string_pretty(&v)?))?;

    // prepend a keep-a-changelog-ish entry when CHANGELOG.md exists (never created)
    let changelog = ctx.join("CHANGELOG.md");
    let mut touched_changelog = false;
    if changelog.is_file() {
        let old = std::fs::read_to_string(&changelog)?;
        std::fs::write(&changelog, format!("## [{}] - {}\n\n{}", next, today(), old))?;
        touched_changelog = true;
    }

    // git is on by default when the project sits inside a repo, skippable entirely
    if !no_git {
        let mut dir = ctx.clone();
        loop {
            if dir.join(".git").exists() {
                let msg = message.unwrap_or_else(|| format!("v{}", next));
                git(&ctx, &["add", "package.json"])?;
                if touched_changelog {
                    git(&ctx, &["add", "CHANGELOG.md"])?;
                }
                git(&ctx, &["commit", "-m", &msg])?;
                git(&ctx, &["tag", &format!("v{}", next)])?;
                println!("{} committed {}, tagged v{}", green("✓"), msg, next);
                break;
            }
            if !dir.pop() {
                break; // not a repo, git silently skipped
            }
        }
    }

    println!("{} version {} → {}", green("✓"), cur, next);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn bumps_strip_prerelease_and_build() {
        for (spec, from, want) in [
            ("major", "1.2.3", "2.0.0"),
            ("minor", "1.2.3", "1.3.0"),
            ("patch", "1.2.3", "1.2.4"),
            ("major", "1.2.3-rc.1+build.7", "2.0.0"),
            ("minor", "1.2.3-rc.1+build.7", "1.3.0"),
            ("patch", "1.2.3-rc.1+build.7", "1.2.4"),
        ] {
            assert_eq!(next_version(&v(from), spec).unwrap().to_string(), want, "{} from {}", spec, from);
        }
    }

    #[test]
    fn exact_set_parses_and_keeps_prerelease() {
        assert_eq!(next_version(&v("1.2.3"), "0.4.2-beta.1").unwrap().to_string(), "0.4.2-beta.1");
        assert_eq!(next_version(&v("1.2.3"), "2.0.0").unwrap().to_string(), "2.0.0");
    }

    #[test]
    fn garbage_is_rejected() {
        for s in ["not-a-version", "1.2", "1.2.x", ""] {
            assert!(next_version(&v("1.2.3"), s).is_err(), "`{}` should be rejected", s);
        }
    }

    #[test]
    fn today_is_iso() {
        let t = today();
        assert_eq!(t.len(), 10);
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[7..8], "-");
    }
}
