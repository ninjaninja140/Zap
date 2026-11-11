// `zap create <template> [args...]`, runs create-<template>'s bin like npm create

use anyhow::{anyhow, Result};

use crate::cmds::dlx_run;

pub const NAME: &str = "create";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "scaffold with a create-* initializer (like npm create)";
pub const ABOUT: &str = "scaffolds a project with a create-* initializer's bin, exactly like
npm create: `zap create vite my-app` runs create-vite.";
pub const EXAMPLE: &str = "  $ zap create vite my-app
  scaffold a vite project with create-vite";

pub fn run(args: &[String]) -> Result<i32> {
    let template = args
        .first()
        .ok_or_else(|| anyhow!("usage: zap create <template> [args...], e.g. zap create vite my-app"))?
        .clone();
    let passthrough: Vec<String> = args.iter().skip(1).cloned().collect();
    let (name, range) = crate::util::split_spec(&template);
    let code = dlx_run(&format!("create-{}", name), range, passthrough)?;
    // the initializer finished scaffolding: give the project a .gitignore
    // with the zap runtime entries (no-op outside git repos)
    let root = std::env::current_dir()?;
    if let Some(state) = crate::cmds::ensure_gitignore(&root)? {
        crate::cmds::print_gitignore_note(state);
    }
    Ok(code)
}
