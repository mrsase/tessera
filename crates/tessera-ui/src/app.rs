//! The Tessera view: tab bar, pane layout, terminal input and suggestions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::{
    div, font, point, px, rgb, rgba, size, AnyElement, Bounds, ClipboardItem, Context, ElementId,
    FocusHandle, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent, SharedString, Subscription,
    Task, WeakEntity, Window,
};
use tessera_core::config::Config;
use tessera_core::line::InputLine;
use tessera_core::mux::{Layout, Mux, PaneId, Snapshot, SplitDir};
use tessera_core::selection::{Pos, Selection};
use tessera_core::shortcut::{Action, FocusDir};
use tessera_core::suggest::HistoryIndex;
use tessera_term::Term;

use crate::keys;
use crate::terminal::{self, Metrics, RenderData};
use crate::theme::Theme;

pub const TAB_BAR: f32 = 34.0;
const EDGE: f32 = 4.0;
const MIN_FONT: f32 = 8.0;
const MAX_FONT: f32 = 32.0;
const BLINK: Duration = Duration::from_millis(530);

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn default_metrics(family: SharedString, font_size: f32) -> Metrics {
    Metrics {
        cell_w: (font_size * 0.6).max(2.0),
        cell_h: (font_size * 1.25).max(8.0),
        ascent: font_size * 0.8,
        descent: font_size * 0.25,
        font_size,
        family,
    }
}

pub struct TerminalApp {
    config: Config,
    theme: Theme,
    focus: FocusHandle,
    mux: Option<Box<dyn Mux>>,
    error: Option<String>,
    history: HistoryIndex,
    input: InputLine,
    suggestion: Option<String>,
    metrics: Metrics,
    font_size: Pixels,
    cols: usize,
    rows: usize,
    offsets: HashMap<PaneId, usize>,
    selections: HashMap<PaneId, Selection>,
    pane_rects: HashMap<PaneId, (f32, f32, f32, f32)>,
    dragging: Option<PaneId>,
    zoomed: Option<PaneId>,
    cursor_on: bool,
    title: String,
    _wake: Option<Task<()>>,
    _blink: Option<Task<()>>,
    _bounds: Option<Subscription>,
}

impl TerminalApp {
    pub fn new(config: Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let theme = Theme::from_config(&config.theme);
        let font_size = px(config.font_size);
        let family: SharedString = config.font_family.clone().into();
        let mut app = Self {
            history: if config.history_suggestions {
                HistoryIndex::from_default_sources()
            } else {
                HistoryIndex::new()
            },
            input: InputLine::new(),
            suggestion: None,
            config,
            theme,
            focus: cx.focus_handle(),
            mux: None,
            error: None,
            metrics: default_metrics(family, f32::from(font_size)),
            font_size,
            cols: 80,
            rows: 24,
            offsets: HashMap::new(),
            selections: HashMap::new(),
            pane_rects: HashMap::new(),
            dragging: None,
            zoomed: None,
            cursor_on: true,
            title: String::new(),
            _wake: None,
            _blink: None,
            _bounds: None,
        };

        app.measure(window, cx);
        app.spawn_backend(window, cx);
        app.focus.focus(window);

        let blink = cx.spawn(
            async move |this: WeakEntity<Self>, cx: &mut gpui::AsyncApp| loop {
                cx.background_executor().timer(BLINK).await;
                if this
                    .update(cx, |app, cx| {
                        app.cursor_on = !app.cursor_on;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            },
        );
        app._blink = Some(blink);

        let bounds = cx.observe_window_bounds(window, |app, window, cx| {
            if app.measure(window, cx) {
                cx.notify();
            }
        });
        app._bounds = Some(bounds);
        app
    }

    fn spawn_backend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (cols, rows) = (self.cols.max(2), self.rows.max(2));
        let result = spawn_mux(&self.config, cols, rows);
        match result {
            Ok(mux) => {
                let rx = mux.wake();
                self.mux = Some(mux);
                self.error = None;
                let task = cx.spawn(
                    async move |this: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                        while rx.recv().await.is_ok() {
                            if this
                                .update(cx, |app, cx| {
                                    app.drain();
                                    cx.notify();
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    },
                );
                self._wake = Some(task);
                window.set_window_title("Tessera");
            }
            Err(err) => {
                self.error = Some(err);
            }
        }
    }

    fn drain(&mut self) {
        let Some(mux) = &mut self.mux else { return };
        let events = mux.poll();
        if events.is_empty() {
            return;
        }
    }

    /// Recompute font metrics and the grid from the current window size.
    /// Returns true when the cell grid changed.
    fn measure(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let family: SharedString = self.config.font_family.clone().into();
        let ts = cx.text_system();
        let font_id = ts.resolve_font(&font(self.config.font_family.clone()));
        let cell_w = ts
            .ch_advance(font_id, self.font_size)
            .unwrap_or(px(f32::from(self.font_size) * 0.6));
        let ascent = ts.ascent(font_id, self.font_size);
        let descent = ts.descent(font_id, self.font_size);
        self.metrics = Metrics::new(family, self.font_size, cell_w, ascent, descent);

        let view = window.viewport_size();
        let avail_w = (f32::from(view.width) - 2.0 * EDGE).max(16.0);
        let avail_h = (f32::from(view.height) - TAB_BAR - 2.0 * EDGE).max(16.0);
        let cols = (avail_w / self.metrics.cell_w).floor().max(1.0) as usize;
        let rows = (avail_h / self.metrics.cell_h).floor().max(1.0) as usize;
        let changed = cols != self.cols || rows != self.rows;
        self.cols = cols;
        self.rows = rows;
        if let Some(mux) = &mut self.mux {
            mux.resize_client(cols, rows);
        }
        changed
    }

    fn active_tab<'a>(&self, snap: &'a Snapshot) -> Option<&'a tessera_core::mux::TabInfo> {
        snap.tabs.get(snap.active_tab)
    }

    fn active_pane(&self) -> Option<PaneId> {
        let mux = self.mux.as_ref()?;
        let snap = mux.snapshot();
        self.active_tab(&snap).map(|tab| tab.active)
    }

    fn term(&self, pane: PaneId) -> Option<&Term> {
        self.mux.as_ref()?.term(pane)
    }

    fn write_to(&mut self, pane: PaneId, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(mux) = &mut self.mux {
            mux.write(pane, bytes);
        }
        self.offsets.insert(pane, 0);
    }

    fn refresh_suggestion(&mut self, pane: PaneId) {
        if !self.config.history_suggestions || !self.input.is_valid() || self.input.is_dismissed() {
            self.suggestion = None;
            return;
        }
        let prefix = self.input.text();
        if prefix.is_empty() {
            self.suggestion = None;
            return;
        }
        let cwd = self.term(pane).and_then(|t| t.cwd()).map(PathBuf::from);
        self.suggestion = self
            .history
            .suggest(prefix, cwd.as_deref(), unix_now(), 1)
            .into_iter()
            .next()
            .map(|s| s.cmd);
    }

    fn accept_suggestion(&mut self, pane: PaneId, word: bool) -> bool {
        let Some(suggestion) = self.suggestion.clone() else {
            return false;
        };
        let accepted = if word {
            self.input.accept_word(&suggestion)
        } else {
            self.input.accept(&suggestion)
        };
        let Some(bytes) = accepted else { return false };
        self.write_to(pane, &bytes);
        self.refresh_suggestion(pane);
        true
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = ev.keystroke.clone();
        if let Some(action) = keys::app_action(&ks) {
            self.run_action(action, window, cx);
            return;
        }
        let Some(pane) = self.active_pane() else {
            return;
        };
        let app_cursor = self.term(pane).is_some_and(|t| t.app_cursor());
        let mods = ks.modifiers;
        let plain = !mods.control && !mods.alt && !mods.platform;

        // Suggestion acceptance happens before the key reaches the shell.
        if self.suggestion.is_some() && plain && ks.key == "right" {
            if self.accept_suggestion(pane, false) {
                cx.notify();
                return;
            }
        }
        if self.suggestion.is_some() && mods.control && ks.key == "right" {
            if self.accept_suggestion(pane, true) {
                cx.notify();
                return;
            }
        }
        if self.suggestion.is_some() && mods.control && ks.key == "space" {
            if self.accept_suggestion(pane, false) {
                cx.notify();
                return;
            }
        }
        if ks.key == "escape" && !mods.control && !mods.alt {
            self.input.dismiss();
            self.suggestion = None;
        }

        let Some(bytes) = keys::translate(&ks, app_cursor) else {
            return;
        };

        match ks.key.as_str() {
            "enter" => self.input.enter(),
            "backspace" if plain => self.input.backspace(),
            "up" | "down" | "left" | "right" | "home" | "end" | "tab" | "delete" | "pageup"
            | "pagedown" | "insert" => self.input.invalidate(),
            _ if mods.control || mods.alt || mods.platform => self.input.invalidate(),
            _ => {
                if let Some(text) = ks.key_char.as_deref() {
                    self.input.push(text);
                } else if ks.key.chars().count() == 1 {
                    self.input.push(&ks.key);
                }
            }
        }

        self.write_to(pane, &bytes);
        self.refresh_suggestion(pane);
        cx.notify();
    }

    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let Some(sel) = self.selections.get(&pane).copied() else {
            return;
        };
        let Some(term) = self.term(pane) else { return };
        let text = sel.text(term);
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let bracketed = self.term(pane).is_some_and(|t| t.bracketed_paste());
        let bytes = keys::paste(&text, bracketed);
        self.write_to(pane, &bytes);
        if !text.contains('\n') && self.input.is_valid() {
            self.input.push(&text);
        } else {
            self.input.invalidate();
        }
        self.refresh_suggestion(pane);
        cx.notify();
    }

    fn select_all(&mut self) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let Some(term) = self.term(pane) else { return };
        let total = term.history_len() + term.rows();
        if total == 0 {
            return;
        }
        let last = total - 1;
        self.selections.insert(
            pane,
            Selection {
                anchor: Pos { line: 0, col: 0 },
                focus: Pos {
                    line: last,
                    col: term.cols().saturating_sub(1),
                },
            },
        );
    }

    fn run_action(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let snap = match &self.mux {
            Some(mux) => mux.snapshot(),
            None => return,
        };
        let active_tab = snap.tabs.get(snap.active_tab).cloned();
        match action {
            Action::NewTab => {
                if let Some(mux) = &mut self.mux {
                    mux.new_tab(None);
                    let after = mux.snapshot();
                    if let Some(tab) = after.tabs.last() {
                        mux.select_tab(tab.id);
                    }
                }
            }
            Action::ClosePane => {
                if let (Some(tab), Some(mux)) = (&active_tab, &mut self.mux) {
                    if tab.panes.len() <= 1 {
                        mux.close_tab(tab.id);
                    } else {
                        mux.close_pane(tab.active);
                    }
                }
            }
            Action::NextTab | Action::PrevTab => {
                if snap.tabs.len() > 1 {
                    let step = if matches!(action, Action::NextTab) {
                        1
                    } else {
                        snap.tabs.len() - 1
                    };
                    let idx = (snap.active_tab + step) % snap.tabs.len();
                    if let Some(mux) = &mut self.mux {
                        mux.select_tab(snap.tabs[idx].id);
                    }
                }
            }
            Action::SelectTab(n) => {
                if let (Some(tab), Some(mux)) = (snap.tabs.get(n as usize), &mut self.mux) {
                    mux.select_tab(tab.id);
                }
            }
            Action::SplitRight | Action::SplitDown => {
                if let Some(tab) = &active_tab {
                    let dir = if matches!(action, Action::SplitRight) {
                        SplitDir::Horizontal
                    } else {
                        SplitDir::Vertical
                    };
                    if let Some(mux) = &mut self.mux {
                        mux.split(tab.active, dir);
                    }
                }
            }
            Action::FocusPane(dir) => {
                let target = active_tab
                    .as_ref()
                    .and_then(|tab| self.neighbour_pane(tab, dir));
                if let (Some(target), Some(mux)) = (target, &mut self.mux) {
                    mux.focus_pane(target);
                }
            }
            Action::NudgeDivider(dir) => {
                if let Some(tab) = &active_tab {
                    if let Some((path, ratio, axis)) = split_for(tab, tab.active) {
                        let delta = 0.04;
                        let new = match (axis, dir) {
                            (SplitDir::Horizontal, FocusDir::Left)
                            | (SplitDir::Vertical, FocusDir::Up) => ratio - delta,
                            (SplitDir::Horizontal, FocusDir::Right)
                            | (SplitDir::Vertical, FocusDir::Down) => ratio + delta,
                            _ => ratio,
                        };
                        if let Some(mux) = &mut self.mux {
                            mux.set_split_ratio(tab.id, &path, new);
                        }
                    }
                }
            }
            Action::ZoomPane => {
                if let Some(tab) = &active_tab {
                    self.zoomed = if self.zoomed == Some(tab.active) {
                        None
                    } else {
                        Some(tab.active)
                    };
                }
            }
            Action::Copy => self.copy_selection(cx),
            Action::Paste => self.paste(cx),
            Action::SelectAll => self.select_all(),
            Action::ScrollPageUp | Action::ScrollPageDown => {
                if let Some(tab) = &active_tab {
                    let by = self.rows as f64;
                    let delta = if matches!(action, Action::ScrollPageUp) {
                        by
                    } else {
                        -by
                    };
                    self.scroll_pane(tab.active, delta);
                }
            }
            Action::ClearScrollback => {
                if let Some(tab) = &active_tab {
                    self.write_to(tab.active, b"\x0c");
                    self.offsets.insert(tab.active, 0);
                }
            }
            Action::FontIncrease | Action::FontDecrease | Action::FontReset => {
                let size = f32::from(self.font_size);
                let next = match action {
                    Action::FontIncrease => (size + 1.0).min(MAX_FONT),
                    Action::FontDecrease => (size - 1.0).max(MIN_FONT),
                    _ => 14.0,
                };
                self.font_size = px(next);
                self.measure(window, cx);
            }
            Action::Quit => cx.quit(),
            Action::Palette | Action::Search | Action::RenameTab | Action::OpenConfig => {}
        }
        let _ = window;
        cx.notify();
    }

    fn neighbour_pane(&self, tab: &tessera_core::mux::TabInfo, dir: FocusDir) -> Option<PaneId> {
        let me = self.pane_rects.get(&tab.active)?;
        let (cx, cy) = (me.0 + me.2 / 2.0, me.1 + me.3 / 2.0);
        let mut best: Option<(f32, PaneId)> = None;
        for pane in &tab.panes {
            if pane.id == tab.active {
                continue;
            }
            let Some(r) = self.pane_rects.get(&pane.id) else {
                continue;
            };
            let (px_, py) = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
            let (ok, dist) = match dir {
                FocusDir::Left => (px_ < cx, cx - px_),
                FocusDir::Right => (px_ > cx, px_ - cx),
                FocusDir::Up => (py < cy, cy - py),
                FocusDir::Down => (py > cy, py - cy),
            };
            if ok && best.map_or(true, |(d, _)| dist < d) {
                best = Some((dist, pane.id));
            }
        }
        best.map(|(_, id)| id)
    }

    fn scroll_pane(&mut self, pane: PaneId, delta: f64) {
        let Some(term) = self.term(pane) else { return };
        if term.alt_active() {
            return;
        }
        let max = term.history_len();
        let current = self.offsets.get(&pane).copied().unwrap_or(0) as f64;
        let next = (current + delta).clamp(0.0, max as f64);
        self.offsets.insert(pane, next as usize);
    }

    fn pane_cell(&self, pane: PaneId, position: Point<Pixels>) -> Option<Pos> {
        let rect = self.pane_rects.get(&pane)?;
        let term = self.term(pane)?;
        let bounds = Bounds::new(point(px(rect.0), px(rect.1)), size(px(rect.2), px(rect.3)));
        let (col, row) =
            terminal::hit_cell(position, bounds, &self.metrics, term.cols(), term.rows())?;
        let offset = self.offsets.get(&pane).copied().unwrap_or(0);
        let top = term.history_len() - offset.min(term.history_len());
        Some(Pos {
            line: top + row,
            col,
        })
    }

    fn word_selection(&self, pane: PaneId, pos: Pos) -> Selection {
        let Some(term) = self.term(pane) else {
            return Selection::new(pos);
        };
        let Some(row) = term.line(pos.line) else {
            return Selection::new(pos);
        };
        let len = row.cells.len();
        if len == 0 {
            return Selection::new(pos);
        }
        let col = pos.col.min(len - 1);
        let is_word =
            |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '~' | ':');
        if !is_word(row.cells[col].ch) {
            return Selection::new(pos);
        }
        let mut start = col;
        while start > 0 && is_word(row.cells[start - 1].ch) {
            start -= 1;
        }
        let mut end = col;
        while end + 1 < len && is_word(row.cells[end + 1].ch) {
            end += 1;
        }
        Selection {
            anchor: Pos {
                line: pos.line,
                col: start,
            },
            focus: Pos {
                line: pos.line,
                col: end,
            },
        }
    }

    fn mouse_down(
        &mut self,
        pane: PaneId,
        ev: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus.focus(window);
        if let Some(mux) = &mut self.mux {
            mux.focus_pane(pane);
        }
        let Some(pos) = self.pane_cell(pane, ev.position) else {
            return;
        };
        let selected = match ev.click_count {
            2 => self.word_selection(pane, pos),
            3 => Selection {
                anchor: Pos {
                    line: pos.line,
                    col: 0,
                },
                focus: Pos {
                    line: pos.line,
                    col: self.cols.saturating_sub(1),
                },
            },
            _ => Selection::new(pos),
        };
        self.selections.insert(pane, selected);
        self.dragging = Some(pane);
        cx.notify();
    }

    fn mouse_move(&mut self, pane: PaneId, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.dragging != Some(pane) || ev.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some(pos) = self.pane_cell(pane, ev.position) else {
            return;
        };
        if let Some(sel) = self.selections.get_mut(&pane) {
            sel.extend_to(pos);
        }
        cx.notify();
    }

    fn mouse_up(&mut self, pane: PaneId, _ev: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.dragging == Some(pane) {
            self.dragging = None;
            if let Some(sel) = self.selections.get(&pane) {
                if sel.is_empty() {
                    self.selections.remove(&pane);
                }
            }
            cx.notify();
        }
    }

    fn scroll_wheel(&mut self, pane: PaneId, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let lines = match ev.delta {
            ScrollDelta::Lines(p) => p.y as f64,
            ScrollDelta::Pixels(p) => f32::from(p.y) as f64 / self.metrics.cell_h as f64,
        };
        self.scroll_pane(pane, lines * 3.0);
        cx.notify();
    }

    fn ghost_for(&self, pane: PaneId) -> Option<(usize, usize, String)> {
        let suggestion = self.suggestion.as_ref()?;
        if self.zoomed.is_some_and(|z| z != pane) {
            return None;
        }
        let prefix = self.input.text();
        let suffix = suggestion.strip_prefix(prefix)?;
        if suffix.is_empty() {
            return None;
        }
        let offset = self.offsets.get(&pane).copied().unwrap_or(0);
        if offset != 0 {
            return None;
        }
        let term = self.term(pane)?;
        if term.alt_active() || !term.cursor_visible() {
            return None;
        }
        let cursor = term.screen().cursor;
        Some((cursor.y, cursor.x, suffix.to_string()))
    }

    fn pane_element(
        &self,
        pane: PaneId,
        rect: (f32, f32, f32, f32),
        cols: usize,
        rows: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(term) = self.term(pane) else {
            return div().into_any_element();
        };
        let offset = self.offsets.get(&pane).copied().unwrap_or(0);
        let ghost = self.ghost_for(pane);
        let data = RenderData::build(
            term,
            cols,
            rows,
            offset,
            self.selections.get(&pane),
            &self.theme,
            &self.metrics,
            self.cursor_on,
            ghost.as_ref().map(|(r, c, t)| (*r, *c, t.as_str())),
        );
        let visual = terminal::visual(Rc::new(data));
        div()
            .absolute()
            .left(px(rect.0))
            .top(px(rect.1))
            .w(px(rect.2))
            .h(px(rect.3))
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    this.mouse_down(pane, ev, window, cx)
                }),
            )
            .on_mouse_move(
                cx.listener(move |this, ev: &MouseMoveEvent, _w, cx| this.mouse_move(pane, ev, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseUpEvent, _w, cx| this.mouse_up(pane, ev, cx)),
            )
            .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, _w, cx| {
                this.scroll_wheel(pane, ev, cx)
            }))
            .child(visual)
            .into_any_element()
    }

    fn tab_bar(&mut self, snap: &Snapshot, cx: &mut Context<Self>) -> AnyElement {
        let mut tabs = div().flex().items_center().h_full().gap_1();
        for (index, tab) in snap.tabs.iter().enumerate() {
            let active = index == snap.active_tab;
            let id = tab.id;
            let title: SharedString = if tab.title.is_empty() {
                "shell".into()
            } else {
                tab.title.clone().into()
            };
            let count = tab.panes.len();
            let label = if count > 1 {
                format!("{title} ({count})")
            } else {
                title.to_string()
            };
            let tab_el = div()
                .id(ElementId::Name(format!("tab-{id}").into()))
                .flex()
                .items_center()
                .gap_2()
                .h(px(TAB_BAR - 6.0))
                .px_3()
                .rounded_md()
                .text_sm()
                .when(active, |this| {
                    this.bg(self.theme.bg).text_color(self.theme.fg)
                })
                .when(!active, |this| this.text_color(self.theme.fg.opacity(0.65)))
                .hover(|style| style.bg(self.theme.selection.opacity(0.6)))
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    if let Some(mux) = &mut this.mux {
                        mux.select_tab(id);
                    }
                    cx.notify();
                }))
                .child(label)
                .child(
                    div()
                        .id(ElementId::Name(format!("tab-close-{id}").into()))
                        .px_1()
                        .rounded_sm()
                        .text_color(self.theme.fg.opacity(0.45))
                        .hover(|style| style.text_color(self.theme.fg))
                        .child("×")
                        .on_click(cx.listener(move |this, _ev, _w, cx| {
                            if let Some(mux) = &mut this.mux {
                                mux.close_tab(id);
                            }
                            cx.stop_propagation();
                            cx.notify();
                        })),
                );
            tabs = tabs.child(tab_el);
        }
        let new_tab = div()
            .id("new-tab")
            .flex()
            .items_center()
            .justify_center()
            .w(px(28.0))
            .h(px(TAB_BAR - 10.0))
            .rounded_md()
            .text_color(self.theme.fg.opacity(0.7))
            .hover(|style| style.bg(self.theme.selection.opacity(0.6)))
            .on_click(cx.listener(|this, _ev, _w, cx| {
                if let Some(mux) = &mut this.mux {
                    mux.new_tab(None);
                    let after = mux.snapshot();
                    if let Some(tab) = after.tabs.last() {
                        mux.select_tab(tab.id);
                    }
                }
                cx.notify();
            }))
            .child("+");

        div()
            .h(px(TAB_BAR))
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .px_2()
            .bg(rgb(0x0021_252b))
            .border_b_1()
            .border_color(rgba(0x0000_0000))
            .child(tabs)
            .child(
                div()
                    .text_xs()
                    .text_color(self.theme.fg.opacity(0.35))
                    .pr_2()
                    .child(
                        if self.config.backend == tessera_core::config::Backend::Native {
                            "native"
                        } else {
                            "tmux"
                        },
                    ),
            )
            .into_any_element()
    }

    fn body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(mux) = &self.mux else {
            let message = self
                .error
                .clone()
                .unwrap_or_else(|| "no session backend available".into());
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(self.theme.fg.opacity(0.8))
                .child(message)
                .into_any_element();
        };
        let snap = mux.snapshot();
        let Some(tab) = self.active_tab(&snap).cloned() else {
            return div().size_full().into_any_element();
        };

        let title = tab.title.clone();
        if title != self.title {
            self.title = title.clone();
            window.set_window_title(&format!("{title} — Tessera"));
        }

        // Cell rectangles, in cells, then in pixels.
        let rects_cells = if let Some(zoom) = self.zoomed {
            if zoom == tab.active {
                vec![(zoom, 0usize, 0usize, self.cols, self.rows)]
            } else {
                tab.layout.rects(self.cols, self.rows)
            }
        } else {
            tab.layout.rects(self.cols, self.rows)
        };

        let mut area = div().relative().flex_1().w_full().overflow_hidden();
        let mut pane_rects = HashMap::new();
        for (pane, x, y, w, h) in rects_cells {
            let rect = (
                EDGE + x as f32 * self.metrics.cell_w,
                EDGE + y as f32 * self.metrics.cell_h,
                w as f32 * self.metrics.cell_w,
                h as f32 * self.metrics.cell_h,
            );
            pane_rects.insert(pane, rect);
            area = area.child(self.pane_element(pane, rect, w, h, cx));
        }
        self.pane_rects = pane_rects;

        let tab_bar = self.tab_bar(&snap, cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(self.theme.bg)
            .text_color(self.theme.fg)
            .child(tab_bar)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .w_full()
                    .p(px(EDGE))
                    .bg(rgba(0x0018_1b20))
                    .child(area),
            )
            .into_any_element()
    }
}

impl Render for TerminalApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(
                cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key(ev, window, cx)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseDownEvent, window, _cx| this.focus.focus(window)),
            )
            .bg(self.theme.bg)
            .child(self.body(window, cx))
    }
}

fn split_for(tab: &tessera_core::mux::TabInfo, pane: PaneId) -> Option<(Vec<bool>, f32, SplitDir)> {
    fn walk(
        layout: &Layout,
        target: PaneId,
        path: &mut Vec<bool>,
    ) -> Option<(Vec<bool>, f32, SplitDir)> {
        match layout {
            Layout::Leaf(id) if *id == target => None,
            Layout::Leaf(_) => None,
            Layout::Split {
                dir,
                ratio,
                first,
                second,
            } => {
                if first.contains(target) {
                    path.push(false);
                    if let Some(found) = walk(first, target, path) {
                        return Some(found);
                    }
                    path.pop();
                    Some((path.clone(), *ratio, *dir))
                } else if second.contains(target) {
                    path.push(true);
                    if let Some(found) = walk(second, target, path) {
                        return Some(found);
                    }
                    path.pop();
                    Some((path.clone(), *ratio, *dir))
                } else {
                    None
                }
            }
        }
    }
    let mut path = Vec::new();
    walk(&tab.layout, pane, &mut path)
}

fn shell_path() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| {
        if cfg!(windows) {
            "powershell.exe".into()
        } else {
            "/bin/bash".into()
        }
    })
}

fn spawn_mux(config: &Config, cols: usize, rows: usize) -> Result<Box<dyn Mux>, String> {
    let shell = config.shell.clone().unwrap_or_else(shell_path);
    let native = || -> Result<Box<dyn Mux>, String> {
        let opts = tessera_core::native::NativeOptions::new(shell.clone(), cols, rows);
        tessera_core::native::NativeMux::new(opts)
            .map(|mux| Box::new(mux) as Box<dyn Mux>)
            .map_err(|err| format!("cannot start {shell}: {err}"))
    };

    #[cfg(unix)]
    {
        let wants_tmux = matches!(
            config.backend,
            tessera_core::config::Backend::Tmux | tessera_core::config::Backend::Auto
        );
        if wants_tmux && tessera_core::tmux::available() {
            if matches!(config.backend, tessera_core::config::Backend::Tmux) {
                let opts = tessera_core::tmux::TmuxOptions::new("tessera", cols, rows);
                return tessera_core::tmux::TmuxMux::new(opts)
                    .map(|mux| Box::new(mux) as Box<dyn Mux>)
                    .map_err(|err| format!("tmux session failed: {err}"));
            }
            // Auto: prefer tmux for persistence, fall back to native silently.
            if let Ok(mux) = tessera_core::tmux::TmuxMux::new(tessera_core::tmux::TmuxOptions::new(
                "tessera", cols, rows,
            )) {
                return Ok(Box::new(mux));
            }
        }
    }
    native()
}
