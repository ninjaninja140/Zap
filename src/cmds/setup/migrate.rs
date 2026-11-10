// `zap migrate [--dry-run] [--yes]`, thin wrapper over the migrate library
// (src/migrate.rs stays as the conversion library)

pub const NAME: &str = "migrate";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "convert npm/yarn/pnpm/bun/deno projects to zap";

pub fn run(args: &[String]) -> anyhow::Result<i32> {
    crate::migrate::migrate_cmd(args)
}
