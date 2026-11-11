// `zap registry`, thin wrapper over the publish library

pub const NAME: &str = "registry";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "switch registries (private -> public flow)";
pub const ABOUT: &str = "manage named registries: `add <name> <url>`, `use <name>`, `remove
<name>`; `current` (the default) prints the active one and a stored token
migrates with the switch, private -> public.";
pub const EXAMPLE: &str = "  $ zap registry add corp https://registry.corp.dev
  register a private registry for `--registry corp` runs";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::publish::registry_cmd(args)
}
