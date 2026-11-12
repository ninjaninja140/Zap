// the api spec table: every registry command's real flags, positional shape
// and structured return type, as the single source of truth for the generated
// ts api (build.rs includes this file, keep it free of crate dependencies).
// the descs feed the generated jsdoc; the flag strings must match the arg
// parsers in src/cmds (the registry-wide test + the argv unit tests enforce it)

// each include point (build script + crate) uses a different subset of the table
#![allow(dead_code)]

pub struct ApiSpec {
    pub name: &'static str,             // matches the registry command
    pub options: &'static [ApiOpt],     // flags the command's parser accepts
    pub positionals: &'static [ApiPos], // argv order, before any flags
    pub returns: &'static str,          // "RunResult" | "LsJson" | "WhyJson" | "ViewJson" | "OutdatedJson"
}

pub struct ApiOpt {
    pub flag: &'static str,             // the long form the builder emits
    pub short: Option<&'static str>,    // human alias, documented not emitted
    pub kind: OptKind,
    pub desc: &'static str,
}

pub struct ApiPos {
    pub name: &'static str,   // the ts param name
    pub kind: PosKind,
    pub desc: &'static str,
    pub optional: bool,
    pub via_sep: bool,        // emitted after a bare `--` (run's shared args)
}

#[derive(Clone, Copy, PartialEq)]
pub enum OptKind {
    Bool,
    Str,
    StrList,
}

#[derive(Clone, Copy, PartialEq)]
pub enum PosKind {
    Str,
    StrList,
}

const RUNRESULT: &str = "RunResult";

macro_rules! pos {
    ($name:literal, $kind:ident, $optional:literal, $via_sep:literal, $desc:literal) => {
        ApiPos { name: $name, kind: PosKind::$kind, desc: $desc, optional: $optional, via_sep: $via_sep }
    };
}

macro_rules! flag {
    ($flag:literal, $short:expr, $kind:ident, $desc:literal) => {
        ApiOpt { flag: $flag, short: $short, kind: OptKind::$kind, desc: $desc }
    };
}

pub static SPECS: &[ApiSpec] = &[
    // install group
    ApiSpec {
        name: "add",
        options: &[
            flag!("--dev", Some("-D"), Bool, "record into devDependencies instead of dependencies"),
            flag!("--global", Some("-g"), Bool, "install into ~/.zap/global with bins shimmed to ~/.zap/bin, no project needed"),
            flag!("--json", None, Bool, "suppress the human output, print the install report document instead"),
        ],
        positionals: &[pos!("specs", StrList, false, false, "package specs: name, name@range or name@latest; workspace members link as workspace:*")],
        returns: "InstallJson",
    },
    ApiSpec {
        name: "ci",
        options: &[flag!("--json", None, Bool, "suppress the human output, print the install report document instead")],
        positionals: &[],
        returns: "InstallJson",
    },
    ApiSpec { name: "init", options: &[], positionals: &[], returns: RUNRESULT },
    ApiSpec {
        name: "install",
        options: &[
            flag!("--force", Some("-f"), Bool, "re-resolve and relink even when the lockfile looks fresh"),
            flag!("--offline", None, Bool, "resolve from the metadata cache only, no registry requests"),
            flag!("--refresh", None, Bool, "bypass the metadata cache and refetch packuments"),
            flag!("--frozen-lockfile", None, Bool, "fail instead of updating .zap-lock.json when manifests changed (CI)"),
            flag!("--production", Some("-P"), Bool, "production deps only, devDependencies skipped"),
            flag!("--json", None, Bool, "suppress the human output, print the install report document instead"),
        ],
        positionals: &[],
        returns: "InstallJson",
    },
    ApiSpec {
        name: "outdated",
        options: &[
            flag!("--json", None, Bool, "print {outdated, upToDate} as a json document, keeping the exit code"),
            flag!("--interactive", Some("-i"), Bool, "open the raw-mode three-target picker (keep / latest-of-major / latest)"),
        ],
        positionals: &[],
        returns: "OutdatedJson",
    },
    ApiSpec {
        name: "remove",
        options: &[
            flag!("--global", Some("-g"), Bool, "remove global installs: the package dir and its shims"),
            flag!("--json", None, Bool, "suppress the human output, print the install report document instead"),
        ],
        positionals: &[pos!("pkgs", StrList, false, false, "package names to drop from the manifest (dev deps from their own section)")],
        returns: "InstallJson",
    },
    ApiSpec {
        name: "upgrade",
        options: &[flag!("--interactive", Some("-i"), Bool, "pick exact versions per named package, or the three-target flow for the whole tree")],
        positionals: &[pos!("pkgs", StrList, true, false, "packages to bump to their absolute latest; empty re-resolves everything within its range")],
        returns: RUNRESULT,
    },
    // run group
    ApiSpec {
        name: "create",
        options: &[],
        positionals: &[
            pos!("template", Str, false, false, "initializer name, `zap create vite` runs create-vite"),
            pos!("args", StrList, true, false, "args passed through to the initializer"),
        ],
        returns: RUNRESULT,
    },
    ApiSpec {
        name: "dlx",
        options: &[],
        positionals: &[
            pos!("spec", Str, false, false, "package with optional range or dist-tag pin"),
            pos!("args", StrList, true, false, "args passed to the package bin"),
        ],
        returns: RUNRESULT,
    },
    ApiSpec {
        name: "exec",
        options: &[],
        positionals: &[
            pos!("command", Str, false, false, "the command to spawn directly, no shell"),
            pos!("args", StrList, true, false, "args for the command"),
        ],
        returns: RUNRESULT,
    },
    ApiSpec {
        name: "run",
        options: &[],
        positionals: &[
            pos!("scripts", StrList, false, false, "package.json script names; several run concurrently in panes"),
            pos!("shared", StrList, true, true, "args after `--`, appended to every script's command line"),
        ],
        returns: RUNRESULT,
    },
    ApiSpec {
        name: "tsc",
        options: &[],
        positionals: &[pos!("args", StrList, true, false, "args passed straight through to tsc")],
        returns: RUNRESULT,
    },
    ApiSpec {
        name: "x",
        options: &[],
        positionals: &[
            pos!("bin", Str, false, false, "bin name, optionally @range-pinned (falls back to a dlx fetch)"),
            pos!("args", StrList, true, false, "args after the bin name, passed through"),
        ],
        returns: RUNRESULT,
    },
    // info group
    ApiSpec { name: "audit", options: &[], positionals: &[], returns: RUNRESULT },
    ApiSpec {
        name: "bin",
        options: &[],
        positionals: &[pos!("name", Str, true, false, "narrow to one bin name; empty lists every name, path pair")],
        returns: RUNRESULT,
    },
    ApiSpec {
        name: "cache",
        options: &[],
        positionals: &[pos!("sub", Str, true, false, "clean (wipe every cache) or inspect (interactive browser); empty prints usage")],
        returns: RUNRESULT,
    },
    ApiSpec { name: "doctor", options: &[], positionals: &[], returns: RUNRESULT },
    ApiSpec { name: "inspect", options: &[], positionals: &[pos!("spec", Str, false, false, "package, optionally @range-pinned; bare name resolves to the latest stable")], returns: RUNRESULT },
    ApiSpec { name: "licenses", options: &[], positionals: &[], returns: RUNRESULT },
    ApiSpec {
        name: "ls",
        options: &[
            flag!("--all", Some("-a"), Bool, "expand to the full dependency tree"),
            flag!("--global", Some("-g"), Bool, "list the global installs instead of the project"),
            flag!("--json", None, Bool, "print the listing as a json document"),
        ],
        positionals: &[],
        returns: "LsJson",
    },
    ApiSpec {
        name: "store",
        options: &[],
        positionals: &[pos!("sub", Str, true, false, "path (print the store dir) or size (sum the disk usage); empty prints both")],
        returns: RUNRESULT,
    },
    ApiSpec {
        name: "view",
        options: &[flag!("--json", None, Bool, "print the whole summary as a json document instead of the human table")],
        positionals: &[
            pos!("pkg", Str, false, false, "package name"),
            pos!("field", Str, true, false, "narrow to one field of the latest version manifest (dist-tags, versions, ...)"),
        ],
        returns: "ViewJson",
    },
    ApiSpec {
        name: "why",
        options: &[
            flag!("--project", None, Bool, "additionally grep source files for imports of the package"),
            flag!("--json", None, Bool, "print the chains as arrays of {name, version} nodes"),
        ],
        positionals: &[pos!("pkg", Str, false, false, "the package to explain")],
        returns: "WhyJson",
    },
    // publish group
    ApiSpec {
        name: "login",
        options: &[flag!("--token", None, Str, "paste a token directly instead of the browser flow")],
        positionals: &[],
        returns: RUNRESULT,
    },
    ApiSpec { name: "logout", options: &[], positionals: &[], returns: RUNRESULT },
    ApiSpec { name: "pack", options: &[], positionals: &[], returns: RUNRESULT },
    ApiSpec {
        name: "publish",
        options: &[
            flag!("--tag", None, Str, "publish under this dist-tag instead of latest"),
            flag!("--access", None, Str, "access level for the published package"),
            flag!("--otp", None, Str, "one-time password for registries that require it"),
            flag!("--registry", None, Str, "publish to a named or url registry for this run"),
            flag!("--dry-run", None, Bool, "go through the whole flow without uploading"),
        ],
        positionals: &[],
        returns: RUNRESULT,
    },
    ApiSpec {
        name: "registry",
        options: &[],
        positionals: &[pos!("args", StrList, true, false, "add <name> <url> | use <name|url> | remove <name> | list | current")],
        returns: RUNRESULT,
    },
    ApiSpec {
        name: "version",
        options: &[
            flag!("--no-git-tag-version", None, Bool, "skip the changelog entry, commit and tag"),
            flag!("--message", Some("-m"), Str, "override the git commit message"),
        ],
        positionals: &[pos!("spec", Str, true, false, "major | minor | patch | an exact x.y.z; empty prints the current version")],
        returns: RUNRESULT,
    },
    ApiSpec { name: "whoami", options: &[], positionals: &[], returns: RUNRESULT },
    // setup group
    ApiSpec {
        name: "allow",
        options: &[
            flag!("--all", None, Bool, "trust every dependency's lifecycle scripts (allowlist file removed)"),
            flag!("--remove", None, Bool, "remove entries (with pkgs) or revert --all (with --all)"),
        ],
        positionals: &[pos!("pkgs", StrList, true, false, "packages to approve or remove; empty lists the allowlist")],
        returns: RUNRESULT,
    },
    ApiSpec {
        name: "config",
        options: &[flag!("--global", Some("-g"), Bool, "target the global config (~/.zap/config.yaml) instead of the project rc")],
        positionals: &[
            pos!("key", Str, true, false, "the config key; empty lists everything"),
            pos!("value", Str, true, false, "the value to set; omitted reads the key"),
        ],
        returns: RUNRESULT,
    },
    ApiSpec {
        name: "migrate",
        options: &[
            flag!("--dry-run", None, Bool, "preview what the conversion would import, write nothing"),
            flag!("--yes", Some("-y"), Bool, "skip the confirmation"),
        ],
        positionals: &[],
        returns: RUNRESULT,
    },
    ApiSpec {
        name: "set-version",
        options: &[],
        positionals: &[pos!("version", Str, true, false, "an exact x.y.z (verified against the releases list) or `latest`; empty prints the pin")],
        returns: RUNRESULT,
    },
    ApiSpec {
        name: "skills",
        options: &[flag!("--remove", None, Bool, "strip the zap usage block again")],
        positionals: &[],
        returns: RUNRESULT,
    },
];
