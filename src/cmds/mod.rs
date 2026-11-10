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
    pub group: &'static str,
    pub run: fn(&[String]) -> anyhow::Result<i32>,
}

/// find a command by primary name or alias
pub fn lookup(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name || c.aliases.contains(&name))
}
