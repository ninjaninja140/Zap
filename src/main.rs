mod cmds;
mod config;
mod devtools;
mod http;
mod linker;
mod lockfile;
mod migrate;
mod model;
mod pnp;
mod publish;
mod resolve;
mod store;
mod util;
mod workspace;

use anyhow::Result;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const LOGO: &str = include_str!("../zap.emblem.txt");
const HEADING: &str = include_str!("../zap.heading.txt");

fn ts_badge() -> String {
    "\x1b[48;2;49;120;198m\x1b[97m TS \x1b[0m".to_string()
}

/// the per-command heading from zap.heading.txt
fn print_heading(args: &[String]) {
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("");
    // usage text comes from the command's own file; flag-only commands
    // (version) aren't registry commands, unknown names are implicit
    // script runs, keep the old fallback
    let usage = cmds::lookup(cmd)
        .map(|c| c.usage)
        .unwrap_or_else(|| match cmd {
            "-v" | "--version" => "print the zap version",
            _ => "run a package.json script",
        });
    // flag aliases collapse to their canonical name in the badge
    let badge = match cmd {
        "-v" | "--version" | "version-flag" => "version",
        _ => cmd,
    };
    let cmd_caps = badge.to_uppercase();
    let styled_cmd = format!("\x1b[48;2;49;86;255m\x1b[97m {} \x1b[0m", cmd_caps);
    let styled_zap = "\x1b[38;2;49;86;255mZap\x1b[0m";
    let styled_usage = format!("\x1b[3m\x1b[2m{}\x1b[0m", usage);
    // the full command as typed, greyed out under the header ("zap" prefix is
    // already implied by the header itself, so it's omitted here)
    let styled_full = util::dim(format!("> {}", args.join(" ")));
    let output = HEADING
        .replace("🗲", &format!("\x1b[38;2;49;86;255m🗲\x1b[0m"))
        .replace("zap", styled_zap)
        .replace("v0.0.0", &format!("v{}", VERSION))
        // FULL_COMMAND first — "COMMAND" is a substring of it
        .replace("> FULL_COMMAND", &styled_full)
        .replace("COMMAND", &styled_cmd)
        .replace("USAGE", &styled_usage);
    println!("{}", output);
}

fn banner() -> String {
    let node = std::process::Command::new("node")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "not found".to_string());

    let mut lines: Vec<String> = LOGO.lines().map(|l| l.to_string()).collect();

    // fit the header line to the underscore strip: "Zap: vX" starts on the first ▁
    // column, "Node vY" ends on the last ▁ column
    if let Some(i) = lines.iter().position(|l| l.contains('▁')) {
        let strip = lines[i].clone();
        let strip_start = strip.chars().position(|c| c == '▁').unwrap_or(0);
        let strip_len = strip.chars().filter(|c| *c == '▁').count();
        if i > 0 {
            let mut header = lines[i - 1].chars().take(strip_start).collect::<String>();
            if header.chars().count() < strip_start {
                header.push_str(&" ".repeat(strip_start - header.chars().count()));
            }
            let zap_part = format!("Zap: v{}", VERSION);
            let node_part = format!("Node {}", node);
            let pad = (strip_len.saturating_sub(zap_part.chars().count() + node_part.chars().count())).max(2);
            header.push_str(&format!("{}{}{}", zap_part, " ".repeat(pad), node_part));
            lines[i - 1] = header;
        }
    }

    lines.join("\n").trim_end().to_string()
}

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

    // heading for all commands (skip bare zap and help — the help body is its own front matter)
    if !cmd.is_empty() && !matches!(cmd, "help" | "-h" | "--help") {
        print_heading(args);
        println!();
    }

    match cmd {
        "version-flag" | "-v" | "--version" => {
            println!("zap {}", VERSION);
            Ok(0)
        }
        "help" | "-h" | "--help" => {
            print_help();
            Ok(0)
        }
        // registry dispatch: every command lives in src/cmds/<group>/<command>.rs
        _ => match cmds::lookup(cmd) {
            Some(c) => (c.run)(&rest),
            // implicit run: `zap test` == `zap run test` (also covers bare `zap`)
            None => crate::cmds::run::run::run(args),
        },
    }
}

fn print_help() {
    println!("{}", util::electric(banner()));
    println!();
    println!("{}", util::bold("usage:"));

    // usage section generated from the command registry, one header per group
    let width = cmds::COMMANDS.iter().map(|c| help_names(c).len()).max().unwrap_or(0);
    let mut last_group = "";
    for c in cmds::COMMANDS {
        if c.group != last_group {
            if last_group == "run" {
                // the implicit-run shortcut stays visible under the run section
                println!("{}", command_style("  zap <script> [args]        shortcut for `zap run`"));
            }
            println!();
            println!("{}", util::bold(format!("{}:", c.group)));
            last_group = c.group;
        }
        let names = help_names(c);
        let pad = " ".repeat(width - names.len());
        println!("{}", command_style(&format!("  zap {}{}  {}", names, pad, c.usage)));
    }

    // static configuration/typescript sections stay hand-written
    println!();
    println!("{}", util::bold("configuration (walks up from cwd, first match wins):"));
    for line in CONFIG_SECTION.lines() {
        println!("{}", zap_refs(line));
    }
    println!();
    println!(
        "{} {} a tsconfig.json (or `typescript: 5.9.2` in .zaprc) makes zap",
        ts_badge(),
        util::bold("typescript is built in:")
    );
    println!("manage the compiler + @types/* for deps that don't ship types. `zap version <ver>`");
    println!("pins the compiler version; version changes install automatically.");
}

/// primary name plus aliases, e.g. "run|run-script|r|task"
fn help_names(c: &cmds::Command) -> String {
    let mut s = c.name.to_string();
    for a in c.aliases {
        s.push('|');
        s.push_str(a);
    }
    s
}

/// the hand-written part of help, printed below the generated usage section
const CONFIG_SECTION: &str = r#"  .zaprc.yaml | .zaprc.yml | .zaprc.json | .zaprc.jsonc | .zaprc.toml | .zaprc
  zaprc.yaml  | zaprc.yml  | zaprc.json  | zaprc.jsonc  | zaprc.toml  | zaprc
  (also accepts just zap alongside zaprc)
  keys: linker (pnp|node-modules), registry, concurrency, strict, save-exact,
        prefer-offline, cache-ttl, lifecycle, store-dir, typescript, token,
        editor-node-modules
  env overrides: ZAP_LINKER, ZAP_REGISTRY, ZAP_TOKEN/NPM_TOKEN, ZAP_CONCURRENCY, ZAP_STRICT
  json/jsonc configs get a $schema auto-injected for editor autocomplete"#;

/// `  zap init        create ...` -> zap electric-bold, command bold, description
/// plain, column alignment preserved
fn command_style(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    let rest = &trimmed["zap".len()..]; // " init        create ..."
    let desc_idx = rest.find("  ").unwrap_or(rest.len());
    let cmd = rest[..desc_idx].trim();
    let desc = rest[desc_idx..].trim_start();
    let gap = rest[desc_idx..].len() - desc.len();
    format!(
        "{}{} {}{}{}",
        indent,
        util::electric_bold("zap"),
        " ",
        util::bold(cmd),
        format!("{}{}", " ".repeat(gap), desc)
    )
}

/// colorize every "zap"/"zaprc" mention (one pass so nested spans can't break)
fn zap_refs(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    loop {
        match rest.find("zap") {
            None => {
                out.push_str(rest);
                break;
            }
            Some(i) => {
                out.push_str(&rest[..i]);
                let after = &rest[i..];
                if after.starts_with("zaprc") {
                    out.push_str(&util::electric("zaprc"));
                    rest = &after["zaprc".len()..];
                } else {
                    out.push_str(&util::electric("zap"));
                    rest = &after[3..];
                }
            }
        }
    }
    out
}
