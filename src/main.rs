mod cmds;
mod config;
mod devtools;
mod http;
mod linker;
mod lockfile;
mod model;
mod pnp;
mod publish;
mod resolve;
mod store;
mod util;
mod workspace;

use anyhow::Result;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("{} {}", util::red("error:"), e);
            for cause in e.chain().skip(1) {
                eprintln!("  {}", cause);
            }
            std::process::exit(1);
        }
    }
}

fn run(args: &[String]) -> Result<i32> {
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
    match cmd {
        "i" | "install" => cmds::install(&rest),
        "add" | "a" => cmds::add(&rest),
        "rm" | "remove" | "uninstall" => cmds::remove(&rest),
        "run" | "run-script" | "r" | "task" => cmds::run_script(&rest),
        "exec" => cmds::exec(&rest),
        "x" => cmds::x(&rest),
        "dlx" => cmds::dlx(&rest),
        "create" => cmds::create(&rest),
        "pack" => publish::pack_cmd(&rest),
        "publish" => publish::publish_cmd(&rest),
        "whoami" => publish::whoami_cmd(&rest),
        "login" | "adduser" => publish::login_cmd(&rest),
        "logout" => publish::logout_cmd(&rest),
        "registry" => publish::registry_cmd(&rest),
        "tsc" => cmds::tsc(&rest),
        "view" | "info" => devtools::info_cmd(&rest),
        "licenses" => devtools::licenses_cmd(&rest),
        "audit" => devtools::audit_cmd(&rest),
        "doctor" => devtools::doctor_cmd(&rest),
        "ls" | "list" => cmds::ls(&rest),
        "why" => cmds::why(&rest),
        "outdated" => cmds::outdated(&rest),
        "upgrade" | "up" => cmds::upgrade(&rest),
        "store" => cmds::store(&rest),
        "bin" => cmds::bin(&rest),
        "init" => cmds::init(&rest),
        "cache" => cmds::cache(&rest),
        "help" | "-h" | "--help" => {
            print_help();
            Ok(0)
        }
        "version" => cmds::version_cmd(&rest),
        "version-flag" | "-v" | "--version" => {
            println!("zap 0.1.0");
            Ok(0)
        }
        // implicit run: `zap test` == `zap run test`
        _ => cmds::run_script(args),
    }
}

fn print_help() {
    println!(
        r#"🗲Zap: a tiny, fast node package manager with a custom pnp runtime

usage:
  zap init                   create a package.json
  zap add <pkg>[@range]...   add dependencies (supports --dev; workspace deps auto-link)
  zap remove <pkg>...        remove dependencies
  zap i[nstall] [--offline]  install everything from package.json (incl. workspaces;
                             also --force, --refresh, --frozen-lockfile for CI)
  zap run <script> [args]    run a package.json script with the linker active
  zap r|task <script>        aliases for run
  zap <script> [args]        shortcut for `zap run`
  zap exec <cmd> [args]      run any command with the linker active
  zap x <bin> [args]         run an installed bin locally, or fetch it (like npx)
  zap dlx <pkg> [args]       force one-off run of a package binary
  zap view|info <pkg> [f]    registry metadata for a package (cached)
  zap licenses               license summary of installed packages
  zap audit                  check installed packages for known advisories
  zap doctor                 sanity checks: lockfile sync, runtime, ts, store
  zap create <tmpl> [args]   scaffold with a create-* initializer (like npm create)
  zap tsc [args]             run the zap-managed typescript compiler
  zap version [maj|min|pat]  bump package.json version (npm version)
  zap pack                   build an npm tarball from package.json
  zap publish [--tag t]      publish a package (auth via ZAP_TOKEN / `token` in .zaprc)
       [--access a] [--otp c] [--registry name|url] [--dry-run]
  zap registry [add|list|use|remove|current]  switch registries (private -> public flow)
  zap whoami                 print the authenticated registry user
  zap login [--token t]      browser login (npm web-auth flow) or paste a token
  zap logout                 remove stored credentials for this registry
  zap upgrade [pkg...]       no args: re-resolve within ranges; with args: bump to latest
  zap outdated               show deps that are behind the registry (exit 1 if any)
  zap ls [--all]             list dependencies and workspace members
  zap why <pkg>              show dependency chains for a package
  zap bin [name]             print bin paths from the dependency tree
  zap store [path|size]      inspect the global content-addressable store
  zap cache clean            wipe the global store, metadata cache and dlx cache

configuration (walks up from cwd, first match wins):
  .zaprc.yaml | .zaprc.yml | .zaprc.json | .zaprc.jsonc | .zaprc.toml | .zaprc
  zaprc.yaml  | zaprc.yml  | zaprc.json  | zaprc.jsonc  | zaprc.toml  | zaprc 
  (also accepts just zap alongside zaprc)
  keys: linker (pnp|node-modules), registry, concurrency, strict, save-exact,
        prefer-offline, cache-ttl, lifecycle, store-dir, typescript, token,
        editor-node-modules
  env overrides: ZAP_LINKER, ZAP_REGISTRY, ZAP_TOKEN/NPM_TOKEN, ZAP_CONCURRENCY, ZAP_STRICT
  json/jsonc configs get a $schema auto-injected for editor autocomplete

typescript is built in: a tsconfig.json (or `typescript: 5.9.2` in .zaprc) makes zap
manage the compiler + @types/* for deps that don't ship types. `zap version <ver>`
pins the compiler version; version changes install automatically.
"#
    );
}
