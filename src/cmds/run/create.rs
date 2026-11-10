// `zap create <template> [args...]`, runs create-<template>'s bin like npm create

use anyhow::{anyhow, Result};

use crate::cmds::dlx_run;

pub const NAME: &str = "create";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "scaffold with a create-* initializer (like npm create)";

pub fn run(args: &[String]) -> Result<i32> {
    let template = args
        .first()
        .ok_or_else(|| anyhow!("usage: zap create <template> [args...], e.g. zap create vite my-app"))?
        .clone();
    let passthrough: Vec<String> = args.iter().skip(1).cloned().collect();
    let (name, range) = crate::util::split_spec(&template);
    dlx_run(&format!("create-{}", name), range, passthrough)
}
