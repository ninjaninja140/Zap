// shared raw-mode terminal plumbing for the full-screen pickers: the
// alternate-screen guard, the key parser, the resize poll and the platform
// size readers. extracted from install/_sel.rs when `zap cache inspect`
// became the third full-screen ui (run/_multirun.rs still owns its own copy
//, the pane grid's input needs differ). included ONCE from cmds/mod.rs and
// referenced by path; a second #[path] include would double-compile the
// module and fork its types

use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

/// shared tick: also the resize-poll cadence
pub const TICK: Duration = Duration::from_millis(150);

/// keys the full-screen uis share: arrows + enter/cancel universally, the
/// letters are plain keys each ui interprets its own way (a = all-latest in
/// the upgrader, mark-all in the cache browser; n = mark-none or the "no"
/// of a y/n prompt)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    All,      // a
    AllMajor, // m
    Enter,
    Cancel, // q or a bare esc
    CtrlC,
    Space,  // space
    Sort,   // s
    Delete, // d
    Yes,    // y
    No,     // n
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
        (0, b's') | (0, b'S') => {
            *state = 0;
            Some(Key::Sort)
        }
        (0, b'd') | (0, b'D') => {
            *state = 0;
            Some(Key::Delete)
        }
        (0, b'y') | (0, b'Y') => {
            *state = 0;
            Some(Key::Yes)
        }
        (0, b'n') | (0, b'N') => {
            *state = 0;
            Some(Key::No)
        }
        (0, b'q') | (0, b'Q') => {
            *state = 0;
            Some(Key::Cancel)
        }
        (0, b' ') => {
            *state = 0;
            Some(Key::Space)
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

/// the per-tick resize poll shared by all loops: true when the terminal
/// changed size (callers recompute geometry and repaint)
pub fn poll_resize(w: &mut usize, h: &mut usize, tick: u32) -> bool {
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

/// first visible list row so the cursor stays inside the viewport (pure)
pub fn window(cursor: usize, n: usize, view: usize) -> usize {
    if view == 0 || n <= view {
        return 0;
    }
    cursor.saturating_sub(view - 1).min(n - view)
}

/// clamp a cursor step into 0..len (free fn so ui state stays dumb) (pure)
pub fn clamp_move(cursor: usize, d: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    (cursor as isize + d).clamp(0, len as isize - 1) as usize
}

/// terminal size: visible console window (windows) / `stty size` (unix), 80x24
/// when neither answers
pub fn term_size() -> (usize, usize) {
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

/// cheap stdin probe for flows that would do work before their first picker
/// opens: bail on piped stdin first. the guard drops immediately, raw mode
/// is restored, nothing is read
pub fn console_available() -> bool {
    TerminalGuard::new().raw_active()
}

/// background reader turning console bytes into keys (the platform readers
/// stay private; this is the only entry uis need)
pub fn spawn_key_thread(tx: mpsc::Sender<Key>) {
    term::spawn_key_thread(tx);
}

/// owns raw mode + the utf-8 codepage; restores both on drop so every exit
/// path (normal end, ctrl+c, panic unwinding) leaves the terminal sane
pub struct TerminalGuard {
    saved: Option<term::Saved>,
    cps: Option<(u32, u32)>, // previous console codepages (input, output)
}

impl TerminalGuard {
    pub fn new() -> Self {
        // the codepage switch rides along with raw mode: both mean a real
        // console is attached; piped runs skip it (nothing visible changes)
        let saved = term::enable_raw();
        let cps = if saved.is_some() { term::utf8_codepage() } else { None };
        TerminalGuard { saved, cps }
    }

    pub fn raw_active(&self) -> bool {
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

    #[test]
    fn key_parse_covers_the_shared_keys() {
        for (bytes, want) in [
            (vec![0x1b, b'[', b'A'], Some(Key::Up)),
            (vec![0x1b, b'[', b'B'], Some(Key::Down)),
            (vec![0x1b, b'[', b'C'], Some(Key::Right)),
            (vec![0x1b, b'[', b'D'], Some(Key::Left)),
            (vec![b'a'], Some(Key::All)),
            (vec![b'm'], Some(Key::AllMajor)),
            (vec![b's'], Some(Key::Sort)),
            (vec![b'd'], Some(Key::Delete)),
            (vec![b'y'], Some(Key::Yes)),
            (vec![b'n'], Some(Key::No)),
            (vec![b' '], Some(Key::Space)),
            (vec![b'q'], Some(Key::Cancel)),
            (vec![0x0d], Some(Key::Enter)),
            (vec![0x03], Some(Key::CtrlC)),
            (vec![0x1b, b'x'], Some(Key::Cancel)), // bare esc then any byte
            (vec![0x1b, b'[', b'Z'], None),        // unknown csi final: dropped
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
    fn window_keeps_the_cursor_visible() {
        assert_eq!(window(0, 25, 10), 0);
        assert_eq!(window(9, 25, 10), 0);
        assert_eq!(window(10, 25, 10), 1);
        assert_eq!(window(24, 25, 10), 15);
        // everything fits: no windowing at all
        assert_eq!(window(4, 5, 10), 0);
    }

    #[test]
    fn clamp_move_clamps_into_range() {
        assert_eq!(clamp_move(0, -1, 3), 0);
        assert_eq!(clamp_move(2, 1, 3), 2);
        assert_eq!(clamp_move(0, 99, 3), 2);
        assert_eq!(clamp_move(2, -99, 3), 0);
        // empty list: no panic, no move
        assert_eq!(clamp_move(0, 1, 0), 0);
    }
}
