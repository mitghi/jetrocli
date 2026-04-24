//! jetrocli — split-pane TUI for jetro.

mod completion;

use anyhow::{anyhow, Result};
use clap::Parser;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste,
        EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    prelude::*,
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
    },
};
use serde_json::Value;
use std::{collections::{HashMap, HashSet}, fs, io, path::PathBuf};
use tui_textarea::TextArea;

use completion::{Candidate, CandKind};

/// jetro interactive TUI.
#[derive(Parser, Debug)]
#[command(name = "jetrocli", version)]
struct Cli {
    /// Load this JSON file into the left pane at startup.
    #[arg(short, long)]
    input: Option<PathBuf>,

    /// Pre-fill the expression input.
    #[arg(short, long)]
    expr: Option<String>,
}

enum Focus { Json, Expr, Result }

struct App<'a> {
    json:        JsonEditor,
    expr_area:   TextArea<'a>,
    result:      JsonEditor,
    focus:       Focus,

    parsed_doc:  Option<Value>,
    parse_err:   Option<String>,

    result_text: String,

    popup_open:  bool,
    candidates:  Vec<Candidate>,
    popup_state: ListState,

    chord:       Option<char>, // pending prefix key (e.g. Some('c') after C-c)
}

impl<'a> App<'a> {
    fn new(json_seed: String, expr_seed: String) -> Self {
        let pretty = serde_json::from_str::<Value>(&json_seed)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or(json_seed);
        let json = JsonEditor::from_text(&pretty);

        let mut expr_area = TextArea::default();
        if !expr_seed.is_empty() {
            for ch in expr_seed.chars() { expr_area.insert_char(ch); }
        }
        expr_area.set_block(Block::default().borders(Borders::ALL).title(" expr "));

        let mut app = Self {
            json,
            expr_area,
            result: JsonEditor::from_text(""),
            focus: Focus::Expr,
            parsed_doc: None,
            parse_err: None,
            result_text: String::new(),
            popup_open: false,
            candidates: vec![],
            popup_state: ListState::default(),
            chord: None,
        };
        app.reparse_json();
        app.evaluate();
        app
    }

    fn reparse_json(&mut self) {
        let src = self.json.text();
        if src.trim().is_empty() {
            self.parsed_doc = None;
            self.parse_err = None;
            return;
        }
        match serde_json::from_str::<Value>(&src) {
            Ok(v)  => { self.parsed_doc = Some(v); self.parse_err = None; }
            Err(e) => { self.parsed_doc = None;    self.parse_err = Some(e.to_string()); }
        }
    }

    fn evaluate(&mut self) {
        let expr = self.expr_text();
        if expr.trim().is_empty() {
            self.result_text.clear();
            self.sync_result_view();
            return;
        }
        let Some(doc) = &self.parsed_doc else {
            if let Some(err) = &self.parse_err {
                self.result_text = format!("(JSON parse error)\n{}", err);
            } else {
                self.result_text.clear();
            }
            self.sync_result_view();
            return;
        };
        match jetro::query(&expr, doc) {
            Ok(v)  => {
                self.result_text = serde_json::to_string_pretty(&v)
                    .unwrap_or_else(|_| v.to_string());
            }
            Err(e) => self.result_text = format!("error: {}", e),
        }
        self.sync_result_view();
    }

    fn sync_result_view(&mut self) {
        let prev_folded = std::mem::take(&mut self.result.folded);
        let prev_scroll = self.result.scroll_row;
        let prev_row    = self.result.row;
        let prev_col    = self.result.col;
        self.result = JsonEditor::from_text(&self.result_text);
        // best-effort preserve scroll/cursor if within bounds
        if prev_row < self.result.lines.len() {
            self.result.row = prev_row;
            self.result.col = prev_col;
            self.result.scroll_row = prev_scroll.min(self.result.lines.len().saturating_sub(1));
            self.result.folded = prev_folded;
            self.result.clamp_all();
        }
    }

    fn refresh_completions(&mut self) {
        let expr = self.expr_text();
        let cursor_byte = expr_cursor_byte(&self.expr_area, &expr);
        let Some(doc) = &self.parsed_doc else {
            self.candidates = vec![];
            return;
        };
        self.candidates = completion::complete(&expr, cursor_byte, doc);
        if self.candidates.is_empty() {
            self.popup_state.select(None);
        } else if self.popup_state.selected().is_none() {
            self.popup_state.select(Some(0));
        }
    }

    fn expr_text(&self) -> String {
        self.expr_area.lines().join("\n")
    }

    fn accept_completion(&mut self) {
        let Some(idx) = self.popup_state.selected() else { return; };
        let Some(cand) = self.candidates.get(idx).cloned() else { return; };
        let current = self.expr_text();
        let cursor_byte = expr_cursor_byte(&self.expr_area, &current);

        // Replace identifier under cursor.
        let (word_start, _) = word_bounds(&current, cursor_byte);
        let before = &current[..word_start];
        let after  = &current[cursor_byte..];

        let insert = cand.text.clone();
        // For method snippets with `()`, place cursor inside parens
        let place_cursor_offset = if insert.ends_with("()") {
            // keep at end-1 so user is inside parens
            insert.len() - 1
        } else {
            insert.len()
        };
        let new_expr = format!("{}{}{}", before, insert, after);

        self.expr_area.select_all();
        self.expr_area.cut();
        for line in new_expr.split('\n') {
            for ch in line.chars() { self.expr_area.insert_char(ch); }
            self.expr_area.insert_newline();
        }
        // remove trailing newline from loop
        self.expr_area.delete_char();

        // best-effort cursor placement: go to end of `before + insert (- offset if snippet)`
        let target_byte = before.len() + place_cursor_offset;
        seek_cursor_to_byte(&mut self.expr_area, target_byte);

        self.popup_open = false;
        self.evaluate();
    }
}

fn expr_cursor_byte(area: &TextArea<'_>, text: &str) -> usize {
    let (row, col) = area.cursor();
    let mut byte = 0usize;
    for (i, line) in text.split('\n').enumerate() {
        if i == row {
            byte += line.char_indices().nth(col).map(|(b, _)| b).unwrap_or(line.len());
            return byte;
        }
        byte += line.len() + 1;
    }
    text.len()
}

fn seek_cursor_to_byte(area: &mut TextArea<'_>, target: usize) {
    let text = area.lines().join("\n");
    let mut seen = 0usize;
    for (row, line) in text.split('\n').enumerate() {
        let line_end = seen + line.len();
        if target <= line_end {
            let col_byte = target - seen;
            let col = line[..col_byte.min(line.len())].chars().count();
            area.move_cursor(tui_textarea::CursorMove::Jump(row as u16, col as u16));
            return;
        }
        seen = line_end + 1;
    }
}

fn word_bounds(s: &str, cursor: usize) -> (usize, usize) {
    let bytes = s.as_bytes();
    let mut start = cursor;
    while start > 0 {
        let c = bytes[start - 1] as char;
        if c.is_alphanumeric() || c == '_' { start -= 1; } else { break; }
    }
    let mut end = cursor;
    while end < bytes.len() {
        let c = bytes[end] as char;
        if c.is_alphanumeric() || c == '_' { end += 1; } else { break; }
    }
    (start, end)
}

// ── JSON editor with structural folding ─────────────────────────────────────

struct JsonEditor {
    lines:      Vec<String>,
    row:        usize,
    col:        usize,
    scroll_row: usize,
    folded:     HashSet<usize>, // fold header rows currently collapsed
    view_h:     usize,          // last rendered inner height (for page-nav)
}

impl JsonEditor {
    fn from_text(s: &str) -> Self {
        let lines: Vec<String> = if s.is_empty() {
            vec![String::new()]
        } else {
            s.split('\n').map(|l| l.to_string()).collect()
        };
        Self { lines, row: 0, col: 0, scroll_row: 0, folded: HashSet::new(), view_h: 20 }
    }

    fn text(&self) -> String { self.lines.join("\n") }

    fn clamp_col(&mut self) {
        let max = self.lines[self.row].chars().count();
        if self.col > max { self.col = max; }
    }

    fn clamp_all(&mut self) {
        if self.lines.is_empty() { self.lines.push(String::new()); }
        if self.row >= self.lines.len() { self.row = self.lines.len() - 1; }
        self.clamp_col();
        let vlen = self.lines.len();
        if self.scroll_row >= vlen { self.scroll_row = vlen.saturating_sub(1); }
    }

    fn col_byte(&self) -> usize {
        self.lines[self.row]
            .char_indices()
            .nth(self.col)
            .map(|(b, _)| b)
            .unwrap_or(self.lines[self.row].len())
    }

    fn insert_char(&mut self, c: char) {
        self.folded.clear();
        let byte = self.col_byte();
        self.lines[self.row].insert(byte, c);
        self.col += 1;
        self.clamp_all();
    }

    fn insert_str(&mut self, s: &str) {
        self.folded.clear();
        for (i, part) in s.split('\n').enumerate() {
            if i > 0 { self.newline_raw(); }
            let byte = self.col_byte();
            self.lines[self.row].insert_str(byte, part);
            self.col += part.chars().count();
        }
        self.clamp_all();
    }

    fn newline_raw(&mut self) {
        let byte = self.col_byte();
        let rest = self.lines[self.row].split_off(byte);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    fn newline(&mut self) { self.folded.clear(); self.newline_raw(); self.clamp_all(); }

    fn backspace(&mut self) {
        self.folded.clear();
        if self.col > 0 {
            let end = self.col_byte();
            let prev = self.lines[self.row][..end]
                .char_indices().last().map(|(b, _)| b).unwrap_or(0);
            self.lines[self.row].replace_range(prev..end, "");
            self.col -= 1;
        } else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.lines[self.row].chars().count();
            self.lines[self.row].push_str(&cur);
        }
        self.clamp_all();
    }

    fn delete(&mut self) {
        self.folded.clear();
        let len = self.lines[self.row].chars().count();
        if self.col < len {
            let byte = self.col_byte();
            let next = self.lines[self.row][byte..]
                .char_indices().nth(1)
                .map(|(b, _)| byte + b)
                .unwrap_or(self.lines[self.row].len());
            self.lines[self.row].replace_range(byte..next, "");
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
        self.clamp_all();
    }

    fn home(&mut self) { self.col = 0; }
    fn end(&mut self)  { self.col = self.lines[self.row].chars().count(); }

    fn kill_line(&mut self) {
        self.folded.clear();
        let len = self.lines[self.row].chars().count();
        if self.col < len {
            let byte = self.col_byte();
            self.lines[self.row].truncate(byte);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
        self.clamp_all();
    }

    fn move_left(&mut self) {
        if self.col > 0 { self.col -= 1; }
        else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].chars().count();
        }
    }

    fn move_right(&mut self) {
        let len = self.lines[self.row].chars().count();
        if self.col < len { self.col += 1; }
        else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    fn move_up(&mut self, folds: &HashMap<usize, usize>) {
        if self.row == 0 { self.col = 0; return; }
        self.row -= 1;
        // snap into header if we landed inside a collapsed range
        for (&h, &e) in folds {
            if self.folded.contains(&h) && self.row > h && self.row <= e {
                self.row = h;
                break;
            }
        }
        self.clamp_col();
    }

    fn move_down(&mut self, folds: &HashMap<usize, usize>) {
        if self.folded.contains(&self.row) {
            if let Some(&e) = folds.get(&self.row) {
                if e + 1 < self.lines.len() { self.row = e + 1; }
                else { self.row = self.lines.len() - 1; }
                self.clamp_col();
                return;
            }
        }
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.clamp_col();
        } else {
            self.col = self.lines[self.row].chars().count();
        }
    }

    fn page_down(&mut self, folds: &HashMap<usize, usize>) {
        let n = self.view_h.saturating_sub(1).max(1);
        for _ in 0..n { self.move_down(folds); }
    }

    fn page_up(&mut self, folds: &HashMap<usize, usize>) {
        let n = self.view_h.saturating_sub(1).max(1);
        for _ in 0..n { self.move_up(folds); }
    }

    fn toggle_fold(&mut self, folds: &HashMap<usize, usize>) {
        if folds.contains_key(&self.row) {
            if self.folded.contains(&self.row) { self.folded.remove(&self.row); }
            else { self.folded.insert(self.row); }
            return;
        }
        // not on a header: collapse enclosing fold
        let mut best: Option<(usize, usize)> = None;
        for (&h, &e) in folds {
            if self.row > h && self.row <= e {
                if best.map_or(true, |(bh, _)| h > bh) { best = Some((h, e)); }
            }
        }
        if let Some((h, _)) = best {
            self.folded.insert(h);
            self.row = h;
            self.clamp_col();
        }
    }

    fn fold_all(&mut self, folds: &HashMap<usize, usize>) {
        for &h in folds.keys() { self.folded.insert(h); }
        // snap cursor to nearest enclosing header
        let cur = self.row;
        let mut best: Option<usize> = None;
        for (&h, &e) in folds {
            if cur >= h && cur <= e {
                if best.map_or(true, |bh| h > bh) { best = Some(h); }
            }
        }
        if let Some(h) = best { self.row = h; self.clamp_col(); }
    }

    fn unfold_all(&mut self) { self.folded.clear(); }
}

fn detect_folds(lines: &[String]) -> HashMap<usize, usize> {
    let mut stack: Vec<(usize, char)> = Vec::new();
    let mut out = HashMap::new();
    for (i, line) in lines.iter().enumerate() {
        let mut in_str = false;
        let mut esc = false;
        for c in line.chars() {
            if esc { esc = false; continue; }
            if in_str {
                if c == '\\' { esc = true; }
                else if c == '"' { in_str = false; }
                continue;
            }
            match c {
                '"' => in_str = true,
                '{' => stack.push((i, '}')),
                '[' => stack.push((i, ']')),
                '}' | ']' => {
                    if let Some((start, close)) = stack.pop() {
                        if close == c && i > start { out.insert(start, i); }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

// ── UI ───────────────────────────────────────────────────────────────────────

// palette
const C_ACCENT:   Color = Color::Rgb(0x7a, 0xa2, 0xf7); // soft blue
const C_ACCENT2:  Color = Color::Rgb(0xbb, 0x9a, 0xf7); // violet
const C_OK:       Color = Color::Rgb(0x9e, 0xce, 0x6a); // green
const C_ERR:      Color = Color::Rgb(0xf7, 0x76, 0x8e); // red
const C_WARN:     Color = Color::Rgb(0xe0, 0xaf, 0x68); // amber
const C_MUTED:    Color = Color::Rgb(0x56, 0x5f, 0x89);
const C_STR:      Color = Color::Rgb(0x9e, 0xce, 0x6a);
const C_KEY:      Color = Color::Rgb(0x7d, 0xcf, 0xff);
const C_NUM:      Color = Color::Rgb(0xff, 0x9e, 0x64);
const C_BOOL:     Color = Color::Rgb(0xbb, 0x9a, 0xf7);
const C_BRACE:    Color = Color::Rgb(0xc0, 0xca, 0xf5);
const C_PUNCT:    Color = Color::Rgb(0x56, 0x5f, 0x89);

fn draw(frame: &mut Frame, app: &mut App) {
    let size = frame.area();

    let vertical = Layout::vertical([
        Constraint::Length(1),  // header
        Constraint::Min(8),     // panes
        Constraint::Length(10), // expr
        Constraint::Length(1),  // status
    ]).split(size);

    draw_header(frame, vertical[0], app);

    let top = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ]).split(vertical[1]);

    draw_json_pane(frame, top[0], app);
    draw_result_pane(frame, top[1], app);
    draw_expr_pane(frame, vertical[2], app);
    draw_status(frame, vertical[3], app);

    if app.popup_open && !app.candidates.is_empty() {
        draw_popup(frame, vertical[2], size, app);
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let mode = match app.focus {
        Focus::Json   => Span::styled(" JSON ",   Style::default().bg(C_ACCENT).fg(Color::Black).bold()),
        Focus::Expr   => Span::styled(" EXPR ",   Style::default().bg(C_ACCENT2).fg(Color::Black).bold()),
        Focus::Result => Span::styled(" RESULT ", Style::default().bg(C_OK).fg(Color::Black).bold()),
    };
    let state = if app.parse_err.is_some() {
        Span::styled(" ● parse error ", Style::default().fg(C_ERR).bold())
    } else if app.result_text.starts_with("error:") {
        Span::styled(" ● eval error ", Style::default().fg(C_WARN).bold())
    } else {
        Span::styled(" ● ready ", Style::default().fg(C_OK).bold())
    };

    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled("✦", Style::default().fg(C_ACCENT2)),
        Span::raw(" "),
        Span::styled("jetro", Style::default().fg(C_ACCENT).bold()),
        Span::styled("cli", Style::default().fg(C_ACCENT2).bold()),
        Span::raw("  "),
        Span::styled("interactive jetro REPL", Style::default().fg(C_MUTED).italic()),
    ]);

    let right = Line::from(vec![state, Span::raw(" "), mode, Span::raw(" ")])
        .alignment(Alignment::Right);

    frame.render_widget(Paragraph::new(title), area);
    frame.render_widget(Paragraph::new(right), area);
}

fn draw_json_pane(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = matches!(app.focus, Focus::Json);
    let (title_icon_color, badge) = match &app.parse_err {
        Some(_) => (C_ERR, Span::styled(" ✗ parse ", Style::default().bg(C_ERR).fg(Color::Black).bold())),
        None    => (C_OK,  Span::styled(" ✓ valid ", Style::default().bg(C_OK).fg(Color::Black).bold())),
    };

    let folds_count = detect_folds(&app.json.lines).len();
    let folded_count = app.json.folded.len();
    let fold_badge = if folds_count > 0 {
        Span::styled(
            format!(" ⋔ {}/{} ", folded_count, folds_count),
            Style::default().bg(C_ACCENT2).fg(Color::Black).bold(),
        )
    } else {
        Span::raw("")
    };

    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled("◆", Style::default().fg(title_icon_color).bold()),
        Span::raw(" "),
        Span::styled("JSON input", Style::default().fg(if focused { C_ACCENT } else { C_MUTED }).bold()),
        Span::raw("  "),
        badge,
        Span::raw(" "),
        fold_badge,
        Span::raw(" "),
    ]);
    let block = pane_block(title, focused);
    draw_editor(frame, area, &mut app.json, block, focused);
}

fn draw_editor(frame: &mut Frame, area: Rect, ed: &mut JsonEditor, block: Block, focused: bool) {
    let folds = detect_folds(&ed.lines);
    let inner_w = area.width.saturating_sub(2) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;
    ed.view_h = inner_h;

    let mut visible: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i < ed.lines.len() {
        visible.push(i);
        if ed.folded.contains(&i) {
            if let Some(&e) = folds.get(&i) { i = e + 1; continue; }
        }
        i += 1;
    }

    if !visible.contains(&ed.row) {
        let cur = ed.row;
        let mut header = cur;
        for (&h, &e) in &folds {
            if cur > h && cur <= e && ed.folded.contains(&h) {
                if h < header || header == cur { header = h; }
            }
        }
        ed.row = header;
        ed.clamp_col();
    }

    let cursor_vis = visible.iter().position(|&r| r == ed.row).unwrap_or(0);
    if cursor_vis < ed.scroll_row { ed.scroll_row = cursor_vis; }
    if inner_h > 0 && cursor_vis >= ed.scroll_row + inner_h {
        ed.scroll_row = cursor_vis + 1 - inner_h;
    }
    let start = ed.scroll_row.min(visible.len().saturating_sub(1));
    let end = (start + inner_h).min(visible.len());

    let gutter_w = 2usize;
    let mut body: Vec<Line> = Vec::with_capacity(end - start);
    for &row in &visible[start..end] {
        let is_fold_header = folds.contains_key(&row);
        let is_folded = ed.folded.contains(&row);
        let gutter = if is_fold_header {
            if is_folded { "▸ " } else { "▾ " }
        } else { "  " };
        let gutter_span = Span::styled(
            gutter.to_string(),
            Style::default().fg(if is_fold_header { C_WARN } else { C_MUTED }),
        );

        let mut spans: Vec<Span<'static>> = vec![gutter_span];
        spans.extend(highlight_json_spans(&ed.lines[row]));

        if is_folded {
            if let Some(&e) = folds.get(&row) {
                let inner = e.saturating_sub(row).saturating_sub(1);
                spans.push(Span::styled(
                    format!("  ⋯ {} lines ", inner + 1),
                    Style::default().bg(C_MUTED).fg(Color::Black).italic(),
                ));
                let close_trim = ed.lines[e].trim_start();
                spans.push(Span::styled(
                    format!(" {}", close_trim),
                    Style::default().fg(C_BRACE).bold(),
                ));
            }
        }

        let mut line = Line::from(spans);
        if focused && row == ed.row {
            line = line.style(Style::default().bg(Color::Rgb(0x29, 0x2e, 0x42)));
        }
        body.push(line);
    }

    let para = Paragraph::new(body).block(block);
    frame.render_widget(para, area);

    if focused && inner_h > 0 && inner_w > 0 {
        let vis_idx = cursor_vis.saturating_sub(start);
        if vis_idx < inner_h {
            let cursor_y = area.y + 1 + vis_idx as u16;
            let cursor_x = area.x + 1 + gutter_w as u16 + ed.col as u16;
            let max_x = area.x + area.width.saturating_sub(1);
            let max_y = area.y + area.height.saturating_sub(1);
            if cursor_x < max_x && cursor_y < max_y {
                frame.set_cursor_position((cursor_x, cursor_y));
            }
        }
    }
}

fn draw_result_pane(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = matches!(app.focus, Focus::Result);
    let is_err = app.result_text.starts_with("error:") || app.result_text.starts_with("(JSON parse error)");
    let badge = if is_err {
        Span::styled(" ! error ", Style::default().bg(C_ERR).fg(Color::Black).bold())
    } else if app.result_text.is_empty() {
        Span::styled(" ∅ empty ", Style::default().bg(C_MUTED).fg(Color::Black).bold())
    } else {
        Span::styled(" » ok ", Style::default().bg(C_OK).fg(Color::Black).bold())
    };

    let folds_count = if !is_err && !app.result_text.is_empty() {
        detect_folds(&app.result.lines).len()
    } else { 0 };
    let folded_count = app.result.folded.len();
    let fold_badge = if folds_count > 0 {
        Span::styled(
            format!(" ⋔ {}/{} ", folded_count, folds_count),
            Style::default().bg(C_ACCENT2).fg(Color::Black).bold(),
        )
    } else { Span::raw("") };

    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled("◈", Style::default().fg(C_ACCENT2).bold()),
        Span::raw(" "),
        Span::styled("result", Style::default().fg(if focused { C_ACCENT } else { C_ACCENT2 }).bold()),
        Span::raw("  "),
        badge,
        Span::raw(" "),
        fold_badge,
        Span::raw(" "),
    ]);
    let block = pane_block(title, focused);

    if is_err {
        let body: Vec<Line> = app.result_text.lines().map(|l| {
            Line::from(Span::styled(l.to_string(), Style::default().fg(C_ERR)))
        }).collect();
        let para = Paragraph::new(body).block(block).wrap(Wrap { trim: false });
        frame.render_widget(para, area);
    } else if app.result_text.is_empty() {
        let body = vec![
            Line::from(""),
            Line::from(Span::styled("  (type an expression below to query the JSON)",
                Style::default().fg(C_MUTED).italic())),
        ];
        let para = Paragraph::new(body).block(block).wrap(Wrap { trim: false });
        frame.render_widget(para, area);
    } else {
        draw_editor(frame, area, &mut app.result, block, focused);
    }
}

fn draw_expr_pane(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = matches!(app.focus, Focus::Expr);
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled("❯", Style::default().fg(if focused { C_ACCENT } else { C_MUTED }).bold()),
        Span::raw(" "),
        Span::styled("expr", Style::default().fg(if focused { C_ACCENT } else { C_MUTED }).bold()),
        Span::raw(" "),
    ]);
    let block = pane_block(title, focused);
    app.expr_area.set_block(block);
    // cursor style
    let cursor_style = if focused {
        Style::default().bg(C_ACCENT).fg(Color::Black)
    } else {
        Style::default().bg(C_MUTED).fg(Color::Black)
    };
    app.expr_area.set_cursor_style(cursor_style);
    frame.render_widget(&app.expr_area, area);
}

fn pane_block<'a>(title: Line<'a>, focused: bool) -> Block<'a> {
    let border_style = if focused {
        Style::default().fg(C_ACCENT).bold()
    } else {
        Style::default().fg(C_MUTED)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(if focused { BorderType::Thick } else { BorderType::Rounded })
        .border_style(border_style)
        .title(title)
}

fn draw_status(frame: &mut Frame, area: Rect, _app: &App) {
    let bg = Style::default().bg(Color::Rgb(0x1a, 0x1b, 0x26));
    frame.render_widget(Paragraph::new("").style(bg), area);

    let key = |k: &'static str| Span::styled(
        format!(" {} ", k),
        Style::default().bg(C_WARN).fg(Color::Black).bold(),
    );
    let lbl = |s: &'static str| Span::styled(
        format!(" {}  ", s),
        Style::default().fg(Color::Rgb(0xa9, 0xb1, 0xd6)),
    );

    let line = Line::from(vec![
        Span::raw(" "),
        key("C-o"), lbl("switch"),
        key("C-c f"), lbl("fold"),
        key("C-c a/u"), lbl("all/none"),
        key("C-␣"), lbl("complete"),
        key("C-c C-f"), lbl("fmt"),
        key("C-x C-s"), lbl("copy"),
        key("S-⏎"), lbl("eval"),
        key("C-c C-c"), lbl("quit"),
    ]).style(bg);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_popup(frame: &mut Frame, expr_area: Rect, size: Rect, app: &mut App) {
    let list_w: u16 = 34;
    let doc_w:  u16 = 48;
    let gap:    u16 = 0;
    let total_w = (list_w + gap + doc_w).min(size.width.saturating_sub(4));
    let popup_h = (app.candidates.len() as u16 + 2).min(14).max(6);

    let popup_x = expr_area.x + 2;
    let popup_y = expr_area.y.saturating_sub(popup_h);
    let outer = Rect { x: popup_x, y: popup_y, width: total_w, height: popup_h };

    // split into list | doc
    let show_doc = total_w > list_w + 8;
    let (list_rect, doc_rect) = if show_doc {
        let cols = Layout::horizontal([
            Constraint::Length(list_w),
            Constraint::Min(8),
        ]).split(outer);
        (cols[0], Some(cols[1]))
    } else {
        (outer, None)
    };

    let items: Vec<ListItem> = app.candidates.iter().map(|c| {
        let (tag, color) = match c.kind {
            CandKind::Field   => ("fld", C_OK),
            CandKind::Method  => ("fn ", C_ACCENT),
            CandKind::Keyword => ("kw ", C_ACCENT2),
            CandKind::Snippet => ("snp", C_WARN),
        };
        ListItem::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(format!(" {} ", tag),
                Style::default().bg(color).fg(Color::Black).bold()),
            Span::raw(" "),
            Span::styled(c.text.clone(), Style::default().fg(Color::Rgb(0xc0, 0xca, 0xf5))),
        ]))
    }).collect();

    let list_title = Line::from(vec![
        Span::raw(" "),
        Span::styled("✨", Style::default().fg(C_WARN)),
        Span::raw(" "),
        Span::styled("completions", Style::default().fg(C_ACCENT).bold()),
        Span::raw(" "),
        Span::styled(format!("({})", app.candidates.len()), Style::default().fg(C_MUTED)),
        Span::raw(" "),
    ]);

    let list = List::new(items)
        .block(Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(C_ACCENT2))
            .title(list_title))
        .highlight_style(Style::default().bg(C_ACCENT).fg(Color::Black).bold())
        .highlight_symbol("▶ ");

    frame.render_widget(Clear, outer);
    frame.render_stateful_widget(list, list_rect, &mut app.popup_state);

    if let Some(doc_area) = doc_rect {
        let selected = app.popup_state.selected()
            .and_then(|i| app.candidates.get(i));
        let body: Vec<Line> = match selected {
            Some(c) => render_doc(c),
            None => vec![Line::from(Span::styled("no selection",
                Style::default().fg(C_MUTED).italic()))],
        };
        let doc_title = Line::from(vec![
            Span::raw(" "),
            Span::styled("📖", Style::default().fg(C_WARN)),
            Span::raw(" "),
            Span::styled("docs", Style::default().fg(C_ACCENT2).bold()),
            Span::raw(" "),
        ]);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(C_MUTED))
            .title(doc_title);
        let para = Paragraph::new(body).block(block).wrap(Wrap { trim: false });
        frame.render_widget(para, doc_area);
    }
}

fn render_doc(c: &Candidate) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut iter = c.doc.lines().peekable();
    // first line = signature / header → bold accent
    if let Some(first) = iter.next() {
        lines.push(Line::from(Span::styled(
            first.to_string(),
            Style::default().fg(C_ACCENT).bold(),
        )));
    }
    let mut in_example = false;
    while let Some(line) = iter.next() {
        let l = line.to_string();
        if l.trim_end() == "Example:" {
            in_example = true;
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Example".to_string(),
                Style::default().fg(C_WARN).bold(),
            )));
            continue;
        }
        if l.is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        if in_example {
            // strip leading 2-space indent, highlight as jetro-like code
            let code = l.trim_start_matches("  ");
            let spans = highlight_expr_spans(code);
            let mut s = vec![Span::raw("  ")];
            s.extend(spans);
            lines.push(Line::from(s));
        } else {
            lines.push(Line::from(Span::styled(
                l,
                Style::default().fg(Color::Rgb(0xa9, 0xb1, 0xd6)),
            )));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no docs)".to_string(),
            Style::default().fg(C_MUTED).italic(),
        )));
    }
    lines
}

/// Lightweight highlight for jetro example lines.
fn highlight_expr_spans(line: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            let start = i; i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < chars.len() { i += 2; continue; }
                i += 1;
            }
            if i < chars.len() { i += 1; }
            let s: String = chars[start..i].iter().collect();
            spans.push(Span::styled(s, Style::default().fg(C_STR)));
        } else if c == '$' || c == '@' {
            spans.push(Span::styled(c.to_string(), Style::default().fg(C_ACCENT2).bold()));
            i += 1;
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
            let s: String = chars[start..i].iter().collect();
            let is_method = i < chars.len() && chars[i] == '(';
            let kws = ["lambda","let","not","and","or","kind","when","for","in","if"];
            let style = if kws.contains(&s.as_str()) {
                Style::default().fg(C_BOOL).bold()
            } else if is_method {
                Style::default().fg(C_ACCENT).bold()
            } else {
                Style::default().fg(C_KEY)
            };
            spans.push(Span::styled(s, style));
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') { i += 1; }
            let s: String = chars[start..i].iter().collect();
            spans.push(Span::styled(s, Style::default().fg(C_NUM)));
        } else if "()[]{}".contains(c) {
            spans.push(Span::styled(c.to_string(), Style::default().fg(C_BRACE).bold()));
            i += 1;
        } else if c == '.' || c == ',' || c == ':' || c == ';' {
            spans.push(Span::styled(c.to_string(), Style::default().fg(C_PUNCT)));
            i += 1;
        } else {
            spans.push(Span::raw(c.to_string()));
            i += 1;
        }
    }
    spans
}

// ── JSON syntax highlight ────────────────────────────────────────────────────

fn highlight_json(s: &str) -> Vec<Line<'static>> {
    s.lines().map(|l| Line::from(highlight_json_spans(l))).collect()
}

fn highlight_json_spans(line: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '"' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && i + 1 < bytes.len() { i += 2; continue; }
                if bytes[i] == b'"' { i += 1; break; }
                i += 1;
            }
            let mut j = i;
            while j < bytes.len() && (bytes[j] as char).is_whitespace() { j += 1; }
            let is_key = j < bytes.len() && bytes[j] == b':';
            let text = line[start..i].to_string();
            let style = if is_key {
                Style::default().fg(C_KEY).bold()
            } else {
                Style::default().fg(C_STR)
            };
            spans.push(Span::styled(text, style));
        } else if c.is_ascii_digit()
            || (c == '-' && i + 1 < bytes.len() && (bytes[i + 1] as char).is_ascii_digit())
        {
            let start = i;
            i += 1;
            while i < bytes.len() {
                let cc = bytes[i] as char;
                if cc.is_ascii_digit() || cc == '.' || cc == 'e' || cc == 'E' || cc == '+' || cc == '-' {
                    i += 1;
                } else { break; }
            }
            spans.push(Span::styled(line[start..i].to_string(),
                Style::default().fg(C_NUM)));
        } else if line[i..].starts_with("true") {
            spans.push(Span::styled("true".to_string(), Style::default().fg(C_BOOL).bold()));
            i += 4;
        } else if line[i..].starts_with("false") {
            spans.push(Span::styled("false".to_string(), Style::default().fg(C_BOOL).bold()));
            i += 5;
        } else if line[i..].starts_with("null") {
            spans.push(Span::styled("null".to_string(), Style::default().fg(C_MUTED).italic()));
            i += 4;
        } else if "{}[]".contains(c) {
            spans.push(Span::styled(c.to_string(),
                Style::default().fg(C_BRACE).bold()));
            i += 1;
        } else if c == ':' || c == ',' {
            spans.push(Span::styled(c.to_string(), Style::default().fg(C_PUNCT)));
            i += 1;
        } else {
            spans.push(Span::raw(c.to_string()));
            i += 1;
        }
    }
    spans
}

// ── Event loop ───────────────────────────────────────────────────────────────

fn run<'a>(app: &mut App<'a>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
    )?;
    terminal.show_cursor()?;
    result
}

fn event_loop<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;

        let key = match event::read()? {
            Event::Key(k) => k,
            Event::Paste(data) => {
                match app.focus {
                    Focus::Json => {
                        app.json.insert_str(&data);
                        app.reparse_json();
                    }
                    Focus::Expr => {
                        app.expr_area.insert_str(&data);
                        app.refresh_completions();
                        app.popup_open = !app.candidates.is_empty();
                    }
                    Focus::Result => { /* read-only */ }
                }
                app.evaluate();
                continue;
            }
            _ => continue,
        };
        if key.kind != event::KeyEventKind::Press { continue; }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // chord resolution: last key was C-c, this key is the action
        if app.chord == Some('c') {
            app.chord = None;
            // C-c C-c → quit
            if ctrl && matches!(key.code, KeyCode::Char('c')) { return Ok(()); }
            // C-g cancels chord
            if ctrl && matches!(key.code, KeyCode::Char('g')) { continue; }
            // C-c C-f → format current focus
            if ctrl && matches!(key.code, KeyCode::Char('f')) {
                match app.focus {
                    Focus::Json   => reformat_json(app),
                    Focus::Expr   => reformat_expr(app),
                    Focus::Result => { /* already pretty */ }
                }
                continue;
            }
            // C-c C-k → clear current buffer
            if ctrl && matches!(key.code, KeyCode::Char('k')) {
                match app.focus {
                    Focus::Json => {
                        app.json = JsonEditor::from_text("");
                        app.reparse_json();
                        app.evaluate();
                    }
                    Focus::Expr => {
                        app.expr_area.select_all();
                        app.expr_area.cut();
                        app.popup_open = false;
                        app.candidates.clear();
                        app.evaluate();
                    }
                    Focus::Result => {
                        app.result_text.clear();
                        app.result = JsonEditor::from_text("");
                    }
                }
                continue;
            }
            // fold ops act on focused editor (Json or Result)
            let editor: Option<&mut JsonEditor> = match app.focus {
                Focus::Json   => Some(&mut app.json),
                Focus::Result => Some(&mut app.result),
                Focus::Expr   => None,
            };
            match key.code {
                KeyCode::Char('f') => {
                    if let Some(e) = editor {
                        let folds = detect_folds(&e.lines);
                        e.toggle_fold(&folds);
                    }
                }
                KeyCode::Char('a') => {
                    if let Some(e) = editor {
                        let folds = detect_folds(&e.lines);
                        e.fold_all(&folds);
                    }
                }
                KeyCode::Char('u') => {
                    if let Some(e) = editor { e.unfold_all(); }
                }
                KeyCode::Esc => {}
                _ => {}
            }
            continue;
        }

        // chord resolution: last key was C-x
        if app.chord == Some('x') {
            app.chord = None;
            // C-x C-s → save / copy focused buffer to clipboard
            if ctrl && matches!(key.code, KeyCode::Char('s')) {
                let text = match app.focus {
                    Focus::Json   => app.json.text(),
                    Focus::Expr   => app.expr_area.lines().join("\n"),
                    Focus::Result => app.result_text.clone(),
                };
                copy_to_clipboard(&text);
                continue;
            }
            if ctrl && matches!(key.code, KeyCode::Char('g')) { continue; }
            continue;
        }

        // C-c / C-x begin chord
        if ctrl && matches!(key.code, KeyCode::Char('c')) && !app.popup_open {
            app.chord = Some('c');
            continue;
        }
        if ctrl && matches!(key.code, KeyCode::Char('x')) && !app.popup_open {
            app.chord = Some('x');
            continue;
        }

        if matches!(key.code, KeyCode::Esc) && !app.popup_open {
            return Ok(());
        }

        // focus cycle: Json → Expr → Result → Json
        if ctrl && matches!(key.code, KeyCode::Char('o')) && !app.popup_open {
            app.focus = match app.focus {
                Focus::Json   => Focus::Expr,
                Focus::Expr   => Focus::Result,
                Focus::Result => Focus::Json,
            };
            continue;
        }

        match app.focus {
            Focus::Json   => handle_json(app, key)?,
            Focus::Expr   => handle_expr(app, key)?,
            Focus::Result => handle_result(app, key)?,
        }
    }
}

fn handle_json(app: &mut App, key: KeyEvent) -> Result<()> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt  = key.modifiers.contains(KeyModifiers::ALT);

    let folds = detect_folds(&app.json.lines);

    // emacs-style motion
    if ctrl {
        match key.code {
            KeyCode::Char('n') => { app.json.move_down(&folds); return Ok(()); }
            KeyCode::Char('p') => { app.json.move_up(&folds);   return Ok(()); }
            KeyCode::Char('b') => { app.json.move_left();       return Ok(()); }
            KeyCode::Char('f') => { app.json.move_right();      return Ok(()); }
            KeyCode::Char('a') => { app.json.home(); return Ok(()); }
            KeyCode::Char('e') => { app.json.end();  return Ok(()); }
            KeyCode::Char('k') => {
                app.json.kill_line();
                app.reparse_json();
                app.evaluate();
                return Ok(());
            }
            KeyCode::Char('v') => { app.json.page_down(&folds); return Ok(()); }
            _ => {}
        }
    }
    if alt {
        match key.code {
            KeyCode::Char('f') => { json_move_word(&mut app.json, true);  return Ok(()); }
            KeyCode::Char('b') => { json_move_word(&mut app.json, false); return Ok(()); }
            KeyCode::Char('v') | KeyCode::Char('c') => { app.json.page_up(&folds); return Ok(()); }
            _ => {}
        }
    }

    let mut dirty = true;
    match key.code {
        KeyCode::Left      => { app.json.move_left();  dirty = false; }
        KeyCode::Right     => { app.json.move_right(); dirty = false; }
        KeyCode::Up        => { app.json.move_up(&folds);   dirty = false; }
        KeyCode::Down      => { app.json.move_down(&folds); dirty = false; }
        KeyCode::Home      => { app.json.home(); dirty = false; }
        KeyCode::End       => { app.json.end();  dirty = false; }
        KeyCode::Backspace => app.json.backspace(),
        KeyCode::Delete    => app.json.delete(),
        KeyCode::Enter     => app.json.newline(),
        KeyCode::Tab       => { app.json.insert_char(' '); app.json.insert_char(' '); }
        KeyCode::Char(c)   => {
            if ctrl { return Ok(()); }
            app.json.insert_char(c);
        }
        _ => { dirty = false; }
    }
    if dirty {
        app.reparse_json();
        app.evaluate();
    }
    Ok(())
}

fn copy_to_clipboard(text: &str) {
    // OSC 52 escape: ESC ] 52 ; c ; <base64> BEL
    // Works in iTerm2, kitty, alacritty, WezTerm, tmux (with set-clipboard on).
    use std::io::Write;
    let b64 = base64_encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{}\x07", b64);
    let mut out = io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let b = &input[i..i + 3];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((n >>  6) & 0x3f) as usize] as char);
        out.push(TABLE[( n        & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let n = (input[i] as u32) << 16;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((input[i] as u32) << 16) | ((input[i + 1] as u32) << 8);
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((n >>  6) & 0x3f) as usize] as char);
        out.push('=');
    }
    out
}

fn handle_result(app: &mut App, key: KeyEvent) -> Result<()> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt  = key.modifiers.contains(KeyModifiers::ALT);

    let folds = detect_folds(&app.result.lines);

    if ctrl {
        match key.code {
            KeyCode::Char('n') => { app.result.move_down(&folds); return Ok(()); }
            KeyCode::Char('p') => { app.result.move_up(&folds);   return Ok(()); }
            KeyCode::Char('b') => { app.result.move_left();       return Ok(()); }
            KeyCode::Char('f') => { app.result.move_right();      return Ok(()); }
            KeyCode::Char('a') => { app.result.home(); return Ok(()); }
            KeyCode::Char('e') => { app.result.end();  return Ok(()); }
            KeyCode::Char('v') => { app.result.page_down(&folds); return Ok(()); }
            _ => {}
        }
    }
    if alt {
        match key.code {
            KeyCode::Char('f') => { json_move_word(&mut app.result, true);  return Ok(()); }
            KeyCode::Char('b') => { json_move_word(&mut app.result, false); return Ok(()); }
            KeyCode::Char('v') | KeyCode::Char('c') => { app.result.page_up(&folds); return Ok(()); }
            _ => {}
        }
    }
    match key.code {
        KeyCode::Left  => app.result.move_left(),
        KeyCode::Right => app.result.move_right(),
        KeyCode::Up    => app.result.move_up(&folds),
        KeyCode::Down  => app.result.move_down(&folds),
        KeyCode::Home  => app.result.home(),
        KeyCode::End   => app.result.end(),
        KeyCode::PageUp   => app.result.page_up(&folds),
        KeyCode::PageDown => app.result.page_down(&folds),
        _ => {}
    }
    Ok(())
}

fn json_move_word(j: &mut JsonEditor, forward: bool) {
    let line_chars: Vec<char> = j.lines[j.row].chars().collect();
    let len = line_chars.len();
    if forward {
        let mut c = j.col;
        while c < len && !line_chars[c].is_alphanumeric() { c += 1; }
        while c < len &&  line_chars[c].is_alphanumeric() { c += 1; }
        if c == j.col && j.row + 1 < j.lines.len() {
            j.row += 1; j.col = 0;
        } else {
            j.col = c;
        }
    } else {
        let mut c = j.col;
        while c > 0 && !line_chars[c - 1].is_alphanumeric() { c -= 1; }
        while c > 0 &&  line_chars[c - 1].is_alphanumeric() { c -= 1; }
        if c == j.col && j.row > 0 {
            j.row -= 1;
            j.col = j.lines[j.row].chars().count();
        } else {
            j.col = c;
        }
    }
}

fn reformat_json(app: &mut App) {
    let src = app.json.text();
    let Ok(v) = serde_json::from_str::<Value>(&src) else { return; };
    let Ok(pretty) = serde_json::to_string_pretty(&v) else { return; };
    app.json = JsonEditor::from_text(&pretty);
    app.reparse_json();
    app.evaluate();
}

fn handle_expr(app: &mut App, key: KeyEvent) -> Result<()> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // popup navigation (only while open)
    if app.popup_open {
        if ctrl && matches!(key.code, KeyCode::Char('n')) { popup_move(app, 1);  return Ok(()); }
        if ctrl && matches!(key.code, KeyCode::Char('p')) { popup_move(app, -1); return Ok(()); }
        if ctrl && matches!(key.code, KeyCode::Char('g')) { app.popup_open = false; return Ok(()); }
        match key.code {
            KeyCode::Up    => { popup_move(app, -1); return Ok(()); }
            KeyCode::Down  => { popup_move(app,  1); return Ok(()); }
            KeyCode::Esc   => { app.popup_open = false; return Ok(()); }
            KeyCode::Enter => { app.accept_completion(); return Ok(()); }
            KeyCode::Tab   => { app.accept_completion(); return Ok(()); }
            _ => {}
        }
    }

    // manual popup toggle
    if ctrl && matches!(key.code, KeyCode::Char(' ')) {
        app.refresh_completions();
        app.popup_open = !app.candidates.is_empty();
        return Ok(());
    }

    // evaluate: Alt-Enter or Shift-Enter (C-e is end-of-line)
    if matches!(key.code, KeyCode::Enter)
        && (key.modifiers.contains(KeyModifiers::SHIFT)
            || key.modifiers.contains(KeyModifiers::ALT))
    {
        app.evaluate();
        return Ok(());
    }

    // regular input — feed textarea (Enter inserts newline), refresh completions
    app.expr_area.input(key);
    app.refresh_completions();
    app.popup_open = !app.candidates.is_empty();
    app.evaluate();
    Ok(())
}

fn reformat_expr(app: &mut App) {
    let src = app.expr_area.lines().join("\n");
    let formatted = format_expr(&src);
    if formatted == src { return; }
    app.expr_area.select_all();
    app.expr_area.cut();
    let parts: Vec<&str> = formatted.split('\n').collect();
    for (i, line) in parts.iter().enumerate() {
        if i > 0 { app.expr_area.insert_newline(); }
        app.expr_area.insert_str(*line);
    }
}

/// Pretty-print jetro path expression. Splits top-level `.`-chains onto
/// indented lines; preserves strings and bracketed groups intact.
fn format_expr(s: &str) -> String {
    // collapse existing whitespace first
    let flat: String = {
        let mut out = String::with_capacity(s.len());
        let mut in_str = false;
        let mut esc = false;
        let mut prev_space = false;
        for c in s.chars() {
            if esc { out.push(c); esc = false; continue; }
            if in_str {
                if c == '\\' { esc = true; }
                else if c == '"' { in_str = false; }
                out.push(c);
                continue;
            }
            if c == '"' { in_str = true; out.push(c); prev_space = false; continue; }
            if c.is_whitespace() {
                if !prev_space { out.push(' '); prev_space = true; }
                continue;
            }
            prev_space = false;
            out.push(c);
        }
        out.trim().to_string()
    };

    let mut out = String::with_capacity(flat.len() + 16);
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut esc = false;
    let mut seen_nonspace_on_line = false;
    let indent = "  ";
    for c in flat.chars() {
        if esc { out.push(c); esc = false; continue; }
        if in_str {
            if c == '\\' { esc = true; }
            else if c == '"' { in_str = false; }
            out.push(c);
            continue;
        }
        match c {
            '"' => { in_str = true; out.push(c); seen_nonspace_on_line = true; }
            '(' | '[' | '{' => { depth += 1; out.push(c); seen_nonspace_on_line = true; }
            ')' | ']' | '}' => { depth -= 1; out.push(c); seen_nonspace_on_line = true; }
            '.' if depth == 0 && seen_nonspace_on_line => {
                out.push('\n');
                out.push_str(indent);
                out.push('.');
            }
            ' ' => {
                if seen_nonspace_on_line { out.push(' '); }
            }
            _ => { out.push(c); seen_nonspace_on_line = true; }
        }
    }
    out
}

fn popup_move(app: &mut App, delta: i32) {
    let n = app.candidates.len();
    if n == 0 { return; }
    let cur = app.popup_state.selected().unwrap_or(0) as i32;
    let next = ((cur + delta).rem_euclid(n as i32)) as usize;
    app.popup_state.select(Some(next));
}

// ── main ─────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    let json_seed = match cli.input {
        Some(p) => fs::read_to_string(&p)
            .map_err(|e| anyhow!("read {}: {}", p.display(), e))?,
        None    => r#"{"store":{"books":[{"title":"Dune","price":12.99},{"title":"Foundation","price":9.99}]}}"#.to_string(),
    };

    let expr_seed = cli.expr.unwrap_or_default();

    let mut app = App::new(json_seed, expr_seed);
    run(&mut app)?;
    Ok(())
}
