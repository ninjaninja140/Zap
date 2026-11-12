// per-command modules under src/cmds/<group>/<command>.rs, discovered by
// build.rs into gen.rs; every command file carries its own NAME/ALIASES/USAGE
// plus a `run` entrypoint, so help/dispatch/heading are all registry-driven

mod _shared;
pub use _shared::*;

#[path = "_progress.rs"]
pub mod progress;

#[path = "_rawui.rs"]
pub mod rawui;

mod gen;

// hoist the generated group modules + registry to cmds level
pub use gen::*;

// the generated-api spec table (build.rs includes the same file for the ts
// generation, keep it free of crate dependencies)
#[path = "../api_spec.rs"]
pub mod api_spec;

/// one registry entry, generated per file under src/cmds/<group>/<command>.rs
pub struct Command {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub usage: &'static str,
    pub about: &'static str,
    pub example: &'static str,
    pub group: &'static str,
    pub run: fn(&[String]) -> anyhow::Result<i32>,
}

/// find a command by primary name or alias
pub fn lookup(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name || c.aliases.contains(&name))
}

/// group name -> one-line description for the `zap help` category overview;
/// build.rs owns which groups exist (gen.rs), this owns the human wording
pub const GROUP_INFO: &[(&str, &str)] = &[
    ("install", "project setup + dependency management"),
    ("run", "scripts, executables and the ts compiler"),
    ("info", "inspecting and auditing the dependency tree"),
    ("publish", "npm registry protocol, pack, publish, auth"),
    ("setup", "config, migration and AI-injection"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// every command must carry real about/example text: a command file
    /// without the consts already fails the build (gen.rs references them
    /// directly), this catches empty placeholders, and the example format
    /// contract (a `  $ zap ...` line) that the help view relies on
    #[test]
    fn every_command_has_about_and_example() {
        for c in COMMANDS {
            assert!(!c.about.is_empty(), "{}: about is empty", c.name);
            assert!(!c.example.is_empty(), "{}: example is empty", c.name);
            assert!(
                c.example.lines().any(|l| l.trim_start().starts_with("$ zap")),
                "{}: example lacks a `$ zap` invocation",
                c.name
            );
            assert!(c.about.lines().count() >= 1 && c.usage.len() < c.about.len(), "{}: about is thinner than its usage", c.name);
        }
    }

    /// the `zap help` category overview is driven by GROUP_INFO, so every
    /// registry group must have an entry (and vice versa, no dead entries)
    #[test]
    fn group_info_covers_the_registry() {
        for c in COMMANDS {
            assert!(
                GROUP_INFO.iter().any(|(g, _)| *g == c.group),
                "{}: group '{}' missing from GROUP_INFO",
                c.name,
                c.group
            );
        }
        for (g, desc) in GROUP_INFO {
            assert!(COMMANDS.iter().any(|c| c.group == *g), "GROUP_INFO group '{}' has no commands", g);
            assert!(!desc.is_empty(), "GROUP_INFO group '{}': empty description", g);
        }
    }

    /// the generated ts metadata (build.rs → packages/api/src/commands.gen.ts)
    /// mirrors the registry: every command name as meta, and the crate version
    /// as ZAP_VERSION. optional read: a fresh clone may not have run a build
    /// that emits the file yet
    #[test]
    fn generated_ts_surface_matches_registry() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packages/api/src/commands.gen.ts");
        let Ok(src) = std::fs::read_to_string(&path) else { return };
        for c in COMMANDS {
            assert!(src.contains(&format!("name: \"{}\"", c.name)), "generated ts missing meta for `{}`", c.name);
        }
        assert!(
            src.contains(&format!("export const ZAP_VERSION = \"{}\";", env!("CARGO_PKG_VERSION"))),
            "generated ts ZAP_VERSION is not the crate version"
        );
    }

    /// the node packages' versions are synced from the crate by build.rs;
    /// build.rs's own unit tests don't run under `cargo test`, so assert the
    /// effect here (a build resyncs a drifted package.json)
    #[test]
    fn node_package_versions_track_the_crate() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for pkg in ["packages/cli/package.json", "packages/api/package.json"] {
            let Ok(src) = std::fs::read_to_string(root.join(pkg)) else { continue };
            let version = src.lines().find_map(|l| l.trim().strip_prefix("\"version\": \"")).and_then(|r| r.strip_suffix("\","));
            assert_eq!(version, Some(env!("CARGO_PKG_VERSION")), "{}: version drifted from the crate", pkg);
        }
    }

    /// the api spec table must cover the registry exactly, both directions,
    /// same completeness guard as about/example
    #[test]
    fn api_spec_covers_the_registry() {
        for c in COMMANDS {
            let spec = api_spec::SPECS
                .iter()
                .find(|s| s.name == c.name)
                .unwrap_or_else(|| panic!("api_spec.rs: no spec for command `{}`", c.name));
            assert!(!spec.returns.is_empty(), "{}: empty returns type", c.name);
            for o in spec.options {
                assert!(o.flag.starts_with("--"), "{}: option flag `{}` must be the long form", c.name, o.flag);
                assert!(!o.desc.is_empty(), "{}: option `{}` has an empty desc", c.name, o.flag);
            }
        }
        for s in api_spec::SPECS {
            assert!(
                COMMANDS.iter().any(|c| c.name == s.name),
                "api_spec.rs: command `{}` is not in the registry (stale spec?)",
                s.name
            );
        }
    }

    /// the generated ts api carries the spec: a builder per command and every
    /// option's flag string (the flags are the drift surface, a spec typo
    /// shows up here and in the ts argv unit tests)
    #[test]
    fn api_gen_ts_carries_the_spec_flags() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packages/api/src/api.gen.ts");
        let Ok(src) = std::fs::read_to_string(&path) else { return };
        for s in api_spec::SPECS {
            // hyphenated names camelCase in ts identifiers ("set-version" → "setVersion")
            let mut ts = String::new();
            for (i, part) in s.name.split('-').enumerate() {
                if i == 0 {
                    ts.push_str(part);
                } else if let Some(c) = part.chars().next() {
                    ts.push(c.to_ascii_uppercase());
                    ts.push_str(&part[1..]);
                }
            }
            assert!(src.contains(&format!("function {ts}Argv")), "api.gen.ts missing builder for `{}`", s.name);
            for o in s.options {
                assert!(src.contains(o.flag), "api.gen.ts missing flag `{}` (spec of `{}`)", o.flag, s.name);
            }
        }
    }
}
