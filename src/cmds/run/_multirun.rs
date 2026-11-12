// split-pane multi-script runner behind `zap run a b c`: scripts run
// concurrently, each pane is a rounded box-drawing frame tailing one script's
// merged stdout/stderr on the alternate screen; arrow keys switch pages when
// the grid doesn't fit. std-only rendering (plus direct kernel32 externs for
// raw input and the utf-8 codepage on windows), assumes a VT-capable terminal
// (windows terminal / win10+ conhost / any unix terminal)

use crate::config::{Config, Linker};
use anyhow::Result;
use std::collections::VecDeque;
use std::io::{BufRead, Read, Write};
use std::path::Path;
#[cfg(not(windows))]
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

const WORDMARK: &str = include_str!("../../../zap.text.txt");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Key {
    Left,
    Right,
    CtrlC,
    Enter,
}

// ctrl+c: raw mode is ours while the panes are up (the default console
// handler is disabled), so the key thread reports 0x03 and we kill the tree
struct PaneInner {
    lines: VecDeque<String>,
    seq: u64,      // bumped on every buffer/status change, repaints watch it
    stage: String, // running stage label shown in the pane title
    done: bool,
    code: i32,
}

type Pane = Arc<Mutex<PaneInner>>;

/// shared kill switch: pids of every spawned child + the abort flag
struct Ctrl {
    pids: Mutex<Vec<u32>>,
    aborted: AtomicBool,
}

impl Ctrl {
    fn track(&self, pid: u32) {
        self.pids.lock().unwrap().push(pid);
    }
    fn kill_all(&self) {
        self.aborted.store(true, Ordering::SeqCst);
        let pids = self.pids.lock().unwrap();
        #[cfg(windows)]
        for &pid in pids.iter() {
            term::kill_pid(pid);
        }
        #[cfg(not(windows))]
        if !pids.is_empty() {
            let list = pids.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(" ");
            let _ = Command::new("sh").arg("-c").arg(format!("kill -9 {}", list)).status();
        }
    }
}

/// grid geometry for a run: cols/rows of the full grid, per-pane size, and
/// how many grid rows fit on one page
struct Layout {
    cols: usize,
    rows: usize,
    pane_w: usize,
    pane_h: usize,
    rpp: usize, // rows of panes per page
    pages: usize,
}

/// cols = ceil(sqrt(n)), rows = ceil(n/cols)
fn grid(n: usize) -> (usize, usize) {
    let cols = (n as f64).sqrt().ceil() as usize;
    (cols, n.div_ceil(cols))
}

/// fit the grid to the terminal: one space between horizontally adjacent
/// panes, pane height clamped to 3..=20 rows, paginate when even 3-row panes
/// can't stack every grid row inside the terminal
fn layout(w: usize, h: usize, header: usize, n: usize) -> Layout {
    let (cols, rows) = grid(n);
    let pane_w = (w.saturating_sub(cols - 1) / cols).max(8);
    let avail = h.saturating_sub(header);
    let pane_h = (avail / rows).clamp(3, 20);
    let rpp = (avail / pane_h).max(1);
    Layout { cols, rows, pane_w, pane_h, rpp, pages: rows.div_ceil(rpp) }
}

/// pane indices visible on a page, in paint order
fn visible_panes(l: &Layout, page: usize, n: usize) -> Vec<usize> {
    let r0 = page * l.rpp;
    let r1 = (r0 + l.rpp).min(l.rows);
    (r0..r1).flat_map(|r| r * l.cols..((r + 1) * l.cols).min(n)).collect()
}

/// screen origin + size of a pane's paint region
#[derive(Clone, Copy)]
struct Frame {
    top: usize,
    left: usize,
    w: usize,
    body_h: usize,
}

/// where a pane's top border lands on screen
fn pane_frame(l: &Layout, header_h: usize, page: usize, i: usize) -> Frame {
    let row = i / l.cols;
    Frame {
        top: header_h + (row - page * l.rpp) * l.pane_h + 1,
        left: (i % l.cols) * (l.pane_w + 1) + 1,
        w: l.pane_w,
        body_h: l.pane_h - 2,
    }
}

/// repaint decision for one pane: dirty running panes always repaint; a
/// finished pane gets exactly one final repaint (borders + frozen tail + final
/// status) and is then static, the tick loop skips it entirely (no repaint,
/// no clear of its region) until a page switch or process end repaints all
fn needs_repaint(force: bool, seq: u64, painted_seq: u64, done: bool, done_shown: bool) -> bool {
    if force {
        return true;
    }
    if done {
        return !done_shown;
    }
    seq != painted_seq
}

/// run every plan concurrently in panes, render the grid, print a summary and
/// return the first failing script's exit code (130 on ctrl+c)
pub fn run_multi(
    ctx: &Path,
    env_root: &Path,
    cfg: &Config,
    names: &[String],
    plans: &[Vec<(String, String)>],
) -> Result<i32> {
    let ctrl = Arc::new(Ctrl { pids: Mutex::new(Vec::new()), aborted: AtomicBool::new(false) });
    let panes: Vec<Pane> = plans
        .iter()
        .map(|_| {
            Arc::new(Mutex::new(PaneInner {
                lines: VecDeque::new(),
                seq: 0,
                stage: String::new(),
                done: false,
                code: 0,
            }))
        })
        .collect();
    for (pane, stages) in panes.iter().zip(plans) {
        let pane = pane.clone();
        let stages = stages.clone();
        let cwd = ctx.to_path_buf();
        let env_root = env_root.to_path_buf();
        let linker = cfg.linker;
        let ctrl = ctrl.clone();
        std::thread::spawn(move || run_stages(&pane, stages, &cwd, &env_root, linker, &ctrl));
    }

    let (mut w, mut h) = term_size();
    let wordmark: Vec<&str> = WORDMARK.lines().collect();
    let n = plans.len();
    // the header is the wordmark plus a page-indicator row (always reserved,
    // dim 1/1 when everything fits on one page)
    let header_h = wordmark.len() + 1;
    let mut l = layout(w, h, header_h, n);
    let (key_tx, key_rx) = mpsc::channel::<Key>();

    // raw mode + alt screen; the guard restores both on every exit path
    let guard = TerminalGuard::new();
    if guard.raw_active() {
        term::spawn_key_thread(key_tx);
    }
    print!("\x1b[?1049h\x1b[?25l");
    std::io::stdout().flush()?;

    let mut out = String::new();
    for (i, line) in wordmark.iter().enumerate() {
        let col = (w.saturating_sub(line.chars().count())) / 2 + 1;
        out.push_str(&format!("\x1b[{};{}H\x1b[2K{}", i + 1, col, crate::util::electric(line)));
    }
    out.push_str(&page_indicator(0, l.pages, w, header_h, false));
    print!("{}", out);
    std::io::stdout().flush()?;

    let mut page = 0usize;
    let mut shown_page = usize::MAX;
    let mut interrupted = false;
    let mut painted = vec![u64::MAX; n];
    let mut done_shown = vec![false; n];
    let mut hanging = false;
    let mut tick = 0u32;

    'render: loop {
        // ~20 repaints/sec, the tick is the per-pane repaint throttle
        std::thread::sleep(Duration::from_millis(50));
        while let Ok(key) = key_rx.try_recv() {
            match key {
                Key::Left if page > 0 => page -= 1,
                Key::Right if page + 1 < l.pages => page += 1,
                // enter only ends the run once the finished view is hanging
                Key::Enter if hanging => break 'render,
                Key::CtrlC => {
                    ctrl.kill_all();
                    interrupted = true;
                    break 'render;
                }
                _ => {}
            }
        }
        // resize poll: the layout was computed once at start, so a mid-run
        // resize would leave stale geometry; re-derive it when the window
        // changed. buffers stay untouched (same tail, new geometry), the
        // force repaint below re-seats every visible pane and the clear
        // re-seats the wordmark
        tick += 1;
        let mut resized = false;
        if size_poll_tick(tick, cfg!(windows)) {
            let (nw, nh) = term_size();
            if (nw, nh) != (w, h) {
                (w, h) = (nw, nh);
                l = layout(w, h, header_h, n);
                if page + 1 > l.pages {
                    page = l.pages - 1;
                }
                shown_page = usize::MAX; // force-repaint everything visible
                resized = true;
            }
        }
        // a page switch repaints everything visible from the tail buffers, so
        // off-page panes show their latest state when you page to them
        let force = shown_page != page;
        let aborted = ctrl.aborted.load(Ordering::SeqCst);
        let mut out = String::new();
        if resized {
            // the old centering and pane rows are stale at the new size:
            // clear once, re-seat the wordmark, the force branch repaints
            // the rest into the new geometry
            out.push_str("\x1b[2J");
            for (i, line) in wordmark.iter().enumerate() {
                let col = (w.saturating_sub(line.chars().count())) / 2 + 1;
                out.push_str(&format!("\x1b[{};{}H\x1b[2K{}", i + 1, col, crate::util::electric(line)));
            }
        }
        if force {
            shown_page = page;
            out.push_str(&page_indicator(page, l.pages, w, header_h, hanging));
        }
        let vis = visible_panes(&l, page, n);
        for &i in &vis {
            let (seq, done) = {
                let p = panes[i].lock().unwrap();
                (p.seq, p.done)
            };
            if !needs_repaint(force, seq, painted[i], done, done_shown[i]) {
                continue;
            }
            painted[i] = seq;
            if done {
                // the one final repaint is happening, freeze from here on
                done_shown[i] = true;
            }
            paint(&mut out, &panes[i], pane_frame(&l, header_h, page, i), &names[i], aborted);
        }
        if force {
            // the last page can show fewer rows, blank what a fuller page left
            let vis_rows = (l.rows - page * l.rpp).min(l.rpp);
            for r in header_h + vis_rows * l.pane_h + 1..=header_h + l.rpp * l.pane_h {
                out.push_str(&format!("\x1b[{};1H\x1b[2K", r));
            }
        }
        print!("{}", out);
        std::io::stdout().flush()?;
        if interrupted {
            break;
        }
        let all_done = panes.iter().all(|p| p.lock().unwrap().done);
        if all_done && !hanging {
            // final pass so exit markers land even on same-tick updates
            let mut fin = String::new();
            for &i in &vis {
                paint(&mut fin, &panes[i], pane_frame(&l, header_h, page, i), &names[i], aborted);
            }
            print!("{}", fin);
            std::io::stdout().flush()?;
            hanging = true;
            // keep the finished view (statuses + hint) up until enter, but
            // only with a real console attached, piped runs exit immediately
            // so automation and tests never hang
            if guard.raw_active() {
                print!("{}", page_indicator(page, l.pages, w, header_h, true));
                std::io::stdout().flush()?;
            } else {
                break;
            }
        }
        if interrupted {
            break;
        }
        // hanging: the loop keeps ticking (pages + ctrl+c still work) until
        // the enter key breaks out; frozen panes repaint nothing
        if hanging {
            continue;
        }
    }

    drop(guard);

    for (name, pane) in names.iter().zip(&panes) {
        let p = pane.lock().unwrap();
        if !p.done {
            println!("  {} {} · interrupted", crate::util::dim("…"), name);
        } else if p.code == 0 {
            println!("  {} {} · exit 0", crate::util::green("✓"), name);
        } else {
            println!("  {} {} · exit {}", crate::util::red("✗"), name, p.code);
        }
    }
    if interrupted {
        return Ok(130);
    }
    Ok(panes.iter().map(|p| p.lock().unwrap().code).find(|&c| c != 0).unwrap_or(0))
}

/// run one pane's stage sequence (pre -> script -> post) sequentially
fn run_stages(pane: &Pane, stages: Vec<(String, String)>, cwd: &Path, env_root: &Path, linker: Linker, ctrl: &Ctrl) {
    for (i, (label, line)) in stages.iter().enumerate() {
        if ctrl.aborted.load(Ordering::SeqCst) {
            finish(pane, 130);
            return;
        }
        if i > 0 {
            push(pane, &format!("── {} ──", label));
        }
        // npm-style: every stage announces its own command line, these scroll
        // with the tail like normal output
        push(pane, &format!("> {}", line));
        {
            let mut p = pane.lock().unwrap();
            p.stage = label.clone();
            p.seq += 1;
        }
        let mut c = crate::util::shell_command(line);
        c.current_dir(cwd);
        crate::pnp::apply_env(&mut c, env_root, linker);
        // multi-run only: children run on pipes, so most tools auto-disable
        // colour, flip the big three ecosystem switches back on
        // shortcut: tools with their own colour flags may still need
        // --color=always passed through the script itself
        c.env("FORCE_COLOR", "1");
        c.env("CLICOLOR_FORCE", "1");
        c.env("CARGO_TERM_COLOR", "always");
        c.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = match c.spawn() {
            Ok(ch) => ch,
            Err(e) => {
                push(pane, &format!("failed to start `{}`: {}", line, e));
                finish(pane, 1);
                return;
            }
        };
        ctrl.track(child.id());
        let out_pipe = child.stdout.take().expect("piped stdout");
        let err_pipe = child.stderr.take().expect("piped stderr");
        // one reader per stream, merged into the same tail buffer line-wise
        let (tx, rx) = mpsc::channel::<()>();
        let p2 = pane.clone();
        let tx2 = tx.clone();
        std::thread::spawn(move || pump(out_pipe, p2, tx2));
        let p3 = pane.clone();
        std::thread::spawn(move || pump(err_pipe, p3, tx));
        let code = match child.wait() {
            Ok(s) => s.code().unwrap_or(1),
            Err(_) => 1,
        };
        // drain the pipes briefly, but don't wait on grandchildren that
        // inherited them and keep them open
        let _ = rx.recv_timeout(Duration::from_millis(200));
        let _ = rx.recv_timeout(Duration::from_millis(200));
        if code != 0 {
            finish(pane, code);
            return;
        }
    }
    finish(pane, 0);
}

/// read one child stream line by line into the pane's tail buffer, keeping
/// sgr colours and carrying open colour state across lines
fn pump<R: Read>(r: R, pane: Pane, done: mpsc::Sender<()>) {
    let mut buf = std::io::BufReader::new(r);
    let mut raw = Vec::new();
    let mut sgr: Option<String> = None;
    loop {
        raw.clear();
        match buf.read_until(b'\n', &mut raw) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let s = String::from_utf8_lossy(&raw);
                let carry = sgr.clone();
                let line = sanitize_sgr(s.trim_end_matches(['\n', '\r']), &mut sgr);
                push(&pane, &with_sgr_frame(line, &carry, sgr.is_some()));
            }
        }
    }
    let _ = done.send(());
}

/// append a display line, keep the tail bounded, mark the pane dirty
fn push(pane: &Pane, line: &str) {
    let mut p = pane.lock().unwrap();
    p.lines.push_back(line.to_string());
    while p.lines.len() > 500 {
        p.lines.pop_front();
    }
    p.seq += 1;
}

fn finish(pane: &Pane, code: i32) {
    let mut p = pane.lock().unwrap();
    p.stage.clear();
    p.done = true;
    p.code = code;
    p.seq += 1;
}

/// repaint one pane: rounded border box, script name inline in the top edge,
/// status right-aligned in the bottom edge, output tail inside
fn paint(out: &mut String, pane: &Pane, f: Frame, name: &str, aborted: bool) {
    let w = f.w;
    let p = pane.lock().unwrap();
    // keep long script names from pushing the border off-pane
    let name_max = w.saturating_sub(8).max(1);
    let shown_name: String = name.chars().take(name_max).collect();
    let fill = w.saturating_sub(5 + shown_name.chars().count());
    let top_line = format!(
        "{}{}{}",
        crate::util::dim("╭─ "),
        crate::util::electric_bold(&shown_name),
        crate::util::dim(format!(" {}╮", "─".repeat(fill)))
    );
    put(out, f.top, f.left, &top_line);
    let inner = w.saturating_sub(2);
    let tail: Vec<String> = p.lines.iter().rev().take(f.body_h).rev().cloned().collect();
    for (i, line) in tail.iter().enumerate() {
        // truncate to the visible budget with escape sequences kept whole,
        // then close any open colour so the padding and borders stay default
        // shortcut: visible chars only, no unicode-width handling, wide CJK glyphs may misalign panes, upgrade when it matters
        let shown = truncate_visible(line, inner.saturating_sub(1));
        let shown = if shown.contains('\x1b') { format!("{}\x1b[0m", shown) } else { shown };
        let vis = strip_ansi(&shown).chars().count();
        let padded = format!(" {}{}", shown, " ".repeat((inner - 1).saturating_sub(vis)));
        // stage separators (pushed by run_stages) render dim; borders dim to match the edges
        let content = if line.starts_with("──") { crate::util::dim(&padded) } else { padded };
        put(out, f.top + 1 + i, f.left, &format!("{}{}{}", crate::util::dim("│"), content, crate::util::dim("│")));
    }
    for i in tail.len()..f.body_h {
        put(out, f.top + 1 + i, f.left, &format!("{}{:<iw$}{}", crate::util::dim("│"), "", crate::util::dim("│"), iw = inner));
    }
    // bottom border with the status right-aligned: ╰──── ✓ exit 0 ╯
    let raw = if p.done {
        if p.code == 0 {
            "✓ exit 0".to_string()
        } else {
            format!("✗ exit {}", p.code)
        }
    } else if aborted {
        "✗ killed".to_string()
    } else {
        "…".to_string()
    };
    // keep the border exactly w visible chars: a longer status would spill
    // into the neighbor's columns; the clamp leaves room for the extra ─
    // between the status and the corner
    let plain: String = raw.chars().take(w.saturating_sub(4).max(1)).collect();
    let status = if p.done {
        if p.code == 0 {
            crate::util::green(&plain)
        } else {
            crate::util::red(&plain)
        }
    } else if aborted {
        crate::util::red(&plain)
    } else {
        crate::util::dim(&plain)
    };
    // the status sits one char further left: an extra ─ between it and ╯
    let fill = w.saturating_sub(4 + plain.chars().count());
    let bottom = format!(
        "{}{} {}{}{}",
        crate::util::dim("╰"),
        crate::util::dim("─".repeat(fill)),
        status,
        crate::util::dim("─"),
        crate::util::dim("╯")
    );
    put(out, f.top + 1 + f.body_h, f.left, &bottom);
}

/// position + write one pane line; deliberately no erase sequence: `ESC[2K`
/// erases the WHOLE row from any column, so the right pane's repaints wiped
/// the left panes off the screen, every pane line is composed to exactly its
/// pane's width, so the overwrite itself is the clear and stays in-bounds
fn put(out: &mut String, row: usize, col: usize, text: &str) {
    out.push_str(&format!("\x1b[{};{}H{}", row, col, text));
}

/// centered `← cur/total →` page indicator under the wordmark; arrows are
/// hidden at the edges and the whole thing is dim when there's one page.
/// while hanging, `↵ enter to exit` folds into the reserved row (single-page
/// runs show just the hint) so the geometry never shifts when it appears
fn page_indicator(page: usize, pages: usize, w: usize, row: usize, hang_hint: bool) -> String {
    let lft = if page == 0 { " " } else { "←" };
    let rgt = if page + 1 == pages { " " } else { "→" };
    let plain = if hang_hint {
        if pages == 1 {
            "↵ enter to exit".to_string()
        } else {
            format!("{} {}/{} {} ↵ enter to exit", lft, page + 1, pages, rgt)
        }
    } else {
        format!("{} {}/{} {}", lft, page + 1, pages, rgt)
    };
    let styled = if hang_hint {
        if pages == 1 {
            crate::util::dim(&plain)
        } else {
            format!(
                "{} {}/{} {} {}",
                crate::util::dim(lft),
                page + 1,
                pages,
                crate::util::dim(rgt),
                crate::util::dim("↵ enter to exit")
            )
        }
    } else if pages == 1 {
        crate::util::dim(&plain)
    } else {
        format!("{} {}/{} {}", crate::util::dim(lft), page + 1, pages, crate::util::dim(rgt))
    };
    let col = (w.saturating_sub(plain.chars().count())) / 2 + 1;
    format!("\x1b[{};{}H\x1b[2K{}", row, col, styled)
}

/// strip ansi escapes (csi + osc) from child output so colors and cursor
/// moves can't corrupt the pane grid
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.peek() {
            Some('[') => {
                // csi: parameter bytes until a final byte in @..~
                it.next();
                for n in it.by_ref() {
                    if ('\x40'..='\x7e').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                // osc: until BEL or the next ESC
                it.next();
                for n in it.by_ref() {
                    if n == '\x07' || n == '\x1b' {
                        break;
                    }
                }
            }
            _ => { it.next(); } // two-byte escape like ESC 7
        }
    }
    out
}

/// keep SGR (colour/style) sequences in a child line, strip every other
/// escape exactly like strip_ansi; also tracks the stream's open colour
/// state in `state` so it can be carried onto the next line
// shortcut: the last non-reset SGR is carried verbatim instead of merging
// attribute combos; 256/truecolor params pass through untouched
fn sanitize_sgr(line: &str, state: &mut Option<String>) -> String {
    let mut out = String::with_capacity(line.len());
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.peek() {
            Some('[') => {
                it.next();
                let mut params = String::new();
                let mut final_byte = '\0';
                for e in it.by_ref() {
                    if ('\x40'..='\x7e').contains(&e) {
                        final_byte = e;
                        break;
                    }
                    params.push(e);
                }
                if final_byte == 'm' {
                    // sgr: keep it; a bare reset clears the carried state
                    if params.is_empty() || params == "0" {
                        *state = None;
                    } else {
                        *state = Some(format!("\x1b[{}m", params));
                    }
                    out.push_str(&format!("\x1b[{}m", params));
                }
                // non-sgr csi falls through: stripped
            }
            Some(']') => {
                it.next();
                for n in it.by_ref() {
                    if n == '\x07' || n == '\x1b' {
                        break;
                    }
                }
            }
            _ => { it.next(); } // two-byte escape like ESC 7
        }
    }
    out
}

/// frame a sanitized line with the stream's carried colour state: an open
/// colour is re-emitted at the start and the line is closed with a reset, so
/// colour never bleeds across lines or into other panes
fn with_sgr_frame(line: String, carry: &Option<String>, open: bool) -> String {
    match (carry, open) {
        (Some(seq), _) => format!("{}{}\x1b[0m", seq, line),
        (None, true) => format!("{}\x1b[0m", line),
        (None, false) => line,
    }
}

/// take up to n visible chars from a (possibly coloured) line, emitting escape
/// sequences whole so a take never cuts one in half and they cost no width
fn truncate_visible(line: &str, n: usize) -> String {
    let mut out = String::with_capacity(line.len());
    let mut it = line.chars().peekable();
    let mut shown = 0usize;
    while shown < n {
        match it.next() {
            None => break,
            Some('\x1b') => match it.peek() {
                Some('[') => {
                    out.push('\x1b');
                    out.push(it.next().unwrap());
                    for e in it.by_ref() {
                        out.push(e);
                        if ('\x40'..='\x7e').contains(&e) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    out.push('\x1b');
                    out.push(it.next().unwrap());
                    for e in it.by_ref() {
                        out.push(e);
                        if e == '\x07' || e == '\x1b' {
                            break;
                        }
                    }
                }
                _ => {
                    out.push('\x1b');
                    if let Some(e) = it.next() {
                        out.push(e);
                    }
                }
            },
            Some(c) => {
                out.push(c);
                shown += 1;
            }
        }
    }
    out
}

/// incremental key decoder shared by the platform readers: arrows arrive as
/// ESC[C / ESC[D, ctrl+c as the raw 0x03 byte, everything else is ignored
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
        (0, 0x1b) => {
            *state = 1;
            None
        }
        (1, b'[') => {
            *state = 2;
            None
        }
        (1, _) => {
            *state = 0;
            None
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

/// owns raw mode + the utf-8 codepage + the alternate screen; restores all
/// three on drop so every exit path (normal end, ctrl+c, panic unwinding)
/// leaves the terminal sane
struct TerminalGuard {
    saved: Option<term::Saved>,
    cps: Option<(u32, u32)>, // previous console codepages (input, output)
}

impl TerminalGuard {
    fn new() -> Self {
        // the codepage switch rides along with raw mode: both mean a real
        // console is attached; piped runs skip it (nothing visible changes)
        let saved = term::enable_raw();
        let cps = match saved {
            Some(_) => term::utf8_codepage(),
            None => None,
        };
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
        }
        // show the cursor and leave the alternate screen, even mid-panic
        let _ = std::io::stdout().write_all(b"\x1b[?25h\x1b[?1049l");
        let _ = std::io::stdout().flush();
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
        fn GetConsoleCP() -> u32;
        fn GetConsoleOutputCP() -> u32;
        fn SetConsoleCP(wcodepageid: u32) -> i32;
        fn SetConsoleOutputCP(wcodepageid: u32) -> i32;
        fn OpenProcess(dwdesiredaccess: u32, binherithandle: i32, dwprocessid: u32) -> *mut c_void;
        fn TerminateProcess(hprocess: *mut c_void, uexitcode: u32) -> i32;
        fn CloseHandle(hobject: *mut c_void) -> i32;
    }

    const STD_INPUT_HANDLE: u32 = (-10i32) as u32;
    // line input, echo and processed input off (ctrl+c becomes a byte),
    // virtual terminal input on so arrows arrive as ESC[C / ESC[D
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

    /// hard-kill a child by pid; grandchildren aren't chased, they finish on
    /// their own or die with the console
    pub fn kill_pid(pid: u32) {        const PROCESS_TERMINATE: u32 = 0x0001;
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !h.is_null() {
                TerminateProcess(h, 1);
                CloseHandle(h);
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

/// windows queries the console directly (cheap enough to check every tick);
/// unix shells out to `stty` per poll, so there it's only every 10th tick
fn size_poll_tick(tick: u32, direct_query: bool) -> bool {
    direct_query || tick.is_multiple_of(10)
}

/// terminal size, std-only (+ one kernel32 call for the visible window)
// moved to src/cmds/_progress.rs (pub(crate)) so the progress bar and the
// pane runner share one copy; the fallback-chain shortcut comment lives there
fn term_size() -> (usize, usize) {
    crate::cmds::progress::term_size()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_stages_pushes_command_lines_into_the_tail() {
        // a real echo child, offline: every stage's `> <line>` lands in the
        // tail after its separator; the child's output follows
        let pane: Pane = Arc::new(Mutex::new(PaneInner {
            lines: VecDeque::new(),
            seq: 0,
            stage: String::new(),
            done: false,
            code: 0,
        }));
        let ctrl = Ctrl { pids: Mutex::new(Vec::new()), aborted: AtomicBool::new(false) };
        let cwd = std::env::temp_dir();
        let stages = vec![
            ("prebuild".to_string(), "echo aaa".to_string()),
            ("build".to_string(), "echo bbb".to_string()),
        ];
        run_stages(&pane, stages, &cwd, &cwd, Linker::NodeModules, &ctrl);
        let p = pane.lock().unwrap();
        assert!(p.done && p.code == 0);
        let tail: Vec<String> = p.lines.iter().cloned().collect();
        // stage 1 (i=0): no separator, just the command line then the output
        assert_eq!(tail[0], "> echo aaa");
        assert!(tail.iter().any(|l| l == "aaa"));
        // stage 2: the separator precedes its command line + output
        assert!(tail.iter().any(|l| l == "── build ──"));
        assert!(tail.iter().any(|l| l == "> echo bbb"));
        assert!(tail.iter().any(|l| l == "bbb"));
        // ordering: the command line lands between the separator and the output
        let sep = tail.iter().position(|l| l == "── build ──").unwrap();
        let cmd = tail.iter().position(|l| l == "> echo bbb").unwrap();
        let out = tail.iter().position(|l| l == "bbb").unwrap();
        assert!(sep < cmd && cmd < out);
    }

    #[test]
    fn grid_sizes() {
        assert_eq!(grid(1), (1, 1));
        assert_eq!(grid(2), (2, 1));
        assert_eq!(grid(3), (2, 2));
        assert_eq!(grid(4), (2, 2));
        assert_eq!(grid(5), (3, 2));
        assert_eq!(grid(6), (3, 2));
        assert_eq!(grid(8), (3, 3));
        assert_eq!(grid(9), (3, 3));
    }

    #[test]
    fn layout_fits_or_paginates() {
        // two panes sit side by side on one page
        let l = layout(120, 40, 2, 2);
        assert_eq!((l.cols, l.rows, l.pages), (2, 1, 1));
        assert_eq!(l.pane_w, (120 - 1) / 2); // 1-space gutter accounted for
        // huge console: pane height capped at 20, still one page
        let l = layout(120, 9001, 2, 5);
        assert_eq!(l.pane_h, 20);
        assert_eq!(l.pages, 1);
        // tiny terminal: minimum 3-row panes, one grid row per page
        let l = layout(100, 6, 2, 5);
        assert_eq!((l.cols, l.rows), (3, 2));
        assert_eq!(l.pane_h, 3);
        assert_eq!(l.rpp, 1);
        assert_eq!(l.pages, 2);
    }

    #[test]
    fn page_pane_mapping() {
        let l = layout(100, 6, 2, 5); // cols=3 rows=2 rpp=1 -> 2 pages
        assert_eq!(visible_panes(&l, 0, 5), vec![0, 1, 2]);
        assert_eq!(visible_panes(&l, 1, 5), vec![3, 4]);
        // last page clamps to the rows that exist
        assert_eq!(visible_panes(&l, 1, 5).len(), 2);
        // every page beyond the last would be empty, callers clamp via pages
        assert_eq!(l.pages, 2);
    }

    #[test]
    fn key_parse_byte() {
        let mut s = 0u8;
        // right arrow = ESC [ C
        assert_eq!(parse_byte(&mut s, 0x1b), None);
        assert_eq!(parse_byte(&mut s, b'['), None);
        assert_eq!(parse_byte(&mut s, b'C'), Some(Key::Right));
        assert_eq!(s, 0);
        // left arrow = ESC [ D
        assert_eq!(parse_byte(&mut s, 0x1b), None);
        assert_eq!(parse_byte(&mut s, b'['), None);
        assert_eq!(parse_byte(&mut s, b'D'), Some(Key::Left));
        // ctrl+c in any state
        s = 2;
        assert_eq!(parse_byte(&mut s, 0x03), Some(Key::CtrlC));
        // enter as cr or lf, in any state
        s = 0;
        assert_eq!(parse_byte(&mut s, 0x0d), Some(Key::Enter));
        s = 2;
        assert_eq!(parse_byte(&mut s, 0x0a), Some(Key::Enter));
        assert_eq!(s, 0);
        // stray bytes reset the state
        s = 1;
        assert_eq!(parse_byte(&mut s, b'x'), None);
        assert_eq!(s, 0);
    }

    #[test]
    fn finished_pane_stays_static_while_neighbor_repaints() {
        // fast finished (frozen tail, exit 0), slow still running next to it
        let done: Pane = Arc::new(Mutex::new(PaneInner {
            lines: VecDeque::from(["a 1".to_string(), "a 2".to_string()]),
            seq: 9,
            stage: String::new(),
            done: true,
            code: 0,
        }));
        let running: Pane = Arc::new(Mutex::new(PaneInner {
            lines: VecDeque::from(["b 1".to_string()]),
            seq: 4,
            stage: "slow".to_string(),
            done: false,
            code: 0,
        }));
        let l = layout(100, 30, 2, 2);
        let f0 = pane_frame(&l, 2, 0, 0);
        let f1 = pane_frame(&l, 2, 0, 1);
        let frame_of = |pane: &Pane, f: Frame, name: &str| {
            let mut s = String::new();
            paint(&mut s, pane, f, name, false);
            s
        };
        // the finished pane's frame is byte-identical on every render
        let done_frame = frame_of(&done, f0, "fast");
        assert_eq!(done_frame, frame_of(&done, f0, "fast"));
        assert!(done_frame.contains("✓ exit 0") && done_frame.contains('╭') && done_frame.contains('╰'));
        // the status sits one char further left: a ─ between it and the corner
        assert!(
            strip_ansi(&done_frame).contains("✓ exit 0─╯"),
            "bottom border must hold the extra fill dash"
        );
        // finished panes render exactly once, then the tick loop skips them
        // even when late reader output bumps their seq
        assert!(needs_repaint(false, 9, 8, true, false)); // the one final repaint
        assert!(!needs_repaint(false, 12, 9, true, true)); // straggler output: frozen
        assert!(needs_repaint(false, 5, 4, false, false)); // running and dirty
        assert!(!needs_repaint(false, 4, 4, false, false)); // running, nothing new
        assert!(needs_repaint(true, 9, 9, true, true)); // page switch repaints all
        // repainting the running neighbor never writes into the finished
        // pane's region: every cursor position in its frame stays in its own
        let nf = frame_of(&running, f1, "slow");
        let s = nf.as_bytes();
        let mut k = 0;
        while k < s.len() {
            if s[k] == 0x1b && k + 1 < s.len() && s[k + 1] == b'[' {
                let mut j = k + 2;
                let start = j;
                while j < s.len() && (s[j].is_ascii_digit() || s[j] == b';') {
                    j += 1;
                }
                if j < s.len() && s[j] == b'H' {
                    // strict "row;colH" (two integer fields), sgr runs have more
                    let text = std::str::from_utf8(&s[start..j]).unwrap();
                    let mut it = text.split(';');
                    let (row, col) = (it.next().unwrap().parse::<usize>().unwrap(), it.next().unwrap().parse::<usize>().unwrap());
                    if it.next().is_none() {
                        assert!((f1.top..=f1.top + f1.body_h + 1).contains(&row), "row {} escapes the running pane", row);
                        assert!((f1.left..f1.left + f1.w).contains(&col), "col {} escapes the running pane", col);
                    }
                }
                k = j;
            }
            k += 1;
        }
    }

    #[test]
    fn four_panes_fit_one_window() {
        // the reported bug: 4 scripts -> 2x2 grid, and only 2 panes were on
        // screen because a scrollback-poisoned height pushed row 2 off it
        let l = layout(100, 30, 2, 4);
        assert_eq!((l.cols, l.rows), (2, 2));
        assert_eq!(l.pages, 1, "2x2 grid must fit one page at 100x30");
        // every pane's whole frame (top border .. bottom border) sits inside
        // the visible window, both grid rows included
        for i in 0..4 {
            let f = pane_frame(&l, 2, 0, i);
            assert!(f.top >= 3);
            assert!(
                f.top + f.body_h + 1 <= 30,
                "pane {} bottom row {} overflows the 30-row window",
                i,
                f.top + f.body_h + 1
            );
        }
    }

    #[test]
    fn relayout_fits_the_resized_window() {
        // a mid-run resize re-derives the layout: at every new size the
        // recomputed frames must stay inside the new window (same content,
        // new geometry)
        for (w, h) in [(120usize, 40usize), (80, 20), (50, 12)] {
            let l = layout(w, h, 2, 4);
            for i in 0..4 {
                let f = pane_frame(&l, 2, 0, i);
                assert!(f.top >= 3, "pane {} header row below the wordmark at {}x{}", i, w, h);
                assert!(
                    f.left + f.w <= w,
                    "pane {} right edge {} overflows {} cols at {}x{}",
                    i,
                    f.left + f.w,
                    w,
                    w,
                    h
                );
                assert!(
                    f.top + f.body_h + 1 <= h,
                    "pane {} bottom row {} overflows {} rows at {}x{}",
                    i,
                    f.top + f.body_h + 1,
                    h,
                    w,
                    h
                );
            }
        }
        // shrinking below one grid row paginates: the current page must be
        // clamped into the new page count
        let l = layout(50, 6, 2, 4); // pane_h 3, one pane row per page -> 2 pages
        assert_eq!(l.pages, 2);
        let page = 1.min(l.pages - 1);
        assert_eq!(page, 1);
        let l = layout(50, 5, 2, 4); // even tighter: still at least one page
        assert!(l.pages >= 1);
    }

    #[test]
    fn resize_poll_gate() {
        // direct console queries (windows) poll every tick; stty-spawning
        // unix polls every 10th tick
        assert!(size_poll_tick(0, true) && size_poll_tick(7, true));
        assert!(size_poll_tick(0, false) && size_poll_tick(10, false));
        assert!(!size_poll_tick(1, false) && !size_poll_tick(9, false));
    }

    #[test]
    fn window_sanity_clamp() {
        // plausible_window moved to src/cmds/_progress.rs (shared with the bar)
        use crate::cmds::progress::plausible_window;
        assert!(plausible_window(80, 24));
        assert!(plausible_window(500, 200));
        assert!(!plausible_window(120, 9001)); // mode con scrollback-poisoned value
        assert!(!plausible_window(0, 24));
    }

    /// every positioned line in a paint frame covers exactly w visible
    /// columns (positioned = `ESC[row;colH`, content counted after stripping
    /// all escapes, so sgr bytes cost no width)
    fn covers_exact_width(frame: &str, w: usize) -> bool {
        let b = frame.as_bytes();
        let mut starts: Vec<usize> = Vec::new();
        let mut k = 0;
        while k < b.len() {
            if b[k] == 0x1b && k + 1 < b.len() && b[k + 1] == b'[' {
                let mut j = k + 2;
                while j < b.len() && (b[j].is_ascii_digit() || b[j] == b';') {
                    j += 1;
                }
                if j < b.len() && b[j] == b'H' && j > k + 2 {
                    let head = std::str::from_utf8(&b[k + 2..j]).unwrap();
                    let mut it = head.split(';');
                    if it.next().map(|x| !x.is_empty()).unwrap_or(false)
                        && it.next().map(|x| !x.is_empty()).unwrap_or(false)
                        && it.next().is_none()
                    {
                        starts.push(k);
                    }
                }
            }
            k += 1;
        }
        if starts.is_empty() {
            return false;
        }
        for n in 0..starts.len() {
            let from = starts[n];
            let to = if n + 1 < starts.len() { starts[n + 1] } else { b.len() };
            let mut m = from + 2;
            while m < to && (b[m].is_ascii_digit() || b[m] == b';') {
                m += 1;
            }
            m += 1; // the 'H'
            let vis = strip_ansi(std::str::from_utf8(&b[m..to]).unwrap()).chars().count();
            if vis != w {
                return false;
            }
        }
        true
    }

    #[test]
    fn pane_paint_covers_its_region_without_line_erases() {
        // the left-pane vanish: put() used to open each write with ESC[2K,
        // which erases the WHOLE row from any column, the right pane's
        // repaints wiped the left pane's columns on every tick. panes must
        // overwrite their exact region instead, never erasing
        let running: Pane = Arc::new(Mutex::new(PaneInner {
            lines: VecDeque::from(["mid 1".to_string(), "mid 2".to_string()]),
            seq: 4,
            stage: "mid".to_string(),
            done: false,
            code: 0,
        }));
        let l = layout(100, 30, 2, 2);
        let f = pane_frame(&l, 2, 0, 1);
        let mut s = String::new();
        paint(&mut s, &running, f, "slow", false);
        assert!(!s.contains("\x1b[2K"), "pane paint must not emit whole-line erases");
        // every positioned line covers exactly the pane's width in visible
        // chars, so the overwrite is complete and neighbors stay untouched
        assert!(covers_exact_width(&s, f.w), "pane lines must cover exactly their width");
        // degenerate tiny pane: a long status clamps so every line stays w
        // wide, the fully stripped frame is title+body+bottom, no overflow
        let done: Pane = Arc::new(Mutex::new(PaneInner {
            lines: VecDeque::new(),
            seq: 2,
            stage: String::new(),
            done: true,
            code: 255,
        }));
        let tiny = Frame { top: 3, left: 1, w: 10, body_h: 1 };
        let mut ts = String::new();
        paint(&mut ts, &done, tiny, "x", false);
        assert!(!ts.contains("\x1b[2K"));
        assert_eq!(strip_ansi(&ts).chars().count(), 3 * tiny.w, "tiny pane must stay within its width");
        assert!(ts.contains("✗ exit"), "clamped status keeps its meaning");
    }

    #[test]
    fn sgr_passes_through_and_truncates_cleanly() {
        // sanitizer keeps sgr, strips every other escape, tracks open colour
        let mut st: Option<String> = None;
        assert_eq!(
            sanitize_sgr("\x1b[31mred\x1b[0m ok \x1b[2K\x1b[?25h.", &mut st),
            "\x1b[31mred\x1b[0m ok ."
        );
        assert_eq!(st, None, "line ended with reset -> nothing carried");
        assert_eq!(sanitize_sgr("\x1b[31;1mhot", &mut st), "\x1b[31;1mhot");
        assert_eq!(st.as_deref(), Some("\x1b[31;1m"));
        assert_eq!(sanitize_sgr("more", &mut st), "more");
        assert_eq!(st.as_deref(), Some("\x1b[31;1m"));
        assert_eq!(sanitize_sgr("\x1b[mdone", &mut st), "\x1b[mdone");
        assert_eq!(st, None);
        // carried state frames the stored line: re-opened at the start,
        // closed at the end, plain lines untouched
        assert_eq!(with_sgr_frame("more".into(), &Some("\x1b[31;1m".into()), true), "\x1b[31;1mmore\x1b[0m");
        assert_eq!(with_sgr_frame("open".into(), &None, true), "open\x1b[0m");
        assert_eq!(with_sgr_frame("plain".into(), &None, false), "plain");
        // a coloured line longer than the pane truncates to the correct
        // VISIBLE width with sequences intact and a clean end
        let pane: Pane = Arc::new(Mutex::new(PaneInner {
            lines: VecDeque::from([format!("\x1b[35m{}\x1b[0m", "x".repeat(60)), "plain after".to_string()]),
            seq: 3,
            stage: String::new(),
            done: false,
            code: 0,
        }));
        let l = layout(100, 30, 2, 2);
        let f = pane_frame(&l, 2, 0, 1);
        let mut s = String::new();
        paint(&mut s, &pane, f, "slow", false);
        assert!(!s.contains("\x1b[2K"));
        let budget = f.w - 2 - 1; // inner minus the leading pad space
        assert!(
            s.contains(&format!("\x1b[35m{}\x1b[0m", "x".repeat(budget))),
            "truncated coloured line keeps its opening sgr and ends closed"
        );
        assert!(!s.contains("x".repeat(budget + 1).as_str()), "truncation is by visible chars");
        assert!(covers_exact_width(&s, f.w), "sgr bytes must cost no width");
    }

    #[test]
    fn page_indicator_folds_enter_hint_while_hanging() {
        // single-page hang: the reserved row shows just the hint
        let s = strip_ansi(&page_indicator(0, 1, 100, 2, true));
        assert!(s.contains("↵ enter to exit"));
        assert!(!s.contains("1/1"), "single-page hang replaces the counter with the hint");
        // multi-page hang: indicator + hint folded into the same row
        let s = strip_ansi(&page_indicator(1, 2, 100, 2, true));
        assert!(s.contains("2/2") && s.contains("↵ enter to exit"));
        // not hanging: unchanged, no hint
        let s = strip_ansi(&page_indicator(0, 2, 100, 2, false));
        assert!(s.contains("1/2") && !s.contains("enter"));
        let s = strip_ansi(&page_indicator(0, 1, 100, 2, false));
        assert!(s.contains("1/1"));
    }

    #[test]
    fn pane_origins_stack_without_gaps() {
        let l = layout(100, 30, 2, 5); // cols=3 rows=2, everything fits
        // first row of panes starts right under the header
        let f0 = pane_frame(&l, 2, 0, 0);
        assert_eq!((f0.top, f0.left, f0.w, f0.body_h), (3, 1, l.pane_w, l.pane_h - 2));
        // 1-space gutter between horizontally adjacent panes
        assert_eq!(pane_frame(&l, 2, 0, 1).left, 1 + l.pane_w + 1);
        // second row sits directly on the first (no blank row between)
        assert_eq!(pane_frame(&l, 2, 0, 3).top, 3 + l.pane_h);
    }
}
