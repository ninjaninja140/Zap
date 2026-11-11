// private helper behind `zap outdated --interactive`: a raw-mode single-list
// selector on the alternate screen (yarn-upgrade-interactive style). the
// terminal guard, key parser and platform readers are copied from
// src/cmds/run/_multirun.rs
// shortcut: duplicates _multirun's terminal machinery instead of refactoring
// its internals apart; merge into a shared module if a third ui shows up

use anyhow::Result;
use std::io::Write;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use super::Row;
use crate::util::{bold, dim, electric_bold};

/// selector tick: also the resize-poll cadence
const TICK: Duration = Duration::from_millis(150);

/// old/new terminal size → geometry must be recomputed and the screen
/// repainted (pure, so the resize decision is testable)
fn needs_relayout(old: (usize, usize), new: (usize, usize)) -> bool {
    old != new
}

/// poll the size every tick on windows (a direct console query); unix shells
/// out to `stty` per poll, so there it's every 4th tick
fn size_poll_tick(tick: u32) -> bool {
    cfg!(windows) || tick.is_multiple_of(4)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Key {
    Up,
    Down,
    Left,
    Right,
    All,      // a: set every row to latest
    AllMajor, // m: set every row to latest-of-major
    Enter,
    Cancel, // q or a bare esc
    CtrlC,
}

/// incremental key decoder shared by the platform readers: arrows arrive as
/// ESC[A/B/C/D, ctrl+c as the raw 0x03 byte, q and a lone esc cancel
fn parse_byte(state: &mut u8, b: u8) -> Option<Key> {
    match (*state, b) {
        (_, 0x03) => {
            *state = 0;
            Some(Key::CtrlC)
        }
        // enter, as cr or lf, in any state
        (_, 0x0d) | (_, 0x0a) => {
            *state = 0;
            Some(Key::Enter)
        }
        (0, b'a') | (0, b'A') => {
            *state = 0;
            Some(Key::All)
        }
        (0, b'm') | (0, b'M') => {
            *state = 0;
            Some(Key::AllMajor)
        }
        (0, b'q') | (0, b'Q') => {
            *state = 0;
            Some(Key::Cancel)
        }
        (0, 0x1b) => {
            *state = 1;
            None
        }
        (1, b'[') => {
            *state = 2;
            None
        }
        // no csi introducer after the esc: a bare esc press, cancel
        (1, _) => {
            *state = 0;
            Some(Key::Cancel)
        }
        (2, b'A') => {
            *state = 0;
            Some(Key::Up)
        }
        (2, b'B') => {
            *state = 0;
            Some(Key::Down)
        }
        (2, b'C') => {
            *state = 0;
            Some(Key::Right)
        }
        (2, b'D') => {
            *state = 0;
            Some(Key::Left)
        }
        _ => {
            *state = 0;
            None
        }
    }
}

/// per-row upgrade target: stay on the current version (the default), bump
/// within the current major, or jump to the absolute latest
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Current,
    Major,
    Latest,
}

impl Target {
    /// right arrow cycles current → major → latest → current
    fn next(self) -> Self {
        match self {
            Target::Current => Target::Major,
            Target::Major => Target::Latest,
            Target::Latest => Target::Current,
        }
    }

    /// left arrow reverses
    fn prev(self) -> Self {
        match self {
            Target::Current => Target::Latest,
            Target::Latest => Target::Major,
            Target::Major => Target::Current,
        }
    }
}

/// selection list state: cursor row + per-row target choice
struct Sel {
    cursor: usize,
    targets: Vec<Target>,
}

impl Sel {
    fn new(n: usize) -> Self {
        Sel { cursor: 0, targets: vec![Target::Current; n] }
    }

    /// move the cursor, clamped to the list (no wraparound)
    fn move_by(&mut self, d: isize) {
        let n = self.targets.len();
        if n == 0 {
            return;
        }
        self.cursor = (self.cursor as isize + d).clamp(0, n as isize - 1) as usize;
    }

    /// cycle the cursor row's target; d = 1 is right, -1 is left. when the
    /// major cell is a no-op (latest-of-major equals current or is unknown)
    /// it's stepped over so the highlight never advertises a change that
    /// wouldn't happen
    fn cycle(&mut self, d: isize, major_selectable: bool) {
        let Some(t) = self.targets.get_mut(self.cursor) else {
            return;
        };
        *t = if d >= 0 { t.next() } else { t.prev() };
        if !major_selectable && *t == Target::Major {
            *t = if d >= 0 { t.next() } else { t.prev() };
        }
    }

    /// `a` / `m`: set every row to one target
    fn set_all(&mut self, t: Target) {
        for x in self.targets.iter_mut() {
            *x = t;
        }
    }

    /// (rows set to major, rows set to latest) for the footer
    fn counts(&self) -> (usize, usize) {
        (
            self.targets.iter().filter(|t| **t == Target::Major).count(),
            self.targets.iter().filter(|t| **t == Target::Latest).count(),
        )
    }
}

/// what leaving the loop means: none = cancel or enter with everything
/// staying on current
fn apply(s: &Sel, cancelled: bool) -> Option<Vec<Target>> {
    if cancelled {
        return None;
    }
    if s.targets.iter().all(|t| *t == Target::Current) {
        return None;
    }
    Some(s.targets.clone())
}

/// whether row i's major target would change anything (a stable release
/// exists in the current major and isn't the current version itself)
fn major_pickable(rows: &[Row], i: usize) -> bool {
    rows.get(i)
        .map(|r| !r.latest_major.is_empty() && r.latest_major != r.current)
        .unwrap_or(false)
}

/// first visible list row so the cursor stays inside the viewport
fn window(cursor: usize, n: usize, view: usize) -> usize {
    if view == 0 || n <= view {
        return 0;
    }
    cursor.saturating_sub(view - 1).min(n - view)
}

/// compose the whole selector screen: full clear + redraw per keypress is
/// plenty for a list this size and needs no dirty tracking; every row is
/// built from fixed-width columns so all rows share one exact width and
/// nothing inside the list ever needs ESC[2K
// shortcut: only a single version string wider than the terminal still wraps
// the frame (the name always gets the remaining budget), not a real scenario;
// a resize mid-session keeps the old size until a key arrives
fn frame(rows: &[Row], s: &Sel, w: usize, h: usize, heading: &str) -> String {
    let mut out = String::from("\x1b[H\x1b[2J");
    // the pre-styled heading block goes in verbatim as the topmost lines, so
    // the command context stays visible during the interaction; everything
    // below shifts down by its line count + one separator blank
    let head_n = heading.lines().count();
    for (i, line) in heading.lines().enumerate() {
        out.push_str(&format!("\x1b[{};1H{}", i + 1, line));
    }
    let title_row = head_n + 2;
    let header_row = head_n + 4;
    let list_start = head_n + 5;
    let view = h.saturating_sub(head_n + 6).max(1); // heading + blank + title + blank + header + gap + footer
    out.push_str(&format!("\x1b[{};1H{}", title_row, electric_bold("pick upgrade targets")));
    let off = window(s.cursor, rows.len(), view);
    // column widths: the header labels set the floor so header and data share
    // one skeleton, then the name gets what's left of the terminal
    let w_cur = rows.iter().map(|r| r.current.chars().count()).max().unwrap_or(0).max("current".len());
    let w_maj = rows.iter().map(|r| r.latest_major.chars().count()).max().unwrap_or(0).max("major".len()).max(1);
    let w_lat = rows.iter().map(|r| r.latest.chars().count()).max().unwrap_or(0).max("latest".len()).max(1);
    // marker+space + the three gaps + the three version columns
    let overhead = 2 + 2 + 2 + 2 + w_cur + w_maj + w_lat;
    let budget = w.saturating_sub(overhead).max("package".len());
    let w_name = rows.iter().map(|r| r.name.chars().count()).max().unwrap_or(0).max("package".len()).min(budget);
    let gap = " ".repeat(2);
    // dim header above the list so the three columns read as a table; same
    // skeleton as the rows (the name cell carries the gutter, gaps only
    // between the version columns)
    out.push_str(&format!(
        "\x1b[{};1H{}",
        header_row,
        dim(format!(
            "  {:<nw$}{:<cw$}{}{:<mw$}{}{:<lw$}",
            "package",
            "current",
            gap,
            "major",
            gap,
            "latest",
            nw = w_name + 2,
            cw = w_cur,
            mw = w_maj,
            lw = w_lat
        ))
    ));
    let mut shown = 0usize;
    for (i, r) in rows.iter().enumerate().skip(off).take(view) {
        let cursor_row = i == s.cursor;
        let base = if cursor_row { Cell::Plain } else { Cell::Dim };
        // every row always highlights exactly one cell: the chosen target
        // (default = the current cell), so "stay as-is" is a visible choice
        let current_style = if s.targets[i] == Target::Current { Cell::Pick } else { base };
        // the major cell is a no-op when nothing stable shares the current
        // major or the current version already IS it: keep it dim even on
        // the cursor row so the user sees it won't move
        let major_noop = !major_pickable(rows, i);
        let major_style = if s.targets[i] == Target::Major {
            Cell::Pick
        } else if major_noop {
            Cell::Dim
        } else {
            base
        };
        let latest_style = if s.targets[i] == Target::Latest { Cell::Pick } else { base };
        let major_text = if r.latest_major.is_empty() { "-" } else { &r.latest_major };
        let name: String = r.name.chars().take(w_name).collect();
        let marker = if cursor_row { electric_bold("❯") } else { " ".to_string() };
        let line = format!(
            "{} {}{}{}{}{}{}",
            marker,
            cell(&name, w_name + 2, base),
            cell(&r.current, w_cur, current_style),
            gap,
            cell(major_text, w_maj, major_style),
            gap,
            cell(&r.latest, w_lat, latest_style),
        );
        out.push_str(&format!("\x1b[{};1H{}", list_start + shown, line));
        shown += 1;
    }
    let (majors, latests) = s.counts();
    // counts are the payload: bold them, the hints stay dim
    let hint = "←→: target · up/down: move · enter: apply · q: cancel · ";
    let counts = format!("{} major · {} latest", majors, latests);
    // truncate the PLAIN text before styling: styled truncation would split
    // escape codes (same clamp discipline as the pane runner)
    let shown_foot: String = format!("{}{}", hint, counts).chars().take(w).collect();
    let split = shown_foot.chars().count().min(hint.chars().count());
    let (h, c) = (
        shown_foot.chars().take(split).collect::<String>(),
        shown_foot.chars().skip(split).collect::<String>(),
    );
    let foot = format!("{}{}", dim(&h), bold(&c));
    out.push_str(&format!("\x1b[{};1H{}", list_start + shown + 1, foot));
    out
}

/// how one column cell renders
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cell {
    Plain,
    Dim,
    Pick, // inverse video: the cursor row's chosen target
}

/// one padded column cell; the pad happens before styling so inverse video
/// covers the whole cell and every row keeps the same visible width
fn cell(text: &str, width: usize, style: Cell) -> String {
    let padded = format!("{:<w$}", text, w = width);
    match style {
        Cell::Plain => padded,
        Cell::Dim => dim(&padded),
        Cell::Pick => format!("\x1b[7m{}\x1b[0m", padded),
    }
}

fn repaint(rows: &[Row], s: &Sel, w: usize, h: usize, heading: &str) {
    let mut out = frame(rows, s, w, h, heading);
    out.push_str("\x1b[H"); // park the cursor so nothing scrolls
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
}

/// run the selector; Ok(None) = cancelled (q, esc, ctrl+c, or enter with
/// everything kept), Ok(Some(targets)) = per-row target choice into `rows`
pub fn select(rows: &[Row], heading: &str) -> Result<Option<Vec<Target>>> {
    let (mut w, mut h) = term_size();
    let guard = TerminalGuard::new();
    if !guard.raw_active() {
        // piped stdin: bail before anything is painted; the idle guard
        // restores nothing and writes no escapes
        anyhow::bail!("interactive mode needs a terminal");
    }
    let (tx, rx) = mpsc::channel::<Key>();
    term::spawn_key_thread(tx);

    let mut s = Sel::new(rows.len());
    let mut out = String::from("\x1b[?1049h\x1b[?25l"); // alt screen, hide cursor
    out.push_str(&frame(rows, &s, w, h, heading));
    let mut stdout = std::io::stdout();
    stdout.write_all(out.as_bytes())?;
    stdout.flush()?;

    let mut cancelled = false;
    let mut dirty = false; // a key or a resize changed what's on screen
    let mut tick = 0u32;
    loop {
        match rx.recv_timeout(TICK) {
            Ok(Key::Up) => {
                s.move_by(-1);
                dirty = true;
            }
            Ok(Key::Down) => {
                s.move_by(1);
                dirty = true;
            }
            Ok(Key::Left) => {
                s.cycle(-1, major_pickable(rows, s.cursor));
                dirty = true;
            }
            Ok(Key::Right) => {
                s.cycle(1, major_pickable(rows, s.cursor));
                dirty = true;
            }
            Ok(Key::All) => {
                s.set_all(Target::Latest);
                dirty = true;
            }
            Ok(Key::AllMajor) => {
                s.set_all(Target::Major);
                dirty = true;
            }
            Ok(Key::Enter) => break,
            Ok(Key::Cancel) | Ok(Key::CtrlC) | Err(RecvTimeoutError::Disconnected) => {
                cancelled = true;
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        // resize poll between repaints
        // shortcut: detection is a 150ms poll, not SIGWINCH, the std-only
        // cross-platform answer; SIGWINCH would be the unix-native way
        tick += 1;
        if poll_resize(&mut w, &mut h, tick) {
            dirty = true; // window() and row widths recompute inside frame()
        }
        if dirty {
            repaint(rows, &s, w, h, heading);
            dirty = false;
        }
    }
    drop(guard); // restore raw mode + leave the alt screen before more output
    Ok(apply(&s, cancelled))
}

/// the per-tick resize poll shared by both loops: true when the terminal
/// changed size (callers recompute geometry and repaint)
fn poll_resize(w: &mut usize, h: &mut usize, tick: u32) -> bool {
    if !size_poll_tick(tick) {
        return false;
    }
    let (nw, nh) = term_size();
    if needs_relayout((*w, *h), (nw, nh)) {
        *w = nw;
        *h = nh;
        return true;
    }
    false
}

/// cheap stdin probe for flows that would fetch network data before their
/// first picker opens: bail on piped stdin before wasting the fetches. the
/// guard drops immediately, raw mode is restored, nothing is read
pub fn console_available() -> bool {
    TerminalGuard::new().raw_active()
}

/// all registry versions of one package, newest first; unparseable keys are
/// dropped (packument keys are version strings in practice) (pure, tested)
pub fn sort_versions(keys: Vec<String>) -> Vec<String> {
    let mut vs: Vec<(semver::Version, String)> =
        keys.into_iter().filter_map(|k| semver::Version::parse(&k).ok().map(|v| (v, k))).collect();
    vs.sort_by(|a, b| b.0.cmp(&a.0));
    vs.into_iter().map(|(_, k)| k).collect()
}

/// where the picker cursor opens: the newest stable release, 0 when the list
/// is all prereleases (pure, tested)
pub fn cursor_start(versions: &[String]) -> usize {
    versions
        .iter()
        .position(|v| semver::Version::parse(v).map(|p| p.pre.is_empty()).unwrap_or(false))
        .unwrap_or(0)
}

/// clamp a cursor step into 0..len (free fn so the picker state stays dumb)
fn clamp_move(cursor: usize, d: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    (cursor as isize + d).clamp(0, len as isize - 1) as usize
}

/// single-select version picker behind `zap upgrade -i <pkg>`: newest-first
/// list, the cursor opens on the newest stable, prereleases render dim (still
/// selectable); enter picks, q/esc/ctrl+c cancels
pub fn pick_version(name: &str, versions: &[String], heading: &str) -> Result<Option<String>> {
    let (mut w, mut h) = term_size();
    let guard = TerminalGuard::new();
    if !guard.raw_active() {
        // piped stdin: bail before anything is painted; the idle guard
        // restores nothing and writes no escapes
        anyhow::bail!("interactive mode needs a terminal");
    }
    let (tx, rx) = mpsc::channel::<Key>();
    term::spawn_key_thread(tx);

    let mut cursor = cursor_start(versions);
    let mut out = String::from("\x1b[?1049h\x1b[?25l"); // alt screen, hide cursor
    out.push_str(&vframe(name, versions, cursor, w, h, heading));
    let mut stdout = std::io::stdout();
    stdout.write_all(out.as_bytes())?;
    stdout.flush()?;

    let mut cancelled = false;
    let mut dirty = false;
    let mut tick = 0u32;
    loop {
        match rx.recv_timeout(TICK) {
            Ok(Key::Up) => {
                cursor = clamp_move(cursor, -1, versions.len());
                dirty = true;
            }
            Ok(Key::Down) => {
                cursor = clamp_move(cursor, 1, versions.len());
                dirty = true;
            }
            Ok(Key::Enter) => break,
            Ok(Key::Cancel) | Ok(Key::CtrlC) | Err(RecvTimeoutError::Disconnected) => {
                cancelled = true;
                break;
            }
            _ => {} // left/right/a/m belong to the target selector; no-ops here
        }
        tick += 1;
        if poll_resize(&mut w, &mut h, tick) {
            dirty = true;
        }
        if dirty {
            vrepaint(name, versions, cursor, w, h, heading);
            dirty = false;
        }
    }
    drop(guard); // restore raw mode + leave the alt screen before more output
    Ok(if cancelled { None } else { versions.get(cursor).cloned() })
}

/// compose the version picker screen: full clear + redraw per keypress,
/// fixed one-column rows so no ESC[2K is ever needed (same discipline as
/// frame()); the pre-styled heading block rides on top; prereleases stay dim
/// even under the cursor so they read as a different tier
fn vframe(name: &str, versions: &[String], cursor: usize, w: usize, h: usize, heading: &str) -> String {
    let mut out = String::from("\x1b[H\x1b[2J");
    let head_n = heading.lines().count();
    for (i, line) in heading.lines().enumerate() {
        out.push_str(&format!("\x1b[{};1H{}", i + 1, line));
    }
    out.push_str(&format!("\x1b[{};1H{}", head_n + 2, electric_bold(format!("pick a version for `{}`", name))));
    let view = h.saturating_sub(head_n + 5).max(1); // heading + blank + title + blank + gap + footer
    let list_start = head_n + 4;
    let off = window(cursor, versions.len(), view);
    let w_ver = versions.iter().map(|v| v.chars().count()).max().unwrap_or(0).min(w.saturating_sub(8).max(4));
    let mut shown = 0usize;
    for (i, v) in versions.iter().enumerate().skip(off).take(view) {
        let cursor_row = i == cursor;
        let pre = !semver::Version::parse(v).map(|p| p.pre.is_empty()).unwrap_or(false);
        let marker = if cursor_row { electric_bold("❯") } else { " ".to_string() };
        let text = format!("{:<vw$}", v.chars().take(w_ver).collect::<String>(), vw = w_ver);
        let body = if cursor_row && !pre { text } else { dim(&text) };
        out.push_str(&format!("\x1b[{};1H{} {}", list_start + shown, marker, body));
        shown += 1;
    }
    let foot = format!("up/down: move · enter: pick · q: cancel · {}/{}", cursor + 1, versions.len());
    // plain-first truncation so a tiny terminal can't wrap the repaint
    let shown_foot: String = foot.chars().take(w).collect();
    out.push_str(&format!("\x1b[{};1H{}", list_start + shown + 1, dim(&shown_foot)));
    out
}

fn vrepaint(name: &str, versions: &[String], cursor: usize, w: usize, h: usize, heading: &str) {
    let mut out = vframe(name, versions, cursor, w, h, heading);
    out.push_str("\x1b[H"); // park the cursor so nothing scrolls
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
}

/// terminal size: visible console window (windows) / `stty size` (unix), 80x24
/// when neither answers
fn term_size() -> (usize, usize) {
    #[cfg(windows)]
    {
        if let Some(s) = term::window_size() {
            return s;
        }
    }
    #[cfg(not(windows))]
    {
        use std::process::{Command, Stdio};
        if let Ok(o) = Command::new("stty").arg("size").stdin(Stdio::inherit()).output() {
            let nums: Vec<usize> = String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .filter_map(|x| x.parse().ok())
                .collect();
            // stty prints "rows cols"
            if nums.len() == 2 && nums[0] > 0 && nums[1] > 0 {
                return (nums[1], nums[0]);
            }
        }
    }
    (80, 24)
}

/// owns raw mode + the utf-8 codepage; restores both on drop so every exit
/// path (normal end, ctrl+c, panic unwinding) leaves the terminal sane
struct TerminalGuard {
    saved: Option<term::Saved>,
    cps: Option<(u32, u32)>, // previous console codepages (input, output)
}

impl TerminalGuard {
    fn new() -> Self {
        // the codepage switch rides along with raw mode: both mean a real
        // console is attached; piped runs skip it (nothing visible changes)
        let saved = term::enable_raw();
        let cps = if saved.is_some() { term::utf8_codepage() } else { None };
        TerminalGuard { saved, cps }
    }

    fn raw_active(&self) -> bool {
        self.saved.is_some()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Some((cin, cout)) = self.cps.take() {
            term::restore_codepage(cin, cout);
        }
        if let Some(s) = self.saved.take() {
            s.restore();
            // show the cursor and leave the alternate screen, but only when we
            // actually took the console over: the non-tty error path must not
            // write escapes into a pipe
            let _ = std::io::stdout().write_all(b"\x1b[?25h\x1b[?1049l");
            let _ = std::io::stdout().flush();
        }
    }
}

// raw console input: windows goes through kernel32 directly, unix through stty
#[cfg(windows)]
mod term {
    use super::Key;
    use std::ffi::c_void;
    use std::sync::mpsc;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(nstdhandle: u32) -> *mut c_void;
        fn GetConsoleMode(hconsole: *mut c_void, lpmode: *mut u32) -> i32;
        fn SetConsoleMode(hconsole: *mut c_void, dwmode: u32) -> i32;
        fn ReadFile(hfile: *mut c_void, lpbuffer: *mut c_void, nnumberofbytes: u32, lpnumberofbytes: *mut u32, lpoverlapped: *mut c_void) -> i32;
        fn GetConsoleScreenBufferInfo(hconsoleoutput: *mut c_void, lpconsolescreenbufferinfo: *mut ScreenBufferInfo) -> i32;
        fn GetConsoleCP() -> u32;
        fn GetConsoleOutputCP() -> u32;
        fn SetConsoleCP(wcodepageid: u32) -> i32;
        fn SetConsoleOutputCP(wcodepageid: u32) -> i32;
    }

    const STD_INPUT_HANDLE: u32 = (-10i32) as u32;
    // line input, echo and processed input off (ctrl+c becomes a byte),
    // virtual terminal input on so arrows arrive as ESC[A / ESC[B
    const COOKED_MASK: u32 = 0x0001 | 0x0002 | 0x0004;
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

    // a raw console handle is just an integer, safe to move across threads
    #[repr(transparent)]
    struct Handle(*mut c_void);
    unsafe impl Send for Handle {}

    pub struct Saved {
        handle: Handle,
        mode: u32,
    }

    /// take over console input; None when stdin is not a console (piped)
    pub fn enable_raw() -> Option<Saved> {
        unsafe {
            let h = GetStdHandle(STD_INPUT_HANDLE);
            if h.is_null() || h == (-1isize) as *mut c_void {
                return None;
            }
            let mut mode = 0u32;
            if GetConsoleMode(h, &mut mode) == 0 {
                return None;
            }
            if SetConsoleMode(h, (mode & !COOKED_MASK) | ENABLE_VIRTUAL_TERMINAL_INPUT) == 0 {
                return None;
            }
            Some(Saved { handle: Handle(h), mode })
        }
    }

    impl Saved {
        pub fn restore(&self) {
            unsafe {
                SetConsoleMode(self.handle.0, self.mode);
            }
        }
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

    /// the VISIBLE console window, not the scrollback buffer
    pub fn window_size() -> Option<(usize, usize)> {
        const STD_OUTPUT_HANDLE: u32 = (-11i32) as u32;
        unsafe {
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

    /// switch the console to utf-8 so the box-drawing glyphs render instead of
    /// turning into mojibake on legacy conhost; returns the previous (input,
    /// output) codepages for the guard to restore
    pub fn utf8_codepage() -> Option<(u32, u32)> {
        unsafe {
            let (cin, cout) = (GetConsoleCP(), GetConsoleOutputCP());
            if cin == 0 || cout == 0 {
                return None; // no console behind these handles
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

    /// background reader turning console bytes into keys
    pub fn spawn_key_thread(tx: mpsc::Sender<Key>) {
        let h = Handle(unsafe { GetStdHandle(STD_INPUT_HANDLE) });
        std::thread::spawn(move || read_loop(h, tx));
    }

    // separate fn so the closure moves the whole Handle (edition 2021 field
    // capture would otherwise grab the raw *mut c_void field, which isn't Send)
    fn read_loop(h: Handle, tx: mpsc::Sender<Key>) {
        let mut buf = [0u8; 32];
        let mut state = 0u8;
        loop {
            let mut got = 0u32;
            let ok = unsafe {
                ReadFile(h.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut got, std::ptr::null_mut())
            };
            if ok == 0 || got == 0 {
                break;
            }
            for &b in &buf[..got as usize] {
                if let Some(k) = super::parse_byte(&mut state, b) {
                    if tx.send(k).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod term {
    use super::Key;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;

    /// unix terminals are utf-8 by default, there is no codepage to switch
    pub fn utf8_codepage() -> Option<(u32, u32)> {
        None
    }

    pub fn restore_codepage(_cin: u32, _cout: u32) {}

    pub struct Saved(String);

    /// take over the tty via stty; None when stdin is not a tty (piped)
    pub fn enable_raw() -> Option<Saved> {
        let out = Command::new("stty").arg("-g").stdin(Stdio::inherit()).output().ok()?;
        let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() || state.is_empty() {
            return None;
        }
        let ok = Command::new("stty")
            .args(["raw", "-echo"])
            .stdin(Stdio::inherit())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return None;
        }
        Some(Saved(state))
    }

    impl Saved {
        pub fn restore(&self) {
            let _ = Command::new("stty").arg(&self.0).stdin(Stdio::inherit()).status();
        }
    }

    /// background reader turning tty bytes into keys
    pub fn spawn_key_thread(tx: mpsc::Sender<Key>) {
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            let mut lock = stdin.lock();
            let mut buf = [0u8; 32];
            let mut state = 0u8;
            loop {
                match lock.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(k) => {
                        for &b in &buf[..k] {
                            if let Some(key) = super::parse_byte(&mut state, b) {
                                if tx.send(key).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a stand-in pre-styled heading block (the real one comes from
    /// crate::heading_block; width math only cares about the line count)
    const HEAD: &str = "h1\nh2\nh3\nh4";

    fn row(name: &str, cur: &str, major: &str, latest: &str) -> Row {
        Row {
            source: String::new(),
            name: name.to_string(),
            current: cur.to_string(),
            wanted: String::new(),
            latest_major: major.to_string(),
            latest: latest.to_string(),
        }
    }

    #[test]
    fn cursor_moves_and_clamps() {
        let mut s = Sel::new(3);
        s.move_by(-1);
        assert_eq!(s.cursor, 0);
        s.move_by(1);
        assert_eq!(s.cursor, 1);
        s.move_by(99);
        assert_eq!(s.cursor, 2);
        s.move_by(-99);
        assert_eq!(s.cursor, 0);
        // an empty list must not panic or move
        let mut e = Sel::new(0);
        e.move_by(1);
        assert_eq!(e.cursor, 0);
        e.cycle(1, true);
        e.set_all(Target::Latest);
        assert_eq!(e.counts(), (0, 0));
    }

    #[test]
    fn target_cycles_forward_and_back() {
        let mut s = Sel::new(2);
        assert_eq!(s.targets[0], Target::Current);
        s.cycle(1, true); // right: current → major
        assert_eq!(s.targets[0], Target::Major);
        s.cycle(1, true); // right: major → latest
        assert_eq!(s.targets[0], Target::Latest);
        s.cycle(1, true); // right: latest → current
        assert_eq!(s.targets[0], Target::Current);
        s.cycle(-1, true); // left: current → latest
        assert_eq!(s.targets[0], Target::Latest);
        s.cycle(-1, true); // left: latest → major
        assert_eq!(s.targets[0], Target::Major);
        s.cycle(-1, true); // left: major → current
        assert_eq!(s.targets[0], Target::Current);
        // only the cursor row moves
        s.move_by(1);
        s.cycle(1, true);
        assert_eq!(s.targets[0], Target::Current);
        assert_eq!(s.targets[1], Target::Major);
    }

    #[test]
    fn set_all_targets_every_row() {
        let mut s = Sel::new(3);
        s.cycle(1, true); // cursor row to major
        s.set_all(Target::Latest);
        assert_eq!(s.targets, vec![Target::Latest; 3]);
        assert_eq!(s.counts(), (0, 3));
        s.set_all(Target::Major);
        assert_eq!(s.targets, vec![Target::Major; 3]);
        assert_eq!(s.counts(), (3, 0));
    }

    #[test]
    fn enter_with_everything_on_current_cancels() {
        let mut s = Sel::new(2);
        assert_eq!(apply(&s, false), None);
        s.cycle(1, true); // one row set to major
        assert_eq!(apply(&s, false), Some(vec![Target::Major, Target::Current]));
        // ctrl+c / q count as cancel even with targets pending
        s.set_all(Target::Latest);
        assert_eq!(apply(&s, true), None);
    }

    #[test]
    fn key_parse_covers_the_selector_keys() {
        for (bytes, want) in [
            (vec![0x1b, b'[', b'A'], Some(Key::Up)),
            (vec![0x1b, b'[', b'B'], Some(Key::Down)),
            (vec![0x1b, b'[', b'C'], Some(Key::Right)),
            (vec![0x1b, b'[', b'D'], Some(Key::Left)),
            (vec![b'a'], Some(Key::All)),
            (vec![b'm'], Some(Key::AllMajor)),
            (vec![b'q'], Some(Key::Cancel)),
            (vec![0x0d], Some(Key::Enter)),
            (vec![0x03], Some(Key::CtrlC)),
            (vec![0x1b, b'x'], Some(Key::Cancel)), // bare esc then any byte
            (vec![0x1b, b'[', b'Z'], None),  // unknown csi final: dropped
            (vec![b' '], None),              // space: no-op in the target model
        ] {
            let mut st = 0u8;
            let mut got = None;
            for b in bytes {
                if let Some(k) = parse_byte(&mut st, b) {
                    got = Some(k);
                }
            }
            assert_eq!(got, want);
        }
    }

    #[test]
    fn frame_highlights_one_cell_per_row() {
        let rows = vec![
            row("ms", "2.0.0", "2.1.3", "2.1.3"),
            row("chalk", "2.4.1", "2.5.6", "6.0.1"),
            row("left-pad", "1.3.0", "1.3.0", "1.3.0"),
        ];
        let mut s = Sel::new(3);
        s.cycle(1, true); // row 0 target = major
        s.cycle(1, true); // row 0 target = latest
        s.move_by(1);
        s.cycle(1, true); // row 1 target = major
        s.move_by(1); // cursor on row 2, still on the default (current)
        let f = frame(&rows, &s, 80, 24, HEAD);
        assert!(!f.contains("\x1b[2K"), "no line erases: rows overwrite by exact width");
        assert!(f.contains("❯"), "cursor marker present");
        // the heading block rides on top, pre-styled and untouched
        assert!(f.contains("h1") && f.contains("h4"), "heading lines at the top");
        // row 0's latest cell is the highlighted (inverse) one; the cell pad
        // follows the label floor ("latest".len() = 6)
        let pick_latest = format!("\x1b[7m{:<w$}", "2.1.3", w = "latest".len());
        assert!(f.contains(&pick_latest), "chosen latest cell renders inverse video");
        // row 1's major cell is highlighted on a dim row
        assert!(f.contains("chalk"), "rows render");
        // row 2 (left-pad) sits on the default: its CURRENT cell is the
        // highlighted one ("current".len() = 7 pad), so "stay as-is" is visible
        let pick_current = format!("\x1b[7m{:<w$}", "1.3.0", w = "current".len());
        assert!(f.contains(&pick_current), "default rows highlight the current cell");
        // row 2 has latest_major == current: its major cell stays dim, no-op
        let noop = dim(&format!("{:<w$}", "1.3.0", w = "1.3.0".len()));
        assert!(f.contains(&noop), "no-op major cell renders dim");
        assert!(f.contains("←→: target · up/down: move · enter: apply · q: cancel"));
        assert!(f.contains(&bold("1 major · 1 latest")), "footer counts are the payload");
        // header and rows share one skeleton: identical column starts and
        // identical line width; heading lines + footer are their own widths
        let ws = line_widths(&f);
        assert_eq!(&ws[..4], &[2, 2, 2, 2], "the four heading lines, verbatim");
        let body = &ws[4..ws.len() - 1]; // title + header + list rows
        assert_eq!(body[0], "pick upgrade targets".len(), "title below the heading");
        assert_eq!(body[1], 2 + 10 + 7 + 2 + 5 + 2 + 6, "label-floored skeleton");
        assert!(body[2..].iter().all(|n| *n == body[1]), "rows share the header's skeleton");
        assert!(*ws.last().unwrap() <= 80, "footer fits the terminal");
        // height math: positioned lines only (the separator blank is left
        // blank by the clear): heading(4) + title + header + 3 rows + footer
        assert_eq!(ws.len(), 4 + 1 + 1 + 3 + 1, "heading + title + header + rows + footer");
    }

    /// visible width of every positioned line: strict `row;colH` starts, the
    /// segment runs to the next position or end, escapes cost nothing
    fn line_widths(f: &str) -> Vec<usize> {
        let b = f.as_bytes();
        let mut starts: Vec<usize> = Vec::new();
        let mut k = 0;
        while k < b.len() {
            if b[k] == 0x1b && k + 1 < b.len() && b[k + 1] == b'[' {
                let mut j = k + 2;
                while j < b.len() && (b[j].is_ascii_digit() || b[j] == b';') {
                    j += 1;
                }
                if j < b.len() && b[j] == b'H' && j > k + 2 {
                    starts.push(k);
                }
            }
            k += 1;
        }
        let mut out = Vec::new();
        for n in 0..starts.len() {
            let from = starts[n];
            let to = if n + 1 < starts.len() { starts[n + 1] } else { b.len() };
            let mut m = from + 2;
            while m < to && (b[m].is_ascii_digit() || b[m] == b';') {
                m += 1;
            }
            m += 1; // the 'H'
            out.push(strip_ansi_bytes(std::str::from_utf8(&b[m..to]).unwrap()).chars().count());
        }
        out
    }

    #[test]
    fn composed_lines_never_exceed_the_terminal() {
        // a 60-char name plus 14-char prerelease versions: the name column
        // takes whatever the terminal has left after the version columns, so
        // no line wraps even at 80 cols; at 70 the footer truncates instead
        let long_name = "n".repeat(60);
        let rows = vec![row(&long_name, "2.0.0", "2.1.3", "14.0.0-beta.12")];
        let s = Sel::new(1);
        let f = frame(&rows, &s, 80, 24, HEAD);
        assert!(!f.contains("\x1b[2K"));
        for wd in line_widths(&f) {
            assert!(wd <= 80, "line of {} visible chars overflows 80 cols", wd);
        }
        // narrower terminal: the footer takes the hit, lines stay in bounds
        let f = frame(&rows, &s, 70, 24, HEAD);
        for wd in line_widths(&f) {
            assert!(wd <= 70, "line of {} visible chars overflows 70 cols", wd);
        }
        let foot = *line_widths(&f).last().unwrap();
        assert!(foot < format!("{}{}", 56, 18).len().max(74), "footer was truncated");
    }

    #[test]
    fn cycle_skips_noop_major_cells() {
        // row 0's latest-of-major equals its current version: stepping onto
        // major would advertise a change that won't happen, so cycle skips it
        let rows = vec![row("ms", "2.1.3", "2.1.3", "2.1.3"), row("chalk", "2.4.1", "2.5.6", "6.0.1")];
        let mut s = Sel::new(2);
        s.cycle(1, false); // current → (major skipped) → latest
        assert_eq!(s.targets[0], Target::Latest);
        s.cycle(-1, false); // latest → (major skipped) → current
        assert_eq!(s.targets[0], Target::Current);
        // with the flag the major cell stays reachable (nothing to skip)
        s.cycle(1, true);
        assert_eq!(s.targets[0], Target::Major);
        // the flag is computed per row: chalk's major IS pickable
        assert!(major_pickable(&rows, 1) && !major_pickable(&rows, 0));
    }

    fn strip_ansi_bytes(s: &str) -> String {
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
    fn sort_versions_puts_newest_first() {
        let vs = sort_versions(vec![
            "4.1.2".into(),
            "6.0.1".into(),
            "4.2.0-beta.1".into(),
            "2.1.3".into(),
            "10.0.0".into(),
        ]);
        assert_eq!(vs, vec!["10.0.0", "6.0.1", "4.2.0-beta.1", "4.1.2", "2.1.3"]);
        // unparseable keys are dropped rather than crashing the sort
        assert_eq!(sort_versions(vec!["next".into(), "1.0.0".into()]), vec!["1.0.0"]);
    }

    #[test]
    fn cursor_opens_on_the_newest_stable() {
        // newest entry is a prerelease: the cursor skips to the first stable
        let vs = sort_versions(vec!["4.1.2".into(), "6.0.1-alpha".into(), "5.0.0".into()]);
        assert_eq!(vs, vec!["6.0.1-alpha", "5.0.0", "4.1.2"]);
        assert_eq!(cursor_start(&vs), 1);
        // all-prerelease list: nothing to anchor on, index 0
        let pre_only = sort_versions(vec!["2.0.0-rc.1".into(), "1.0.0-beta".into()]);
        assert_eq!(cursor_start(&pre_only), 0);
    }

    #[test]
    fn version_cursor_moves_and_clamps() {
        assert_eq!(clamp_move(0, -1, 3), 0);
        assert_eq!(clamp_move(2, 1, 3), 2);
        assert_eq!(clamp_move(0, 99, 3), 2);
        assert_eq!(clamp_move(2, -99, 3), 0);
        // empty list: no panic, no move
        assert_eq!(clamp_move(0, 1, 0), 0);
    }

    #[test]
    fn vpicker_rows_mark_prereleases_and_share_width() {
        let versions = vec!["6.0.1".to_string(), "5.0.0-rc.1".to_string()];
        let f = vframe("chalk", &versions, 1, 80, 24, HEAD); // cursor on the prerelease
        assert!(!f.contains("\x1b[2K"), "no line erases: rows overwrite by exact width");
        assert!(f.contains("❯"), "cursor marker present");
        // the heading block rides on top
        assert!(f.contains("h1") && f.contains("h4"), "heading lines at the top");
        // the prerelease stays dim even under the cursor; the stable cursor
        // row renders plain
        assert!(f.contains(&dim(&format!("{:<w$}", "5.0.0-rc.1", w = 6))));
        // uniform row width: fixed one-column rows (the title has its own)
        let ws = line_widths(&f);
        let rows = &ws[5..ws.len() - 1];
        assert!(rows.iter().all(|n| *n == rows[0]), "uniform version rows");
        assert!(f.contains("up/down: move · enter: pick · q: cancel"));
        assert!(f.contains("2/2"), "picker position in the footer");
    }

    #[test]
    fn window_keeps_the_cursor_visible() {
        assert_eq!(window(0, 25, 10), 0);
        assert_eq!(window(9, 25, 10), 0);
        assert_eq!(window(10, 25, 10), 1);
        assert_eq!(window(24, 25, 10), 15);
        // everything fits: no windowing at all
        assert_eq!(window(4, 5, 10), 0);
    }

    #[test]
    fn frame_windows_long_lists() {
        let rows: Vec<Row> = (0..30)
            .map(|i| row(&format!("pkg{}", i), "1.0.0", "1.9.0", "2.0.0"))
            .collect();
        let mut s = Sel::new(30);
        s.move_by(29);
        let f = frame(&rows, &s, 80, 24, HEAD);
        // only the viewport's rows are drawn (view = 24 - 10 = 14 with the
        // heading block counted in)
        assert!(f.contains("pkg29") && !f.contains("pkg0 "));
        assert!(f.contains("pkg16"), "the window opens so the cursor is visible");
    }

    #[test]
    fn resize_detection_and_poll_gate() {
        assert!(needs_relayout((80, 24), (80, 25)));
        assert!(needs_relayout((80, 24), (100, 24)));
        assert!(!needs_relayout((80, 24), (80, 24)));
        // windows queries the console directly every tick; unix spawns stty,
        // so it polls every 4th tick
        if cfg!(windows) {
            assert!(size_poll_tick(0) && size_poll_tick(3));
        } else {
            assert!(size_poll_tick(4) && size_poll_tick(8));
            assert!(!size_poll_tick(1) && !size_poll_tick(3));
        }
    }

    #[test]
    fn shrinking_the_terminal_keeps_the_cursor_visible() {
        let rows: Vec<Row> = (0..12)
            .map(|i| row(&format!("pkg{:02}", i), "1.0.0", "1.9.0", "2.0.0"))
            .collect();
        let mut s = Sel::new(12);
        for _ in 0..8 {
            s.move_by(1); // cursor on row 8
        }
        // wide console: everything fits (view = 30 - 10 = 20 with the heading)
        assert_eq!(window(s.cursor, rows.len(), 20), 0);
        let wide = frame(&rows, &s, 120, 30, HEAD);
        assert!(wide.contains("pkg08") && wide.contains("pkg00"));
        // shrunk console: view = 12 - 10 = 2 rows, the window slides so the
        // cursor row stays on screen
        assert_eq!(window(s.cursor, rows.len(), 2), 7);
        let small = frame(&rows, &s, 80, 12, HEAD);
        assert!(small.contains("pkg08"), "cursor row stays visible after the shrink");
        assert!(!small.contains("pkg00"), "early rows scrolled out of the viewport");
        // footer still lands inside the tiny window
        assert!(small.contains("0 major · 0 latest"));
    }
}
