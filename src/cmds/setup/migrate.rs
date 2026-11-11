// `zap migrate [--dry-run] [--yes]`, thin wrapper over the migrate library
// (src/migrate.rs stays as the conversion library)

pub const NAME: &str = "migrate";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "convert npm/yarn/pnpm/bun/deno projects to zap";
pub const ABOUT: &str = "converts an existing npm/yarn/pnpm/bun/deno project to zap: imports
its lockfile into .zap-lock.json and installs. --dry-run previews, --yes
skips the confirmation.";
pub const EXAMPLE: &str = "  $ zap migrate --dry-run
  preview what the conversion would import, write nothing";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::migrate::migrate_cmd(args)
}
