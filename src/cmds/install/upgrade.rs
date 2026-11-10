// `zap upgrade`, no args: re-resolve within ranges; with args: bump to latest

use anyhow::Result;
use crate::util::dim;

pub const NAME: &str = "upgrade";
pub const ALIASES: &[&str] = &["up"];
pub const USAGE: &str = "no args: re-resolve within ranges; with args: bump to latest";

pub fn run(args: &[String]) -> Result<i32> {
    if args.is_empty() {
        println!("{}", dim("re-resolving all dependencies within their ranges..."));
        return super::install::run(&["--force".to_string()]);
    }
    let bumped: Vec<String> = args
        .iter()
        .map(|a| {
            if a.starts_with('-') {
                return a.clone();
            }
            let (_, range) = crate::util::split_spec(a);
            if range.is_empty() {
                format!("{}@latest", a)
            } else {
                a.clone()
            }
        })
        .collect();
    super::add::run(&bumped)
}
