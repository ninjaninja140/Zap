// one-line progress bar with stage status for the install flow (shared by
// add/remove/install/ci/migrate through install_in). a renderer thread
// redraws a single line in place (`\r` + clear-to-EOL, safe: one line, no
// neighbor columns), piped/CI runs get nothing at all. term_size moved here
// from _multirun.rs so there's exactly one copy

use std::io::Write;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// indeterminate-stage spinner frames, cycled per tick
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// how the bar line fills: (filled, empty) segments out of `width`
fn bar_fill(done: u64, total: u64, width: usize) -> (usize, usize) {
    if total == 0 {
        return (0, width);
    }
    let filled = ((done.min(total) as f64 / total as f64) * width as f64).round() as usize;
    (filled.min(width), width - filled.min(width))
}

/// one bar line: `  ● downloading ████████░░░░ 12/22 (8.4 MB)`. quantified
/// stages (total > 0) draw the bar + counts; indeterminate ones spin instead.
/// pure so the clamp, fill math and humanized bytes are testable
fn bar_line(stage: &str, done: u64, total: u64, bytes: u64, term_w: usize, spin: usize) -> String {
    let quantified = total > 0;
    let glyph = if quantified {
        crate::util::electric("●")
    } else {
        crate::util::electric(SPINNER[spin % SPINNER.len()])
    };
    let label_w = stage.chars().count() + 2; // glyph + space
    let tail = if quantified {
        let bytes_part = if bytes > 0 {
            format!(" ({})", crate::util::human(bytes))
        } else {
            String::new()
        };
        format!(" {}/{}{}", done, total, bytes_part)
    } else {
        String::new()
    };
    // bar fills what's left between the label and the counts, capped at 24
    let used = 2 + label_w + 1 + tail.chars().count(); // +1: the bar's lead space
    let bar_w = term_w.saturating_sub(used).min(24);
    let bar = if bar_w == 0 {
        String::new()
    } else {
        let (filled, empty) = bar_fill(done, total, bar_w);
        format!(
            " {}{}",
            crate::util::electric("█".repeat(filled)),
            crate::util::dim("░".repeat(empty))
        )
    };
    format!("  {} {}{}{}", glyph, stage, bar, crate::util::dim(&tail))
}

/// shared state behind the bar; the renderer thread watches `stop`
struct Inner {
    stage: Mutex<String>,
    done: AtomicU64,
    total: AtomicU64,
    bytes: AtomicU64,
    lanes: Vec<Arc<Lane>>,
    completed: Mutex<Vec<String>>, // finished downloads (name@version), completion order
    stop: AtomicBool,
    cleared: AtomicBool, // the final clear landed (finish waits for it)
}

/// one in-flight download lane: package id + its own byte counters; a zero
/// total means the server sent no content-length (indeterminate lane)
pub struct Lane {
    name: Mutex<String>,
    done: AtomicU64,
    total: AtomicU64,
    active: AtomicBool,
}

impl Lane {
    fn new() -> Self {
        Lane {
            name: Mutex::new(String::new()),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            active: AtomicBool::new(false),
        }
    }

    /// idle again for the next queued job
    fn clear(&self) {
        self.active.store(false, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
        self.total.store(0, Ordering::Relaxed);
    }
}

/// how many download lanes fit: capped at 8 and at what the terminal can
/// hold without scrolling (block = lanes + separator + main bar + one spare)
fn lane_budget(wanted: usize, term_h: usize) -> usize {
    wanted.min(8).min(term_h.saturating_sub(3))
}

/// block shape for the renderer's cursor-up math: one line per active lane
/// (collapses to none when idle), blank separator, main bar last
fn compose_block(lane_lines: &[String], main: &str) -> Vec<String> {
    let mut block: Vec<String> = lane_lines.to_vec();
    if !lane_lines.is_empty() {
        block.push(String::new());
    }
    block.push(main.to_string());
    block
}

/// the shared name-column width for lane lines (finished + in-flight must
/// align): name cell capped so a small bar + tail always fit
fn lane_name_w(w: usize) -> usize {
    28.min(w.saturating_sub(20).max(6))
}

/// one lane line: `  chalk@4.1.2   ████████░░░░  68%  412 KB`; None = idle
/// (the line collapses out of the block). indeterminate lanes (no
/// content-length) show the shared spinner instead of a bar
fn lane_line(lane: &Lane, w: usize, spin: usize) -> Option<String> {
    if !lane.active.load(Ordering::Relaxed) {
        return None;
    }
    let total = lane.total.load(Ordering::Relaxed);
    let done = lane.done.load(Ordering::Relaxed);
    // name budget: keep room for a small bar + tail on narrow terminals
    let name_w = lane_name_w(w);
    let name = lane.name.lock().unwrap();
    let shown: String = name.chars().take(name_w).collect();
    let head = format!("  {:<nw$}", shown, nw = name_w);
    if total == 0 {
        // indeterminate: spinner glyph, no percent (also covers the moment
        // before the first chunk lands)
        let glyph = crate::util::electric(SPINNER[spin % SPINNER.len()]);
        return Some(format!("{}{}{}", head, glyph, if done == 0 { String::new() } else { format!(" {}", crate::util::human(done)) }));
    }
    let tail = format!("  {}%  {}", done.min(total) * 100 / total, crate::util::human(done));
    // bar takes what's left between name and tail, capped at 16 per lane
    let used = name_w + 4 + tail.chars().count(); // name cell + the bar's lead gap
    let bar_w = w.saturating_sub(used).min(16);
    let bar = if bar_w == 0 {
        String::new()
    } else {
        let (filled, empty) = bar_fill(done, total, bar_w);
        // lane fills are ELECTRIC GREEN (vivid, reads as "good") against the
        // electric blue aggregate bar below
        format!(
            "  {}{}",
            crate::util::electric_green("█".repeat(filled)),
            crate::util::dim("░".repeat(empty))
        )
    };
    Some(format!("{}{}{}", head, bar, crate::util::dim(&tail)))
}

/// a finished download's persistent line, in-line with the in-flight lanes:
/// the ✓ lands exactly where their bars start (same head + lead gap)
fn done_line(name: &str, w: usize) -> String {
    format!(
        "{}  {}",
        format!("  {:<nw$}", name, nw = lane_name_w(w)),
        crate::util::electric_green("✓ downloaded")
    )
}

/// a claimed download lane; the hook closure feeds it chunk progress and
/// dropping it clears the lane for the next queued job. Clone keeps the lane
/// alive past the guard's own scope (the hook closure holds a clone)
#[derive(Clone)]
pub struct LaneGuard {
    lane: Arc<Lane>,
    inner: Arc<Inner>,
}

/// the owned hook shape download tasks hand to the store
pub type LaneHook = Arc<dyn Fn(u64, Option<u64>) + Send + Sync>;

/// the lane decision: only the user's declared surface gets a lane;
/// transitive/internal downloads ride the aggregate bar lane-less
pub fn lane_for(id: &str, direct: &HashSet<String>, progress: &Progress) -> Option<LaneGuard> {
    if direct.contains(id) {
        progress.claim_lane(id)
    } else {
        None
    }
}

impl LaneGuard {
    /// the download finished cleanly: queue its name for a persistent
    /// `✓ downloaded` lane line (the slot itself frees on drop). aborted or
    /// failed downloads never call this, their lane just vanishes
    pub fn complete(&self) {
        let name = self.lane.name.lock().unwrap().clone();
        self.inner.completed.lock().unwrap().push(name);
    }
    /// the download hook for this lane: (bytes done, content-length total)
    pub fn hook(&self) -> impl Fn(u64, Option<u64>) + Send + Sync {
        let lane = self.lane.clone();
        move |done, total| {
            lane.done.store(done, Ordering::Relaxed);
            if let Some(t) = total {
                lane.total.store(t, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for LaneGuard {
    fn drop(&mut self) {
        self.lane.clear();
    }
}

/// install progress bar; inactive (no thread, no output) when stdout isn't a
/// console. finish() tells the thread to clear the line and exit; the Drop
/// backstop joins so error paths leave the terminal clean too
pub struct Progress {
    inner: Option<(Arc<Inner>, mpsc::Sender<()>)>,
    handle: Option<std::thread::JoinHandle<()>>,
    cps: Option<(u32, u32)>, // previous console codepages (input, output)
}

impl Progress {
    /// probe stdout for a console; piped/CI runs get an inert bar. the lane
    /// budget comes from the caller (min(concurrency, 8)) and degrades with
    /// the terminal height
    pub fn start(lanes_wanted: usize) -> Self {
        let (_w, h) = term_size();
        let lanes = lane_budget(lanes_wanted, h);
        Self::start_with(probe(), lanes)
    }

    pub(crate) fn start_with(active: bool, lanes_wanted: usize) -> Self {
        let lanes: Vec<Arc<Lane>> = (0..lanes_wanted).map(|_| Arc::new(Lane::new())).collect();
        if !active {
            return Progress { inner: None, handle: None, cps: None };
        }
        // the codepage switch rides along: both mean a real console is
        // attached; without it the bar glyphs turn into mojibake on legacy
        // conhost (windows terminal always decodes utf-8)
        let cps = term::utf8_codepage();
        let inner = Arc::new(Inner {
            stage: Mutex::new(String::new()),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            lanes,
            completed: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            cleared: AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::channel::<()>();
        let i2 = inner.clone();
        let handle = std::thread::spawn(move || render_loop(i2, rx));
        Progress { inner: Some((inner, tx)), handle: Some(handle), cps }
    }

    /// test/debug introspection: names of lanes that are active right now or
    /// sitting in the completed queue
    #[cfg(test)]
    pub fn lane_history(&self) -> Vec<String> {
        let Some((inner, _)) = &self.inner else { return vec![] };
        let mut v: Vec<String> = inner
            .lanes
            .iter()
            .filter(|l| l.active.load(Ordering::Relaxed))
            .map(|l| l.name.lock().unwrap().clone())
            .collect();
        v.extend(inner.completed.lock().unwrap().iter().cloned());
        v
    }

    /// claim a free download lane for a package; None when the bar is
    /// inactive or every lane is busy (aggregate-only download)
    pub fn claim_lane(&self, name: &str) -> Option<LaneGuard> {
        let (inner, _) = self.inner.as_ref()?;
        let lane = inner.lanes.iter().find(|l| !l.active.load(Ordering::Relaxed))?;
        *lane.name.lock().unwrap() = name.to_string();
        lane.done.store(0, Ordering::Relaxed);
        lane.total.store(0, Ordering::Relaxed);
        lane.active.store(true, Ordering::Relaxed);
        Some(LaneGuard { lane: lane.clone(), inner: Arc::clone(inner) })
    }

    /// move to a new stage: label swap, counters reset, lanes collapsed
    pub fn stage(&self, name: &str) {
        let Some((inner, _)) = &self.inner else { return };
        *inner.stage.lock().unwrap() = name.to_string();
        inner.done.store(0, Ordering::Relaxed);
        inner.total.store(0, Ordering::Relaxed);
        inner.bytes.store(0, Ordering::Relaxed);
        inner.completed.lock().unwrap().clear();
        for l in &inner.lanes {
            l.clear();
        }
    }

    /// set the job counters (absolute values, the download/link loops know
    /// their own totals and running counts)
    pub fn bump_jobs(&self, done: u64, total: u64, bytes: u64) {
        let Some((inner, _)) = &self.inner else { return };
        inner.done.store(done, Ordering::Relaxed);
        inner.total.store(total, Ordering::Relaxed);
        inner.bytes.store(bytes, Ordering::Relaxed);
    }

    /// stop the bar and wait (bounded, ≤ one tick) for the renderer's final
    /// clear, so summary lines that follow can't race a stale draw; the Drop
    /// does the join for error paths
    pub fn finish(&self) {
        let Some((inner, _)) = &self.inner else { return };
        inner.cleared.store(false, Ordering::SeqCst);
        inner.stop.store(true, Ordering::Relaxed);
        for _ in 0..400 {
            if inner.cleared.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        // stop + bounded wait for the clear, restore codepage, join
        self.finish();
        if let Some((cin, cout)) = self.cps.take() {
            #[cfg(windows)]
            term::restore_codepage(cin, cout);
            #[cfg(not(windows))]
            let _ = (cin, cout);
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// the renderer: ~80ms ticks redrawing the block in place; on shutdown the
/// whole block clears exactly once. multi-line redraw discipline: cursor-up
/// over the previous block, rewrite every line, clear rows a SHRUNKEN block
/// left behind (lanes going idle shrink the block)
fn render_loop(inner: Arc<Inner>, rx: mpsc::Receiver<()>) {
    let mut spin = 0usize;
    let mut last_h = 0usize; // previous block height, for the cursor-up math
    loop {
        match rx.recv_timeout(Duration::from_millis(80)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if inner.stop.load(Ordering::Relaxed) {
            break;
        }
        let stage = inner.stage.lock().unwrap();
        if stage.is_empty() {
            // nothing to show yet (a fully cached install starts at linking)
            spin += 1;
            continue;
        }
        let (w, h) = term_size();
        // lanes only run during the downloading stage: finished downloads
        // persist as aligned `✓ downloaded` lines (oldest evicted when the
        // block would overflow), in-flight lanes below them, main bar last
        let lane_lines: Vec<String> = if stage.as_str() == "downloading" {
            let active: Vec<String> =
                inner.lanes.iter().filter_map(|l| lane_line(l, w, spin)).collect();
            let mut completed = inner.completed.lock().unwrap();
            // bound the stored list so long installs can't grow it forever
            if completed.len() > 64 {
                let excess = completed.len() - 64;
                completed.drain(0..excess);
            }
            // visible slice: never let the block scroll the terminal
            let budget = h.saturating_sub(3);
            let vis = completed.len().min(budget.saturating_sub(active.len() + 2));
            let start = completed.len() - vis;
            let mut lines: Vec<String> =
                completed[start..].iter().map(|n| done_line(n, w)).collect();
            lines.extend(active);
            lines
        } else {
            Vec::new()
        };
        let main = bar_line(
            &stage,
            inner.done.load(Ordering::Relaxed),
            inner.total.load(Ordering::Relaxed),
            inner.bytes.load(Ordering::Relaxed),
            w,
            spin,
        );
        let block = compose_block(&lane_lines, &main);
        print!("{}", draw_block(&block, last_h));
        let _ = std::io::stdout().flush();
        last_h = block.len();
        spin += 1;
    }
    print!("{}", clear_block(last_h));
    let _ = std::io::stdout().flush();
    inner.cleared.store(true, Ordering::SeqCst);
}

/// paint the block in place: up over the previous block, rewrite each line,
/// clear the rows a shrunken block left behind. pure (tested)
fn draw_block(block: &[String], last_h: usize) -> String {
    let mut out = String::new();
    if last_h > 1 {
        // cursor sits at the end of the previous block's last line
        out.push_str(&format!("\x1b[{}A\r", last_h - 1));
    } else {
        out.push('\r');
    }
    for (i, line) in block.iter().enumerate() {
        if i > 0 {
            out.push_str("\x1b[1B\r");
        }
        out.push_str(&format!("\x1b[2K{}", line));
    }
    if last_h > block.len() {
        // the block shrank (lanes went idle): wipe the leftover rows, then
        // return the cursor to the new block's last line
        for _ in block.len()..last_h {
            out.push_str("\x1b[1B\x1b[2K");
        }
        out.push_str(&format!("\x1b[{}A", last_h - block.len()));
    }
    out
}

/// erase the whole block; the cursor parks at the block's last row col 1 so
/// the summary lines that follow start clean. pure (tested)
fn clear_block(last_h: usize) -> String {
    let mut out = String::new();
    if last_h > 1 {
        out.push_str(&format!("\x1b[{}A\r", last_h - 1));
    } else {
        out.push('\r');
    }
    for i in 0..last_h {
        out.push_str("\x1b[2K");
        if i + 1 < last_h {
            out.push_str("\x1b[1B");
        }
    }
    out
}

/// does stdout render in place? (a real console behind it), the windows
/// probe mirrors _sel.rs's GetConsoleMode pattern; unix gets std's
/// IsTerminal (no process spawn per check)
fn probe() -> bool {
    #[cfg(windows)]
    {
        term::stdout_is_console()
    }
    #[cfg(not(windows))]
    {
        std::io::IsTerminal::is_terminal(&std::io::stdout())
    }
}

/// terminal size: visible console window (windows) / `stty size` (unix), 80x24
/// when neither answers
pub(crate) fn term_size() -> (usize, usize) {
    if let (Ok(c), Ok(l)) = (std::env::var("COLUMNS"), std::env::var("LINES")) {
        if let (Ok(c), Ok(l)) = (c.trim().parse::<usize>(), l.trim().parse::<usize>()) {
            if plausible_window(c, l) {
                return (c, l);
            }
        }
    }
    #[cfg(windows)]
    {
        if let Some(size) = term::window_size() {
            return size;
        }
        if let Ok(o) = std::process::Command::new("cmd").args(["/d", "/c", "mode con"]).output() {
            let text = String::from_utf8_lossy(&o.stdout).to_string()
                + String::from_utf8_lossy(&o.stderr).as_ref();
            let (mut cols, mut lines) = (None, None);
            for line in text.lines() {
                let mut it = line.split_whitespace();
                while let Some(label) = it.next() {
                    let Some(value) = it.next().and_then(|v| v.parse::<usize>().ok()) else {
                        continue;
                    };
                    let l = label.trim_end_matches(':').to_ascii_lowercase();
                    if l.starts_with("col") {
                        cols = Some(value);
                    } else if l.starts_with("lin") {
                        lines = Some(value);
                    }
                }
            }
            if let (Some(c), Some(l)) = (cols, lines) {
                if plausible_window(c, l) {
                    return (c, l);
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        // stty reads the tty, not our (piped) stdout, so redirect stdin to it
        if let Ok(o) = std::process::Command::new("sh").args(["-c", "stty size < /dev/tty"]).output() {
            let nums: Vec<usize> = String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .filter_map(|x| x.parse().ok())
                .collect();
            // stty prints "rows cols"
            if nums.len() == 2 && plausible_window(nums[1], nums[0]) {
                return (nums[1], nums[0]);
            }
        }
    }
    (80, 24)
}

/// a visible console window is never thousands of rows tall; values beyond
/// this are scrollback-poisoned (`mode con` buffer dims on some boxes) or
/// broken env vars, and layout against them paints below the viewport
pub(crate) fn plausible_window(w: usize, h: usize) -> bool {
    (1..=500).contains(&w) && (1..=200).contains(&h)
}

// console probes: windows goes through kernel32 directly, unix through std
#[cfg(windows)]
mod term {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(nstdhandle: u32) -> *mut c_void;
        fn GetConsoleMode(hconsole: *mut c_void, lpmode: *mut u32) -> i32;
        fn GetConsoleScreenBufferInfo(hconsoleoutput: *mut c_void, lpconsolescreenbufferinfo: *mut ScreenBufferInfo) -> i32;
        fn GetConsoleCP() -> u32;
        fn GetConsoleOutputCP() -> u32;
        fn SetConsoleCP(wcodepageid: u32) -> i32;
        fn SetConsoleOutputCP(wcodepageid: u32) -> i32;
    }

    // COORD/SMALL_RECT/CONSOLE_SCREEN_BUFFER_INFO, field order per wincon.h
    #[repr(C)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }
    #[repr(C)]
    struct ScreenBufferInfo {
        size_x: i16,
        size_y: i16,
        cursor_x: i16,
        cursor_y: i16,
        attrs: u16,
        win: SmallRect,
        max_x: i16,
        max_y: i16,
    }

    const STD_OUTPUT_HANDLE: u32 = (-11i32) as u32;

    /// is stdout a real console (vs piped/redirected)? GetConsoleMode fails
    /// on anything that isn't one, same probe pattern as _sel.rs's stdin
    pub fn stdout_is_console() -> bool {
        unsafe {
            let h = GetStdHandle(STD_OUTPUT_HANDLE);
            if h.is_null() || h == (-1isize) as *mut c_void {
                return false;
            }
            let mut mode = 0u32;
            GetConsoleMode(h, &mut mode) != 0
        }
    }

    /// switch the console to utf-8 so the bar glyphs (● █ ░ spinner) decode
    /// instead of turning into mojibake on legacy conhost; returns the
    /// previous (input, output) codepages for the caller to restore
    pub fn utf8_codepage() -> Option<(u32, u32)> {
        unsafe {
            let h = GetStdHandle(STD_OUTPUT_HANDLE);
            if h.is_null() || h == (-1isize) as *mut c_void {
                return None;
            }
            let (cin, cout) = (GetConsoleCP(), GetConsoleOutputCP());
            if cin == 0 || cout == 0 {
                return None;
            }
            SetConsoleCP(65001);
            SetConsoleOutputCP(65001);
            Some((cin, cout))
        }
    }

    pub fn restore_codepage(cin: u32, cout: u32) {
        unsafe {
            SetConsoleCP(cin);
            SetConsoleOutputCP(cout);
        }
    }

    /// the VISIBLE console window, not the scrollback buffer
    pub fn window_size() -> Option<(usize, usize)> {        unsafe {
            let h = GetStdHandle(STD_OUTPUT_HANDLE);
            if h.is_null() || h == (-1isize) as *mut c_void {
                return None;
            }
            let mut info = ScreenBufferInfo {
                size_x: 0,
                size_y: 0,
                cursor_x: 0,
                cursor_y: 0,
                attrs: 0,
                win: SmallRect { left: 0, top: 0, right: 0, bottom: 0 },
                max_x: 0,
                max_y: 0,
            };
            if GetConsoleScreenBufferInfo(h, &mut info) == 0 {
                return None;
            }
            let w = (info.win.right - info.win.left + 1).max(0) as usize;
            let h = (info.win.bottom - info.win.top + 1).max(0) as usize;
            if w > 0 && h > 0 {
                Some((w, h))
            } else {
                None
            }
        }
    }
}

#[cfg(not(windows))]
mod term {
    /// unix terminals are utf-8 by default: no codepage to switch
    pub fn utf8_codepage() -> Option<(u32, u32)> {
        None
    }

    pub fn restore_codepage(_cin: u32, _cout: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(s: &str) -> String {
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

    #[test]
    fn bar_fill_math() {
        assert_eq!(bar_fill(0, 22, 24), (0, 24));
        assert_eq!(bar_fill(11, 22, 24), (12, 12));
        assert_eq!(bar_fill(22, 22, 24), (24, 0));
        assert_eq!(bar_fill(1, 3, 24), (8, 16));
        // degenerate: no division by zero, empty bar
        assert_eq!(bar_fill(5, 0, 24), (0, 24));
    }

    #[test]
    fn bar_line_composes_the_stages() {
        // quantified: electric dot, bar, counts, humanized bytes, fits 80
        let l = bar_line("downloading", 12, 22, 8_400_000, 80, 0);
        assert!(l.contains("downloading") && l.contains("12/22"));
        assert!(l.contains(&crate::util::human(8_400_000)), "humanized bytes");
        assert!(l.contains('█') && l.contains('░'), "fill + track glyphs");
        let vis = strip(&l).chars().count();
        assert!(vis <= 80, "{} visible chars overflow 80 cols", vis);
        // the bar caps at 24 wide even on huge terminals
        assert!(strip(&l).matches('█').count() + strip(&l).matches('░').count() <= 24);
        // indeterminate: spinner glyph cycles, no counts, no bar
        let l = bar_line("resolving", 0, 0, 0, 80, 3);
        assert!(l.contains(SPINNER[3]), "spinner frame");
        assert!(!l.contains('/') && !l.contains('█'), "indeterminate stage has no counts");
        // linking shows counts but no bytes part when bytes are 0
        let l = bar_line("linking", 7, 22, 0, 80, 0);
        assert!(l.contains("7/22") && !l.contains('('), "no empty bytes parens");
        // tiny terminal: the bar collapses, the line still fits
        let l = bar_line("downloading", 1, 2, 0, 24, 0);
        let vis = strip(&l).chars().count();
        assert!(vis <= 24, "{} visible chars overflow 24 cols: {:?}", vis, strip(&l));
    }

    #[test]
    fn inactive_progress_prints_nothing_and_spawns_nothing() {
        // the probe said pipe: no thread, all methods are no-ops
        let p = Progress::start_with(false, 4);
        assert!(p.handle.is_none(), "no renderer thread when piped");
        p.stage("downloading");
        p.bump_jobs(1, 2, 3);
        // lanes exist but the bar is inert: claim finds nothing to show
        assert!(p.claim_lane("pkg@1.0.0").is_none());
        p.finish();
        assert!(p.handle.is_none());
    }

    #[test]
    fn lane_budget_degrades_with_the_terminal() {
        assert_eq!(lane_budget(16, 30), 8, "hard cap 8");
        assert_eq!(lane_budget(4, 30), 4, "no more lanes than concurrency");
        assert_eq!(lane_budget(16, 6), 3, "6 rows: 3 lanes + sep + bar + spare");
        assert_eq!(lane_budget(16, 4), 1);
        assert_eq!(lane_budget(16, 3), 0, "too short: degrade to the single line");
        assert_eq!(lane_budget(0, 30), 0);
    }

    #[test]
    fn compose_block_tracks_lanes_and_collapses() {
        assert_eq!(compose_block(&[], "bar").len(), 1, "no lanes: just the main bar");
        let b = compose_block(&["l1".into(), "l2".into()], "bar");
        assert_eq!(b, vec!["l1", "l2", "", "bar"], "lanes + separator + main bar last");
    }

    /// a guard wired to a real (inert) Inner, for hook/complete tests
    fn test_guard() -> LaneGuard {
        let inner = Arc::new(Inner {
            stage: Mutex::new(String::new()),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            lanes: vec![Arc::new(Lane::new())],
            completed: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            cleared: AtomicBool::new(false),
        });
        LaneGuard { lane: inner.lanes[0].clone(), inner }
    }

    #[test]
    fn lane_hook_feeds_the_lane_and_drop_clears_it() {
        let g = test_guard();
        let lane = g.lane.clone();
        lane.active.store(true, Ordering::Relaxed);
        let hook = g.hook();
        hook(1024, Some(4096));
        assert_eq!(lane.done.load(Ordering::Relaxed), 1024);
        assert_eq!(lane.total.load(Ordering::Relaxed), 4096);
        // a None total keeps the last known content-length
        hook(2048, None);
        assert_eq!(lane.done.load(Ordering::Relaxed), 2048);
        assert_eq!(lane.total.load(Ordering::Relaxed), 4096);
        assert!(lane.active.load(Ordering::Relaxed), "the lane is live");
        drop(g);
        assert!(!lane.active.load(Ordering::Relaxed), "dropped guard frees the lane");
        assert_eq!(lane.done.load(Ordering::Relaxed), 0, "cleared for the next job");
    }

    #[test]
    fn lane_lines_render_widths_and_collapse() {
        let lane = Arc::new(Lane::new());
        *lane.name.lock().unwrap() = "chalk@4.1.2".to_string();
        lane.active.store(true, Ordering::Relaxed);
        lane.done.store(280, Ordering::Relaxed);
        lane.total.store(412, Ordering::Relaxed);
        let l = lane_line(&lane, 80, 0).unwrap();
        assert!(l.contains("chalk@4.1.2") && l.contains("67%"), "name + percent (280/412)");
        assert!(l.contains('█') && l.contains('░'), "lane bar");
        // lane fills are electric green (vivid, reads as "good"), the
        // aggregate bar keeps electric blue
        assert!(l.contains("\x1b[38;2;0;230;118m█"), "electric-green lane fill");
        assert!(!l.contains("\x1b[38;2;49;86;255m█"), "no electric-blue fill on lanes");
        let vis = strip(&l).chars().count();
        assert!(vis <= 80, "{} visible chars overflow 80", vis);
        // indeterminate lane: spinner instead of bar/percent
        let lane2 = Arc::new(Lane::new());
        *lane2.name.lock().unwrap() = "left-pad@1.3.0".to_string();
        lane2.active.store(true, Ordering::Relaxed);
        let l = lane_line(&lane2, 80, 2).unwrap();
        assert!(l.contains(SPINNER[2]) && !l.contains('%'), "indeterminate lane spins");
        // a 40-char name truncates into the budget on a narrow terminal
        let lane3 = Arc::new(Lane::new());
        *lane3.name.lock().unwrap() = "n".repeat(40);
        lane3.active.store(true, Ordering::Relaxed);
        lane3.total.store(100, Ordering::Relaxed);
        let l = lane_line(&lane3, 40, 0).unwrap();
        let vis = strip(&l).chars().count();
        assert!(vis <= 40, "{} visible chars overflow 40", vis);
        assert!(!strip(&l).contains("n".repeat(30).as_str()), "name truncated to the budget");
        // idle lanes collapse out of the block entirely
        lane3.clear();
        assert!(lane_line(&lane3, 80, 0).is_none());
    }

    #[test]
    fn finished_lanes_render_aligned_downloaded_lines() {
        // the ✓ must start exactly where an in-flight lane's bar starts:
        // head (2 + name cell) + the bar's 2-space lead
        let name = "chalk@4.1.2";
        let done = done_line(name, 80);
        let s = strip(&done);
        assert!(s.contains("✓ downloaded"), "finished line says downloaded");
        assert!(!s.contains('█') && !s.contains('░'), "no bar on a finished lane");
        // alignment: same name cell as lane_line, then the same 2-space lead
        let inflight = {
            let lane = Arc::new(Lane::new());
            *lane.name.lock().unwrap() = name.to_string();
            lane.active.store(true, Ordering::Relaxed);
            lane.total.store(412, Ordering::Relaxed);
            lane.done.store(280, Ordering::Relaxed);
            lane_line(&lane, 80, 0).unwrap()
        };
        let bar_col = strip(&inflight).find('█').unwrap();
        let check_col = strip(&done).find('✓').unwrap();
        assert_eq!(bar_col, check_col, "finished line aligns with the bars");
        assert!(done.contains("\x1b[38;2;0;230;118m"), "electric green");
    }

    #[test]
    fn completed_queue_window_evicts_the_oldest() {
        let (_w, h): (usize, usize) = (80, 12);
        let budget = h.saturating_sub(3);
        // 40 finished + 1 in-flight: visible finished slice = budget - active(1) - 2
        let completed: Vec<String> = (0..40).map(|i| format!("pkg@1.0.{} ", i)).collect();
        let active_len = 1usize;
        let vis = completed.len().min(budget.saturating_sub(active_len + 2));
        assert_eq!(vis, budget - 3, "window math");
        let start = completed.len() - vis;
        // the newest stay, the oldest fall off
        assert!(completed[start..].last().unwrap().contains(&format!("pkg@1.0.{}", 39)));
        assert!(!completed[start..].first().unwrap().contains("pkg@1.0.0"));
    }

    #[test]
    fn block_draw_and_clear_track_the_previous_height() {
        let block = vec!["a".to_string(), String::new(), "b".to_string()];
        // first paint: no cursor-up, just carriage return + the lines
        let s = draw_block(&block, 0);
        assert!(s.starts_with('\r') && !s.contains('A'), "first paint has no cursor-up");
        // same-height repaint: cursor-up the previous block, redraw
        let s = draw_block(&block, 3);
        assert!(s.starts_with("\x1b[2A\r"), "cursor-up over the previous 3-line block");
        // shrunken block: leftover rows cleared, cursor returns to the new last row
        let s = draw_block(&block[..1], 3);
        assert!(s.contains("\x1b[1B\x1b[2K"), "leftover rows wiped");
        assert!(s.ends_with("\x1b[2A"), "cursor back on the new last row");
        // full clear: every line erased, cursor parked on the last row
        let s = clear_block(3);
        let ups = s.matches('A').count();
        assert_eq!(s.matches("\x1b[2K").count(), 3, "every line cleared");
        assert_eq!(ups, 1, "one cursor-up to the block top");
        assert!(!s.ends_with('B'), "cursor parked on the last row, not below it");
    }

    #[test]
    fn window_sanity_clamp() {
        assert!(plausible_window(80, 24));
        assert!(plausible_window(500, 200));
        assert!(!plausible_window(120, 9001)); // mode con scrollback poison
        assert!(!plausible_window(0, 24));
    }
}
