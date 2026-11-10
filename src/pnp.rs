use anyhow::Result;
use std::path::Path;

use crate::config::Linker;
use crate::model::Tree;

/// write the pnp runtime (.pnp.json map + hooks) for pnp mode
pub fn write_runtime(root: &Path, tree: &Tree, strict: bool) -> Result<()> {
    let registry = root.join(".zap").join("v");
    let mut packages = serde_json::Map::new();
    let mut by_dir = serde_json::Map::new();
    for (id, pkg) in &tree.packages {
        let mut deps = serde_json::Map::new();
        for (n, i) in &pkg.deps {
            deps.insert(n.clone(), serde_json::json!(i));
        }
        let dir_name = crate::util::pkg_dir_name(id);
        packages.insert(
            id.clone(),
            serde_json::json!({ "dir": dir_name, "deps": deps }),
        );
        by_dir.insert(crate::util::pkg_dir_name(id), serde_json::json!(id));
    }
    let mut root_deps = serde_json::Map::new();
    for (n, i) in &tree.root {
        root_deps.insert(n.clone(), serde_json::json!(i));
    }
    // zap-managed type deps (@types/*, typescript) resolve like regular deps
    for (n, i) in &tree.types {
        root_deps.insert(n.clone(), serde_json::json!(i));
    }
    let mut fallback = serde_json::Map::new();
    if !strict {
        for (n, i) in &tree.fallback {
            fallback.insert(n.clone(), serde_json::json!(i));
        }
    }
    let mut workspaces = Vec::new();
    let mut workspace_by_name = serde_json::Map::new();
    for m in &tree.members {
        let mut deps = serde_json::Map::new();
        for (n, i) in &m.deps {
            deps.insert(n.clone(), serde_json::json!(i));
        }
        workspaces.push(serde_json::json!({
            "name": m.name,
            "dir": m.dir,
            "deps": deps,
        }));
        workspace_by_name.insert(m.name.clone(), serde_json::json!(m.dir));
    }
    let map = serde_json::json!({
        "registry": registry.to_string_lossy(),
        "workspaceRoot": root.to_string_lossy(),
        "strict": strict,
        "byDir": by_dir,
        "packages": packages,
        "root": { "deps": root_deps },
        "workspaces": workspaces,
        "workspaceByName": workspace_by_name,
        "fallback": fallback,
    });
    std::fs::write(root.join(".pnp.json"), serde_json::to_vec_pretty(&map)?)?;
    std::fs::write(root.join(".pnp.cjs"), CJS_RUNTIME)?;
    std::fs::write(root.join(".pnp.mjs"), ESM_ENTRY)?;
    std::fs::write(root.join(".pnp-resolve.mjs"), ESM_RESOLVER)?;
    Ok(())
}

/// remove pnp runtime files (used when the linker switches to node-modules)
pub fn remove_runtime(root: &Path) {
    for f in [".pnp.json", ".pnp.cjs", ".pnp.mjs", ".pnp-resolve.mjs"] {
        let _ = std::fs::remove_file(root.join(f));
    }
}

/// NODE_OPTIONS fragment that activates the pnp hooks, or None in node-modules mode
pub fn node_options(root: &Path, linker: Linker) -> Option<String> {
    if linker == Linker::NodeModules {
        return None;
    }
    // NODE_OPTIONS parsing strips backslashes inside quoted args, use forward slashes
    let cjs = root.join(".pnp.cjs").display().to_string().replace('\\', "/");
    let mjs_path = root.join(".pnp.mjs").display().to_string().replace('\\', "/");
    let mjs = if mjs_path.starts_with('/') {
        format!("file://{}", mjs_path)
    } else {
        format!("file:///{}", mjs_path)
    }
    .replace(' ', "%20");
    Some(format!("--require \"{}\" --import \"{}\"", cjs, mjs))
}

pub fn apply_env(cmd: &mut std::process::Command, root: &Path, linker: Linker) {
    if let Some(opts) = node_options(root, linker) {
        let existing = std::env::var("NODE_OPTIONS").unwrap_or_default();
        let opts = format!("{} {}", existing.trim(), opts);
        cmd.env("NODE_OPTIONS", opts.trim().to_string());
    }
    let bin = root.join(".zap").join("bin").display().to_string();
    let path = std::env::var("PATH").unwrap_or_default();
    let sep = if cfg!(windows) { ";" } else { ":" };
    cmd.env("PATH", format!("{}{}{}", bin, sep, path));
}

/// shim filename: .cmd wrappers on windows, extensionless scripts elsewhere
pub fn shim_file_name(bin_name: &str) -> String {
    if cfg!(windows) {
        format!("{}.cmd", bin_name)
    } else {
        bin_name.to_string()
    }
}

/// shim script body for both platforms; `hook` activates the pnp runtime
pub fn shim_content(bin_file: &Path, hook: Option<&str>, extra_env: &[(&str, &str)]) -> String {
    if cfg!(windows) {
        let mut out = String::from("@echo off\r\n");
        if let Some(opts) = hook {
            out.push_str(&format!("set NODE_OPTIONS={}\r\n", opts));
        }
        for (k, v) in extra_env {
            out.push_str(&format!("set {}={}\r\n", k, v));
        }
        out.push_str(&format!("node \"{}\" %*\r\n", bin_file.display()));
        out
    } else {
        let mut envs = String::new();
        if let Some(opts) = hook {
            // hook values contain double quotes, single-quote wrap for sh
            envs.push_str(&format!("NODE_OPTIONS='{}' ", opts));
        }
        for (k, v) in extra_env {
            envs.push_str(&format!("{}='{}' ", k, v));
        }
        format!(
            "#!/bin/sh\n{}exec node \"{}\" \"$@\"\n",
            envs,
            bin_file.display()
        )
    }
}

/// write one shim with the right name + exec bit for the platform
pub fn write_shim(
    bin_dir: &Path,
    bin_name: &str,
    bin_file: &Path,
    hook: Option<&str>,
    extra_env: &[(&str, &str)],
) -> Result<()> {
    let path = bin_dir.join(shim_file_name(bin_name));
    std::fs::write(&path, shim_content(bin_file, hook, extra_env))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    let _ = path;
    Ok(())
}

/// write .zap/bin shims for every installed package's bins
pub fn write_shims(root: &Path, tree: &Tree, hook: Option<&str>) -> Result<()> {
    let bin_dir = root.join(".zap").join("bin");
    std::fs::create_dir_all(&bin_dir)?;
    let registry = root.join(".zap").join("v");
    for (id, pkg) in &tree.packages {
        let dir = registry.join(crate::util::pkg_dir_name(id));
        for (bin_name, rel) in &pkg.bin {
            let bin_file = dir.join(rel.trim_start_matches("./"));
            write_shim(&bin_dir, bin_name, &bin_file, hook, &[])?;
        }
    }
    Ok(())
}

const CJS_RUNTIME: &str = r##""use strict";
const fs = require("fs");
const path = require("path");
const Module = require("module");
const BUILTINS = new Set(Module.builtinModules.concat(Module.builtinModules.map((m) => "node:" + m)));
const MAP = JSON.parse(fs.readFileSync(path.join(__dirname, ".pnp.json"), "utf8"));

function isBareSpecifier(request) {
  if (typeof request !== "string") return false;
  if (request.startsWith("./") || request.startsWith("../")) return false;
  if (path.isAbsolute(request) || request.startsWith("file:")) return false;
  return true;
}
function isBuiltin(request) {
  return BUILTINS.has(request) || BUILTINS.has(request.replace(/^node:/, ""));
}
function contextFor(file) {
  if (!file) return { deps: MAP.root.deps, label: "workspace root", dir: MAP.workspaceRoot };
  // inside the virtual store?
  let rel = path.relative(MAP.registry, file);
  if (!rel.startsWith("..") && !path.isAbsolute(rel)) {
    const top = rel.split(path.sep)[0];
    const id = top && MAP.byDir[top];
    if (id && MAP.packages[id]) {
      return { deps: MAP.packages[id].deps, label: id, dir: path.join(MAP.registry, MAP.packages[id].dir) };
    }
  }
  // inside a workspace member?
  rel = path.relative(MAP.workspaceRoot, file);
  if (!rel.startsWith("..") && !path.isAbsolute(rel)) {
    const relPosix = rel.split(path.sep).join("/");
    let best = null;
    for (const ws of MAP.workspaces || []) {
      if (relPosix === ws.dir || relPosix.startsWith(ws.dir + "/")) {
        if (!best || ws.dir.length > best.dir.length) best = ws;
      }
    }
    if (best) {
      return { deps: best.deps, label: best.name, dir: path.join(MAP.workspaceRoot, best.dir) };
    }
  }
  return { deps: MAP.root.deps, label: "workspace root", dir: MAP.workspaceRoot };
}
function baseOf(request) {
  if (request.startsWith("@")) {
    const i = request.indexOf("/", 1);
    if (i < 0) return request;
    const j = request.indexOf("/", i + 1);
    return j < 0 ? request : request.slice(0, j);
  }
  const i = request.indexOf("/");
  return i < 0 ? request : request.slice(0, i);
}
function isFile(p) { try { return fs.statSync(p).isFile(); } catch { return false; } }
function isDir(p) { try { return fs.statSync(p).isDirectory(); } catch { return false; } }
function readPkg(dir) {
  try { return JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8")); } catch { return null; }
}
function exportTarget(entry) {
  // unwrap up to two levels of condition objects ({ require, node, default, import })
  let v = entry;
  for (let i = 0; i < 2; i++) {
    if (v && typeof v === "object") {
      v = v.require || v.node || v.default || v.import || null;
    } else break;
  }
  return typeof v === "string" ? v : null;
}
function pnpError(msg) {
  const e = new Error(msg + " (zap pnp)");
  e.code = "MODULE_NOT_FOUND";
  e.pnp = true;
  return e;
}
function resolveMain(dir) {
  const pkg = readPkg(dir);
  if (pkg && pkg.exports) {
    // exports may be a string, a subpath map (with ".") or a root-level conditions object
    let t = null;
    if (typeof pkg.exports === "string") {
      t = pkg.exports;
    } else if (typeof pkg.exports === "object") {
      t = "." in pkg.exports ? exportTarget(pkg.exports["."]) : exportTarget(pkg.exports);
    }
    if (t) { const f = path.join(dir, t); if (isFile(f)) return f; }
  }
  if (pkg && typeof pkg.main === "string" && pkg.main) {
    const f = path.join(dir, pkg.main);
    for (const c of [f, f + ".js", f + ".json", f + ".node"]) { if (isFile(c)) return c; }
    if (isDir(f)) {
      for (const c of [path.join(f, "index.js"), path.join(f, "index.json")]) { if (isFile(c)) return c; }
    }
  }
  for (const c of [path.join(dir, "index.js"), path.join(dir, "index.json")]) { if (isFile(c)) return c; }
  throw pnpError("cannot resolve entry of package at " + dir);
}
function resolveWithin(dir, sub) {
  const pkg = readPkg(dir);
  if (pkg && pkg.exports && typeof pkg.exports === "object") {
    // only exact exports entries; wildcard export patterns fall back to the filesystem
    const t = exportTarget(pkg.exports["./" + sub.split(path.sep).join("/")]);
    if (t) { const f = path.join(dir, t); if (isFile(f)) return f; }
  }
  const cand = path.join(dir, sub);
  for (const c of [cand, cand + ".js", cand + ".json", cand + ".node"]) { if (isFile(c)) return c; }
  if (isDir(cand)) {
    const inner = readPkg(cand);
    if (inner && typeof inner.main === "string" && inner.main) {
      const f = path.join(cand, inner.main);
      for (const c of [f, f + ".js", f + ".json"]) { if (isFile(c)) return c; }
    }
    for (const c of [path.join(cand, "index.js"), path.join(cand, "index.json")]) { if (isFile(c)) return c; }
  }
  throw pnpError("cannot find '" + sub + "' in package at " + dir);
}
function resolveImports(request, dir) {
  // package.json "imports": string or conditions entries, plus simple "*" patterns
  const pkg = readPkg(dir);
  if (pkg && pkg.imports) {
    let entry = pkg.imports[request];
    if (entry === undefined) {
      for (const key of Object.keys(pkg.imports)) {
        const star = key.indexOf("*");
        if (star < 0) continue;
        const pre = key.slice(0, star);
        const post = key.slice(star + 1);
        if (request.startsWith(pre) && request.endsWith(post) && request.length >= pre.length + post.length) {
          const mid = request.slice(pre.length, request.length - post.length);
          const target = exportTarget(pkg.imports[key]);
          if (typeof target === "string" && target.includes("*")) {
            entry = target.replace("*", mid);
            break;
          }
        }
      }
    }
    const t = exportTarget(entry);
    if (t) {
      const cand = path.join(dir, t);
      for (const c of [cand, cand + ".js", cand + ".json"]) { if (isFile(c)) return c; }
    }
  }
  throw pnpError("cannot find import '" + request + "' in package at " + dir);
}
function dirOfId(depId) {
  if (depId.startsWith("ws:")) {
    const wsDir = (MAP.workspaceByName || {})[depId.slice(3)];
    if (!wsDir) throw pnpError("workspace package '" + depId.slice(3) + "' is not linked");
    return path.join(MAP.workspaceRoot, wsDir);
  }
  const info = MAP.packages[depId];
  if (!info) throw pnpError("broken resolution map entry '" + depId + "'");
  return path.join(MAP.registry, info.dir);
}
function resolveSpecifier(request, parentFile) {
  const ctx = contextFor(parentFile);
  if (request.startsWith("#")) return resolveImports(request, ctx.dir);
  const base = baseOf(request);
  const sub = base === request ? "" : request.slice(base.length + 1);
  let depId = ctx.deps[base];
  if (!depId && !MAP.strict) depId = MAP.fallback[base];
  if (!depId) {
    throw pnpError("cannot find package '" + base + "' required by " + ctx.label + ", try `zap add " + base + "`");
  }
  const dir = dirOfId(depId);
  return sub ? resolveWithin(dir, sub) : resolveMain(dir);
}

const ORIGINAL = Module._resolveFilename;
Module._resolveFilename = function patchedResolveFilename(request, parent, isMain, options) {
  if (isBareSpecifier(request) && !isBuiltin(request)) {
    try {
      return resolveSpecifier(request, parent && parent.filename);
    } catch (e) {
      if (e && e.pnp) throw e;
      // unexpected hook errors: let node try (and produce its own error)
    }
  }
  return ORIGINAL.call(this, request, parent, isMain, options);
};

module.exports = { isBareSpecifier, isBuiltin, resolveSpecifier };
"##;

const ESM_ENTRY: &str = r##"import { register, registerHooks } from "node:module";
import * as hooks from "./.pnp-resolve.mjs";
if (registerHooks) {
  registerHooks(hooks);
} else {
  register("./.pnp-resolve.mjs", import.meta.url);
}
"##;

const ESM_RESOLVER: &str = r##"import { pathToFileURL, fileURLToPath } from "node:url";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const pnp = require("./.pnp.cjs");

export function resolve(specifier, context, nextResolve) {
  if (!pnp.isBareSpecifier(specifier) || pnp.isBuiltin(specifier)) {
    return nextResolve(specifier, context);
  }
  try {
    let parentFile = null;
    if (context.parentURL) {
      try { parentFile = fileURLToPath(context.parentURL); } catch { parentFile = null; }
    }
    const file = pnp.resolveSpecifier(specifier, parentFile);
    return { url: pathToFileURL(file).href, shortCircuit: true };
  } catch (e) {
    if (e && e.pnp) throw e;
    return nextResolve(specifier, context);
  }
}
"##;
