// `zap view|info <pkg> [field]`, thin wrapper over the devtools library

pub const NAME: &str = "view";
pub const ALIASES: &[&str] = &["info"];
pub const USAGE: &str = "registry metadata for a package, cached (<pkg> [field], --json)";
pub const ABOUT: &str = "prints registry metadata for a package, versions, dist-tags, the
latest release, through the same metadata cache installs use. `zap view
<pkg> <field>` narrows the output (already json); --json prints the whole
summary as a json document instead of the human table. alias: info.";
pub const EXAMPLE: &str = "  $ zap view chalk version
  read one field of chalk's registry metadata
  $ zap view chalk --json
  the full summary as a json document, for tools";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::devtools::info_cmd(args)
}
