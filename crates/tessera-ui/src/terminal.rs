//! Painting a single pane: grid rows to GPUI text runs and quads.
//!
//! A pane is a `canvas` element. `prepaint` shapes every visible row (GPUI
//! caches line layouts by text+runs) and turns background/selection runs into
//! quads; `paint` just blits. All geometry is derived from integer cell
//! metrics, so text, quads and the mouse hit-test agree exactly.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    canvas, fill, font, point, px, size, Bounds, FontStyle, FontWeight, Hsla, PaintQuad, Pixels,
    Point, ShapedLine, SharedString, StrikethroughStyle, TextRun, UnderlineStyle,
};
use tessera_core::selection::{Pos, Selection};
use tessera_term::{Attrs, CursorStyle, Row, Term};

use crate::theme::Theme;

/// Space between the pane edge and the first cell.
pub const PADDING: f32 = 0.0;

#[derive(Clone)]
pub struct Metrics {
    pub cell_w: f32,
    pub cell_h: f32,
    pub ascent: f32,
    pub descent: f32,
    pub font_size: f32,
    pub family: SharedString,
}

impl Metrics {
    pub fn new(
        family: SharedString,
        font_size: Pixels,
        cell_w: Pixels,
        ascent: Pixels,
        descent: Pixels,
    ) -> Self {
        let line = (f32::from(ascent - descent) + 2.0).max(8.0);
        Self {
            cell_w: f32::from(cell_w).max(1.0),
            cell_h: line,
            ascent: f32::from(ascent),
            descent: f32::from(descent),
            font_size: f32::from(font_size),
            family,
        }
    }
}

#[derive(Clone)]
pub struct RunData {
    pub len: usize,
    pub color: Hsla,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
}

#[derive(Clone)]
pub struct BgSpan {
    pub col: usize,
    pub len: usize,
    pub color: Hsla,
}

#[derive(Clone)]
pub struct RowData {
    pub text: SharedString,
    pub runs: Vec<RunData>,
    pub bg: Vec<BgSpan>,
}

impl RowData {
    fn blank(cols: usize) -> Self {
        Self {
            text: " ".repeat(cols).into(),
            runs: vec![RunData {
                len: cols,
                color: Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 1.0,
                    a: 0.0,
                },
                bold: false,
                italic: false,
                underline: false,
                strike: false,
            }],
            bg: Vec::new(),
        }
    }
}

#[derive(Clone, Copy)]
pub struct CursorData {
    pub row: usize,
    pub col: usize,
    pub style: CursorStyle,
    pub on: bool,
}

pub struct RenderData {
    pub rows: Vec<RowData>,
    pub cursor: Option<CursorData>,
    /// Dim completion after the cursor: (row, col, text).
    pub ghost: Option<(usize, usize, SharedString)>,
    pub metrics: Metrics,
    pub theme: Theme,
}

impl RenderData {
    /// Build a full pane snapshot. `offset` is the scrollback distance in rows;
    /// `selection` uses absolute line indices (history + screen).
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        term: &Term,
        cols: usize,
        rows: usize,
        offset: usize,
        selection: Option<&Selection>,
        theme: &Theme,
        metrics: &Metrics,
        cursor_on: bool,
        ghost: Option<(usize, usize, &str)>,
    ) -> Self {
        let history = term.history_len();
        let offset = offset.min(history);
        let top = history - offset;

        let hide_cursor = offset > 0 || !term.cursor_visible();
        let mut out_rows = Vec::with_capacity(rows);
        for r in 0..rows {
            let line = top + r;
            let row = term.line(line);
            out_rows.push(build_row(row, cols, line, selection, theme));
        }

        let cursor = if hide_cursor {
            None
        } else {
            let c = term.screen().cursor;
            Some(CursorData {
                row: c.y.min(rows.saturating_sub(1)),
                col: c.x.min(cols.saturating_sub(1)),
                style: term.cursor_style(),
                on: cursor_on,
            })
        };
        let ghost = ghost.map(|(row, col, text)| (row, col, text.to_string().into()));
        Self {
            rows: out_rows,
            cursor,
            ghost,
            metrics: metrics.clone(),
            theme: theme.clone(),
        }
    }
}

fn build_row(
    row: Option<&Row>,
    cols: usize,
    line: usize,
    selection: Option<&Selection>,
    theme: &Theme,
) -> RowData {
    let Some(row) = row else {
        return RowData::blank(cols);
    };
    let mut text = String::with_capacity(cols);
    let mut runs: Vec<RunData> = Vec::new();
    let mut bg: Vec<BgSpan> = Vec::new();
    let mut run_start_col = 0usize;

    for col in 0..cols {
        let cell = row.cells.get(col).copied().unwrap_or_default();
        if cell.cont {
            continue;
        }
        let selected = selection.is_some_and(|sel| sel.contains(Pos { line, col }));
        let (mut fg, mut cell_bg) = theme.cell_colors(&cell);
        if selected {
            cell_bg = theme.selection;
        }
        if cell.attrs.has(Attrs::BLINK) {
            // Blink is cosmetic here; keep the text legible.
        }
        if cell.attrs.has(Attrs::HIDDEN) {
            fg = cell_bg;
        }
        text.push(cell.ch);

        let bold = cell.attrs.has(Attrs::BOLD);
        let italic = cell.attrs.has(Attrs::ITALIC);
        let underline = cell.attrs.has(Attrs::UNDERLINE);
        let strike = cell.attrs.has(Attrs::STRIKE);

        let same = runs.last().is_some_and(|r| {
            r.color == fg
                && r.bold == bold
                && r.italic == italic
                && r.underline == underline
                && r.strike == strike
        });
        if same {
            if let Some(last) = runs.last_mut() {
                last.len += cell.ch.len_utf8();
            }
        } else {
            runs.push(RunData {
                len: cell.ch.len_utf8(),
                color: fg,
                bold,
                italic,
                underline,
                strike,
            });
            run_start_col = col;
        }
        let _ = run_start_col;

        if cell_bg != theme.bg {
            let same_bg = bg
                .last()
                .is_some_and(|span| span.col + span.len == col && span.color == cell_bg);
            if same_bg {
                if let Some(span) = bg.last_mut() {
                    span.len += 1;
                }
            } else {
                bg.push(BgSpan {
                    col,
                    len: 1,
                    color: cell_bg,
                });
            }
        }
    }
    if text.is_empty() {
        text.push(' ');
    }
    RowData {
        text: text.into(),
        runs,
        bg,
    }
}

struct Prepared {
    lines: Vec<(ShapedLine, Point<Pixels>)>,
    quads: Vec<PaintQuad>,
    ghost: Option<(ShapedLine, Point<Pixels>)>,
    cursor: Option<PaintQuad>,
}

fn text_runs(row: &RowData, family: &SharedString, font_size: Pixels) -> Vec<TextRun> {
    row.runs
        .iter()
        .map(|run| TextRun {
            len: run.len,
            font: gpui::Font {
                family: family.clone(),
                weight: if run.bold {
                    FontWeight::BOLD
                } else {
                    FontWeight::NORMAL
                },
                style: if run.italic {
                    FontStyle::Italic
                } else {
                    FontStyle::Normal
                },
                ..font(family.clone())
            },
            color: run.color,
            background_color: None,
            underline: run.underline.then(|| UnderlineStyle {
                thickness: px(1.0),
                color: Some(run.color),
                wavy: false,
            }),
            strikethrough: run.strike.then(|| StrikethroughStyle {
                thickness: px(1.0),
                color: Some(run.color),
            }),
        })
        .collect()
}

/// A `canvas` element plus the shared data needed by its two phases.
pub fn visual(data: Rc<RenderData>) -> impl gpui::IntoElement {
    let prepaint_data = data.clone();
    let font_size = px(data.metrics.font_size);
    let _ = font_size;
    canvas(
        move |bounds, window, _cx| {
            let font_size = px(prepaint_data.metrics.font_size);
            let m = &prepaint_data.metrics;
            let cell_w = px(m.cell_w);
            let cell_h = px(m.cell_h);
            let mut prepared = Prepared {
                lines: Vec::new(),
                quads: Vec::new(),
                ghost: None,
                cursor: None,
            };
            for (row_idx, row) in prepaint_data.rows.iter().enumerate() {
                let runs = text_runs(row, &m.family, font_size);
                if runs.is_empty() {
                    continue;
                }
                let line = window.text_system().shape_line(
                    row.text.clone(),
                    font_size,
                    &runs,
                    Some(cell_w),
                );
                let y = bounds.top() + px(row_idx as f32 * m.cell_h);
                let pad = px(((m.cell_h - (m.ascent + m.descent)) * 0.5).max(0.0));
                prepared.lines.push((line, point(bounds.left(), y + pad)));
                for span in &row.bg {
                    let x = bounds.left() + px(span.col as f32 * m.cell_w);
                    prepared.quads.push(fill(
                        Bounds::new(point(x, y), size(px(span.len as f32 * m.cell_w), cell_h)),
                        span.color,
                    ));
                }
            }
            if let Some((row, col, text)) = &prepaint_data.ghost {
                let color = prepaint_data.theme.fg.opacity(0.45);
                let run = TextRun {
                    len: text.len(),
                    font: gpui::Font {
                        family: m.family.clone(),
                        ..font(m.family.clone())
                    },
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line =
                    window
                        .text_system()
                        .shape_line(text.clone(), font_size, &[run], Some(cell_w));
                let y = bounds.top() + px(*row as f32 * m.cell_h);
                let pad = px(((m.cell_h - (m.ascent + m.descent)) * 0.5).max(0.0));
                let x = bounds.left() + px(*col as f32 * m.cell_w);
                prepared.ghost = Some((line, point(x, y + pad)));
            }
            if let Some(cursor) = prepaint_data.cursor {
                if cursor.on
                    || !matches!(
                        cursor.style,
                        CursorStyle::BlockBlink
                            | CursorStyle::BarBlink
                            | CursorStyle::UnderlineBlink
                    )
                {
                    let m = &prepaint_data.metrics;
                    let x = bounds.left() + px(cursor.col as f32 * m.cell_w);
                    let y = bounds.top() + px(cursor.row as f32 * m.cell_h);
                    let color = prepaint_data.theme.cursor.opacity(0.65);
                    let quad = if cursor.style.is_bar() {
                        fill(Bounds::new(point(x, y), size(px(2.0), cell_h)), color)
                    } else if cursor.style.is_underline() {
                        fill(
                            Bounds::new(
                                point(x, y + px(m.cell_h - 2.0)),
                                size(px(m.cell_w), px(2.0)),
                            ),
                            color,
                        )
                    } else {
                        fill(Bounds::new(point(x, y), size(px(m.cell_w), cell_h)), color)
                    };
                    prepared.cursor = Some(quad);
                }
            }
            prepared
        },
        move |_bounds, mut prepared, window, cx| {
            for quad in prepared.quads.drain(..) {
                window.paint_quad(quad);
            }
            if let Some((line, origin)) = prepared.ghost.take() {
                let _ = line.paint(origin, px(data.metrics.cell_h), window, cx);
            }
            for (line, origin) in &prepared.lines {
                let _ = line.paint(*origin, px(data.metrics.cell_h), window, cx);
            }
            if let Some(quad) = prepared.cursor.take() {
                window.paint_quad(quad);
            }
        },
    )
    .size_full()
}

/// Cell under a point inside the pane bounds, or `None` outside the grid.
pub fn hit_cell(
    position: Point<Pixels>,
    bounds: Bounds<Pixels>,
    metrics: &Metrics,
    cols: usize,
    rows: usize,
) -> Option<(usize, usize)> {
    let x = f32::from(position.x - bounds.left()) - PADDING;
    let y = f32::from(position.y - bounds.top()) - PADDING;
    if x < 0.0 || y < 0.0 {
        return None;
    }
    let col = (x / metrics.cell_w) as usize;
    let row = (y / metrics.cell_h) as usize;
    (col < cols && row < rows).then_some((col, row))
}

/// Bounds of one cell inside a pane, for overlay UI.
pub fn cell_bounds(
    bounds: Bounds<Pixels>,
    metrics: &Metrics,
    col: usize,
    row: usize,
) -> Bounds<Pixels> {
    let origin = point(
        bounds.left() + px(PADDING + col as f32 * metrics.cell_w),
        bounds.top() + px(PADDING + row as f32 * metrics.cell_h),
    );
    Bounds::new(origin, size(px(metrics.cell_w), px(metrics.cell_h)))
}
