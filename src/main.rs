mod allow;
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

/// the styled per-command heading block (emblem line, strip, badge+usage,
/// full command), the same replacement order as always, with
/// FULL_COMMAND before COMMAND ("COMMAND" is a substring of it). shared by
/// print_heading and the interactive uis, which paint it inside their alt
/// screen so it stays visible during interaction
pub fn heading_block(args: &[String]) -> String {
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
    HEADING
        .replace("🗲", &format!("\x1b[38;2;49;86;255m🗲\x1b[0m"))
        .replace("zap", styled_zap)
        .replace("v0.0.0", &format!("v{}", VERSION))
        // FULL_COMMAND first, "COMMAND" is a substring of it
        .replace("> FULL_COMMAND", &styled_full)
        .replace("COMMAND", &styled_cmd)
        .replace("USAGE", &styled_usage)
}

/// the per-command heading from zap.heading.txt
fn print_heading(args: &[String]) {
    println!("{}", heading_block(args));
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

    // fit the header line to the underscore strip: "zap: vX" starts on the first ▁
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
            let zap_part = format!("zap: v{}", VERSION);
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

    // heading for all commands (skip bare zap and help, the help body is its own front matter)
    if !cmd.is_empty() && !matches!(cmd, "help" | "-h" | "--help") {
        print_heading(args);
        println!();
    }

    match cmd {
        // bare `zap`: the category overview, same as `zap help`, the
        // implicit-run path never sees empty args (outside projects that was
        // a find_root error, inside them the scripts listing)
        "" => help_command(&[]),
        "version-flag" | "-v" | "--version" => {
            println!("zap {}", VERSION);
            Ok(0)
        }
        "help" | "-h" | "--help" => help_command(&rest),
        // registry dispatch: every command lives in src/cmds/<group>/<command>.rs
        _ => match cmds::lookup(cmd) {
            Some(c) => {
                // `zap <cmd> --help` / -h: the command's own help view, under
                // the per-command heading that already printed above. help
                // wins over any other args; unknown names and bare `--help`
                // keep their old paths
                if rest.iter().any(|a| a == "--help" || a == "-h") {
                    println!("{}", command_help(c));
                    return Ok(0);
                }
                (c.run)(&rest)
            }
            // implicit run: `zap test` == `zap run test` (also covers bare `zap`)
            None => crate::cmds::run::run::run(args),
        },
    }
}

/// `zap <cmd> --help`: the command's own usage view, bold name, dim short
/// usage, the about paragraph, aliases when present, then the example block
/// with the `$` prompt in electric and explanation lines dim. pure so the
/// rendering test can share it
fn command_help(c: &cmds::Command) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}, {}\n", util::bold(c.name.to_uppercase()), util::dim(c.usage)));
    out.push_str(&format!("{}\n", c.about));
    if !c.aliases.is_empty() {
        out.push_str(&format!("{}\n", util::dim(format!("aliases: {}", c.aliases.join(", ")))));
    }
    out.push_str(&format!("{}\n", util::dim("example:")));
    for line in c.example.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("$ ") {
            out.push_str(&format!("  {} {}\n", util::electric("$"), rest));
        } else {
            out.push_str(&format!("{}\n", util::dim(line.trim_start())));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_block_holds_all_four_lines() {
        let args = vec![
            "upgrade".to_string(),
            "--interactive".to_string(),
            "chalk".to_string(),
        ];
        let s = heading_block(&args);
        assert_eq!(s.lines().count(), 4, "emblem, strip, badge, full command");
        assert!(s.contains("UPGRADE"), "the badge collapses to the canonical name");
        assert!(s.contains("> upgrade --interactive chalk"), "full command as typed");
        assert!(s.contains("no args: re-resolve"), "usage from the registry");
    }

    #[test]
    fn help_view_renders_every_section() {        let c = cmds::lookup("install").expect("install in registry");
        let s = command_help(c);
        assert!(s.contains("INSTALL"), "bold uppercased name");
        assert!(s.contains("aliases: i"), "aliases listed when present");
        assert!(s.contains("example:"), "example label");
        // the `$` prompt renders electric, so the plain sequence is split
        assert!(s.contains(&util::electric("$")), "electric prompt");
        assert!(s.contains("zap install --frozen-lockfile"), "the example invocation");
        // alias-free commands hide the aliases line entirely
        let c = cmds::lookup("outdated").expect("outdated in registry");
        let s = command_help(c);
        assert!(!s.contains("aliases:"), "no aliases line when none exist");
        assert!(s.contains("zap outdated -i"), "outdated example present");
    }

    #[test]
    fn bare_help_is_categories_only() {
        let s = categories_view();
        assert!(s.contains("categories:"), "category overview");
        assert!(s.contains("commands in one category"), "usage block");
        assert!(s.contains("everything"), "usage block");
        for (group, desc) in cmds::GROUP_INFO {
            let count = cmds::COMMANDS.iter().filter(|c| c.group == *group).count();
            assert!(s.contains(&format!("({} commands)", count)), "{}: live count", group);
            assert!(s.contains(desc), "{}: description", group);
        }
        // zero command rows: no group headers, no alias pipes, no prose tail
        assert!(!s.contains("install:"), "no group sections");
        assert!(!s.contains('|'), "no command rows (aliases carry pipes)");
        assert!(!s.contains("configuration"), "no prose sections");
    }

    #[test]
    fn category_help_shows_only_that_group() {
        let s = category_view("run");
        assert!(s.contains("run:"), "group header");
        assert!(s.contains("shortcut for `zap run`"), "implicit-run shortcut stays");
        assert!(s.contains("see also: zap help --all"), "dim hint at the end");
        let run = cmds::lookup("run").expect("run in registry");
        assert!(s.contains(help_names(run).as_str()), "run row present");
        assert!(!s.contains("install:"), "no other group sections");
        assert!(!s.contains("publish:"), "no other group sections");
        assert!(!s.contains("configuration"), "no prose sections");
    }

    #[test]
    fn all_help_lists_every_row_and_the_prose() {
        let s = full_help_view();
        for c in cmds::COMMANDS {
            // command_style paints "zap" and the name separately, so the plain
            // "zap add" substring never exists; match the name+usage pieces
            assert!(
                s.contains(help_names(c).as_str()) && s.contains(c.usage),
                "{} row present",
                c.name
            );
        }
        assert!(s.contains("configuration (walks up from cwd"), "config prose");
        assert!(s.contains("typescript is built in:"), "ts prose");
        assert!(!s.contains("categories:"), "the overview stays out of --all");
    }

    #[test]
    fn unknown_category_error_lists_the_valid_ones() {
        let s = help_unknown("nope");
        assert!(s.contains("nope"), "echoes the bad name");
        for (g, _) in cmds::GROUP_INFO {
            assert!(s.contains(g), "lists valid category {}", g);
        }
    }

    #[test]
    fn bare_zap_routes_to_the_categories_view() {
        // bare `zap` == `zap help`: overview + exit 0, never the implicit-run
        // path. discriminable here because the crate root has no package.json
        // (the old route returned the find_root error); the view bytes are
        // covered by bare_help_is_categories_only
        assert!(run(&[]).is_ok(), "bare zap exits 0");
    }
}

/// `zap help`: bare = category overview, `--all` = the full grouped listing,
/// `<category>` = just that group's section. `--help`/`-h` among the args
/// keep the bare view (help itself is not a registry command)
fn help_command(rest: &[String]) -> Result<i32> {
    let arg = rest.iter().map(|s| s.as_str()).find(|a| *a != "--help" && *a != "-h");
    match arg {
        None => {
            // views carry their own trailing newline, print! keeps the exact bytes
            print!("{}", categories_view());
            Ok(0)
        }
        Some("--all") => {
            print!("{}", full_help_view());
            Ok(0)
        }
        Some(name) => {
            if cmds::GROUP_INFO.iter().any(|(g, _)| *g == name) {
                print!("{}", category_view(name));
                Ok(0)
            } else {
                Err(anyhow::anyhow!(help_unknown(name)))
            }
        }
    }
}

/// the unknown-category error, listing the valid categories
fn help_unknown(name: &str) -> String {
    format!(
        "unknown help category '{}', valid categories: {}",
        name,
        cmds::GROUP_INFO.iter().map(|(g, _)| *g).collect::<Vec<_>>().join(", ")
    )
}

/// `zap help`: the category overview only, banner, help usage, one line per
/// group with a live command count from the registry
fn categories_view() -> String {
    let mut out = format!("{}\n\n{}\n", util::electric(banner()), util::bold("usage:"));
    out.push_str(&format!(
        "{}\n",
        command_style("  zap help [category]        commands in one category")
    ));
    out.push_str(&format!("{}\n", command_style("  zap help --all             everything")));
    out.push_str(&format!("\n{}\n", util::bold("categories:")));
    let width = cmds::GROUP_INFO.iter().map(|(g, _)| g.len()).max().unwrap_or(0);
    for (group, desc) in cmds::GROUP_INFO {
        let count = cmds::COMMANDS.iter().filter(|c| c.group == *group).count();
        let pad = " ".repeat(width - group.len());
        out.push_str(&format!("  {}{}  {} ({} commands)\n", util::bold(group), pad, desc, count));
    }
    out
}

/// `zap help <category>`: banner plus only that group's section, rendered
/// exactly as its slice of the full help, ending with a see-also hint
fn category_view(group: &str) -> String {
    let mut out = format!("{}\n\n", util::electric(banner()));
    out.push_str(&group_section(group));
    out.push_str(&format!("\n{}\n", util::dim("see also: zap help --all")));
    out
}

/// `zap help --all`: today's full grouped listing, unchanged
fn full_help_view() -> String {
    let mut out = format!("{}\n\n{}\n", util::electric(banner()), util::bold("usage:"));

    // groups in registry order, one blank line before each section
    let mut groups: Vec<&str> = Vec::new();
    for c in cmds::COMMANDS {
        if !groups.contains(&c.group) {
            groups.push(c.group);
        }
    }
    for group in groups {
        out.push('\n');
        out.push_str(&group_section(group));
    }

    // static configuration/typescript sections stay hand-written
    out.push('\n');
    out.push_str(&format!(
        "{}\n",
        util::bold("configuration (walks up from cwd, first match wins):")
    ));
    for line in CONFIG_SECTION.lines() {
        out.push_str(&format!("{}\n", zap_refs(line)));
    }
    out.push('\n');
    out.push_str(&format!(
        "{} {} a tsconfig.json (or `typescript: 5.9.2` in .zaprc) makes zap\n",
        ts_badge(),
        util::bold("typescript is built in:")
    ));
    out.push_str("manage the compiler + @types/* for deps that don't ship types. `zap version <ver>`\n");
    out.push_str("pins the compiler version; version changes install automatically.\n");
    out
}

/// one group's usage section exactly as it appears in the full help: group
/// header, command rows, and the implicit-run shortcut under the run section
fn group_section(group: &str) -> String {
    let entries: Vec<&cmds::Command> = cmds::COMMANDS.iter().filter(|c| c.group == group).collect();
    // rows align to the global name width across every group, matching the
    // full listing column-for-column
    let width = cmds::COMMANDS.iter().map(|c| help_names(c).len()).max().unwrap_or(0);
    let mut out = format!("{}\n", util::bold(format!("{}:", group)));
    for c in entries {
        let names = help_names(c);
        let pad = " ".repeat(width - names.len());
        out.push_str(&format!("{}\n", command_style(&format!("  zap {}{}  {}", names, pad, c.usage))));
    }
    if group == "run" {
        // the implicit-run shortcut stays visible under the run section
        out.push_str(&format!(
            "{}\n",
            command_style("  zap <script> [args]        shortcut for `zap run`")
        ));
    }
    out
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
