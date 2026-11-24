//! tmux control-mode backend.
//!
//! One `tmux -C` client owns the session. tmux windows map to tabs and tmux
//! panes map to split panes. A dedicated reader thread only parses the control
//! stream and enqueues bytes/notifications; every `Term` is mutated by
//! [`Mux::poll`] on the caller's thread.
//!
//! Structural state (windows, panes, layouts, focus) is not reconstructed from
//! individual control events. Events merely set a dirty flag; the next
//! [`Mux::poll`] re-reads the authoritative state with short-lived `tmux`
//! clients on the same socket. Control clients have no reliable transactional
//! view of layout edits, so re-querying is both simpler and more robust.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tessera_term::Term;

use crate::mux::{
    clamp_ratio, Layout, Mux, MuxEvent, PaneId, PaneInfo, Snapshot, SplitDir, TabInfo,
};

/// Unit separator: safe for tmux's `-F` output, which sanitises control
/// characters in names but keeps layout fields numeric.
const SEP: char = '\u{1f}';

const WINDOW_FORMAT: &str = "#{window_id}\u{1f}#{window_layout}\u{1f}#{window_index}\u{1f}#{window_active}\u{1f}#{window_name}";
const PANE_FORMAT: &str = "#{pane_id}\u{1f}#{pane_index}\u{1f}#{window_id}\u{1f}#{pane_width}\u{1f}#{pane_height}\u{1f}#{pane_active}\u{1f}#{pane_left}\u{1f}#{pane_top}\u{1f}#{pane_current_path}\u{1f}#{pane_title}";

/// How to start (or attach to) the tmux session backing a [`TmuxMux`].
#[derive(Clone, Debug)]
pub struct TmuxOptions {
    pub session: String,
    pub cols: usize,
    pub rows: usize,
    /// Use a named server (`tmux -L <socket>`). Tests pass a unique name so
    /// they never touch the user's default server.
    pub socket: Option<String>,
}

impl TmuxOptions {
    pub fn new(session: impl Into<String>, cols: usize, rows: usize) -> Self {
        Self {
            session: session.into(),
            cols: cols.max(1),
            rows: rows.max(1),
            socket: None,
        }
    }
}

/// True when a `tmux` binary that answers `tmux -V` is on `PATH`.
pub fn available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

enum ReaderMsg {
    /// Escaped `%output` payload, already decoded to bytes.
    Output(PaneId, Vec<u8>),
    /// A structural event (`%window-add`, `%layout-change`, ...).
    Dirty,
    /// `%exit` or the control stream closed.
    Exit,
}

enum Parsed {
    Output(PaneId, Vec<u8>),
    Dirty,
    Exit,
}

/// A tmux control-mode client presented through the shared [`Mux`] contract.
pub struct TmuxMux {
    session: String,
    socket: Option<String>,
    cols: usize,
    rows: usize,
    stdin: Option<ChildStdin>,
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
    rx: Receiver<ReaderMsg>,
    wake_tx: async_channel::Sender<()>,
    wake_rx: async_channel::Receiver<()>,
    panes: HashMap<PaneId, Term>,
    snapshot: Snapshot,
    /// Set by actions and by reader events; consumed by `poll`.
    dirty: bool,
    exited: bool,
}

impl TmuxMux {
    /// Spawn the control client and wait for the session's first window.
    ///
    /// `new-session -A` attaches to an existing session of the same name, so
    /// reconnecting to a detached session is the normal path.
    pub fn new(options: TmuxOptions) -> io::Result<Self> {
        let mut command = Command::new("tmux");
        if let Some(socket) = &options.socket {
            command.arg("-L").arg(socket);
        }
        command
            .args(["-f", "/dev/null", "-u", "-C", "new-session", "-A", "-s"])
            .arg(&options.session)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("tmux client stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("tmux client stdout unavailable"))?;

        let (tx, rx) = mpsc::channel();
        let (wake_tx, wake_rx) = async_channel::bounded(1);
        let reader_tx = tx.clone();
        let reader_wake = wake_tx.clone();
        let reader = thread::Builder::new()
            .name("tmux-control-reader".into())
            .spawn(move || read_loop(stdout, reader_tx, reader_wake))
            .map_err(|err| {
                let _ = child.kill();
                io::Error::other(err)
            })?;
        drop(tx);

        let mut mux = Self {
            session: options.session,
            socket: options.socket,
            cols: options.cols.max(1),
            rows: options.rows.max(1),
            stdin: Some(stdin),
            child: Some(child),
            reader: Some(reader),
            rx,
            wake_tx,
            wake_rx,
            panes: HashMap::new(),
            snapshot: Snapshot::default(),
            dirty: false,
            exited: false,
        };

        let _ = mux.send_line(&format!("refresh-client -C {}x{}", mux.cols, mux.rows));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if mux.refresh_state().is_ok() && !mux.snapshot.tabs.is_empty() {
                break;
            }
            let status = mux.child.as_mut().and_then(|child| child.try_wait().ok());
            if let Some(Some(status)) = status {
                mux.shutdown();
                return Err(io::Error::other(format!(
                    "tmux client exited before the session was ready ({status})"
                )));
            }
            if Instant::now() >= deadline {
                mux.shutdown();
                return Err(io::Error::other("tmux session did not start"));
            }
            thread::sleep(Duration::from_millis(20));
        }
        Ok(mux)
    }

    fn target(&self) -> String {
        // `=` forces an exact session-name match, so a sibling session with a
        // common prefix can never be addressed by accident.
        format!("={}", self.session)
    }

    fn send_line(&mut self, line: &str) -> io::Result<()> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        };
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    fn poke(&self) {
        let _ = self.wake_tx.try_send(());
    }

    /// Run a one-shot tmux command on the same server and return stdout.
    fn run_tmux(&self, args: &[&str]) -> io::Result<String> {
        let mut command = Command::new("tmux");
        if let Some(socket) = &self.socket {
            command.arg("-L").arg(socket);
        }
        command.args(["-f", "/dev/null"]);
        command.args(args);
        let output = command.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(io::Error::other(format!(
                "tmux {} failed: {}",
                args.join(" "),
                stderr.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Re-read windows and panes and rebuild the cached snapshot.
    fn refresh_state(&mut self) -> io::Result<()> {
        let target = self.target();
        let windows = self.run_tmux(&["list-windows", "-t", &target, "-F", WINDOW_FORMAT])?;
        let panes = self.run_tmux(&["list-panes", "-s", "-t", &target, "-F", PANE_FORMAT])?;

        let mut window_rows: Vec<WindowRow> =
            windows.lines().filter_map(parse_window_line).collect();
        window_rows.sort_by_key(|window| window.index);

        let mut panes_by_window: HashMap<u64, Vec<PaneRow>> = HashMap::new();
        for pane in panes.lines().filter_map(parse_pane_line) {
            panes_by_window.entry(pane.window).or_default().push(pane);
        }

        let mut tabs = Vec::new();
        let mut active_tab = 0;
        for window in window_rows {
            let Some(mut rows) = panes_by_window.remove(&window.id) else {
                continue;
            };
            if rows.is_empty() {
                continue;
            }
            rows.sort_by_key(|pane| pane.index);

            let layout = match parse_layout(&window.layout) {
                Some(node) if layout_covers(&node, &rows) => build_layout(&node),
                _ => rect_layout(&rows),
            };
            let active = rows
                .iter()
                .find(|pane| pane.active)
                .map_or_else(|| layout.first_leaf(), |pane| pane.id);
            let title = rows
                .iter()
                .find(|pane| pane.active)
                .filter(|pane| !pane.title.is_empty())
                .map(|pane| pane.title.clone())
                .unwrap_or_else(|| window.name.clone());
            let pane_infos = rows
                .iter()
                .map(|pane| PaneInfo {
                    id: pane.id,
                    title: pane.title.clone(),
                    cwd: pane.cwd.clone(),
                    size: (pane.width, pane.height),
                })
                .collect();

            for pane in &rows {
                let width = pane.width.max(1);
                let height = pane.height.max(1);
                let term = self
                    .panes
                    .entry(pane.id)
                    .or_insert_with(|| Term::new(width, height));
                if term.cols() != width || term.rows() != height {
                    term.resize(width, height);
                }
            }

            if window.active {
                active_tab = tabs.len();
            }
            tabs.push(TabInfo {
                id: window.id,
                title,
                layout,
                active,
                panes: pane_infos,
            });
        }

        let alive: HashSet<PaneId> = tabs
            .iter()
            .flat_map(|tab| tab.panes.iter().map(|pane| pane.id))
            .collect();
        self.panes.retain(|id, _| alive.contains(id));
        self.snapshot = Snapshot { tabs, active_tab };
        Ok(())
    }

    /// Re-query after an action and ask the UI to refresh.
    fn refresh_after_action(&mut self) {
        self.dirty = false;
        if self.refresh_state().is_err() {
            self.dirty = true;
        }
        self.poke();
    }

    fn split_ratio_target(&self, tab_id: u64, path: &[bool]) -> Option<(SplitDir, usize, PaneId)> {
        let tab = self.snapshot.tabs.iter().find(|tab| tab.id == tab_id)?;
        let (dir, avail) = split_geometry(&tab.layout, path, self.cols, self.rows)?;
        let leaf = second_leaf(&tab.layout, path)?;
        Some((dir, avail, leaf))
    }

    fn shutdown_inner(&mut self) {
        if self.child.is_none() && self.reader.is_none() {
            self.stdin = None;
            return;
        }
        // Detach first so a cooperative server keeps the session alive. The
        // kill below only reaps a client that ignored the detach or is stuck.
        let _ = self.send_line("detach-client");
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        self.exited = true;
        self.poke();
    }
}

impl Mux for TmuxMux {
    fn snapshot(&self) -> Snapshot {
        self.snapshot.clone()
    }

    fn term(&self, pane: PaneId) -> Option<&Term> {
        self.panes.get(&pane)
    }

    fn poll(&mut self) -> Vec<MuxEvent> {
        let mut events = Vec::new();
        let mut dirty = self.dirty;
        self.dirty = false;
        let mut saw_exit = self.exited;

        loop {
            match self.rx.try_recv() {
                Ok(ReaderMsg::Output(pane, bytes)) => {
                    let width = self.cols;
                    let height = self.rows;
                    let term = self
                        .panes
                        .entry(pane)
                        .or_insert_with(|| Term::new(width, height));
                    term.feed(&bytes);
                    if !events
                        .iter()
                        .any(|event| matches!(event, MuxEvent::Output(id) if *id == pane))
                    {
                        events.push(MuxEvent::Output(pane));
                    }
                }
                Ok(ReaderMsg::Dirty) => dirty = true,
                Ok(ReaderMsg::Exit) => saw_exit = true,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    saw_exit = true;
                    break;
                }
            }
        }

        if dirty && !self.exited {
            match self.refresh_state() {
                Ok(()) => events.push(MuxEvent::Tabs),
                Err(_) => self.dirty = true,
            }
        }

        if saw_exit && !self.exited {
            self.exited = true;
            events.push(MuxEvent::Exit);
        }
        events
    }

    fn wake(&self) -> async_channel::Receiver<()> {
        self.wake_rx.clone()
    }

    fn write(&mut self, pane: PaneId, bytes: &[u8]) {
        let mut printable: Vec<u8> = Vec::new();
        let mut hex: Vec<u8> = Vec::new();
        let mut commands: Vec<String> = Vec::new();
        for &byte in bytes {
            if byte < 0x20 || byte == 0x7f {
                flush_printable(pane, &mut printable, &mut commands);
                hex.push(byte);
            } else {
                flush_hex(pane, &mut hex, &mut commands);
                printable.push(byte);
                if printable.len() >= 1024 {
                    flush_printable(pane, &mut printable, &mut commands);
                }
            }
        }
        flush_printable(pane, &mut printable, &mut commands);
        flush_hex(pane, &mut hex, &mut commands);

        for command in commands {
            if self.send_line(&command).is_err() {
                break;
            }
        }
    }

    fn resize_client(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        self.cols = cols;
        self.rows = rows;
        if self
            .send_line(&format!("refresh-client -C {cols}x{rows}"))
            .is_ok()
        {
            self.dirty = true;
            self.poke();
        }
    }

    fn new_tab(&mut self, cwd: Option<&Path>) {
        let target = self.target();
        let cwd = cwd.map(|path| path.to_string_lossy().into_owned());
        let mut args = vec!["new-window", "-t", target.as_str()];
        if let Some(cwd) = cwd.as_deref() {
            args.push("-c");
            args.push(cwd);
        }
        let _ = self.run_tmux(&args);
        self.dirty = true;
        self.poke();
    }

    fn close_tab(&mut self, tab: u64) {
        let target = format!("@{tab}");
        let _ = self.run_tmux(&["kill-window", "-t", &target]);
        self.dirty = true;
        self.poke();
    }

    fn select_tab(&mut self, tab: u64) {
        let target = format!("@{tab}");
        let _ = self.run_tmux(&["select-window", "-t", &target]);
        self.dirty = true;
        self.poke();
    }

    fn split(&mut self, pane: PaneId, dir: SplitDir) -> Option<PaneId> {
        let target = format!("%{pane}");
        let direction = match dir {
            SplitDir::Horizontal => "-h",
            SplitDir::Vertical => "-v",
        };
        let new_pane = self
            .run_tmux(&[
                "split-window",
                direction,
                "-t",
                &target,
                "-P",
                "-F",
                "#{pane_id}",
            ])
            .ok()
            .and_then(|output| parse_pane_id(output.trim()));
        if new_pane.is_some() {
            self.dirty = true;
            self.poke();
        }
        new_pane
    }

    fn close_pane(&mut self, pane: PaneId) {
        let target = format!("%{pane}");
        let _ = self.run_tmux(&["kill-pane", "-t", &target]);
        self.dirty = true;
        self.poke();
    }

    fn focus_pane(&mut self, pane: PaneId) {
        let target = format!("%{pane}");
        let _ = self.run_tmux(&["select-pane", "-t", &target]);
        self.dirty = true;
        self.poke();
    }

    fn set_split_ratio(&mut self, tab: u64, path: &[bool], ratio: f32) {
        let Some((dir, avail, leaf)) = self.split_ratio_target(tab, path) else {
            return;
        };
        let ratio = clamp_ratio(ratio);
        let avail = avail.max(1);
        let first = ((avail as f32) * ratio).round() as usize;
        let first = first.clamp(1, avail.saturating_sub(1).max(1));
        let second = avail.saturating_sub(first).max(1);
        let target = format!("%{leaf}");
        let size = second.to_string();
        let flag = match dir {
            SplitDir::Horizontal => "-x",
            SplitDir::Vertical => "-y",
        };
        let _ = self.run_tmux(&["resize-pane", "-t", &target, flag, &size]);
        self.refresh_after_action();
    }

    fn shutdown(&mut self) {
        self.shutdown_inner();
    }
}

impl Drop for TmuxMux {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

// ---------------------------------------------------------------------------
// Control-stream parsing
// ---------------------------------------------------------------------------

fn read_loop(stdout: ChildStdout, tx: mpsc::Sender<ReaderMsg>, wake: async_channel::Sender<()>) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        while line
            .last()
            .is_some_and(|&byte| byte == b'\n' || byte == b'\r')
        {
            line.pop();
        }
        let Some(parsed) = parse_line(&line) else {
            continue;
        };
        let exit = matches!(parsed, Parsed::Exit);
        let message = match parsed {
            Parsed::Output(pane, bytes) => ReaderMsg::Output(pane, bytes),
            Parsed::Dirty => ReaderMsg::Dirty,
            Parsed::Exit => ReaderMsg::Exit,
        };
        if tx.send(message).is_err() {
            break;
        }
        let _ = wake.try_send(());
        if exit {
            break;
        }
    }
    let _ = tx.send(ReaderMsg::Exit);
    let _ = wake.try_send(());
}

fn parse_line(line: &[u8]) -> Option<Parsed> {
    if let Some(rest) = line.strip_prefix(b"%output ") {
        parse_output(rest, false)
    } else if let Some(rest) = line.strip_prefix(b"%extended-output ") {
        parse_output(rest, true)
    } else if line.starts_with(b"%exit") {
        Some(Parsed::Exit)
    } else if line.starts_with(b"%begin")
        || line.starts_with(b"%end")
        || line.starts_with(b"%error")
    {
        None
    } else if line.starts_with(b"%") {
        // Every other control message reports a structural change. Marking the
        // snapshot dirty is cheap and deliberately tolerant of tmux adding new
        // event names.
        Some(Parsed::Dirty)
    } else {
        None
    }
}

fn parse_output(rest: &[u8], extended: bool) -> Option<Parsed> {
    let (id_token, mut data) = match rest.iter().position(|&byte| byte == b' ') {
        Some(index) => (&rest[..index], &rest[index + 1..]),
        None => (rest, &[][..]),
    };
    let pane = parse_pane_id(std::str::from_utf8(id_token).ok()?)?;
    if extended {
        let marker = data.windows(3).position(|window| window == b" : ")?;
        data = &data[marker + 3..];
    }
    Some(Parsed::Output(pane, decode_escapes(data)))
}

fn parse_pane_id(token: &str) -> Option<PaneId> {
    token.strip_prefix('%')?.parse().ok()
}

/// tmux escapes bytes outside printable ASCII (and backslash) as `\ooo`.
fn decode_escapes(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut index = 0;
    while index < data.len() {
        if data[index] == b'\\'
            && index + 3 < data.len()
            && data[index + 1..index + 4]
                .iter()
                .all(|byte| (b'0'..=b'7').contains(byte))
        {
            let value = (data[index + 1] - b'0') * 64
                + (data[index + 2] - b'0') * 8
                + (data[index + 3] - b'0');
            out.push(value);
            index += 4;
        } else {
            out.push(data[index]);
            index += 1;
        }
    }
    out
}

/// Quote a literal `send-keys -l` payload for tmux's command parser. Besides
/// the usual backslash and quote escapes, `$` must be escaped or tmux expands
/// it as an environment variable.
fn quote_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '$' => out.push_str("\\$"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn flush_printable(pane: PaneId, buffer: &mut Vec<u8>, commands: &mut Vec<String>) {
    if buffer.is_empty() {
        return;
    }
    match std::str::from_utf8(buffer) {
        Ok(text) => commands.push(format!("send-keys -t %{pane} -l {}", quote_literal(text))),
        Err(_) => commands.push(hex_command(pane, buffer)),
    }
    buffer.clear();
}

fn flush_hex(pane: PaneId, buffer: &mut Vec<u8>, commands: &mut Vec<String>) {
    if buffer.is_empty() {
        return;
    }
    commands.push(hex_command(pane, buffer));
    buffer.clear();
}

fn hex_command(pane: PaneId, bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut command = format!("send-keys -t %{pane} -H");
    for byte in bytes {
        let _ = write!(command, " {byte:02x}");
    }
    command
}

// ---------------------------------------------------------------------------
// State queries
// ---------------------------------------------------------------------------

struct WindowRow {
    id: u64,
    index: u64,
    name: String,
    active: bool,
    layout: String,
}

struct PaneRow {
    id: PaneId,
    index: u64,
    window: u64,
    width: usize,
    height: usize,
    active: bool,
    title: String,
    cwd: Option<PathBuf>,
    left: usize,
    top: usize,
}

fn parse_window_line(line: &str) -> Option<WindowRow> {
    let mut parts = line.splitn(5, SEP);
    let id = parse_number(parts.next()?.strip_prefix('@')?)?;
    let layout = parts.next()?.to_string();
    let index = parse_number(parts.next()?)?;
    let active = parts.next()? == "1";
    let name = parts.next().unwrap_or_default().to_string();
    Some(WindowRow {
        id,
        index,
        name,
        active,
        layout,
    })
}

fn parse_pane_line(line: &str) -> Option<PaneRow> {
    let mut parts = line.splitn(10, SEP);
    let id = parse_pane_id(parts.next()?)?;
    let index = parse_number(parts.next()?)?;
    let window = parse_number(parts.next()?.strip_prefix('@')?)?;
    let width = parse_number(parts.next()?)? as usize;
    let height = parse_number(parts.next()?)? as usize;
    let active = parts.next()? == "1";
    let left = parse_number(parts.next()?)? as usize;
    let top = parse_number(parts.next()?)? as usize;
    let cwd = match parts.next().unwrap_or_default() {
        "" => None,
        path => Some(PathBuf::from(path)),
    };
    let title = parts.next().unwrap_or_default().to_string();
    Some(PaneRow {
        id,
        index,
        window,
        width,
        height,
        active,
        title,
        cwd,
        left,
        top,
    })
}

fn parse_number(token: &str) -> Option<u64> {
    token.parse().ok()
}

// ---------------------------------------------------------------------------
// Layout strings
// ---------------------------------------------------------------------------

/// A parsed `#{window_layout}` cell. tmux's grammar is
/// `[checksum,]WxH,X,Y` followed by either `,<pane-id>` (leaf) or
/// `{child,child,...}` (side by side) / `[child,child,...]` (stacked).
struct LayoutNode {
    pane: Option<PaneId>,
    dir: SplitDir,
    width: usize,
    height: usize,
    children: Vec<LayoutNode>,
}

impl LayoutNode {
    fn extent(&self, dir: SplitDir) -> usize {
        match dir {
            SplitDir::Horizontal => self.width,
            SplitDir::Vertical => self.height,
        }
    }
}

fn parse_layout(text: &str) -> Option<LayoutNode> {
    let bytes = text.as_bytes();
    let mut position = 0;
    if let Some(comma) = bytes.iter().position(|&byte| byte == b',') {
        if !bytes[..comma].contains(&b'x') {
            position = comma + 1;
        }
    }
    let node = parse_layout_node(bytes, &mut position)?;
    Some(node)
}

fn parse_layout_node(bytes: &[u8], position: &mut usize) -> Option<LayoutNode> {
    let width = parse_layout_number(bytes, position)? as usize;
    if bytes.get(*position) != Some(&b'x') {
        return None;
    }
    *position += 1;
    let height = parse_layout_number(bytes, position)? as usize;
    if bytes.get(*position) != Some(&b',') {
        return None;
    }
    *position += 1;
    parse_layout_number(bytes, position)?; // x offset
    if bytes.get(*position) != Some(&b',') {
        return None;
    }
    *position += 1;
    parse_layout_number(bytes, position)?; // y offset

    match bytes.get(*position).copied()? {
        b',' => {
            *position += 1;
            let pane = parse_layout_number(bytes, position)?;
            Some(LayoutNode {
                pane: Some(pane),
                dir: SplitDir::Horizontal,
                width,
                height,
                children: Vec::new(),
            })
        }
        open @ (b'{' | b'[') => {
            *position += 1;
            let dir = if open == b'{' {
                SplitDir::Horizontal
            } else {
                SplitDir::Vertical
            };
            let mut children = Vec::new();
            loop {
                children.push(parse_layout_node(bytes, position)?);
                match bytes.get(*position).copied()? {
                    b',' => *position += 1,
                    b'}' | b']' => {
                        *position += 1;
                        break;
                    }
                    _ => return None,
                }
            }
            if children.is_empty() {
                return None;
            }
            Some(LayoutNode {
                pane: None,
                dir,
                width,
                height,
                children,
            })
        }
        _ => None,
    }
}

fn parse_layout_number(bytes: &[u8], position: &mut usize) -> Option<u64> {
    let start = *position;
    while bytes.get(*position).is_some_and(u8::is_ascii_digit) {
        *position += 1;
    }
    if start == *position {
        return None;
    }
    std::str::from_utf8(&bytes[start..*position])
        .ok()?
        .parse()
        .ok()
}

fn layout_covers(node: &LayoutNode, panes: &[PaneRow]) -> bool {
    let mut ids = Vec::new();
    collect_layout_ids(node, &mut ids);
    if ids.len() != panes.len() || ids.is_empty() {
        return false;
    }
    let got: HashSet<PaneId> = ids.iter().copied().collect();
    let want: HashSet<PaneId> = panes.iter().map(|pane| pane.id).collect();
    got.len() == ids.len() && got == want
}

fn collect_layout_ids(node: &LayoutNode, out: &mut Vec<PaneId>) {
    match node.pane {
        Some(id) => out.push(id),
        None => {
            for child in &node.children {
                collect_layout_ids(child, out);
            }
        }
    }
}

fn build_layout(node: &LayoutNode) -> Layout {
    match node.pane {
        Some(id) => Layout::leaf(id),
        None => build_layout_children(&node.children, node.dir, node.extent(node.dir)),
    }
}

fn build_layout_children(children: &[LayoutNode], dir: SplitDir, total: usize) -> Layout {
    if children.len() == 1 {
        return build_layout(&children[0]);
    }
    let first = &children[0];
    let avail = total.saturating_sub(1).max(1);
    let ratio = clamp_ratio(first.extent(dir) as f32 / avail as f32);
    let rest = total.saturating_sub(first.extent(dir) + 1).max(1);
    Layout::Split {
        dir,
        ratio,
        first: Box::new(build_layout(first)),
        second: Box::new(build_layout_children(&children[1..], dir, rest)),
    }
}

/// Deterministic fallback used only when a layout string cannot be parsed:
/// recursively cut the pane rectangles along the widest clean gap.
fn rect_layout(panes: &[PaneRow]) -> Layout {
    let refs: Vec<&PaneRow> = panes.iter().collect();
    rect_layout_refs(&refs)
}

fn rect_layout_refs(panes: &[&PaneRow]) -> Layout {
    if panes.len() == 1 {
        return Layout::leaf(panes[0].id);
    }
    if let Some(layout) = rect_split(panes, SplitDir::Horizontal) {
        return layout;
    }
    if let Some(layout) = rect_split(panes, SplitDir::Vertical) {
        return layout;
    }
    let mut layout = Layout::leaf(panes[0].id);
    for pane in &panes[1..] {
        let leaf = layout.first_leaf();
        layout.split(leaf, pane.id, SplitDir::Horizontal);
    }
    layout
}

fn rect_split(panes: &[&PaneRow], dir: SplitDir) -> Option<Layout> {
    let mut sorted: Vec<&PaneRow> = panes.to_vec();
    sorted.sort_by_key(|pane| match dir {
        SplitDir::Horizontal => (pane.left, pane.top),
        SplitDir::Vertical => (pane.top, pane.left),
    });
    let origin = sorted.iter().map(|pane| axis_start(pane, dir)).min()?;
    let total = sorted.iter().map(|pane| axis_end(pane, dir)).max()? - origin;
    let avail = total.saturating_sub(1).max(1);
    for split in 1..sorted.len() {
        let first_end = sorted[..split]
            .iter()
            .map(|pane| axis_end(pane, dir))
            .max()?;
        let second_start = sorted[split..]
            .iter()
            .map(|pane| axis_start(pane, dir))
            .min()?;
        if first_end < second_start {
            let ratio = clamp_ratio((first_end - origin) as f32 / avail as f32);
            return Some(Layout::Split {
                dir,
                ratio,
                first: Box::new(rect_layout_refs(&sorted[..split])),
                second: Box::new(rect_layout_refs(&sorted[split..])),
            });
        }
    }
    None
}

fn axis_start(pane: &PaneRow, dir: SplitDir) -> usize {
    match dir {
        SplitDir::Horizontal => pane.left,
        SplitDir::Vertical => pane.top,
    }
}

fn axis_end(pane: &PaneRow, dir: SplitDir) -> usize {
    match dir {
        SplitDir::Horizontal => pane.left + pane.width,
        SplitDir::Vertical => pane.top + pane.height,
    }
}

// ---------------------------------------------------------------------------
// Ratio edits
// ---------------------------------------------------------------------------

fn child_extents(
    dir: SplitDir,
    ratio: f32,
    width: usize,
    height: usize,
) -> (usize, usize, usize, usize) {
    match dir {
        SplitDir::Horizontal => {
            let avail = width.saturating_sub(1).max(2);
            let left = ((avail as f32 * clamp_ratio(ratio)).round() as usize).clamp(1, avail - 1);
            (left, height, avail - left, height)
        }
        SplitDir::Vertical => {
            let avail = height.saturating_sub(1).max(2);
            let top = ((avail as f32 * clamp_ratio(ratio)).round() as usize).clamp(1, avail - 1);
            (width, top, width, avail - top)
        }
    }
}

/// Available cells along the split direction for the split at `path`.
fn split_geometry(
    layout: &Layout,
    path: &[bool],
    cols: usize,
    rows: usize,
) -> Option<(SplitDir, usize)> {
    let mut node = layout;
    let mut width = cols.max(1);
    let mut height = rows.max(1);
    for step in path {
        let Layout::Split {
            dir,
            ratio,
            first,
            second,
        } = node
        else {
            return None;
        };
        let (fw, fh, sw, sh) = child_extents(*dir, *ratio, width, height);
        if *step {
            node = second;
            width = sw;
            height = sh;
        } else {
            node = first;
            width = fw;
            height = fh;
        }
    }
    let Layout::Split { dir, .. } = node else {
        return None;
    };
    let avail = match dir {
        SplitDir::Horizontal => width.saturating_sub(1).max(1),
        SplitDir::Vertical => height.saturating_sub(1).max(1),
    };
    Some((*dir, avail))
}

/// First leaf of the second subtree at `path`: the pane next to the divider
/// `resize-pane` has to move.
fn second_leaf(layout: &Layout, path: &[bool]) -> Option<PaneId> {
    let mut node = layout;
    for step in path {
        let Layout::Split { first, second, .. } = node else {
            return None;
        };
        node = if *step { second } else { first };
    }
    match node {
        Layout::Split { second, .. } => Some(second.first_leaf()),
        Layout::Leaf(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tessera_term::{Color, Row};

    static SOCKET_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    /// Owns a private tmux server for one test and guarantees it is killed,
    /// even when an assertion panics.
    struct TestServer {
        socket: String,
    }

    impl TestServer {
        fn new() -> Self {
            let sequence = SOCKET_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let socket = format!("tessera-test-{}-{sequence}", std::process::id());
            let _ = Command::new("tmux")
                .args(["-L", &socket, "kill-server"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            Self { socket }
        }

        fn options(&self, cols: usize, rows: usize) -> TmuxOptions {
            let mut options = TmuxOptions::new("tessera", cols, rows);
            options.socket = Some(self.socket.clone());
            options
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .args(["-L", &self.socket, "kill-server"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    fn poll_until<F>(mux: &mut TmuxMux, seconds: u64, mut ready: F)
    where
        F: FnMut(&TmuxMux) -> bool,
    {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            mux.poll();
            if ready(mux) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out; last snapshot: {:?}",
                mux.snapshot()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn screen_text(term: &Term) -> String {
        let mut text = String::new();
        for abs in 0..term.history_len() + term.rows() {
            if let Some(row) = term.line(abs) {
                push_row(&mut text, row);
            }
        }
        text
    }

    fn push_row(text: &mut String, row: &Row) {
        text.push_str(&row.text());
        text.push('\n');
    }

    fn has_red_red(term: &Term) -> bool {
        for y in 0..term.rows() {
            for x in 0..term.cols().saturating_sub(2) {
                let (Some(r), Some(e), Some(d)) =
                    (term.cell(x, y), term.cell(x + 1, y), term.cell(x + 2, y))
                else {
                    continue;
                };
                if r.ch == 'r'
                    && e.ch == 'e'
                    && d.ch == 'd'
                    && r.fg == Color::Indexed(1)
                    && e.fg == Color::Indexed(1)
                    && d.fg == Color::Indexed(1)
                {
                    return true;
                }
            }
        }
        false
    }

    fn first_pane(mux: &TmuxMux) -> PaneId {
        mux.snapshot()
            .tabs
            .first()
            .expect("initial tab")
            .layout
            .first_leaf()
    }

    #[test]
    fn escape_decoding_and_layout_parsing() {
        assert_eq!(
            decode_escapes(b"\\033[31mred\\033[0m\\134\\015\\012"),
            b"\x1b[31mred\x1b[0m\\\r\n".to_vec()
        );
        assert_eq!(decode_escapes(b"plain"), b"plain".to_vec());
        assert_eq!(
            quote_literal("a$b\\c\"d"),
            "\"a\\$b\\\\c\\\"d\"",
            "tmux expands $ and consumes backslash escapes in double quotes"
        );

        let node = parse_layout("8205,80x24,0,0{40x24,0,0,0,39x24,41,0,1}").expect("layout");
        let layout = build_layout(&node);
        assert_eq!(layout.sizes(80, 24), vec![(0, 40, 24), (1, 39, 24)]);

        let nested =
            parse_layout("d67e,80x24,0,0{40x24,0,0,0,39x24,41,0[39x12,41,0,1,39x11,41,13,2]}")
                .expect("nested layout");
        assert_eq!(
            build_layout(&nested).sizes(80, 24),
            vec![(0, 40, 24), (1, 39, 12), (2, 39, 11)]
        );
    }

    #[test]
    fn available_and_roundtrip() {
        assert!(available(), "tmux is required for this test");
        let server = TestServer::new();
        let mut mux = TmuxMux::new(server.options(80, 24)).expect("start tmux client");
        let pane = first_pane(&mux);

        mux.write(pane, b"printf 'TMUX-TOKEN\\n'\n");
        poll_until(&mut mux, 15, |mux| {
            mux.term(pane)
                .map(|term| screen_text(term).contains("TMUX-TOKEN"))
                .unwrap_or(false)
        });

        mux.write(pane, b"printf '\\033[31mred\\033[0m\\n'\n");
        poll_until(&mut mux, 15, |mux| {
            mux.term(pane).map(has_red_red).unwrap_or(false)
        });

        mux.shutdown();
        assert!(mux.child.is_none(), "client process must be reaped");
    }

    #[test]
    fn windows_become_tabs() {
        let server = TestServer::new();
        let mut mux = TmuxMux::new(server.options(80, 24)).expect("start tmux client");
        let first_tab = mux.snapshot().tabs[0].id;

        mux.new_tab(None);
        poll_until(&mut mux, 10, |mux| {
            let snapshot = mux.snapshot();
            snapshot.tabs.len() == 2
                && snapshot
                    .tabs
                    .iter()
                    .map(|tab| tab.id)
                    .collect::<HashSet<_>>()
                    .len()
                    == 2
        });

        mux.select_tab(first_tab);
        poll_until(&mut mux, 10, |mux| {
            let snapshot = mux.snapshot();
            snapshot
                .tabs
                .get(snapshot.active_tab)
                .is_some_and(|tab| tab.id == first_tab)
        });

        mux.shutdown();
    }

    #[test]
    fn split_creates_panes() {
        let server = TestServer::new();
        let mut mux = TmuxMux::new(server.options(80, 24)).expect("start tmux client");
        let first = first_pane(&mux);

        let second = mux
            .split(first, SplitDir::Horizontal)
            .expect("split should return the new pane");
        assert_ne!(first, second);

        poll_until(&mut mux, 10, |mux| {
            mux.snapshot()
                .tabs
                .first()
                .is_some_and(|tab| tab.panes.len() == 2 && tab.layout.contains(second))
                && mux.term(second).is_some()
        });

        mux.write(second, b"printf 'PANE-TWO-TOKEN\\n'\n");
        poll_until(&mut mux, 15, |mux| {
            mux.term(second)
                .map(|term| screen_text(term).contains("PANE-TWO-TOKEN"))
                .unwrap_or(false)
        });
        assert!(
            !screen_text(mux.term(first).expect("first pane")).contains("PANE-TWO-TOKEN"),
            "output must stay in the pane it was written to"
        );

        let snapshot = mux.snapshot();
        let tab = &snapshot.tabs[0];
        let widths: usize = tab.panes.iter().map(|pane| pane.size.0).sum();
        assert_eq!(widths + 1, 80, "one divider between two 80-column panes");
        let layout_sizes = tab.layout.sizes(80, 24);
        assert_eq!(layout_sizes.len(), 2);
        for (id, width, height) in layout_sizes {
            let pane = tab.panes.iter().find(|pane| pane.id == id).expect("pane");
            assert!(
                pane.size.0.abs_diff(width) <= 1 && pane.size.1.abs_diff(height) <= 1,
                "layout {width}x{height} disagrees with tmux {:?}",
                pane.size
            );
        }

        mux.shutdown();
    }

    #[test]
    fn set_split_ratio_moves_the_divider() {
        let server = TestServer::new();
        let mut mux = TmuxMux::new(server.options(80, 24)).expect("start tmux client");
        let first = first_pane(&mux);
        mux.split(first, SplitDir::Horizontal).expect("split");
        poll_until(&mut mux, 10, |mux| mux.snapshot().tabs[0].panes.len() == 2);

        let tab_id = mux.snapshot().tabs[0].id;
        mux.set_split_ratio(tab_id, &[], 0.25);

        let snapshot = mux.snapshot();
        let tab = &snapshot.tabs[0];
        let total: usize = tab.panes.iter().map(|pane| pane.size.0).sum();
        let first_width = tab
            .panes
            .iter()
            .find(|pane| pane.id == tab.layout.first_leaf())
            .expect("first pane")
            .size
            .0;
        assert_eq!(total + 1, 80);
        assert!(
            first_width.abs_diff(20) <= 1,
            "ratio 0.25 of 79 columns should give about 20, got {first_width}"
        );

        mux.shutdown();
    }

    #[test]
    fn close_and_shutdown() {
        let server = TestServer::new();
        let mut mux = TmuxMux::new(server.options(80, 24)).expect("start tmux client");
        let first = first_pane(&mux);
        let second = mux.split(first, SplitDir::Vertical).expect("split");
        poll_until(&mut mux, 10, |mux| mux.snapshot().tabs[0].panes.len() == 2);

        mux.close_pane(second);
        poll_until(&mut mux, 10, |mux| {
            let snapshot = mux.snapshot();
            snapshot.tabs.len() == 1 && snapshot.tabs[0].panes.len() == 1
        });

        mux.shutdown();
        assert!(mux.child.is_none(), "client process must be reaped");
        assert!(mux.stdin.is_none(), "client stdin must be closed");

        let sessions = Command::new("tmux")
            .args(["-L", &server.socket, "list-sessions"])
            .output()
            .expect("run tmux list-sessions");
        assert!(
            sessions.status.success(),
            "detaching must keep the tmux session alive"
        );
    }
}
