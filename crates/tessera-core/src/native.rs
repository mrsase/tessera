//! Native PTY backend (owner: NativeMux agent).
//!
//! One `portable-pty` child per pane. A reader thread per pane copies bytes
//! into a channel and pokes the wake channel; everything else (feeding the
//! `Term`, resizing, layout changes) happens on the thread that calls
//! [`Mux::poll`].

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use async_channel::{Receiver as WakeRx, Sender as WakeTx};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tessera_term::Term;

use crate::mux::{Layout, Mux, MuxEvent, PaneId, PaneInfo, Snapshot, SplitDir, TabInfo};

/// How a pane is created: shell, arguments, working directory and initial
/// cell geometry.
pub struct NativeOptions {
    pub shell: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub cols: usize,
    pub rows: usize,
}

impl NativeOptions {
    pub fn new(shell: impl Into<String>, cols: usize, rows: usize) -> Self {
        Self {
            shell: shell.into(),
            args: Vec::new(),
            cwd: None,
            cols: cols.max(1),
            rows: rows.max(1),
        }
    }
}

enum PaneMsg {
    Data(Vec<u8>),
    Eof,
}

struct Pane {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    writer: Option<Box<dyn Write + Send>>,
    reader: Option<JoinHandle<()>>,
    rx: Receiver<PaneMsg>,
    term: Term,
    size: (usize, usize),
    last_title: String,
    last_cwd: Option<String>,
}

impl Pane {
    fn resize(&mut self, cols: usize, rows: usize) {
        if self.size == (cols, rows) {
            return;
        }
        self.size = (cols, rows);
        let _ = self.master.resize(pty_size(cols, rows));
        self.term.resize(cols, rows);
    }

    fn kill(&mut self) {
        // Closing the writer sends EOF before the child is terminated.
        self.writer = None;
        let _ = self.child.kill();
        self.reader = None;
    }
}

struct Tab {
    id: u64,
    layout: Layout,
    active: PaneId,
}

pub struct NativeMux {
    options: NativeOptions,
    panes: HashMap<PaneId, Pane>,
    tabs: Vec<Tab>,
    active_tab: usize,
    cols: usize,
    rows: usize,
    next_pane: AtomicU64,
    next_tab: AtomicU64,
    wake_tx: WakeTx<()>,
    wake_rx: WakeRx<()>,
    /// `MuxEvent::Exit` is emitted at most once, and re-armed by new tabs.
    exit_sent: bool,
}

impl NativeMux {
    pub fn new(options: NativeOptions) -> io::Result<Self> {
        let cols = options.cols.max(1);
        let rows = options.rows.max(1);
        let (wake_tx, wake_rx) = async_channel::bounded(1);

        let mut mux = Self {
            options: NativeOptions {
                cols,
                rows,
                ..options
            },
            panes: HashMap::new(),
            tabs: Vec::new(),
            active_tab: 0,
            cols,
            rows,
            next_pane: AtomicU64::new(1),
            next_tab: AtomicU64::new(1),
            wake_tx,
            wake_rx,
            exit_sent: false,
        };

        let pane = mux.spawn_pane(None, cols, rows)?;
        let tab_id = mux.next_tab.fetch_add(1, Ordering::Relaxed);
        mux.tabs.push(Tab {
            id: tab_id,
            layout: Layout::leaf(pane),
            active: pane,
        });
        Ok(mux)
    }

    fn spawn_pane(&mut self, cwd: Option<&Path>, cols: usize, rows: usize) -> io::Result<PaneId> {
        let id = self.next_pane.fetch_add(1, Ordering::Relaxed);
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(pty_size(cols, rows))
            .map_err(pty_error)?;

        let mut command = CommandBuilder::new(&self.options.shell);
        command.args(self.options.args.iter());
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        if let Some(dir) = cwd.or(self.options.cwd.as_deref()) {
            command.cwd(dir);
        }

        let mut child = pair.slave.spawn_command(command).map_err(pty_error)?;
        drop(pair.slave);
        let master = pair.master;
        let writer = master.take_writer().map_err(pty_error)?;
        let reader = master.try_clone_reader().map_err(pty_error)?;

        let (tx, rx) = mpsc::channel();
        let wake = self.wake_tx.clone();
        let reader = match thread::Builder::new()
            .name(format!("tessera-pane-{id}"))
            .spawn(move || read_pane(reader, tx, wake))
        {
            Ok(handle) => handle,
            Err(err) => {
                let _ = child.kill();
                return Err(io::Error::other(err.to_string()));
            }
        };

        self.panes.insert(
            id,
            Pane {
                master,
                child,
                writer: Some(writer),
                reader: Some(reader),
                rx,
                term: Term::new(cols, rows),
                size: (cols, rows),
                last_title: String::new(),
                last_cwd: None,
            },
        );
        Ok(id)
    }

    fn kill_pane(&mut self, pane: PaneId) {
        if let Some(mut pane) = self.panes.remove(&pane) {
            pane.kill();
        }
    }

    fn close_tab_inner(&mut self, tab: u64) {
        let Some(index) = self.tabs.iter().position(|t| t.id == tab) else {
            return;
        };
        let ids = self.tabs[index].layout.ids();
        self.tabs.remove(index);
        for id in ids {
            self.kill_pane(id);
        }
        if self.tabs.is_empty() {
            self.active_tab = 0;
        } else if index < self.active_tab {
            self.active_tab -= 1;
        } else {
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        }
    }

    /// Recompute a tab's geometry and push it into every PTY and `Term`.
    fn apply_tab_sizes(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let sizes = tab.layout.sizes(self.cols, self.rows);
        for (id, cols, rows) in sizes {
            if let Some(pane) = self.panes.get_mut(&id) {
                pane.resize(cols, rows);
            }
        }
    }

    /// Handle a pane whose reader reached EOF. Returns true when the tab
    /// structure changed.
    fn handle_pane_exit(&mut self, pane: PaneId) -> bool {
        let Some(index) = self.tabs.iter().position(|t| t.layout.contains(pane)) else {
            self.kill_pane(pane);
            return false;
        };
        if matches!(self.tabs[index].layout, Layout::Leaf(id) if id == pane) {
            let tab = self.tabs[index].id;
            self.close_tab_inner(tab);
            return true;
        }

        let sibling = sibling_of(&self.tabs[index].layout, pane);
        self.tabs[index].layout.remove(pane);
        if !self.tabs[index].layout.contains(self.tabs[index].active) {
            let fallback = sibling.unwrap_or_else(|| self.tabs[index].layout.first_leaf());
            self.tabs[index].active = fallback;
        }
        self.kill_pane(pane);
        self.apply_tab_sizes(index);
        true
    }

    fn kill_all(&mut self) {
        let ids: Vec<PaneId> = self.panes.keys().copied().collect();
        for id in ids {
            self.kill_pane(id);
        }
        self.tabs.clear();
        self.active_tab = 0;
    }
}

impl Mux for NativeMux {
    fn snapshot(&self) -> Snapshot {
        let tabs = self
            .tabs
            .iter()
            .map(|tab| {
                let panes = tab
                    .layout
                    .ids()
                    .into_iter()
                    .filter_map(|id| self.panes.get(&id).map(|pane| (id, pane)))
                    .map(|(id, pane)| PaneInfo {
                        id,
                        title: pane_title(pane),
                        cwd: pane.term.cwd().map(PathBuf::from),
                        size: pane.size,
                    })
                    .collect();
                let title = self
                    .panes
                    .get(&tab.active)
                    .map(pane_title)
                    .unwrap_or_else(|| "shell".to_owned());
                TabInfo {
                    id: tab.id,
                    title,
                    layout: tab.layout.clone(),
                    active: tab.active,
                    panes,
                }
            })
            .collect();

        let active_tab = if self.tabs.is_empty() {
            0
        } else {
            self.active_tab.min(self.tabs.len() - 1)
        };
        Snapshot { tabs, active_tab }
    }

    fn term(&self, pane: PaneId) -> Option<&Term> {
        self.panes.get(&pane).map(|pane| &pane.term)
    }

    fn poll(&mut self) -> Vec<MuxEvent> {
        let mut events = Vec::new();
        if self.tabs.is_empty() {
            if !self.exit_sent {
                self.exit_sent = true;
                events.push(MuxEvent::Exit);
            }
            return events;
        }

        let ids: Vec<PaneId> = self.panes.keys().copied().collect();
        let mut exited = Vec::new();
        let mut tabs_dirty = false;

        for id in ids {
            let Some(pane) = self.panes.get_mut(&id) else {
                continue;
            };
            loop {
                match pane.rx.try_recv() {
                    Ok(PaneMsg::Data(data)) => {
                        pane.term.feed(&data);
                        events.push(MuxEvent::Output(id));
                    }
                    Ok(PaneMsg::Eof) | Err(TryRecvError::Disconnected) => {
                        exited.push(id);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
            if exited.last() == Some(&id) {
                continue;
            }

            // Only allocate when a title or cwd actually changed; the common
            // case is two string comparisons.
            let title_changed = pane.term.title() != pane.last_title.as_str();
            if title_changed {
                pane.last_title = pane.term.title().to_owned();
                tabs_dirty = true;
            }
            let cwd_changed = pane.term.cwd() != pane.last_cwd.as_deref();
            if cwd_changed {
                pane.last_cwd = pane.term.cwd().map(str::to_owned);
                tabs_dirty = true;
            }
        }

        for id in exited {
            if self.handle_pane_exit(id) {
                tabs_dirty = true;
            }
        }

        if tabs_dirty {
            events.push(MuxEvent::Tabs);
        }
        if self.tabs.is_empty() && !self.exit_sent {
            self.exit_sent = true;
            events.push(MuxEvent::Exit);
        }
        events
    }

    fn wake(&self) -> async_channel::Receiver<()> {
        self.wake_rx.clone()
    }

    fn write(&mut self, pane: PaneId, bytes: &[u8]) {
        if let Some(pane) = self.panes.get_mut(&pane) {
            if let Some(writer) = pane.writer.as_mut() {
                let _ = writer.write_all(bytes);
                let _ = writer.flush();
            }
        }
    }

    fn resize_client(&mut self, cols: usize, rows: usize) {
        self.cols = cols.max(1);
        self.rows = rows.max(1);
        for index in 0..self.tabs.len() {
            self.apply_tab_sizes(index);
        }
    }

    fn new_tab(&mut self, cwd: Option<&Path>) {
        let cols = self.cols;
        let rows = self.rows;
        if let Ok(pane) = self.spawn_pane(cwd, cols, rows) {
            let tab_id = self.next_tab.fetch_add(1, Ordering::Relaxed);
            self.tabs.push(Tab {
                id: tab_id,
                layout: Layout::leaf(pane),
                active: pane,
            });
            self.active_tab = self.tabs.len() - 1;
            self.exit_sent = false;
        }
    }

    fn close_tab(&mut self, tab: u64) {
        self.close_tab_inner(tab);
    }

    fn select_tab(&mut self, tab: u64) {
        if let Some(index) = self.tabs.iter().position(|t| t.id == tab) {
            self.active_tab = index;
        }
    }

    fn split(&mut self, pane: PaneId, dir: SplitDir) -> Option<PaneId> {
        let index = self.tabs.iter().position(|t| t.layout.contains(pane))?;
        let cwd = self
            .panes
            .get(&pane)
            .and_then(|pane| pane.term.cwd())
            .map(PathBuf::from);
        let cols = self.cols;
        let rows = self.rows;
        let new_pane = self.spawn_pane(cwd.as_deref(), cols, rows).ok()?;
        if !self.tabs[index].layout.split(pane, new_pane, dir) {
            self.kill_pane(new_pane);
            return None;
        }
        self.tabs[index].active = new_pane;
        self.active_tab = index;
        self.exit_sent = false;
        self.apply_tab_sizes(index);
        Some(new_pane)
    }

    fn close_pane(&mut self, pane: PaneId) {
        let Some(index) = self.tabs.iter().position(|t| t.layout.contains(pane)) else {
            self.kill_pane(pane);
            return;
        };
        if matches!(self.tabs[index].layout, Layout::Leaf(id) if id == pane) {
            let tab = self.tabs[index].id;
            self.close_tab_inner(tab);
            return;
        }

        let sibling = sibling_of(&self.tabs[index].layout, pane);
        self.tabs[index].layout.remove(pane);
        if !self.tabs[index].layout.contains(self.tabs[index].active) {
            let fallback = sibling.unwrap_or_else(|| self.tabs[index].layout.first_leaf());
            self.tabs[index].active = fallback;
        }
        self.kill_pane(pane);
        self.apply_tab_sizes(index);
    }

    fn focus_pane(&mut self, pane: PaneId) {
        if let Some(index) = self.tabs.iter().position(|t| t.layout.contains(pane)) {
            self.tabs[index].active = pane;
            self.active_tab = index;
        }
    }

    fn set_split_ratio(&mut self, tab: u64, path: &[bool], ratio: f32) {
        let Some(index) = self.tabs.iter().position(|t| t.id == tab) else {
            return;
        };
        if self.tabs[index].layout.set_ratio_at(path, ratio) {
            self.apply_tab_sizes(index);
        }
    }

    fn shutdown(&mut self) {
        self.kill_all();
    }
}

impl Drop for NativeMux {
    fn drop(&mut self) {
        self.kill_all();
    }
}

fn read_pane(mut reader: Box<dyn Read + Send>, tx: Sender<PaneMsg>, wake: WakeTx<()>) {
    let mut buf = vec![0u8; 8 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(PaneMsg::Data(buf[..n].to_vec())).is_err() {
                    return;
                }
                let _ = wake.try_send(());
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let _ = tx.send(PaneMsg::Eof);
    let _ = wake.try_send(());
}

fn pane_title(pane: &Pane) -> String {
    let title = pane.term.title();
    if !title.is_empty() {
        return title.to_owned();
    }
    if let Some(cwd) = pane.term.cwd() {
        if let Some(name) = Path::new(cwd).file_name() {
            return name.to_string_lossy().into_owned();
        }
    }
    "shell".to_owned()
}

/// The leaf that would take the removed pane's place after collapsing its
/// parent split.
fn sibling_of(layout: &Layout, target: PaneId) -> Option<PaneId> {
    match layout {
        Layout::Leaf(_) => None,
        Layout::Split { first, second, .. } => {
            if matches!(**first, Layout::Leaf(id) if id == target) {
                Some(second.first_leaf())
            } else if matches!(**second, Layout::Leaf(id) if id == target) {
                Some(first.first_leaf())
            } else if first.contains(target) {
                sibling_of(first, target)
            } else if second.contains(target) {
                sibling_of(second, target)
            } else {
                None
            }
        }
    }
}

fn pty_size(cols: usize, rows: usize) -> PtySize {
    PtySize {
        rows: rows.min(usize::from(u16::MAX)) as u16,
        cols: cols.min(usize::from(u16::MAX)) as u16,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn pty_error(err: impl std::fmt::Display) -> io::Error {
    io::Error::other(err.to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn bash_mux(cols: usize, rows: usize) -> NativeMux {
        let mut options = NativeOptions::new("bash", cols, rows);
        options.args = vec!["--noprofile".to_owned(), "--norc".to_owned()];
        NativeMux::new(options).expect("spawn bash")
    }

    fn grid(mux: &NativeMux, pane: PaneId) -> String {
        mux.term(pane)
            .map(|term| {
                term.screen()
                    .rows
                    .iter()
                    .map(|row| row.text())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    /// Poll with a hard deadline; the closure sees the accumulated events.
    fn wait_until<F>(mux: &mut NativeMux, mut cond: F) -> Option<Vec<MuxEvent>>
    where
        F: FnMut(&NativeMux, &[MuxEvent]) -> bool,
    {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut events = Vec::new();
        loop {
            events.extend(mux.poll());
            if cond(mux, &events) {
                return Some(events);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn bash_roundtrip() {
        let mut mux = bash_mux(40, 8);
        let wake = mux.wake();
        let mut woke = false;

        mux.write(1, b"printf 'TOKEN-%s\\n' 42\n");
        let deadline = Instant::now() + Duration::from_secs(15);
        let found = loop {
            mux.poll();
            while wake.try_recv().is_ok() {
                woke = true;
            }
            if grid(&mux, 1).contains("TOKEN-42") {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(10));
        };

        assert!(found, "TOKEN-42 missing; grid:\n{}", grid(&mux, 1));
        assert!(woke, "wake channel never fired");
        mux.shutdown();
    }

    #[test]
    fn split_and_focus() {
        let mut mux = bash_mux(80, 24);
        let second = mux.split(1, SplitDir::Horizontal).expect("split");
        assert_eq!(second, 2);

        let snapshot = mux.snapshot();
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(snapshot.tabs[0].panes.len(), 2);
        assert_eq!(snapshot.tabs[0].layout.ids(), vec![1, 2]);

        mux.focus_pane(second);
        assert_eq!(mux.snapshot().tabs[0].active, second);

        mux.write(second, b"printf 'ONLY-%s\\n' 2\n");
        let found = wait_until(&mut mux, |mux, _| grid(mux, second).contains("ONLY-2"));
        assert!(
            found.is_some(),
            "second pane stalled:\n{}",
            grid(&mux, second)
        );
        assert!(
            !grid(&mux, 1).contains("ONLY-2"),
            "output leaked into pane 1:\n{}",
            grid(&mux, 1)
        );
        mux.shutdown();
    }

    #[test]
    fn resize_updates_children() {
        let mut mux = bash_mux(40, 8);
        mux.resize_client(100, 40);

        let snapshot = mux.snapshot();
        let expected = snapshot.tabs[0].layout.sizes(100, 40);
        assert_eq!(
            snapshot.tabs[0].panes[0].size,
            (expected[0].1, expected[0].2)
        );

        mux.write(1, b"stty size\n");
        let found = wait_until(&mut mux, |mux, _| {
            grid(mux, 1).lines().any(|line| line.trim() == "40 100")
        });
        assert!(
            found.is_some(),
            "stty size mismatch; grid:\n{}",
            grid(&mux, 1)
        );
        mux.shutdown();
    }

    #[test]
    fn close_and_exit() {
        let mut mux = bash_mux(80, 24);
        let second = mux.split(1, SplitDir::Horizontal).expect("split");

        mux.close_pane(second);
        let snapshot = mux.snapshot();
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(snapshot.tabs[0].layout.ids(), vec![1]);
        assert_eq!(snapshot.tabs[0].active, 1);
        assert_eq!(snapshot.tabs[0].panes.len(), 1);

        mux.close_pane(1);
        assert!(mux.snapshot().tabs.is_empty());

        let events = wait_until(&mut mux, |_, events| events.contains(&MuxEvent::Exit));
        assert!(
            events.is_some(),
            "no Exit event after closing the last pane"
        );
        mux.shutdown();
    }
}
