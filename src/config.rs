use anyhow::Result;
use std::path::{Path, PathBuf};

/// zap configuration, loaded from the first .zaprc* file found walking up from the
/// working directory. every file shape is a flat key/value map under the hood.
#[derive(Clone)]
pub struct Config {
    pub linker: Linker,
    pub registry: Option<String>,
    pub concurrency: usize,
    pub strict: bool,
    pub save_exact: bool,
    pub prefer_offline: bool,
    pub cache_ttl_hours: u64,
    pub lifecycle: bool,
    pub store_dir: Option<PathBuf>,
    /// pinned typescript version ("5.9", "5.9.2", ...), auto-installed when ts is detected
    pub typescript: Option<String>,
    /// also lay down node_modules junctions for editors/tsc when using pnp (default true)
    pub editor_node_modules: bool,
    /// registry auth token for publish/whoami and private registries
    pub token: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Linker {
    Pnp,
    NodeModules,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            linker: Linker::Pnp,
            registry: None,
            concurrency: 16,
            strict: false,
            save_exact: false,
            prefer_offline: false,
            cache_ttl_hours: 6,
            lifecycle: false,
            store_dir: None,
            typescript: None,
            editor_node_modules: true,
            token: None,
        }
    }
}

/// candidate rc filenames, dotted before undotted, most specific extension first
pub const RC_FILES: [&str; 24] = [
    ".zaprc.yaml",
    ".zap.yaml",
    ".zaprc.yml",
    ".zap.yml",
    ".zaprc.json",
    ".zap.json",
    ".zaprc.jsonc",
    ".zap.jsonc",
    ".zaprc.toml",
    ".zap.toml",
    ".zaprc",
    ".zap",
    "zaprc.yaml",
    "zap.yaml",
    "zaprc.yml",
    "zap.yml",
    "zaprc.json",
    "zap.json",
    "zaprc.jsonc",
    "zap.jsonc",
    "zaprc.toml",
    "zap.toml",
    "zaprc",
    "zap",
];

/// JSON schema for zaprc files, written next to the project so editors can validate/autocomplete
pub const ZAPRC_SCHEMA: &str = r##"{
  "$schema": "http://json-schema.org/draft-07/schema#",
  "title": "zap configuration",
  "type": "object",
  "properties": {
    "$schema": { "type": "string", "description": "path to this schema file" },
    "linker": { "enum": ["pnp", "node-modules", "node_modules", "nm"], "description": "how packages are exposed to node: pnp runtime or pnpm-style node_modules junctions" },
    "registry": { "type": "string", "description": "registry base url override" },
    "typescript": { "type": "string", "description": "pin the zap-managed typescript version, e.g. 5.9.3, auto-installed on change" },
    "concurrency": { "type": "integer", "minimum": 1, "description": "parallel download limit" },
    "strict": { "type": "boolean", "description": "pnp: disable the fallback pool (fully strict resolution)" },
    "save-exact": { "type": "boolean", "description": "zap add writes exact versions instead of ^" },
    "prefer-offline": { "type": "boolean", "description": "always use cached registry metadata when present" },
    "cache-ttl": { "type": "integer", "description": "metadata cache lifetime in hours" },
    "lifecycle": { "type": "boolean", "description": "opt in to preinstall/install/postinstall scripts" },
    "store-dir": { "type": "string", "description": "custom global store location" },
    "token": { "type": "string", "description": "registry auth token (prefer env vars in CI)" },
    "editor-node-modules": { "type": "boolean", "description": "also create node_modules junctions for editors/tsc when using pnp" }
  },
  "additionalProperties": false
}"##;

/// when a .zaprc.json/.jsonc is detected: write the schema into .zap/ and wire it up
/// plain json gets `$schema` injected; jsonc only gets a hint (rewriting would kill comments)
fn ensure_schema(path: &Path) {
    let Some(dir) = path.parent() else { return };
    let schema_path = dir.join(".zap").join("zaprc.schema.json");
    if let Some(p) = schema_path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let _ = std::fs::write(&schema_path, ZAPRC_SCHEMA);
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let Ok(text) = std::fs::read_to_string(path) else { return };
    if text.contains("\"$schema\"") {
        return;
    }
    if name.ends_with(".jsonc") {
        eprintln!(
            "{} add \"\\\"$schema\\\": \\\"./.zap/zaprc.schema.json\\\"\" to {} for editor autocomplete",
            crate::util::dim("hint:"),
            path.display()
        );
        return;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    let mut new = serde_json::Map::new();
    new.insert(
        "$schema".to_string(),
        serde_json::json!("./.zap/zaprc.schema.json"),
    );
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            new.insert(k.clone(), val.clone());
        }
    }
    let out = serde_json::Value::Object(new);
    if let Ok(pretty) = serde_json::to_string_pretty(&out) {
        if std::fs::write(path, format!("{}\n", pretty)).is_ok() {
            eprintln!(
                "{} added $schema to {} (editor autocomplete enabled)",
                crate::util::dim("hint:"),
                path.display()
            );
        }
    }
}

impl Config {
    /// walk up from `start` looking for an rc file,, env vars override file values
    pub fn load(start: &Path) -> Config {
        let mut cfg = Config::default();
        let mut dir = Some(start.to_path_buf());
        while let Some(d) = dir.take() {
            let mut found = false;
            for name in RC_FILES {
                let p = d.join(name);
                if p.is_file() {
                    match read_pairs(&p) {
                        Ok(pairs) => {
                            apply_pairs(&mut cfg, &pairs, &p);
                            found = true;
                            let fname = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                            if fname.ends_with(".json") || fname.ends_with(".jsonc") {
                                ensure_schema(&p);
                            }
                            break;
                        }
                        Err(e) => {
                            eprintln!("{} could not parse {}: {}", crate::util::yellow("warning:"), p.display(), e);
                        }
                    }
                }
            }
            if !found {
                dir = d.parent().map(Path::to_path_buf);
            }
        }
        apply_env(&mut cfg);
        cfg
    }
}

fn apply_pairs(cfg: &mut Config, pairs: &[(String, String)], file: &Path) {
    for (k, v) in pairs {
        if k == "$schema" {
            continue; // injected for editors, not a config key
        }
        let ok = match k.as_str() {
            "linker" | "nodelinker" | "node_linker" => {
                cfg.linker = match v.as_str() {
                    "node-modules" | "node_modules" | "nm" | "hoisted" => Linker::NodeModules,
                    "pnp" | "" => Linker::Pnp,
                    other => {
                        unknown(file, k, other);
                        cfg.linker
                    }
                };
                true
            }
            "registry" => {
                cfg.registry = Some(v.clone());
                true
            }
            "concurrency" | "jobs" => match v.parse::<usize>() {
                Ok(n) if n > 0 => {
                    cfg.concurrency = n;
                    true
                }
                _ => {
                    unknown(file, k, &v);
                    true
                }
            },
            "strict" | "pnpstrict" | "pnp_strict" => {
                cfg.strict = is_truthy(&v);
                true
            }
            "saveexact" | "save_exact" | "save-exact" => {
                cfg.save_exact = is_truthy(&v);
                true
            }
            "preferoffline" | "prefer_offline" | "prefer-offline" => {
                cfg.prefer_offline = is_truthy(&v);
                true
            }
            "cachettl" | "cache_ttl" | "cache-ttl" | "cachettlhours" | "cache_ttl_hours" => {
                match v.parse::<u64>() {
                    Ok(n) => {
                        cfg.cache_ttl_hours = n;
                        true
                    }
                    _ => {
                        unknown(file, k, &v);
                        true
                    }
                }
            }
            "lifecycle" | "lifecycles" | "enablescripts" | "enable_scripts" => {
                cfg.lifecycle = is_truthy(&v);
                true
            }
            "editor_node_modules" | "editornodemodules" | "editor-node-modules" => {
                cfg.editor_node_modules = is_truthy(&v);
                true
            }
            "typescript" | "ts" | "tsversion" | "ts_version" | "typescriptversion" => {
                cfg.typescript = Some(v.clone());
                true
            }
            "token" | "accesstoken" | "auth_token" | "authtoken" => {
                cfg.token = Some(v.clone());
                true
            }
            "storedir" | "store_dir" | "store-dir" => {
                cfg.store_dir = Some(PathBuf::from(v.clone()));
                true
            }
            _ => {
                unknown(file, k, &v);
                true
            }
        };
        let _ = ok;
    }
}

fn unknown(file: &Path, k: &str, v: &str) {
    eprintln!(
        "{} unknown config key `{}` (value `{}`) in {}",
        crate::util::yellow("warning:"),
        k,
        v,
        file.display()
    );
}

fn is_truthy(v: &str) -> bool {
    matches!(v, "true" | "1" | "yes" | "on")
}

fn apply_env(cfg: &mut Config) {
    if let Ok(l) = std::env::var("ZAP_LINKER") {
        if l == "node-modules" || l == "node_modules" || l == "nm" {
            cfg.linker = Linker::NodeModules;
        } else if l == "pnp" {
            cfg.linker = Linker::Pnp;
        }
    }
    if let Ok(n) = std::env::var("ZAP_CONCURRENCY") {
        if let Ok(n) = n.parse::<usize>() {
            if n > 0 {
                cfg.concurrency = n;
            }
        }
    }
    if let Ok(s) = std::env::var("ZAP_STRICT") {
        cfg.strict = s == "1" || s.eq_ignore_ascii_case("true");
    }
}

/// read one rc file into flat (key, value) pairs regardless of format
fn read_pairs(path: &Path) -> Result<Vec<(String, String)>> {
    let text = std::fs::read_to_string(path)?;
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if name.ends_with(".json") || name == ".zaprc" && looks_like_json(&text) {
        return json_pairs(&text);
    }
    if name.ends_with(".jsonc") {
        return json_pairs(&strip_jsonc(&text));
    }
    if name.ends_with(".toml") {
        return Ok(flat_pairs(&text, '='));
    }
    // .yaml, .yml, extensionless: flat yaml subset (also tolerates key = value)
    Ok(flat_pairs(&text, ':'))
}

fn looks_like_json(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('{') || t.starts_with('[')
}

fn json_pairs(text: &str) -> Result<Vec<(String, String)>> {
    let v: serde_json::Value = serde_json::from_str(text)?;
    let mut out = Vec::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            out.push((k.to_lowercase(), value_to_string(val)));
        }
    }
    Ok(out)
}

fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => v.to_string(),
    }
}

/// strip // and /* */ comments (not inside string literals)
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == '/' {
            while i < bytes.len() && bytes[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < bytes.len() && bytes[i + 1] == '*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == '*' && bytes[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// flat yaml/toml subset: "key: value" or "key = value", # comments, quoted strings
/// covers everything zap needs without a yaml/toml dependency
fn flat_pairs(text: &str, primary: char) -> Vec<(String, String)> {    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let sep = line
            .find(primary)
            .map(|i| (i, primary))
            .into_iter()
            .chain(line.find('=').map(|i| (i, '=')))
            .min_by_key(|(i, _)| *i);
        let Some((idx, _)) = sep else { continue };
        let key = line[..idx].trim().to_lowercase().replace(['-', '_'], "");
        if key.is_empty() {
            continue;
        }
        let mut val = line[idx + 1..].trim().to_string();
        // strip trailing "# comment" when value is unquoted
        if !val.starts_with('"') && !val.starts_with('\'') {
            if let Some(h) = val.find(" #") {
                val = val[..h].trim().to_string();
            }
        }
        if (val.starts_with('"') && val.ends_with('"') && val.len() >= 2)
            || (val.starts_with('\'') && val.ends_with('\'') && val.len() >= 2)
        {
            val = val[1..val.len() - 1].to_string();
        }
        if val.is_empty() {
            continue;
        }
        out.push((key, val));
    }
    out
}
