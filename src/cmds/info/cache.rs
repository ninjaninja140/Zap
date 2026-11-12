// `zap cache clean`, wipe the global store, metadata cache and dlx cache;
// `zap cache inspect`, an interactive keep & delete browser over the same
// dirs (built on the shared raw-mode layer in crate::cmds::rawui)

use anyhow::{bail, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};

use crate::config::Config;
use crate::cmds::rawui;
use crate::util::green;

pub const NAME: &str = "cache";
pub const ALIASES: &[&str] = &[];
pub const USAGE: &str = "clean: wipe every cache · inspect: interactive keep & delete browser";
pub const ABOUT: &str = "`cache clean` wipes the global content-addressable store, the registry
metadata cache and the dlx cache, logging each target's size before
deleting, the reset button when a corrupted artifact misbehaves.
`cache inspect` opens a raw-mode browser over the same dirs: every store
entry with its size, mark rows (space/a/n), sort by size, delete the
marked ones live.";
pub const EXAMPLE: &str = "  $ zap cache clean
  drop all local caches; the next install refetches

  $ zap cache inspect
  browse the store, mark the heavy entries, delete them";

/// the wipe targets with their stats, computed before any deletion
fn cache_targets(dirs: &[(&str, &Path)]) -> Vec<(String, PathBuf, u64, u64)> {
    dirs.iter()
        .map(|(label, path)| {
            let (files, bytes) = crate::cmds::dir_stats(path);
            ((*label).to_string(), path.to_path_buf(), files, bytes)
        })
        .collect()
}

pub fn run(args: &[String]) -> Result<i32> {
    match args.first().map(|s| s.as_str()) {
        Some("clean") | Some("clear") => {
            let cfg = Config::load(&std::env::current_dir()?);
            let store = match &cfg.store_dir {
                Some(d) => d.clone(),
                None => crate::util::store_dir()?,
            };
            let meta = crate::util::meta_cache()?;
            let dlx = crate::util::zap_home()?.join("dlx");

            // the target log: sizes before deletion, always printed (a log,
            // not a bar, piped runs keep it)
            let targets = cache_targets(&[
                ("store", &store),
                ("metadata cache", &meta),
                ("dlx cache", &dlx),
            ]);
            let mut freed_files = 0u64;
            let mut freed_bytes = 0u64;
            for (label, path, files, bytes) in &targets {
                println!(
                    "  {} {}: {} ({}, {} files)",
                    crate::util::dim("removing"),
                    label,
                    path.display(),
                    crate::util::human(*bytes),
                    files
                );
                freed_files += files;
                freed_bytes += bytes;
            }

            // the store is the big one: per-entry removal under the bar (one
            // giant remove_dir_all would make the bar meaningless)
            let entries: Vec<PathBuf> = std::fs::read_dir(&store)
                .map(|rd| rd.flatten().map(|e| e.path()).collect())
                .unwrap_or_default();
            if !entries.is_empty() {
                let progress = crate::cmds::progress::Progress::start(0);
                progress.stage("removing store");
                progress.bump_jobs(0, entries.len() as u64, 0);
                for (k, e) in entries.iter().enumerate() {
                    let _ = std::fs::remove_dir_all(e);
                    progress.bump_jobs(k as u64 + 1, entries.len() as u64, 0);
                }
                progress.finish();
                // the root goes too (today's end-state: the whole store gone)
                let _ = std::fs::remove_dir(&store);
            }
            // metadata + dlx are small: quick wipes after the bar
            let _ = std::fs::remove_dir_all(&meta);
            let _ = std::fs::remove_dir_all(&dlx);

            println!("{} store, metadata cache and dlx cache cleared", green("done"));
            println!("  {} freed {} ({} files)", green("✓"), crate::util::human(freed_bytes), freed_files);
            Ok(0)
        }
        Some("inspect") => cache_inspect(),
        _ => bail!("usage: zap cache clean|inspect"),
    }
}

// cache inspect: the interactive keep & delete browser

/// one browsable row: a store package dir or one of the two cache areas
#[derive(Clone)]
struct Entry {
    label: String,
    path: PathBuf,
    bytes: u64,
    special: bool, // the meta/dlx cache rows pin to the top
}

/// the browsable rows: the two cache areas first (always listed, deletable
/// like entries), then the store's package dirs, sized up front via dir_stats
fn load_entries(store: &Path, meta: &Path, dlx: &Path) -> Vec<Entry> {
    let stat = |p: &Path| {
        let (_, bytes) = crate::cmds::dir_stats(p);
        Entry {
            label: String::new(),
            path: p.to_path_buf(),
            bytes,
            special: true,
        }
    };
    let mut out = vec![
        Entry { label: "metadata cache".to_string(), ..stat(meta) },
        Entry { label: "dlx cache".to_string(), ..stat(dlx) },
    ];
    let mut pkgs: Vec<Entry> = std::fs::read_dir(store)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .map(|p| {
                    let (_, bytes) = crate::cmds::dir_stats(&p);
                    Entry {
                        label: p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                        path: p,
                        bytes,
                        special: false,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    pkgs.sort_by(|a, b| a.label.cmp(&b.label));
    out.extend(pkgs);
    out
}

/// browser state: cursor + the marked set + the sort order. marks are keyed
/// by path so they survive a sort toggle (they follow the package, not the
/// row position); the cursor indexes the current sorted view
struct Browser {
    cursor: usize,
    marked: std::collections::HashSet<PathBuf>,
    by_size: bool,
}

impl Browser {
    fn new() -> Self {
        Browser { cursor: 0, marked: std::collections::HashSet::new(), by_size: false }
    }

    /// move the cursor, clamped to the list (no wraparound)
    fn move_by(&mut self, d: isize, n: usize) {
        self.cursor = rawui::clamp_move(self.cursor, d, n);
    }

    /// space: flip the cursor row's mark
    fn toggle(&mut self, e: &Entry) {
        if !self.marked.remove(&e.path) {
            self.marked.insert(e.path.clone());
        }
    }

    /// a: mark everything, the cache areas included
    fn mark_all(&mut self, entries: &[Entry]) {
        for e in entries {
            self.marked.insert(e.path.clone());
        }
    }

    /// n: mark nothing
    fn mark_none(&mut self) {
        self.marked.clear();
    }

    /// s: alphabetical <-> size-descending
    fn toggle_sort(&mut self) {
        self.by_size = !self.by_size;
    }

    fn marked_count(&self) -> usize {
        self.marked.len()
    }

    /// the deletion accounting: bytes behind the marked set
    fn marked_bytes(&self, entries: &[Entry]) -> u64 {
        entries.iter().filter(|e| self.marked.contains(&e.path)).map(|e| e.bytes).sum()
    }
}

/// the list order: the cache areas pinned on top, then the packages
/// alphabetically, or size-descending after `s`, the "what's eating my
/// disk" view
fn sorted(entries: &[Entry], by_size: bool) -> Vec<&Entry> {
    let (mut pinned, mut pkgs): (Vec<&Entry>, Vec<&Entry>) = entries.iter().partition(|e| e.special);
    if by_size {
        pkgs.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.label.cmp(&b.label)));
    } else {
        pkgs.sort_by(|a, b| a.label.cmp(&b.label));
    }
    pinned.append(&mut pkgs);
    pinned
}

/// compose the browser screen: full clear + redraw per keypress (the same
/// discipline as the pickers, fixed-width rows, no ESC[2K), a dim title
/// with the totals up top; `confirming` swaps the footer for the y/n
/// prompt, `status` holds the last delete's freed summary
fn frame(entries: &[Entry], b: &Browser, w: usize, h: usize, store: &Path, confirming: bool, status: &Option<String>) -> String {
    use crate::util::{bold, dim, electric_bold, red};
    let total: u64 = entries.iter().map(|e| e.bytes).sum();
    let view_list = sorted(entries, b.by_size);
    let mut out = String::from("\x1b[H\x1b[2J");
    out.push_str(&format!(
        "\x1b[1;1H{}",
        dim(format!(
            "zap cache, {} entr{}, {} total{}",
            entries.len(),
            if entries.len() == 1 { "y" } else { "ies" },
            crate::util::human(total),
            if b.by_size { " · by size" } else { "" }
        ))
    ));
    out.push_str(&format!("\x1b[2;1H{}", dim(format!("store: {}", store.display()))));
    let status_n = usize::from(status.is_some());
    if let Some(s) = status {
        out.push_str(&format!("\x1b[3;1H{}", green(format!("✓ {}", s))));
    }
    let list_start = 3 + status_n; // title + store line [+ status] + one blank
    let view = h.saturating_sub(list_start + 2).max(1); // + gap + footer
    let off = rawui::window(b.cursor, view_list.len(), view);
    let w_size = view_list.iter().map(|e| crate::util::human(e.bytes).chars().count()).max().unwrap_or(2).max(2);
    let budget = w.saturating_sub(w_size + 8).max(4); // marker + gaps + the ✓ mark
    let w_name = view_list.iter().map(|e| e.label.chars().count()).max().unwrap_or(0).min(budget);
    let mut shown = 0usize;
    for (i, e) in view_list.iter().enumerate().skip(off).take(view) {
        let cursor_row = i == b.cursor;
        let marker = if cursor_row { electric_bold("❯") } else { " ".to_string() };
        let name: String = e.label.chars().take(w_name).collect();
        let size = crate::util::human(e.bytes);
        let mark = if b.marked.contains(&e.path) { green(" ✓") } else { String::new() };
        let line = format!(
            "{} {:<nw$}  {:<sw$}{}",
            marker,
            name,
            size,
            mark,
            nw = w_name,
            sw = w_size
        );
        out.push_str(&format!("\x1b[{};1H{}", list_start + shown, line));
        shown += 1;
    }
    let foot_row = list_start + shown + 1;
    if confirming {
        // the y/n prompt replaces the hints; truncate the plain text first so
        // escape codes never split
        let n = b.marked_count();
        let plain: String = format!(
            "delete {} entr{} ({})? y/n",
            n,
            if n == 1 { "y" } else { "ies" },
            crate::util::human(b.marked_bytes(entries))
        )
        .chars()
        .take(w)
        .collect();
        out.push_str(&format!("\x1b[{};1H{}", foot_row, red(&plain)));
    } else {
        let hints = "space: mark · a: all · n: none · s: sort · d: delete · q: cancel";
        let (n, mine, tot) = (
            b.marked_count(),
            crate::util::human(b.marked_bytes(entries)),
            crate::util::human(total),
        );
        let counts = format!("marked {} ({} of {} total)", n, mine, tot);
        let plain = format!("{} · {}", hints, counts);
        // the counts are the payload: the marked size bold, the totals dim;
        // a terminal too narrow for both falls back to a dim truncation
        let foot = if plain.chars().count() <= w {
            format!(
                "{} · {}{}{}{}{}",
                dim(hints),
                dim("marked "),
                bold(n.to_string()),
                dim(" ("),
                bold(&mine),
                dim(format!(" of {} total)", tot)),
            )
        } else {
            dim(plain.chars().take(w).collect::<String>())
        };
        out.push_str(&format!("\x1b[{};1H{}", foot_row, foot));
    }
    out
}

/// remove the marked entries from disk, under the progress bar (same wiring
/// as cache clean's sweep); remove_dir_all unlinks junctions, never follows
/// them, so nothing outside the target dirs is touched. returns freed bytes
fn delete_entries(to_delete: &[Entry]) -> u64 {
    let progress = crate::cmds::progress::Progress::start(0);
    progress.stage("removing");
    progress.bump_jobs(0, to_delete.len() as u64, 0);
    let mut bytes = 0u64;
    for (k, e) in to_delete.iter().enumerate() {
        let _ = std::fs::remove_dir_all(&e.path);
        bytes += e.bytes;
        progress.bump_jobs(k as u64 + 1, to_delete.len() as u64, 0);
    }
    progress.finish();
    bytes
}
/// the browser loop: keys from `rx` (the real console reader, or a scripted
/// channel in tests), repaint on change, deletion the moment y lands. the
/// list updates live and the loop runs on until q/esc/ctrl+c or stdin EOF
fn browse_loop(
    rx: mpsc::Receiver<rawui::Key>,
    entries: &mut Vec<Entry>,
    b: &mut Browser,
    status: &mut Option<String>,
    store: &Path,
    mut w: usize,
    mut h: usize,
) -> Result<()> {
    let mut stdout = std::io::stdout();
    let mut confirming = false;
    let mut dirty = false;
    let mut tick = 0u32;
    loop {
        match rx.recv_timeout(rawui::TICK) {
            Ok(rawui::Key::Up) => {
                b.move_by(-1, entries.len());
                dirty = true;
            }
            Ok(rawui::Key::Down) => {
                b.move_by(1, entries.len());
                dirty = true;
            }
            Ok(rawui::Key::Space) => {
                if let Some(e) = sorted(entries, b.by_size).get(b.cursor).copied() {
                    b.toggle(e);
                }
                dirty = true;
            }
            Ok(rawui::Key::All) => {
                b.mark_all(entries);
                dirty = true;
            }
            Ok(rawui::Key::No) => {
                if confirming {
                    confirming = false; // n denies the delete
                } else {
                    b.mark_none();
                }
                dirty = true;
            }
            Ok(rawui::Key::Sort) => {
                b.toggle_sort();
                dirty = true;
            }
            Ok(rawui::Key::Delete) => {
                // the prompt only arms when there's something to delete
                if !confirming && b.marked_count() > 0 {
                    confirming = true;
                }
                dirty = true;
            }
            Ok(rawui::Key::Yes) => {
                if confirming {
                    confirming = false;
                    let to_delete: Vec<Entry> = sorted(entries, b.by_size)
                        .into_iter()
                        .filter(|e| b.marked.contains(&e.path))
                        .cloned()
                        .collect();
                    let n = to_delete.len();
                    let bytes = delete_entries(&to_delete);
                    entries.retain(|e| !b.marked.contains(&e.path));
                    b.mark_none();
                    b.move_by(0, entries.len()); // clamp after the shrink
                    *status = Some(format!(
                        "freed {} ({} entr{})",
                        crate::util::human(bytes),
                        n,
                        if n == 1 { "y" } else { "ies" }
                    ));
                }
                dirty = true;
            }
            Ok(rawui::Key::Cancel) | Ok(rawui::Key::CtrlC) | Err(RecvTimeoutError::Disconnected) => break,
            // enter/left/right/m belong to the other uis sharing the parser
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {}
        }
        // resize poll between repaints (same cadence as the pickers)
        tick += 1;
        if rawui::poll_resize(&mut w, &mut h, tick) {
            dirty = true;
        }
        if dirty {
            let mut out = frame(entries, b, w, h, store, confirming, status);
            out.push_str("\x1b[H"); // park the cursor so nothing scrolls
            let _ = stdout.write_all(out.as_bytes());
            let _ = stdout.flush();
            dirty = false;
        }
    }
    Ok(())
}

/// `zap cache inspect`: the interactive keep & delete browser over the cache
/// dirs; global state, cwd-irrelevant, deletions happen the moment y lands
fn cache_inspect() -> Result<i32> {
    // piped stdin: bail before anything is painted (the shared probe)
    if !rawui::console_available() {
        bail!("interactive mode needs a terminal");
    }
    let cfg = Config::load(&std::env::current_dir()?);
    let store = match &cfg.store_dir {
        Some(d) => d.clone(),
        None => crate::util::store_dir()?,
    };
    let meta = crate::util::meta_cache()?;
    let dlx = crate::util::zap_home()?.join("dlx");
    let mut entries = load_entries(&store, &meta, &dlx);
    let mut b = Browser::new();
    let mut status: Option<String> = None;

    let (w, h) = rawui::term_size();
    let guard = rawui::TerminalGuard::new();
    if !guard.raw_active() {
        // raced away between probe and takeover: same error, nothing painted
        bail!("interactive mode needs a terminal");
    }
    let (tx, rx) = mpsc::channel::<rawui::Key>();
    rawui::spawn_key_thread(tx);
    // alt screen, hide cursor, first paint
    let mut out = String::from("\x1b[?1049h\x1b[?25l");
    out.push_str(&frame(&entries, &b, w, h, &store, false, &status));
    let mut stdout = std::io::stdout();
    stdout.write_all(out.as_bytes())?;
    stdout.flush()?;

    browse_loop(rx, &mut entries, &mut b, &mut status, &store, w, h)?;
    drop(guard); // restore raw mode + leave the alt screen before more output
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// env-touching tests serialize: ZAP_HOME leaks process-wide
    
    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zap-cache-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cache_targets_lists_sizes_and_files() {
        let _env = crate::cmds::env_test_lock();
        let home = tempdir("home");
        std::env::set_var("ZAP_HOME", &home);
        // store: two package entries, two files with known sizes
        let store = crate::util::store_dir().unwrap();
        std::fs::create_dir_all(store.join("a@1.0.0")).unwrap();
        std::fs::write(store.join("a@1.0.0").join("f.txt"), "12345").unwrap();
        std::fs::create_dir_all(store.join("b@2.0.0").join("sub")).unwrap();
        std::fs::write(store.join("b@2.0.0").join("sub").join("g.txt"), "1234567").unwrap();
        // meta + dlx: one file each
        let meta = crate::util::meta_cache().unwrap();
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("m.json"), "123").unwrap();
        let dlx = crate::util::zap_home().unwrap().join("dlx");
        std::fs::create_dir_all(&dlx).unwrap();
        std::fs::write(dlx.join("d.txt"), "12").unwrap();

        let targets = cache_targets(&[
            ("store", &store),
            ("metadata cache", &meta),
            ("dlx cache", &dlx),
            ("missing", &home.join("nope")),
        ]);
        assert_eq!(targets.len(), 4);
        let (label, path, files, bytes) = &targets[0];
        assert_eq!((label.as_str(), *files, *bytes), ("store", 2, 12));
        assert_eq!(path.file_name().unwrap(), "store");
        assert_eq!((targets[1].0.as_str(), targets[1].2, targets[1].3), ("metadata cache", 1, 3));
        assert_eq!((targets[2].0.as_str(), targets[2].2, targets[2].3), ("dlx cache", 1, 2));
        // a missing dir stats as zeros, still listed
        assert_eq!((targets[3].0.as_str(), targets[3].2, targets[3].3), ("missing", 0, 0));
        std::env::remove_var("ZAP_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn clean_wipes_everything_and_frees_the_logged_sizes() {
        let _env = crate::cmds::env_test_lock();
        let home = tempdir("clean");
        std::env::set_var("ZAP_HOME", &home);
        let store = crate::util::store_dir().unwrap();
        std::fs::create_dir_all(store.join("a@1.0.0").join("d")).unwrap();
        std::fs::write(store.join("a@1.0.0").join("d").join("f.txt"), "12345").unwrap();
        let meta = crate::util::meta_cache().unwrap();
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("m.json"), "123").unwrap();
        let dlx = crate::util::zap_home().unwrap().join("dlx");
        std::fs::create_dir_all(&dlx).unwrap();
        std::fs::write(dlx.join("d.txt"), "12").unwrap();

        // the accounting the log lines will show: 3 files, 10 bytes
        let targets = cache_targets(&[
            ("store", &store),
            ("metadata cache", &meta),
            ("dlx cache", &dlx),
        ]);
        let freed: (u64, u64) = targets.iter().fold((0, 0), |a, t| (a.0 + t.2, a.1 + t.3));
        assert_eq!(freed, (3, 10), "pre-deletion accounting");

        let code = run(&["clean".to_string()]).unwrap();
        assert_eq!(code, 0);
        // everything wiped
        assert!(!store.exists());
        assert!(!meta.exists());
        assert!(!dlx.exists());
        // second run: zeros + trivial bar + exit 0
        let code = run(&["clean".to_string()]).unwrap();
        assert_eq!(code, 0);
        std::env::remove_var("ZAP_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    // cache inspect (browser state machine, pure + offline fs)

    /// visible text of a styled frame (the pickers' tests assert styled
    /// pieces directly; the piecewise footer reads better stripped)
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\x1b' {
                if it.peek() == Some(&'[') {
                    it.next();
                    for e in it.by_ref() {
                        if ('\x40'..='\x7e').contains(&e) {
                            break;
                        }
                    }
                } else {
                    it.next();
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    fn entry(label: &str, bytes: u64, special: bool) -> Entry {
        Entry {
            label: label.to_string(),
            path: PathBuf::from(label),
            bytes,
            special,
        }
    }

    #[test]
    fn browser_marks_toggles_and_accounts() {
        let es = vec![entry("a@1.0.0", 100, false), entry("b@2.0.0", 250, false)];
        let mut b = Browser::new();
        assert_eq!(b.marked_count(), 0);
        // space marks the cursor row, space again unmarks
        b.move_by(1, es.len());
        b.toggle(&es[1]);
        assert_eq!((b.marked_count(), b.marked_bytes(&es)), (1, 250));
        b.toggle(&es[1]);
        assert_eq!((b.marked_count(), b.marked_bytes(&es)), (0, 0));
        // a / n
        b.mark_all(&es);
        assert_eq!((b.marked_count(), b.marked_bytes(&es)), (2, 350));
        b.mark_none();
        assert_eq!(b.marked_count(), 0);
        // marks are keyed by path: they survive a sort toggle
        b.toggle(&es[0]);
        b.toggle_sort();
        assert_eq!(b.marked_bytes(&es), 100);
        // movement clamps, empty lists never panic
        b.move_by(-99, es.len());
        b.move_by(99, es.len());
        assert_eq!(b.cursor, 1);
        b.toggle(&es[1]);
        assert_eq!(b.marked_bytes(&es), 350);
        let mut e = Browser::new();
        e.move_by(1, 0);
        assert_eq!(e.cursor, 0);
    }

    #[test]
    fn sort_toggles_between_alpha_and_size() {
        let es = vec![
            entry("metadata cache", 10, true),
            entry("dlx cache", 20, true),
            entry("zzz@1.0.0", 500, false),
            entry("aaa@1.0.0", 900, false),
            entry("mmm@2.0.0", 100, false),
        ];
        let alpha: Vec<&str> = sorted(&es, false).iter().map(|e| e.label.as_str()).collect();
        assert_eq!(alpha, vec!["metadata cache", "dlx cache", "aaa@1.0.0", "mmm@2.0.0", "zzz@1.0.0"]);
        // size-descending among the packages, the cache areas stay pinned
        let by_size: Vec<&str> = sorted(&es, true).iter().map(|e| e.label.as_str()).collect();
        assert_eq!(by_size, vec!["metadata cache", "dlx cache", "aaa@1.0.0", "zzz@1.0.0", "mmm@2.0.0"]);
        // equal sizes fall back to the label so the order is stable
        let ties = vec![entry("b@1.0.0", 5, false), entry("a@1.0.0", 5, false)];
        let t: Vec<&str> = sorted(&ties, true).iter().map(|e| e.label.as_str()).collect();
        assert_eq!(t, vec!["a@1.0.0", "b@1.0.0"]);
    }

    #[test]
    fn load_entries_pins_the_cache_areas_and_sorts_packages() {
        let _env = crate::cmds::env_test_lock();
        let home = tempdir("load");
        std::env::set_var("ZAP_HOME", &home);
        let store = crate::util::store_dir().unwrap();
        std::fs::create_dir_all(store.join("zz@1.0.0")).unwrap();
        std::fs::write(store.join("zz@1.0.0").join("f.txt"), "12345").unwrap();
        std::fs::create_dir_all(store.join("aa@2.0.0")).unwrap();
        let meta = crate::util::meta_cache().unwrap();
        std::fs::create_dir_all(&meta).unwrap();
        let dlx = crate::util::zap_home().unwrap().join("dlx");
        std::fs::create_dir_all(&dlx).unwrap();

        let es = load_entries(&store, &meta, &dlx);
        let labels: Vec<&str> = es.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["metadata cache", "dlx cache", "aa@2.0.0", "zz@1.0.0"],
            "cache areas pinned, packages alphabetical"
        );
        assert!(es[0].special && es[1].special && !es[2].special);
        assert_eq!(es[3].bytes, 5, "sizes come from dir_stats");
        assert_eq!(es[2].bytes, 0, "an empty package dir stats as zeros");
        std::env::remove_var("ZAP_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn frame_renders_rows_sizes_marks_and_footer() {
        let es = vec![
            entry("metadata cache", 3, true),
            entry("dlx cache", 2, true),
            entry("a@1.0.0", 12345, false),
            entry("b@2.0.0", 7, false),
        ];
        let mut b = Browser::new();
        b.move_by(2, es.len()); // cursor on a@1.0.0
        b.toggle(&es[2]);
        let f = frame(&es, &b, 80, 24, Path::new("C:/store"), false, &None);
        assert!(f.contains("zap cache, 4 entries, "), "title totals: {}", f);
        assert!(f.contains("store: C:/store"), "store path line: {}", f);
        for label in ["metadata cache", "dlx cache", "a@1.0.0", "b@2.0.0"] {
            assert!(f.contains(label), "row {} rendered: {}", label, f);
        }
        assert!(f.contains(&crate::util::human(12345)), "per-entry size: {}", f);
        assert!(f.contains("❯"), "cursor marker present");
        assert!(f.contains("✓"), "the marked row shows its mark");
        // the full footer only fits past ~100 cols: at 120 hints + counts both show
        let f120 = frame(&es, &b, 120, 24, Path::new("C:/store"), false, &None);
        assert!(f120.contains("space: mark · a: all · n: none · s: sort · d: delete · q: cancel"));
        let p120 = strip_ansi(&f120);
        assert!(p120.contains(&format!("marked 1 ({} of {} total)", crate::util::human(12345), crate::util::human(12357))), "footer counts: {}", p120);
        // at 80 hints + counts don't fit: the footer truncates dim instead
        // of wrapping (the counts ride the wide terminal)
        let counts_full = format!("marked 1 ({} of {} total)", crate::util::human(12345), crate::util::human(12357));
        let full = format!("space: mark · a: all · n: none · s: sort · d: delete · q: cancel · {}", counts_full);
        let truncated: String = full.chars().take(80).collect();
        assert!(f.contains(&crate::util::dim(&truncated)), "narrow footer truncates dim: {}", strip_ansi(&f));
        // confirm mode swaps the footer for the y/n prompt
        let fc = frame(&es, &b, 80, 24, Path::new("C:/store"), true, &None);
        assert!(fc.contains("delete 1 entry (12.1 KB)? y/n"), "confirm prompt: {}", fc);
        assert!(!fc.contains("space: mark"), "hints replaced while confirming");
        // the freed summary renders as a status line when present
        let fs = frame(&es, &b, 80, 24, Path::new("C:/store"), false, &Some("freed 3 B (1 entry)".to_string()));
        assert!(fs.contains("✓ freed 3 B (1 entry)"), "status line: {}", fs);
        // no marked rows: no mark glyph, zero counts (wide enough to fit)
        let bn = Browser::new();
        let fnone = frame(&es, &bn, 120, 24, Path::new("s"), false, &None);
        assert!(!fnone.contains("✓"), "nothing marked");
        let pnone = strip_ansi(&fnone);
        assert!(pnone.contains("marked 0 (0 B of"), "zero counts: {}", pnone);
    }

    #[test]
    fn frame_windows_long_lists() {
        let mut es = vec![entry("metadata cache", 0, true), entry("dlx cache", 0, true)];
        es.extend((0..30).map(|i| entry(&format!("p{:02}@1.0.0", i), 1, false)));
        let mut b = Browser::new();
        for _ in 0..31 {
            b.move_by(1, es.len()); // cursor on the last row
        }
        let f = frame(&es, &b, 80, 24, Path::new("s"), false, &None);
        assert!(f.contains("p29@1.0.0"), "the cursor row is visible");
        assert!(!f.contains("p00@1.0.0"), "early rows scrolled out of the viewport");
    }

    #[test]
    fn delete_marked_removes_dirs_and_accounts() {
        let _env = crate::cmds::env_test_lock();
        let home = tempdir("delete");
        std::env::set_var("ZAP_HOME", &home);
        let store = crate::util::store_dir().unwrap();
        std::fs::create_dir_all(store.join("a@1.0.0")).unwrap();
        std::fs::write(store.join("a@1.0.0").join("f.txt"), "12345").unwrap();
        std::fs::create_dir_all(store.join("b@2.0.0")).unwrap();
        std::fs::write(store.join("b@2.0.0").join("g.txt"), "123").unwrap();
        let meta = crate::util::meta_cache().unwrap();
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("m.json"), "123").unwrap();
        let dlx = crate::util::zap_home().unwrap().join("dlx");
        std::fs::create_dir_all(&dlx).unwrap();
        std::fs::write(dlx.join("d.txt"), "12").unwrap();

        let es = load_entries(&store, &meta, &dlx);
        let mut b = Browser::new();
        for e in &es {
            if e.label == "a@1.0.0" || e.label == "dlx cache" {
                b.toggle(e);
            }
        }
        let to_delete: Vec<Entry> = sorted(&es, b.by_size)
            .into_iter()
            .filter(|e| b.marked.contains(&e.path))
            .cloned()
            .collect();
        assert_eq!(to_delete.len(), 2);
        let bytes = delete_entries(&to_delete);
        assert_eq!(bytes, 7, "5 + 2 bytes behind the marked set");
        assert!(!store.join("a@1.0.0").exists(), "marked entry gone");
        assert!(store.join("b@2.0.0").exists(), "unmarked survivor untouched");
        assert_eq!(std::fs::read_to_string(store.join("b@2.0.0").join("g.txt")).unwrap(), "123", "survivor content byte-identical");
        assert!(!dlx.exists(), "the dlx cache area is deletable like an entry");
        assert!(meta.exists(), "the unmarked cache area stays");
        std::env::remove_var("ZAP_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn browse_loop_takes_a_key_script_and_stays_live() {
        // the "dump harness": the loop runs against a scripted key channel
        // instead of a console, down to a@1.0.0, mark it, d, y, quit; the
        // deletion happens live and the list/totals update in place
        let _env = crate::cmds::env_test_lock();
        let home = tempdir("script");
        std::env::set_var("ZAP_HOME", &home);
        let store = crate::util::store_dir().unwrap();
        std::fs::create_dir_all(store.join("a@1.0.0")).unwrap();
        std::fs::write(store.join("a@1.0.0").join("f.txt"), "12345").unwrap();
        std::fs::create_dir_all(store.join("b@2.0.0")).unwrap();
        std::fs::write(store.join("b@2.0.0").join("g.txt"), "123").unwrap();
        let meta = crate::util::meta_cache().unwrap();
        std::fs::create_dir_all(&meta).unwrap();
        let dlx = crate::util::zap_home().unwrap().join("dlx");
        std::fs::create_dir_all(&dlx).unwrap();

        let mut entries = load_entries(&store, &meta, &dlx);
        assert_eq!(entries.len(), 4);
        let mut b = Browser::new();
        let mut status: Option<String> = None;
        let (tx, rx) = mpsc::channel::<rawui::Key>();
        // meta(0) dlx(1) a@1.0.0(2) b@2.0.0(3): down twice, mark, confirm, y
        for k in [
            rawui::Key::Down,
            rawui::Key::Down,
            rawui::Key::Space,
            rawui::Key::Delete,
            rawui::Key::Yes,
            rawui::Key::Cancel,
        ] {
            tx.send(k).unwrap();
        }
        drop(tx);
        browse_loop(rx, &mut entries, &mut b, &mut status, &store, 80, 24).unwrap();

        assert!(!store.join("a@1.0.0").exists(), "the marked entry was deleted live");
        assert!(store.join("b@2.0.0").exists(), "the unmarked survivor stayed");
        assert_eq!(status.as_deref(), Some("freed 5 B (1 entry)"), "the freed summary");
        assert_eq!(b.marked_count(), 0, "marks consumed by the delete");
        assert_eq!(entries.len(), 3, "the list re-rendered without the deleted row");
        assert!(b.cursor < entries.len(), "cursor clamped into the shrunken list");
        std::env::remove_var("ZAP_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn browse_loop_q_quits_without_touching_anything() {
        let _env = crate::cmds::env_test_lock();
        let home = tempdir("quit");
        std::env::set_var("ZAP_HOME", &home);
        let store = crate::util::store_dir().unwrap();
        std::fs::create_dir_all(store.join("a@1.0.0")).unwrap();
        std::fs::write(store.join("a@1.0.0").join("f.txt"), "12345").unwrap();
        let meta = crate::util::meta_cache().unwrap();
        std::fs::create_dir_all(&meta).unwrap();
        let dlx = crate::util::zap_home().unwrap().join("dlx");
        std::fs::create_dir_all(&dlx).unwrap();

        let mut entries = load_entries(&store, &meta, &dlx);
        let mut b = Browser::new();
        let mut status: Option<String> = None;
        let (tx, rx) = mpsc::channel::<rawui::Key>();
        tx.send(rawui::Key::Space).unwrap(); // mark the metadata cache
        tx.send(rawui::Key::Delete).unwrap(); // arm the prompt
        tx.send(rawui::Key::Cancel).unwrap(); // q: quit, nothing deleted
        drop(tx);
        browse_loop(rx, &mut entries, &mut b, &mut status, &store, 80, 24).unwrap();

        assert!(meta.exists(), "marked but never confirmed: untouched");
        assert!(store.join("a@1.0.0").exists());
        assert_eq!(status.as_deref(), None, "no delete, no summary");
        std::env::remove_var("ZAP_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
