// `zap set-version <version|"latest">`, pin the version the script
// installers should install (~/.zap/version). script installs only:
// npm-managed zaps update through npm.

use anyhow::{anyhow, bail, Result};

pub const NAME: &str = "set-version";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "pin the version script installs should install (<version> | latest; no arg = print)";
pub const ABOUT: &str = "sets the version pin the shell installers (install-zap.sh/.ps1) read:
`zap set-version 0.7.0` pins an exact release, `zap set-version latest`
fetches and pins the newest one. both verify against the real releases
list, an unknown version or an unpublished repo errors. no arg prints the
pin. script installs only, npm-managed zaps update through npm.";
pub const EXAMPLE: &str = "  $ zap set-version latest
  pin the newest published release
  $ zap set-version 0.7.0
  pin that exact release, verified against the releases list";

/// explicit pin specs parse as semver; `latest` resolves through the api
fn parse_pin_spec(spec: &str) -> Result<semver::Version> {
    semver::Version::parse(spec).map_err(|_| anyhow!("`{}` is not a valid semver version (or `latest`)", spec))
}

pub fn run(args: &[String]) -> Result<i32> {
    // no arg: print the current pin (with install-source context, the pin
    // only matters to script installs)
    let Some(spec) = args.iter().find(|a| !a.starts_with('-')) else {
        match crate::util::read_version_pin()? {
            Some(p) => {
                println!("{}", p);
                println!(
                    "{}",
                    crate::util::dim(format!(
                        "  the pin applies to script installs; this zap came from: {}",
                        crate::util::install_source()
                    ))
                );
                return Ok(0);
            }
            None => bail!("no version pin set"),
        }
    };

    if spec == "latest" {
        // resolve-and-pin: the concrete version is stored (a "latest" pin
        // would mean the same as no pin at all)
        let tags = crate::http::github_release_tags()?;
        let Some(newest) = tags.first() else {
            bail!("no releases published yet");
        };
        crate::util::write_version_pin(newest)?;
        println!("{} pinned {}", crate::util::green("✓"), newest);
        return Ok(0);
    }

    // explicit: parse, then verify it's a real release before writing
    let v = parse_pin_spec(spec)?;
    let tags = crate::http::github_release_tags()?;
    if tags.is_empty() {
        bail!("no releases published yet");
    }
    if !tags.iter().any(|t| t == &v.to_string()) {
        bail!("{} is not a published release (published: {})", v, tags.join(", "));
    }
    crate::util::write_version_pin(&v.to_string())?;
    println!("{} pinned {}", crate::util::green("✓"), v);
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_specs_parse_or_reject() {
        assert!(parse_pin_spec("0.7.0").is_ok());
        assert!(parse_pin_spec("1.2.3-beta.1").is_ok());
        for s in ["latest", "garbage", "1.2", "1.2.x", ""] {
            assert!(parse_pin_spec(s).is_err(), "`{}` should be rejected", s);
        }
    }

    #[test]
    fn pin_store_round_trips_through_zap_home() {
        fn tempdir(tag: &str) -> std::path::PathBuf {
            let dir = std::env::temp_dir().join(format!("zap-pin-{}-{}", tag, std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }
        let _env = crate::cmds::env_test_lock();
        let home = tempdir("pinstore");
        std::env::set_var("ZAP_HOME", &home);
        assert_eq!(crate::util::read_version_pin().unwrap(), None, "nothing set");
        crate::util::write_version_pin("0.7.0").unwrap();
        assert_eq!(crate::util::read_version_pin().unwrap().as_deref(), Some("0.7.0"));
        // the file shape: `<version>\n`
        assert_eq!(
            std::fs::read_to_string(crate::util::version_pin_path().unwrap()).unwrap(),
            "0.7.0\n"
        );
        std::env::remove_var("ZAP_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
