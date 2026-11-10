use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use flate2::write::GzEncoder;
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::Config;
use crate::util::{dim, green, yellow};

pub struct Packed {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub files: Vec<String>,
    pub integrity: String,
    pub shasum: String,
    pub name: String,
    pub version: String,
}

const ALWAYS_EXCLUDE: [&str; 9] = [
    "node_modules", ".zap", ".git", ".pnp.cjs", ".pnp.json", ".pnp.mjs", ".pnp-resolve.mjs", ".zap-lock.json", "zap.probe",
];

/// npm-pack a project directory into a tarball following the `files` field /
/// .npmignore conventions (subset). returns bytes + integrity for publish.
pub fn npm_pack(dir: &Path) -> Result<Packed> {
    let v = crate::util::read_json(&dir.join("package.json"))?;
    let name = v
        .get("name")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("package.json has no name, nothing to pack"))?
        .to_string();
    let version = v
        .get("version")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow!("package.json has no version, nothing to pack"))?
        .to_string();

    let files_field: Vec<String> = v
        .get("files")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|f| f.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    let npmignore: Vec<String> = {
        // .zapignore is zap's own ignore file; .npmignore kept for npm parity.
        // both only apply when package.json has no `files` field (npm rule: files wins)
        let zap = dir.join(".zapignore");
        let path = if zap.is_file() { zap } else { dir.join(".npmignore") };
        std::fs::read_to_string(path)
            .map(|t| {
                t.lines()
                    .filter(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))
                    .map(|l| l.trim().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };

    let mut files: Vec<PathBuf> = Vec::new();

    // always shipped
    for always in ["package.json", "README.md", "README", "LICENSE", "LICENCE", "CHANGELOG.md"] {
        let p = dir.join(always);
        if p.is_file() {
            files.push(p);
        }
    }
    if !files_field.is_empty() {
        for pat in &files_field {
            glob_files(dir, pat, &mut files);
        }
    } else {
        walk_all(dir, dir, &mut files, &npmignore);
    }

    let mut rels: Vec<String> = files
        .iter()
        .filter_map(|p| p.strip_prefix(dir).ok())
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .collect();
    rels.sort();
    rels.dedup();

    // build package/<rel> tar.gz in memory
    let enc = GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);
    for rel in &rels {
        let abs = dir.join(rel.replace('/', "\\"));
        tar.append_path_with_name(&abs, format!("package/{}", rel))
            .with_context(|| format!("packing {}", rel))?;
    }
    let bytes = tar.into_inner()?.finish()?;

    let digest = sha2::Sha512::digest(&bytes);
    let integrity = format!("sha512-{}", base64::engine::general_purpose::STANDARD.encode(digest.as_slice()));
    let sha1_digest = Sha1::digest(&bytes);
    let shasum = format!("sha1-{}", base64::engine::general_purpose::STANDARD.encode(sha1_digest.as_slice()));
    let slug = name.trim_start_matches('@').replace('/', "-");
    let path = dir.join(format!("{}-{}.tgz", slug, version));
    std::fs::write(&path, &bytes)?;

    Ok(Packed {
        path,
        bytes,
        files: rels,
        integrity,
        shasum,
        name,
        version,
    })
}

/// glob-walk a files-field pattern ("dist", "src/**", "*.js", "lib/*")
fn glob_files(base: &Path, pattern: &str, out: &mut Vec<PathBuf>) {
    let normalized = pattern.trim().replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return;
    }
    walk_pattern(base, &parts, out);
}

fn walk_pattern(dir: &Path, parts: &[&str], out: &mut Vec<PathBuf>) {
    if parts.is_empty() {
        // terminal match: file -> push, dir -> everything under it
        if dir.is_file() {
            out.push(dir.to_path_buf());
        } else if dir.is_dir() {
            walk_all(dir, dir, out, &[]);
        }
        return;
    }
    let part = parts[0];
    let rest = &parts[1..];
    if part == "**" {
        walk_pattern(dir, rest, out);
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    walk_pattern(&e.path(), parts, out);
                }
            }
        }
        return;
    }
    if part == "*" {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                walk_pattern(&e.path(), rest, out);
            }
        }
        return;
    }
    let sub = dir.join(part);
    if sub.exists() {
        walk_pattern(&sub, rest, out);
    }
}

fn walk_all(root: &Path, dir: &Path, out: &mut Vec<PathBuf>, ignore: &[String]) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if ALWAYS_EXCLUDE.iter().any(|x| name == *x) || name.ends_with(".tgz") {
                continue;
            }
            // dotfiles are excluded unless explicitly listed (npm behavior)
            if name.starts_with('.') {
                continue;
            }
            let rel = e
                .path()
                .strip_prefix(root)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| e.path().to_path_buf())
                .to_string_lossy()
                .replace('\\', "/");
            if is_ignored(&rel, ignore) {
                continue;
            }
            if e.path().is_dir() {
                walk_all(root, &e.path(), out, ignore);
            } else {
                out.push(e.path());
            }
        }
    }
}

fn is_ignored(rel: &str, rules: &[String]) -> bool {
    rules.iter().any(|r| {
        let r = r.trim_end_matches('/');
        rel == r || rel.starts_with(&format!("{}/", r)) || (r.ends_with('*') && rel.starts_with(r.trim_end_matches('*')))
    })
}

// commands

pub fn pack_cmd(_args: &[String]) -> Result<i32> {
    let dir = crate::util::find_root()?;
    let p = npm_pack(&dir)?;
    println!(
        "{} {} ({}), {} files",
        green("packed"),
        p.path.display(),
        crate::util::human(p.bytes.len() as u64),
        p.files.len()
    );
    Ok(0)
}

pub fn publish_cmd(args: &[String]) -> Result<i32> {
    let mut tag = "latest".to_string();
    let mut access: Option<String> = None;
    let mut otp: Option<String> = None;
    let mut dry_run = false;
    let mut registry_flag: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--tag" => {
                tag = args.get(i + 1).cloned().ok_or_else(|| anyhow!("--tag needs a value"))?;
                i += 1;
            }
            "--access" => {
                access = Some(args.get(i + 1).cloned().ok_or_else(|| anyhow!("--access needs a value"))?);
                i += 1;
            }
            "--otp" => {
                otp = Some(args.get(i + 1).cloned().ok_or_else(|| anyhow!("--otp needs a value"))?);
                i += 1;
            }
            "--registry" => {
                registry_flag = Some(args.get(i + 1).cloned().ok_or_else(|| anyhow!("--registry needs a value"))?);
                i += 1;
            }
            "--dry-run" => dry_run = true,
            other => bail!("unknown publish flag `{}` (supported: --tag, --access, --otp, --registry, --dry-run)", other),
        }
        i += 1;
    }

    let dir = crate::util::find_root()?;
    let manifest = crate::util::read_json(&dir.join("package.json"))?;
    if manifest.get("private").and_then(|x| x.as_bool()).unwrap_or(false) {
        bail!("package.json has `private: true`, remove it (or override) before publishing");
    }
    let cfg = Config::load(&dir);
    let base = match &registry_flag {
        Some(r) => {
            let (_, named) = load_registries();
            named.get(r).cloned().unwrap_or_else(|| r.clone())
                .trim_end_matches('/')
                .to_string()
        }
        None => registry_base(&cfg)?,
    };
    let token = resolve_token(&cfg, &base);
    if !dry_run && token.is_none() {
        bail!("no auth token, run `zap login`, set ZAP_TOKEN / NPM_TOKEN, or `token: <token>` in .zaprc");
    }
    let aliases: Vec<String> = manifest
        .get("nameAliases")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();

    let p = npm_pack(&dir)?;
    println!(
        "{} {}@{} ({}), {} files",
        green("packed"),
        p.name,
        p.version,
        crate::util::human(p.bytes.len() as u64),
        p.files.len()
    );
    if dry_run {
        for f in &p.files {
            println!("  {}", f);
        }
        if !aliases.is_empty() {
            println!("  would also publish under nameAliases: {}", aliases.join(", "));
        }
        println!("{}", yellow("dry run, nothing published, tarball kept on disk"));
        return Ok(0);
    }

    // classic npm publish protocol: PUT /{name} with versions + _attachments
    let tarball_name = p.path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let build_doc = |pkg_name: &str| -> serde_json::Value {
        let slug = pkg_name.trim_start_matches('@').replace('/', "-");
        let tarball_url = format!("{}/{}/-/{}-{}.tgz", base, pkg_name, slug, p.version);
        let mut version_meta = manifest.clone();
        if let Some(obj) = version_meta.as_object_mut() {
            obj.remove("private");
            obj.insert("name".into(), serde_json::json!(pkg_name));
            obj.insert("_id".into(), serde_json::json!(format!("{}@{}", pkg_name, p.version)));
            obj.insert(
                "dist".into(),
                serde_json::json!({
                    "integrity": p.integrity,
                    "shasum": p.shasum,
                    "tarball": tarball_url,
                }),
            );
        }
        let readme = std::fs::read_to_string(dir.join("README.md")).unwrap_or_default();
        let mut root_obj = serde_json::Map::new();
        root_obj.insert("_id".into(), serde_json::json!(pkg_name));
        root_obj.insert("name".into(), serde_json::json!(pkg_name));
        if let Some(d) = manifest.get("description") {
            root_obj.insert("description".into(), d.clone());
        }
        root_obj.insert(
            "dist-tags".into(),
            serde_json::json!({ tag.clone(): p.version }),
        );
        root_obj.insert("versions".into(), serde_json::json!({ p.version.clone(): version_meta }));
        root_obj.insert(
            "_attachments".into(),
            serde_json::json!({
                tarball_name.clone(): {
                    "content_type": "application/octet-stream",
                    "data": base64::engine::general_purpose::STANDARD.encode(&p.bytes),
                    "length": p.bytes.len(),
                }
            }),
        );
        if let Some(a) = &access {
            root_obj.insert("access".into(), serde_json::json!(a));
        } else if !pkg_name.starts_with('@') {
            root_obj.insert("access".into(), serde_json::json!("public"));
        }
        if !readme.is_empty() {
            root_obj.insert("readme".into(), serde_json::json!(readme));
        }
        serde_json::Value::Object(root_obj)
    };

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let mut otp_state = otp.clone();
    let token_str = token.as_deref().unwrap_or("");
    publish_doc(&client, &base, &p.name, &build_doc(&p.name), token_str, &mut otp_state)?;
    println!("{} {}@{} published to {}", green("+"), p.name, p.version, base);

    // nameAliases: republish the same tarball under each alias
    let mut alias_failures = 0usize;
    for alias in &aliases {
        match publish_doc(&client, &base, alias, &build_doc(alias), token_str, &mut otp_state) {
            Ok(()) => println!("{} alias {}@{} published", green("+"), alias, p.version),
            Err(e) => {
                eprintln!("{} alias {} failed: {}", yellow("warning:"), alias, e);
                alias_failures += 1;
            }
        }
    }
    if alias_failures > 0 {
        println!("{}", yellow(format!("{} nameAlias publish(es) failed", alias_failures)));
        return Ok(1);
    }
    Ok(0)
}

/// one PUT with interactive OTP retry (up to 3 prompts)
fn publish_doc(
    client: &reqwest::blocking::Client,
    base: &str,
    pkg_name: &str,
    doc: &serde_json::Value,
    token: &str,
    otp: &mut Option<String>,
) -> Result<()> {
    let url = format!("{}/{}", base, pkg_name.replace('/', "%2F"));
    let attempt = |code: &Option<String>| -> Result<(reqwest::StatusCode, String)> {
        let mut req = client
            .put(&url)
            .header("authorization", format!("Bearer {}", token))
            .header("content-type", "application/json")
            .header("accept", "application/json");
        if let Some(code) = code {
            req = req.header("npm-otp", code);
        }
        let resp = req.body(doc.to_string()).send()?;
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        Ok((status, text))
    };
    let (mut status, mut text) = attempt(otp)?;
    let mut tries = 0;
    while status.as_u16() == 403 && text.to_lowercase().contains("otp") {
        if otp.is_some() || tries >= 3 {
            bail!("otp rejected ({}): {}", status, text);
        }
        tries += 1;
        print!("one-time password (from your authenticator): ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            bail!("otp required, non-interactive session, retry with `--otp <code>`");
        }
        let code = line.trim().to_string();
        if code.is_empty() {
            bail!("otp required, retry with `--otp <code>`");
        }
        *otp = Some(code);
        (status, text) = attempt(otp)?;
    }
    if status.is_success() {
        return Ok(());
    }
    bail!("publish failed ({}): {}", status, text);
}

pub fn whoami_cmd(_args: &[String]) -> Result<i32> {
    let dir = std::env::current_dir()?;
    let cfg = Config::load(&dir);
    let base = registry_base(&cfg)?;
    let token = resolve_token(&cfg, &base)
        .ok_or_else(|| anyhow!("no auth token, run `zap login`, set ZAP_TOKEN / NPM_TOKEN, or `token: <token>` in .zaprc"))?;
    let username = whoami(&base, &token)?;
    println!("{}", username);
    Ok(0)
}

// auth

fn registry_base(cfg: &Config) -> Result<String> {
    // precedence: project .zaprc / env > saved "current" registry > npmjs default
    if let Some(u) = cfg
        .registry
        .clone()
        .or_else(|| std::env::var("ZAP_REGISTRY").ok())
    {
        return Ok(u.trim_end_matches('/').to_string());
    }
    let (current, named) = load_registries();
    if let Some(c) = current {
        let url = named.get(&c).cloned().unwrap_or(c); // name or raw url
        return Ok(url.trim_end_matches('/').to_string());
    }
    Ok("https://registry.npmjs.org".to_string())
}

// registries

fn registries_path() -> Result<PathBuf> {
    Ok(crate::util::zap_home()?.join("registries.json"))
}

/// (current name-or-url, named registry map)
fn load_registries() -> (Option<String>, BTreeMap<String, String>) {
    let v: serde_json::Value = std::fs::read_to_string(registries_path().unwrap_or_default())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let current = v.get("current").and_then(|x| x.as_str()).map(|s| s.to_string());
    let named = v
        .get("named")
        .and_then(|x| x.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    (current, named)
}

fn save_registries(current: Option<String>, named: &BTreeMap<String, String>) -> Result<()> {
    let path = registries_path()?;
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let mut obj = serde_json::Map::new();
    if let Some(c) = current {
        obj.insert("current".into(), serde_json::json!(c));
    }
    obj.insert(
        "named".into(),
        serde_json::Value::Object(
            named
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::json!(v)))
                .collect(),
        ),
    );
    std::fs::write(&path, serde_json::to_vec_pretty(&serde_json::Value::Object(obj))?)?;
    Ok(())
}

/// `zap registry add|list|use|remove|current`, switch between registries so you can
/// publish to a private registry first and flip to npmjs when ready
pub fn registry_cmd(args: &[String]) -> Result<i32> {
    let (mut current, mut named) = load_registries();
    let sub = args.first().map(|s| s.as_str()).unwrap_or("current");
    match sub {
        "add" => {
            let name = args.get(1).ok_or_else(|| anyhow!("usage: zap registry add <name> <url>"))?;
            let url = args.get(2).ok_or_else(|| anyhow!("usage: zap registry add <name> <url>"))?;
            named.insert(name.clone(), url.trim_end_matches('/').to_string());
            save_registries(current.clone(), &named)?;
            println!("{} registry {} -> {}", green("+"), name, url);
        }
        "list" | "ls" => {
            for (name, url) in &named {
                let marker = if current.as_deref() == Some(name.as_str()) { " (current)" } else { "" };
                println!("  {}{} -> {}", name, dim(marker), url);
            }
            if let Some(c) = &current {
                if !named.contains_key(c) {
                    println!("  {} {}", dim("(unnamed)"), c);
                }
            }
        }
        "use" => {
            let target = args
                .get(1)
                .ok_or_else(|| anyhow!("usage: zap registry use <name|url>"))?
                .clone();
            let resolved = named.get(&target).cloned().unwrap_or_else(|| target.clone());
            if !resolved.starts_with("http://") && !resolved.starts_with("https://") {
                bail!("`{}` is not a known registry name or an http(s) url", target);
            }
            save_registries(Some(target.clone()), &named)?;
            println!("{} registry switched to {}", green("→"), resolved);
        }
        "remove" | "rm" => {
            let name = args.get(1).ok_or_else(|| anyhow!("usage: zap registry remove <name>"))?;
            if named.remove(name).is_none() {
                bail!("no registry named `{}`", name);
            }
            if current.as_deref() == Some(name.as_str()) {
                current = None;
            }
            save_registries(current, &named)?;
            println!("{} removed {}", green("-"), name);
        }
        "current" | "" => {
            let base = registry_base(&Config::load(&std::env::current_dir()?))?;
            println!("{}", base);
        }
        other => bail!("usage: zap registry [add|list|use|remove|current] (unknown `{}`)", other),
    }
    Ok(0)
}

/// token precedence: env > project .zaprc > global ~/.zap/credentials.json
fn resolve_token(cfg: &Config, base: &str) -> Option<String> {
    std::env::var("ZAP_TOKEN")
        .ok()
        .or_else(|| std::env::var("NPM_TOKEN").ok())
        .or_else(|| cfg.token.clone())
        .or_else(|| creds().get(base).and_then(|v| v.as_str()).map(|s| s.to_string()))
}

fn credentials_path() -> Result<PathBuf> {
    Ok(crate::util::zap_home()?.join("credentials.json"))
}

fn creds() -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(credentials_path().unwrap_or_default())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .and_then(|v: serde_json::Value| v.as_object().cloned())
        .unwrap_or_default()
}

fn save_token(base: &str, token: &str) -> Result<()> {
    let path = credentials_path()?;
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let mut map = creds();
    map.insert(base.to_string(), serde_json::json!(token));
    std::fs::write(&path, serde_json::to_vec_pretty(&serde_json::Value::Object(map))?)?;
    Ok(())
}

fn drop_token(base: &str) -> Result<bool> {
    let path = credentials_path()?;
    let mut map = creds();
    let removed = map.remove(base).is_some();
    if removed {
        std::fs::write(&path, serde_json::to_vec_pretty(&serde_json::Value::Object(map))?)?;
    }
    Ok(removed)
}

fn whoami(base: &str, token: &str) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let resp = client
        .get(format!("{}/-/whoami", base))
        .header("authorization", format!("Bearer {}", token))
        .header("accept", "application/json")
        .send()?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        bail!("whoami failed ({}): {}", status, text);
    }
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::json!({}));
    Ok(v.get("username")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("name").and_then(|x| x.as_str()))
        .unwrap_or("unknown")
        .to_string())
}

/// `zap login`, npm web-auth flow: POST /-/v1/login, open loginUrl in the browser,
/// poll doneUrl until the token lands. `--token <t>` skips the browser entirely.
pub fn login_cmd(args: &[String]) -> Result<i32> {
    let dir = std::env::current_dir()?;
    let cfg = Config::load(&dir);
    let base = registry_base(&cfg)?;

    if let Some(i) = args.iter().position(|a| a == "--token") {
        let token = args
            .get(i + 1)
            .ok_or_else(|| anyhow!("--token needs a value"))?
            .clone();
        let user = whoami(&base, &token)
            .map_err(|e| anyhow!("token rejected by {}: {}", base, e))?;
        save_token(&base, &token)?;
        println!("{} logged in as {} ({}), token stored in {}", green("+"), user, base, credentials_path()?.display());
        return Ok(0);
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let resp = client
        .post(format!("{}/-/v1/login", base))
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .header("npm-auth-type", "web")
        .header("user-agent", "zap/0.4.0 (node package manager)")
        .body("{}")
        .send()?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND || resp.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
        bail!(
            "{} does not support web login, use `zap login --token <token>` or `token: <token>` in .zaprc",
            base
        );
    }
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        bail!("login failed ({}): {}", status, text);
    }
    let v: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("unexpected login response from {}", base))?;
    let login_url = v.get("loginUrl").and_then(|x| x.as_str()).unwrap_or("");
    let done_url = v.get("doneUrl").and_then(|x| x.as_str()).unwrap_or("");
    if login_url.is_empty() || done_url.is_empty() {
        bail!("registry {} did not return a web login flow, use `zap login --token <token>`", base);
    }

    println!("authenticate at:");
    println!("  {}", login_url);
    // best effort: open the default browser
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", login_url])
        .spawn();
    println!("{}", dim("waiting for browser login (up to 3 minutes)..."));

    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    let mut token: Option<String> = None;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(2));
        let poll = client
            .get(done_url)
            .header("accept", "application/json")
            .send()?;
        match poll.status().as_u16() {
            200 => {
                let body: serde_json::Value =
                    serde_json::from_str(&poll.text().unwrap_or_default()).unwrap_or_default();
                token = body.get("token").and_then(|x| x.as_str()).map(|s| s.to_string());
                break;
            }
            202 => continue, // Retry-After honored loosely; 2s cadence
            other => bail!("login poll failed ({}), try again or use --token", other),
        }
    }
    let Some(token) = token else {
        bail!("timed out waiting for browser login, run `zap login` again or use --token");
    };
    let user = whoami(&base, &token).unwrap_or_else(|_| "unknown".to_string());
    save_token(&base, &token)?;
    println!("{} logged in as {} ({}), token stored in {}", green("+"), user, base, credentials_path()?.display());
    Ok(0)
}

pub fn logout_cmd(_args: &[String]) -> Result<i32> {
    let dir = std::env::current_dir()?;
    let cfg = Config::load(&dir);
    let base = registry_base(&cfg)?;
    let removed = drop_token(&base)?;
    if removed {
        println!("{} removed stored credentials for {}", green("-"), base);
    } else {
        println!("{}", dim(format!("no stored credentials for {}", base)));
    }
    Ok(0)
}
