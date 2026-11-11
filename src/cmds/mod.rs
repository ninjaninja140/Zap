// per-command modules under src/cmds/<group>/<command>.rs, discovered by
// build.rs into gen.rs; every command file carries its own NAME/ALIASES/USAGE
// plus a `run` entrypoint, so help/dispatch/heading are all registry-driven

mod _shared;
pub use _shared::*;

mod gen;

// hoist the generated group modules + registry to cmds level
pub use gen::*;

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
}
