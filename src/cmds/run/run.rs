// `zap run <script>... [-- args...]`, main.rs falls back to this for unknown
// commands too, so bare `zap` and implicit `zap <script>` land here as well

#[path = "_multirun.rs"]
mod multirun;

use crate::cmds::{ensure_runtime_ready, spawn_shell};
use crate::config::Config;
use crate::workspace::install_root_for;
use anyhow::{anyhow, Result};
use std::path::Path;

pub const NAME: &str = "run";
pub const ALIASES: &[&str] = &["run-script", "r", "task"];
pub const USAGE: &str = "run package.json scripts (several names run in live panes, args after `--` go to each)";
pub const ABOUT: &str = "runs one or more package.json scripts; several names run concurrently
in split-screen panes. literal pre<script>/post<script> hooks fire around
each script, and args after `--` are appended to every script. unknown
command names fall through here, so `zap test` runs the test script.";
pub const EXAMPLE: &str = "  $ zap run build
  run one script (pre/post hooks fire)
  $ zap run api worker
  run both scripts concurrently, one pane each";

pub fn run(args: &[String]) -> Result<i32> {
    let ctx = crate::util::find_root()?;
    let install_root = install_root_for(&ctx);
    let cfg = Config::load(&ctx);
    ensure_runtime_ready(&install_root, &cfg)?;
    // everything after the first `--` is shared args appended to each script's
    // command line, everything before it is the script name list
    let (names, shared) = match args.iter().position(|a| a == "--") {
        Some(i) => (&args[..i], &args[i + 1..]),
        None => (args, &args[args.len()..]),
    };
    if names.is_empty() {
        let v = crate::util::read_json(&ctx.join("package.json"))?;
        println!("available scripts:");
        if let Some(s) = v.get("scripts").and_then(|x| x.as_object()) {
            for (k, val) in s {
                println!("  {}, {}", k, val.as_str().unwrap_or(""));
            }
        }
        return Ok(0);
    }
    let v = crate::util::read_json(&ctx.join("package.json"))?;
    let scripts = v.get("scripts").and_then(|x| x.as_object());
    let plans: Vec<Vec<(String, String)>> =
        names.iter().map(|n| plan(scripts, n, shared)).collect::<Result<_>>()?;
    if plans.len() == 1 {
        run_single(&ctx, &install_root, &cfg, &names[0], &plans[0])
    } else {
        multirun::run_multi(&ctx, &install_root, &cfg, names, &plans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_banner_composes_npm_style_lines() {
        let v = serde_json::json!({ "name": "myapp", "version": "1.2.3" });
        assert_eq!(run_banner(&v, "build", "tsc -p ."), "> myapp@1.2.3 build\n> tsc -p .\n");
        // missing version: name only
        let v = serde_json::json!({ "name": "myapp" });
        assert_eq!(run_banner(&v, "build", "echo hi"), "> myapp build\n> echo hi\n");
        // missing both: bare script
        let v = serde_json::json!({});
        assert_eq!(run_banner(&v, "build", "echo hi"), "> build\n> echo hi\n");
        // the composed line carries appended shared args
        let v = serde_json::json!({ "name": "a", "version": "1" });
        assert_eq!(run_banner(&v, "s", "echo x --flag"), "> a@1 s\n> echo x --flag\n");
    }
}
/// build the (script key, shell line) stage sequence for one script:
/// pre<S> -> S (with the shared args) -> post<S>, exact literal keys only,
/// so aliases and hooks of hooks get no pre/post expansion
fn plan(
    scripts: Option<&serde_json::Map<String, serde_json::Value>>,
    name: &str,
    shared: &[String],
) -> Result<Vec<(String, String)>> {
    let get = |k: &str| scripts.and_then(|s| s.get(k)).and_then(|x| x.as_str()).map(|s| s.to_string());
    let main = get(name).ok_or_else(|| anyhow!("script `{}` not found in package.json", name))?;
    let suffix = if shared.is_empty() { String::new() } else { format!(" {}", shared.join(" ")) };
    let mut stages = Vec::new();
    if let Some(pre) = get(&format!("pre{}", name)) {
        stages.push((format!("pre{}", name), pre));
    }
    stages.push((name.to_string(), format!("{}{}", main, suffix)));
    if let Some(post) = get(&format!("post{}", name)) {
        stages.push((format!("post{}", name), post));
    }
    Ok(stages)
}

/// npm's two-line banner: `> name@ver script` + `> the command line`; the
/// manifest's name/version parts print when present, skipped gracefully
/// otherwise (pure, tested)
fn run_banner(manifest: &serde_json::Value, script: &str, line: &str) -> String {
    let name = manifest.get("name").and_then(|x| x.as_str()).unwrap_or("");
    let version = manifest.get("version").and_then(|x| x.as_str()).unwrap_or("");
    let head = if !name.is_empty() && !version.is_empty() {
        format!("{}@{} ", name, version)
    } else if !name.is_empty() {
        format!("{} ", name)
    } else {
        String::new()
    };
    format!("> {}{}\n> {}\n", head, script, line)
}

/// plain streaming single-script run: pre<S> -> S -> post<S>, npm semantics
/// (a failing stage stops the sequence and becomes the exit code). npm's
/// banner prints once for the MAIN script (hooks run silent, output only)
fn run_single(
    ctx: &Path,
    install_root: &Path,
    cfg: &Config,
    name: &str,
    stages: &[(String, String)],
) -> Result<i32> {
    let v = crate::util::read_json(&ctx.join("package.json"))?;
    // the main stage's line (script + the appended shared args)
    if let Some((_, main_line)) = stages.iter().find(|(l, _)| l == name) {
        print!("{}", run_banner(&v, name, main_line));
        println!();
    }
    for (_label, line) in stages {
        let code = spawn_shell(ctx, install_root, line, cfg)?;
        if code != 0 {
            return Ok(code);
        }
    }
    Ok(0)
}
