//! jetrocli — split-pane TUI for jetro.

mod completion;
mod editor;
mod eval;
mod shape;
mod theme;

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
use regex::Regex;
use serde_json::Value;
use std::{collections::HashMap, fs, io, path::PathBuf, time::Duration};
use tui_textarea::TextArea;

use completion::{Candidate, CandKind};
use editor::{detect_folds, JsonEditor};
use eval::{DocState, EvalState, EvalWorker};
use theme::{
    c_accent, c_accent2, c_body, c_bool, c_brace, c_cursor_line, c_err, c_hint, c_key, c_modeline_bg,
    c_muted, c_num, c_ok, c_pane_bg, c_punct, c_str, c_warn, init_palette, toggle_theme, ThemeArg,
};

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

    /// Color theme.
    #[arg(long, value_enum, default_value_t = ThemeArg::Dark)]
    theme: ThemeArg,
}

enum Focus { Json, Expr, Result }

struct App<'a> {
    json:        JsonEditor,
    expr_area:   TextArea<'a>,
    result:      JsonEditor,
    focus:       Focus,

    /// Parsed JSON; held on UI thread for completion shape inference.
    parsed_doc:  Option<Value>,

    /// Unified result/error state. Drives the result pane and status chips.
    eval_state:  EvalState,

    /// Background eval worker — owns the cached `Jetro` and `JetroEngine`.
    eval:        EvalWorker,

    popup_open:  bool,
    candidates:  Vec<Candidate>,
    popup_state: ListState,

    chord:       Option<char>,

    eval_count:  u64,

    search:      Option<SearchState>,
    palette:     Option<PaletteState>,
    help_open:   bool,
}

#[derive(Clone, Copy, PartialEq)]
enum SearchDir  { Forward, Backward }
#[derive(Clone, Copy, PartialEq)]
enum SearchMode { Word, Regex }

struct SearchState {
    query:        String,
    direction:    SearchDir,
    mode:         SearchMode,
    target:       SearchTarget,
    failed:       bool,
    invalid_re:   bool,
    origin_json:   (usize, usize, usize),
    origin_result: (usize, usize, usize),
    origin_expr:   (u16, u16),
}

#[derive(Clone, Copy, PartialEq)]
enum SearchTarget { Json, Result, Expr }

struct PaletteState {
    query:    String,
    selected: usize,
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
            eval_state: EvalState::Empty,
            eval: EvalWorker::spawn(),
            popup_open: false,
            candidates: vec![],
            popup_state: ListState::default(),
            chord: None,
            search: None,
            palette: None,
            help_open: false,
            eval_count: 0,
        };
        app.reparse_json();
        app.evaluate();
        app
    }

    fn reparse_json(&mut self) {
        let src = self.json.text();
        if src.trim().is_empty() {
            self.parsed_doc = None;
            self.eval.set_doc(DocState::None);
            self.eval_state = EvalState::Empty;
            self.sync_result_view();
            return;
        }
        match serde_json::from_str::<Value>(&src) {
            Ok(v) => {
                self.parsed_doc = Some(v.clone());
                self.eval.set_doc(DocState::Ok(v));
            }
            Err(e) => {
                self.parsed_doc = None;
                let msg = e.to_string();
                self.eval.set_doc(DocState::ParseErr(msg.clone()));
                self.eval_state = EvalState::ParseErr(format!("(JSON parse error)\n{}", msg));
                self.sync_result_view();
            }
        }
    }

    fn evaluate(&mut self) {
        let expr = self.expr_text();
        self.eval.submit_expr(expr);
    }

    /// Apply a result delivered by the eval worker.
    fn apply_eval_result(&mut self, r: eval::EvalResult) {
        if matches!(r.state, EvalState::Ok { .. }) {
            self.eval_count = self.eval_count.saturating_add(1);
        }
        self.eval_state = r.state;
        self.sync_result_view();
    }

    fn sync_result_view(&mut self) {
        let prev_folded = std::mem::take(&mut self.result.folded);
        let prev_scroll = self.result.scroll_row;
        let prev_row    = self.result.row;
        let prev_col    = self.result.col;
        self.result = JsonEditor::from_text(self.eval_state.display_text());
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

// ── UI ───────────────────────────────────────────────────────────────────────

fn draw(frame: &mut Frame, app: &mut App) {
    let size = frame.area();

    // canvas background (light theme uses cream; dark uses Reset = terminal default)
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(c_pane_bg())),
        size,
    );

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

    if app.search.is_some() {
        draw_search_prompt(frame, vertical[3], app);
    } else if app.palette.is_some() {
        draw_palette_prompt(frame, vertical[3], app);
    } else {
        draw_status(frame, vertical[3], app);
    }

    if app.palette.is_some() {
        draw_palette_list(frame, vertical[2], size, app);
    }
    if app.popup_open && !app.candidates.is_empty() {
        draw_popup(frame, vertical[2], size, app);
    }

    if app.help_open {
        draw_help_popup(frame, size);
    }
}

fn draw_help_popup(frame: &mut Frame, size: Rect) {
    let w = 78u16.min(size.width.saturating_sub(4));
    let h = 28u16.min(size.height.saturating_sub(2));
    let x = size.x + (size.width.saturating_sub(w)) / 2;
    let y = size.y + (size.height.saturating_sub(h)) / 2;
    let area = Rect { x, y, width: w, height: h };

    let key_st  = Style::default().bg(c_warn()).fg(Color::Black).bold();
    let lbl_st  = Style::default().fg(c_body());
    let hd_st   = Style::default().fg(c_accent2()).bold();
    let mt_st   = Style::default().fg(c_muted()).italic();

    let key = |k: &str| Span::styled(format!(" {} ", k), key_st);
    let lbl = |l: &str| Span::styled(format!("  {}", l), lbl_st);
    let hd  = |s: &str| Line::from(Span::styled(format!(" {} ", s), hd_st));
    let row = |k: Vec<Span<'static>>, l: &str| {
        let mut spans = vec![Span::raw("  ")];
        spans.extend(k);
        spans.push(lbl(l));
        Line::from(spans)
    };

    let lines: Vec<Line> = vec![
        Line::from(""),
        hd("Focus & quit"),
        row(vec![key("C-o")],            "cycle pane (JSON → expr → result)"),
        row(vec![key("C-c C-c")],        "quit"),
        row(vec![key("Esc")],            "quit (when no popup)"),
        Line::from(""),
        hd("Edit / motion (JSON & expr)"),
        row(vec![key("C-f"), key("C-b")],"char forward / back"),
        row(vec![key("M-f"), key("M-b")],"word forward / back"),
        row(vec![key("C-n"), key("C-p")],"line down / up"),
        row(vec![key("C-a"), key("C-e")],"line home / end"),
        row(vec![key("C-v"), key("M-v")],"page down / up (also M-c)"),
        row(vec![key("C-k")],            "kill to end of line"),
        row(vec![key("⏎")],              "newline (expr)"),
        row(vec![key("S-⏎"), key("M-⏎")],"evaluate expression"),
        Line::from(""),
        hd("Folding (JSON / result)"),
        row(vec![key("C-c f")],          "toggle fold at cursor"),
        row(vec![key("C-c a")],          "fold all"),
        row(vec![key("C-c u")],          "unfold all"),
        Line::from(""),
        hd("Completion popup (expr)"),
        row(vec![key("C-␣")],            "open / close popup"),
        row(vec![key("C-n"), key("C-p")],"navigate"),
        row(vec![key("⏎"), key("Tab")],  "accept"),
        row(vec![key("C-g"), key("Esc")],"close"),
        Line::from(""),
        hd("Search & commands"),
        row(vec![key("C-s"), key("C-r")],"isearch forward / backward (repeat to advance)"),
        row(vec![key("C-w")],            "isearch: word mode"),
        row(vec![key("M-r")],            "isearch: toggle regex"),
        row(vec![key("M-x")],            "command palette"),
        Line::from(""),
        hd("Buffer ops"),
        row(vec![key("C-c C-f")],        "format buffer (JSON / expr)"),
        row(vec![key("C-c C-k")],        "clear buffer"),
        row(vec![key("C-x C-s")],        "copy buffer to clipboard (OSC 52)"),
        row(vec![key("C-c h")],          "this help"),
        Line::from(""),
        Line::from(Span::styled("  press any key to close",
            mt_st)),
    ];

    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled("?", Style::default().fg(c_warn()).bold()),
        Span::raw(" "),
        Span::styled("help — keybindings", Style::default().fg(c_accent()).bold()),
        Span::raw(" "),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(c_accent2()))
        .title(title);

    let para = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    frame.render_widget(Clear, area);
    frame.render_widget(para, area);
}

fn draw_search_prompt(frame: &mut Frame, area: Rect, app: &App) {
    let s = app.search.as_ref().unwrap();
    let bg = Style::default().bg(c_modeline_bg());
    let label_color = if s.failed { c_err() } else { c_accent() };
    let mode_tag = match s.mode {
        SearchMode::Word  => " word ",
        SearchMode::Regex => " regex ",
    };
    let dir_label = match s.direction {
        SearchDir::Forward  => "I-search",
        SearchDir::Backward => "I-search-bwd",
    };
    let status = if s.invalid_re { " ✗ bad regex " }
        else if s.failed { " ∅ no match " }
        else { "" };

    let line = Line::from(vec![
        Span::styled(format!(" {dir_label} "),
            Style::default().bg(label_color).fg(Color::Black).bold()),
        Span::styled(mode_tag.to_string(),
            Style::default().bg(c_accent2()).fg(Color::Black).bold()),
        Span::raw(" "),
        Span::styled(s.query.clone(),
            Style::default().fg(c_body()).bold()),
        Span::styled("▏", Style::default().fg(c_accent())),
        Span::styled(status.to_string(), Style::default().fg(c_err()).italic()),
        Span::raw("   "),
        Span::styled("C-s/C-r next/prev  C-w word  M-r regex  ⏎ accept  C-g cancel",
            Style::default().fg(c_muted()).italic()),
    ]).style(bg);
    frame.render_widget(Paragraph::new("").style(bg), area);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_palette_prompt(frame: &mut Frame, area: Rect, app: &App) {
    let p = app.palette.as_ref().unwrap();
    let bg = Style::default().bg(c_modeline_bg());
    let line = Line::from(vec![
        Span::styled(" M-x ",
            Style::default().bg(c_warn()).fg(Color::Black).bold()),
        Span::raw(" "),
        Span::styled(p.query.clone(),
            Style::default().fg(c_body()).bold()),
        Span::styled("▏", Style::default().fg(c_accent())),
        Span::raw("   "),
        Span::styled("⏎ run  C-n/C-p nav  C-g cancel",
            Style::default().fg(c_muted()).italic()),
    ]).style(bg);
    frame.render_widget(Paragraph::new("").style(bg), area);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_palette_list(frame: &mut Frame, expr_area: Rect, size: Rect, app: &App) {
    let p = app.palette.as_ref().unwrap();
    let entries = palette_filtered(&p.query);
    if entries.is_empty() { return; }
    let h = (entries.len() as u16 + 2).min(12);
    let w = 70.min(size.width.saturating_sub(4));
    let x = expr_area.x + 2;
    let y = expr_area.y.saturating_sub(h);
    let area = Rect { x, y, width: w, height: h };

    let items: Vec<ListItem> = entries.iter().enumerate().map(|(i, (_, name, desc))| {
        let mark = if i == p.selected { "▶ " } else { "  " };
        ListItem::new(Line::from(vec![
            Span::styled(mark.to_string(), Style::default().fg(c_accent()).bold()),
            Span::styled(name.to_string(),  Style::default().fg(Color::Rgb(0xc0,0xca,0xf5)).bold()),
            Span::raw("  "),
            Span::styled(desc.to_string(), Style::default().fg(c_muted()).italic()),
        ]))
    }).collect();

    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled("⌘", Style::default().fg(c_warn())),
        Span::raw(" "),
        Span::styled("commands", Style::default().fg(c_accent()).bold()),
        Span::raw(" "),
        Span::styled(format!("({})", entries.len()), Style::default().fg(c_muted())),
        Span::raw(" "),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(c_warn()))
        .title(title);
    let list = List::new(items).block(block);
    frame.render_widget(Clear, area);
    frame.render_widget(list, area);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let mode = match app.focus {
        Focus::Json   => Span::styled(" JSON ",   Style::default().bg(c_accent()).fg(Color::Black).bold()),
        Focus::Expr   => Span::styled(" EXPR ",   Style::default().bg(c_accent2()).fg(Color::Black).bold()),
        Focus::Result => Span::styled(" RESULT ", Style::default().bg(c_ok()).fg(Color::Black).bold()),
    };
    let state = if app.eval_state.is_parse_err() {
        Span::styled(" ● parse error ", Style::default().fg(c_err()).bold())
    } else if app.eval_state.is_err() {
        Span::styled(" ● eval error ", Style::default().fg(c_warn()).bold())
    } else {
        Span::styled(" ● ready ", Style::default().fg(c_ok()).bold())
    };

    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled("✦", Style::default().fg(c_accent2())),
        Span::raw(" "),
        Span::styled("jetro", Style::default().fg(c_accent()).bold()),
        Span::styled("cli", Style::default().fg(c_accent2()).bold()),
        Span::raw("  "),
        Span::styled("interactive jetro REPL", Style::default().fg(c_muted()).italic()),
    ]);

    let right = Line::from(vec![state, Span::raw(" "), mode, Span::raw(" ")])
        .alignment(Alignment::Right);

    frame.render_widget(Paragraph::new(title), area);
    frame.render_widget(Paragraph::new(right), area);
}

fn draw_json_pane(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = matches!(app.focus, Focus::Json);
    let (title_icon_color, badge) = if app.eval_state.is_parse_err() {
        (c_err(), Span::styled(" ✗ parse ", Style::default().bg(c_err()).fg(Color::Black).bold()))
    } else {
        (c_ok(),  Span::styled(" ✓ valid ", Style::default().bg(c_ok()).fg(Color::Black).bold()))
    };

    let folds_count = detect_folds(&app.json.lines).len();
    let folded_count = app.json.folded.len();
    let fold_badge = if folds_count > 0 {
        Span::styled(
            format!(" ⋔ {}/{} ", folded_count, folds_count),
            Style::default().bg(c_accent2()).fg(Color::Black).bold(),
        )
    } else {
        Span::raw("")
    };

    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled("◆", Style::default().fg(title_icon_color).bold()),
        Span::raw(" "),
        Span::styled("JSON input", Style::default().fg(if focused { c_accent() } else { c_muted() }).bold()),
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
            Style::default().fg(if is_fold_header { c_warn() } else { c_muted() }),
        );

        let mut spans: Vec<Span<'static>> = vec![gutter_span];
        spans.extend(highlight_json_spans(&ed.lines[row]));

        if is_folded {
            if let Some(&e) = folds.get(&row) {
                let inner = e.saturating_sub(row).saturating_sub(1);
                spans.push(Span::styled(
                    format!("  ⋯ {} lines ", inner + 1),
                    Style::default().bg(c_muted()).fg(Color::Black).italic(),
                ));
                let close_trim = ed.lines[e].trim_start();
                spans.push(Span::styled(
                    format!(" {}", close_trim),
                    Style::default().fg(c_brace()).bold(),
                ));
            }
        }

        let mut line = Line::from(spans);
        if focused && row == ed.row {
            line = line.style(Style::default().bg(c_cursor_line()));
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
    let is_err = app.eval_state.is_err();
    let display = app.eval_state.display_text();
    let is_empty = display.is_empty();
    let badge = if is_err {
        Span::styled(" ! error ", Style::default().bg(c_err()).fg(Color::Black).bold())
    } else if is_empty {
        Span::styled(" ∅ empty ", Style::default().bg(c_muted()).fg(Color::Black).bold())
    } else {
        Span::styled(" » ok ", Style::default().bg(c_ok()).fg(Color::Black).bold())
    };

    let folds_count = if !is_err && !is_empty {
        detect_folds(&app.result.lines).len()
    } else { 0 };
    let folded_count = app.result.folded.len();
    let fold_badge = if folds_count > 0 {
        Span::styled(
            format!(" ⋔ {}/{} ", folded_count, folds_count),
            Style::default().bg(c_accent2()).fg(Color::Black).bold(),
        )
    } else { Span::raw("") };

    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled("◈", Style::default().fg(c_accent2()).bold()),
        Span::raw(" "),
        Span::styled("result", Style::default().fg(if focused { c_accent() } else { c_accent2() }).bold()),
        Span::raw("  "),
        badge,
        Span::raw(" "),
        fold_badge,
        Span::raw(" "),
    ]);
    let block = pane_block(title, focused);

    if is_err {
        let body: Vec<Line> = display.lines().map(|l| {
            Line::from(Span::styled(l.to_string(), Style::default().fg(c_err())))
        }).collect();
        let para = Paragraph::new(body).block(block).wrap(Wrap { trim: false });
        frame.render_widget(para, area);
    } else if is_empty {
        let body = vec![
            Line::from(""),
            Line::from(Span::styled("  (type an expression below to query the JSON)",
                Style::default().fg(c_muted()).italic())),
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
        Span::styled("❯", Style::default().fg(if focused { c_accent() } else { c_muted() }).bold()),
        Span::raw(" "),
        Span::styled("expr", Style::default().fg(if focused { c_accent() } else { c_muted() }).bold()),
        Span::raw(" "),
    ]);
    let block = pane_block(title, focused);
    app.expr_area.set_block(block);
    // cursor style
    let cursor_style = if focused {
        Style::default().bg(c_accent()).fg(Color::Black)
    } else {
        Style::default().bg(c_muted()).fg(Color::Black)
    };
    app.expr_area.set_cursor_style(cursor_style);
    frame.render_widget(&app.expr_area, area);
}

fn pane_block<'a>(title: Line<'a>, focused: bool) -> Block<'a> {
    let border_style = if focused {
        Style::default().fg(c_accent()).bold()
    } else {
        Style::default().fg(c_muted())
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(if focused { BorderType::Thick } else { BorderType::Rounded })
        .border_style(border_style)
        .title(title)
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let bg = Style::default().bg(c_modeline_bg());
    let txt = c_hint();
    frame.render_widget(Paragraph::new("").style(bg), area);

    // pane info
    let (pane_name, pane_color, line_ix, col_ix, total_lines, total_chars) = match app.focus {
        Focus::Json => {
            let total_chars: usize = app.json.lines.iter().map(|l| l.chars().count()).sum();
            ("JSON", c_accent(),
             app.json.row + 1, app.json.col + 1,
             app.json.lines.len(), total_chars)
        }
        Focus::Expr => {
            let lines = app.expr_area.lines();
            let (r, c) = app.expr_area.cursor();
            let total_chars: usize = lines.iter().map(|l| l.chars().count()).sum();
            ("EXPR", c_accent2(), r + 1, c + 1, lines.len(), total_chars)
        }
        Focus::Result => {
            let total_chars: usize = app.result.lines.iter().map(|l| l.chars().count()).sum();
            ("RESULT", c_ok(),
             app.result.row + 1, app.result.col + 1,
             app.result.lines.len(), total_chars)
        }
    };
    let percent = if total_lines == 0 { 0 } else {
        ((line_ix * 100) / total_lines).min(100)
    };
    let pos_label = if line_ix == 1 && total_lines <= 1 { "All".to_string() }
        else if line_ix == 1 { "Top".to_string() }
        else if line_ix >= total_lines { "Bot".to_string() }
        else { format!("{:>2}%", percent) };

    // status indicators
    let mut indicators = String::new();
    if app.eval_state.is_parse_err() { indicators.push('!'); }
    else if app.parsed_doc.is_some() { indicators.push('*'); }
    else { indicators.push('-'); }
    if app.popup_open { indicators.push('C'); }
    if !app.json.folded.is_empty() || !app.result.folded.is_empty() { indicators.push('F'); }
    if app.chord.is_some() { indicators.push('K'); }

    let eval_ns = app.eval_state.eval_ns();
    let eval_str = if eval_ns == 0 {
        "—".to_string()
    } else if eval_ns < 1_000 {
        format!("{}ns", eval_ns)
    } else if eval_ns < 1_000_000 {
        format!("{:.1}µs", eval_ns as f64 / 1_000.0)
    } else if eval_ns < 1_000_000_000 {
        format!("{:.2}ms", eval_ns as f64 / 1_000_000.0)
    } else {
        format!("{:.2}s", eval_ns as f64 / 1_000_000_000.0)
    };

    let bytes_str = format_bytes(app.eval_state.bytes());

    let sep = || Span::styled("  ─  ", Style::default().fg(c_muted()));
    let dim = |s: String| Span::styled(s, Style::default().fg(txt));
    let key = |s: String| Span::styled(s, Style::default().fg(c_warn()).bold());

    let line = Line::from(vec![
        // emacs-style left chrome: -U:**-
        Span::styled(format!("─{}─ ", indicators),
            Style::default().fg(c_muted())),
        Span::styled(" jetrocli ",
            Style::default().bg(c_accent2()).fg(Color::Black).bold()),
        sep(),
        Span::styled(format!(" {} ", pane_name),
            Style::default().bg(pane_color).fg(Color::Black).bold()),
        sep(),
        // position
        Span::styled("L", Style::default().fg(c_muted())),
        key(format!("{}", line_ix)),
        Span::styled(":", Style::default().fg(c_muted())),
        key(format!("{}", col_ix)),
        Span::raw(" "),
        Span::styled(format!("({}L {}c)", total_lines, total_chars),
            Style::default().fg(c_muted()).italic()),
        Span::raw(" "),
        Span::styled(pos_label, Style::default().fg(c_accent()).bold()),
        sep(),
        // evaluation time
        Span::styled("⏱ ", Style::default().fg(c_warn())),
        dim(format!("eval {}", eval_str)),
        Span::raw("  "),
        Span::styled("Σ ", Style::default().fg(c_warn())),
        dim(format!("{}", app.eval_count)),
        Span::raw("  "),
        Span::styled("◈ ", Style::default().fg(c_accent2())),
        dim(bytes_str),
        sep(),
        // hints
        Span::styled("C-c h", Style::default().fg(c_warn()).bold()),
        Span::styled(" help",  Style::default().fg(c_muted())),
    ]).style(bg);

    frame.render_widget(Paragraph::new(line), area);
}

fn format_bytes(n: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * 1024;
    if n < KB { format!("{}B", n) }
    else if n < MB { format!("{:.1}K", n as f64 / KB as f64) }
    else { format!("{:.2}M", n as f64 / MB as f64) }
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
            CandKind::Field   => ("fld", c_ok()),
            CandKind::Method  => ("fn ", c_accent()),
            CandKind::Keyword => ("kw ", c_accent2()),
            CandKind::Snippet => ("snp", c_warn()),
        };
        ListItem::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(format!(" {} ", tag),
                Style::default().bg(color).fg(Color::Black).bold()),
            Span::raw(" "),
            Span::styled(c.text.clone(), Style::default().fg(c_body())),
        ]))
    }).collect();

    let list_title = Line::from(vec![
        Span::raw(" "),
        Span::styled("✨", Style::default().fg(c_warn())),
        Span::raw(" "),
        Span::styled("completions", Style::default().fg(c_accent()).bold()),
        Span::raw(" "),
        Span::styled(format!("({})", app.candidates.len()), Style::default().fg(c_muted())),
        Span::raw(" "),
    ]);

    let list = List::new(items)
        .block(Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(c_accent2()))
            .title(list_title))
        .highlight_style(Style::default().bg(c_accent()).fg(Color::Black).bold())
        .highlight_symbol("▶ ");

    frame.render_widget(Clear, outer);
    frame.render_stateful_widget(list, list_rect, &mut app.popup_state);

    if let Some(doc_area) = doc_rect {
        let selected = app.popup_state.selected()
            .and_then(|i| app.candidates.get(i));
        let body: Vec<Line> = match selected {
            Some(c) => render_doc(c),
            None => vec![Line::from(Span::styled("no selection",
                Style::default().fg(c_muted()).italic()))],
        };
        let doc_title = Line::from(vec![
            Span::raw(" "),
            Span::styled("📖", Style::default().fg(c_warn())),
            Span::raw(" "),
            Span::styled("docs", Style::default().fg(c_accent2()).bold()),
            Span::raw(" "),
        ]);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(c_muted()))
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
            Style::default().fg(c_accent()).bold(),
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
                Style::default().fg(c_warn()).bold(),
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
                Style::default().fg(c_hint()),
            )));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no docs)".to_string(),
            Style::default().fg(c_muted()).italic(),
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
            spans.push(Span::styled(s, Style::default().fg(c_str())));
        } else if c == '$' || c == '@' {
            spans.push(Span::styled(c.to_string(), Style::default().fg(c_accent2()).bold()));
            i += 1;
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
            let s: String = chars[start..i].iter().collect();
            let is_method = i < chars.len() && chars[i] == '(';
            let kws = ["lambda","let","not","and","or","kind","when","for","in","if"];
            let style = if kws.contains(&s.as_str()) {
                Style::default().fg(c_bool()).bold()
            } else if is_method {
                Style::default().fg(c_accent()).bold()
            } else {
                Style::default().fg(c_key())
            };
            spans.push(Span::styled(s, style));
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') { i += 1; }
            let s: String = chars[start..i].iter().collect();
            spans.push(Span::styled(s, Style::default().fg(c_num())));
        } else if "()[]{}".contains(c) {
            spans.push(Span::styled(c.to_string(), Style::default().fg(c_brace()).bold()));
            i += 1;
        } else if c == '.' || c == ',' || c == ':' || c == ';' {
            spans.push(Span::styled(c.to_string(), Style::default().fg(c_punct())));
            i += 1;
        } else {
            spans.push(Span::raw(c.to_string()));
            i += 1;
        }
    }
    spans
}

// ── JSON syntax highlight ────────────────────────────────────────────────────

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
                Style::default().fg(c_key()).bold()
            } else {
                Style::default().fg(c_str())
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
                Style::default().fg(c_num())));
        } else if line[i..].starts_with("true") {
            spans.push(Span::styled("true".to_string(), Style::default().fg(c_bool()).bold()));
            i += 4;
        } else if line[i..].starts_with("false") {
            spans.push(Span::styled("false".to_string(), Style::default().fg(c_bool()).bold()));
            i += 5;
        } else if line[i..].starts_with("null") {
            spans.push(Span::styled("null".to_string(), Style::default().fg(c_muted()).italic()));
            i += 4;
        } else if "{}[]".contains(c) {
            spans.push(Span::styled(c.to_string(),
                Style::default().fg(c_brace()).bold()));
            i += 1;
        } else if c == ':' || c == ',' {
            spans.push(Span::styled(c.to_string(), Style::default().fg(c_punct())));
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
    let mut dirty = true;
    loop {
        // Drain any results delivered by the eval worker since last redraw.
        while let Some(r) = app.eval.poll_latest() {
            app.apply_eval_result(r);
            dirty = true;
        }

        if dirty {
            terminal.draw(|f| draw(f, app))?;
            dirty = false;
        }

        if !event::poll(Duration::from_millis(40))? {
            continue;
        }

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
                dirty = true;
                continue;
            }
            Event::Resize(_, _) => {
                dirty = true;
                continue;
            }
            _ => continue,
        };
        if key.kind != event::KeyEventKind::Press { continue; }
        dirty = true;

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt  = key.modifiers.contains(KeyModifiers::ALT);

        // help popup: any key closes
        if app.help_open {
            app.help_open = false;
            continue;
        }
        // search overlay consumes all keys
        if app.search.is_some() {
            handle_search_key(app, key);
            continue;
        }
        // command palette overlay
        if app.palette.is_some() {
            if handle_palette_key(app, key) { return Ok(()); }
            continue;
        }

        // C-s / C-r start isearch; M-x opens palette (only if popup not open)
        if !app.popup_open {
            if ctrl && matches!(key.code, KeyCode::Char('s')) {
                start_search(app, SearchDir::Forward);
                continue;
            }
            if ctrl && matches!(key.code, KeyCode::Char('r')) {
                start_search(app, SearchDir::Backward);
                continue;
            }
            if alt && matches!(key.code, KeyCode::Char('x')) {
                app.palette = Some(PaletteState { query: String::new(), selected: 0 });
                continue;
            }
        }

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
            // C-c h → show help popup
            if matches!(key.code, KeyCode::Char('h')) {
                app.help_open = true;
                continue;
            }
            // C-c l → toggle theme (light/dark)
            if matches!(key.code, KeyCode::Char('l')) {
                toggle_theme();
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
                        app.eval_state = EvalState::Empty;
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
                    Focus::Result => app.eval_state.display_text().to_string(),
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

// ── Incremental search ──────────────────────────────────────────────────────

fn start_search(app: &mut App, dir: SearchDir) {
    let target = match app.focus {
        Focus::Json   => SearchTarget::Json,
        Focus::Result => SearchTarget::Result,
        Focus::Expr   => SearchTarget::Expr,
    };
    let prev_mode = app.search.as_ref().map(|s| s.mode).unwrap_or(SearchMode::Word);
    let prev_query = app.search.as_ref().map(|s| s.query.clone()).unwrap_or_default();
    let (cr, cc) = app.expr_area.cursor();
    app.search = Some(SearchState {
        query:        prev_query,
        direction:    dir,
        mode:         prev_mode,
        target,
        failed:       false,
        invalid_re:   false,
        origin_json:   (app.json.row, app.json.col, app.json.scroll_row),
        origin_result: (app.result.row, app.result.col, app.result.scroll_row),
        origin_expr:   (cr as u16, cc as u16),
    });
    run_search(app, false);
}

fn build_pattern(query: &str, mode: SearchMode) -> Option<Regex> {
    if query.is_empty() { return None; }
    let pat = match mode {
        SearchMode::Regex => query.to_string(),
        SearchMode::Word  => regex::escape(query),
    };
    Regex::new(&pat).ok()
}

fn col_to_byte(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map(|(b,_)| b).unwrap_or(s.len())
}
fn byte_to_col(s: &str, byte: usize) -> usize {
    s[..byte.min(s.len())].chars().count()
}

fn search_lines(
    lines: &[String],
    pat: &Regex,
    dir: SearchDir,
    from_row: usize,
    from_col: usize,
    advance: bool,
) -> Option<(usize, usize)> {
    if lines.is_empty() { return None; }
    match dir {
        SearchDir::Forward => {
            for r in from_row..lines.len() {
                let line = &lines[r];
                let start_byte = if r == from_row {
                    let cb = col_to_byte(line, from_col);
                    if advance { (cb + 1).min(line.len()) } else { cb }
                } else { 0 };
                if start_byte > line.len() { continue; }
                if let Some(m) = pat.find_at(line, start_byte) {
                    return Some((r, byte_to_col(line, m.start())));
                }
            }
            // wrap
            for r in 0..from_row.min(lines.len()) {
                let line = &lines[r];
                if let Some(m) = pat.find(line) {
                    return Some((r, byte_to_col(line, m.start())));
                }
            }
        }
        SearchDir::Backward => {
            for r in (0..=from_row.min(lines.len().saturating_sub(1))).rev() {
                let line = &lines[r];
                let upper = if r == from_row {
                    let cb = col_to_byte(line, from_col);
                    if advance { cb } else { (cb + 1).min(line.len()) }
                } else { line.len() };
                let slice = &line[..upper.min(line.len())];
                if let Some(m) = pat.find_iter(slice).last() {
                    return Some((r, byte_to_col(line, m.start())));
                }
            }
            // wrap
            for r in (from_row + 1..lines.len()).rev() {
                let line = &lines[r];
                if let Some(m) = pat.find_iter(line).last() {
                    return Some((r, byte_to_col(line, m.start())));
                }
            }
        }
    }
    None
}

fn run_search(app: &mut App, advance: bool) {
    let Some(s) = app.search.as_mut() else { return; };
    if s.query.is_empty() {
        s.failed = false;
        s.invalid_re = false;
        return;
    }
    let pat = match build_pattern(&s.query, s.mode) {
        Some(p) => p,
        None => { s.invalid_re = true; s.failed = true; return; }
    };
    s.invalid_re = false;
    let dir = s.direction;
    match s.target {
        SearchTarget::Json => {
            let from = (app.json.row, app.json.col);
            if let Some((r, c)) = search_lines(&app.json.lines, &pat, dir, from.0, from.1, advance) {
                let folds = detect_folds(&app.json.lines);
                unfold_around(&mut app.json, r, &folds);
                app.json.row = r;
                app.json.col = c;
                app.json.clamp_all();
                s.failed = false;
            } else { s.failed = true; }
        }
        SearchTarget::Result => {
            let from = (app.result.row, app.result.col);
            if let Some((r, c)) = search_lines(&app.result.lines, &pat, dir, from.0, from.1, advance) {
                let folds = detect_folds(&app.result.lines);
                unfold_around(&mut app.result, r, &folds);
                app.result.row = r;
                app.result.col = c;
                app.result.clamp_all();
                s.failed = false;
            } else { s.failed = true; }
        }
        SearchTarget::Expr => {
            let lines: Vec<String> = app.expr_area.lines().iter().map(|l| l.to_string()).collect();
            let (from_row, from_col) = app.expr_area.cursor();
            if let Some((r, c)) = search_lines(&lines, &pat, dir, from_row, from_col, advance) {
                app.expr_area.move_cursor(tui_textarea::CursorMove::Jump(r as u16, c as u16));
                s.failed = false;
            } else { s.failed = true; }
        }
    }
}

fn unfold_around(ed: &mut JsonEditor, row: usize, folds: &HashMap<usize, usize>) {
    let folded: Vec<usize> = ed.folded.iter().copied().collect();
    for h in folded {
        if let Some(&e) = folds.get(&h) {
            if row > h && row <= e { ed.folded.remove(&h); }
        }
    }
}

fn cancel_search(app: &mut App) {
    let Some(s) = app.search.take() else { return; };
    // restore origin
    app.json.row = s.origin_json.0;
    app.json.col = s.origin_json.1;
    app.json.scroll_row = s.origin_json.2;
    app.json.clamp_all();
    app.result.row = s.origin_result.0;
    app.result.col = s.origin_result.1;
    app.result.scroll_row = s.origin_result.2;
    app.result.clamp_all();
    app.expr_area.move_cursor(tui_textarea::CursorMove::Jump(
        s.origin_expr.0,
        s.origin_expr.1,
    ));
}

fn handle_search_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt  = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Esc => { cancel_search(app); return; }
        KeyCode::Char('g') if ctrl => { cancel_search(app); return; }
        KeyCode::Enter => { app.search = None; return; }
        KeyCode::Char('s') if ctrl => {
            if let Some(s) = app.search.as_mut() { s.direction = SearchDir::Forward; }
            run_search(app, true);
            return;
        }
        KeyCode::Char('r') if ctrl => {
            if let Some(s) = app.search.as_mut() { s.direction = SearchDir::Backward; }
            run_search(app, true);
            return;
        }
        KeyCode::Char('w') if ctrl => {
            if let Some(s) = app.search.as_mut() { s.mode = SearchMode::Word; }
            run_search(app, false);
            return;
        }
        KeyCode::Char('r') if alt => {
            if let Some(s) = app.search.as_mut() {
                s.mode = if s.mode == SearchMode::Regex { SearchMode::Word } else { SearchMode::Regex };
            }
            run_search(app, false);
            return;
        }
        KeyCode::Backspace => {
            if let Some(s) = app.search.as_mut() { s.query.pop(); }
            run_search(app, false);
            return;
        }
        KeyCode::Char(c) if !ctrl && !alt => {
            if let Some(s) = app.search.as_mut() { s.query.push(c); }
            run_search(app, false);
            return;
        }
        _ => {}
    }
}

// ── Command palette (M-x) ───────────────────────────────────────────────────

const COMMANDS: &[(&str, &str)] = &[
    ("format-buffer",     "Pretty-format focused buffer (JSON or expr)"),
    ("fold-all",          "Collapse every block in current editor"),
    ("unfold-all",        "Expand every block in current editor"),
    ("clear-buffer",      "Clear focused buffer"),
    ("copy-buffer",       "Copy focused buffer to clipboard"),
    ("evaluate",          "Re-evaluate expression"),
    ("search-forward",    "Incremental search forward"),
    ("search-backward",   "Incremental search backward"),
    ("toggle-regex",      "Toggle regex mode in next search"),
    ("toggle-word",       "Toggle word (literal) mode in next search"),
    ("focus-next",        "Cycle pane focus"),
    ("quit",              "Exit jetrocli"),
];

fn palette_filtered(query: &str) -> Vec<(usize, &'static str, &'static str)> {
    let q = query.to_lowercase();
    COMMANDS.iter().enumerate()
        .filter(|(_, (n, _))| q.is_empty() || n.contains(&q))
        .map(|(i, (n, d))| (i, *n, *d))
        .collect()
}

/// Returns true if event loop should `return Ok(())` (quit).
fn handle_palette_key(app: &mut App, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => { app.palette = None; }
        KeyCode::Char('g') if ctrl => { app.palette = None; }
        KeyCode::Up   => { if let Some(p) = app.palette.as_mut() { p.selected = p.selected.saturating_sub(1); } }
        KeyCode::Down => {
            if let Some(p) = app.palette.as_mut() {
                let n = palette_filtered(&p.query).len();
                if n > 0 && p.selected + 1 < n { p.selected += 1; }
            }
        }
        KeyCode::Char('p') if ctrl => { if let Some(p) = app.palette.as_mut() { p.selected = p.selected.saturating_sub(1); } }
        KeyCode::Char('n') if ctrl => {
            if let Some(p) = app.palette.as_mut() {
                let n = palette_filtered(&p.query).len();
                if n > 0 && p.selected + 1 < n { p.selected += 1; }
            }
        }
        KeyCode::Backspace => { if let Some(p) = app.palette.as_mut() { p.query.pop(); p.selected = 0; } }
        KeyCode::Enter => {
            let cmd = app.palette.as_ref().and_then(|p| {
                palette_filtered(&p.query).get(p.selected).map(|(_, n, _)| *n)
            });
            app.palette = None;
            if let Some(name) = cmd { return run_command(app, name); }
        }
        KeyCode::Char(c) if !ctrl => {
            if let Some(p) = app.palette.as_mut() { p.query.push(c); p.selected = 0; }
        }
        _ => {}
    }
    false
}

fn run_command(app: &mut App, name: &str) -> bool {
    match name {
        "format-buffer" => match app.focus {
            Focus::Json   => reformat_json(app),
            Focus::Expr   => reformat_expr(app),
            Focus::Result => {}
        },
        "fold-all" => {
            let target: Option<&mut JsonEditor> = match app.focus {
                Focus::Json   => Some(&mut app.json),
                Focus::Result => Some(&mut app.result),
                Focus::Expr   => None,
            };
            if let Some(e) = target {
                let folds = detect_folds(&e.lines);
                e.fold_all(&folds);
            }
        }
        "unfold-all" => {
            let target: Option<&mut JsonEditor> = match app.focus {
                Focus::Json   => Some(&mut app.json),
                Focus::Result => Some(&mut app.result),
                Focus::Expr   => None,
            };
            if let Some(e) = target { e.unfold_all(); }
        }
        "clear-buffer" => match app.focus {
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
                app.eval_state = EvalState::Empty;
                app.result = JsonEditor::from_text("");
            }
        },
        "copy-buffer" => {
            let text = match app.focus {
                Focus::Json   => app.json.text(),
                Focus::Expr   => app.expr_area.lines().join("\n"),
                Focus::Result => app.eval_state.display_text().to_string(),
            };
            copy_to_clipboard(&text);
        }
        "evaluate"        => app.evaluate(),
        "search-forward"  => start_search(app, SearchDir::Forward),
        "search-backward" => start_search(app, SearchDir::Backward),
        "toggle-regex" => {
            // remember mode for next isearch
            let mode = match app.search.as_ref().map(|s| s.mode) {
                Some(SearchMode::Regex) => SearchMode::Word,
                _ => SearchMode::Regex,
            };
            app.search = Some(SearchState {
                query:      String::new(),
                direction:  SearchDir::Forward,
                mode,
                target:     SearchTarget::Json,
                failed:     false, invalid_re: false,
                origin_json:   (app.json.row, app.json.col, app.json.scroll_row),
                origin_result: (app.result.row, app.result.col, app.result.scroll_row),
                origin_expr:   (0, 0),
            });
            // start a session immediately so user sees mode applied
        }
        "toggle-word" => {
            app.search = Some(SearchState {
                query:      String::new(),
                direction:  SearchDir::Forward,
                mode:       SearchMode::Word,
                target:     SearchTarget::Json,
                failed:     false, invalid_re: false,
                origin_json:   (app.json.row, app.json.col, app.json.scroll_row),
                origin_result: (app.result.row, app.result.col, app.result.scroll_row),
                origin_expr:   (0, 0),
            });
        }
        "focus-next" => {
            app.focus = match app.focus {
                Focus::Json   => Focus::Expr,
                Focus::Expr   => Focus::Result,
                Focus::Result => Focus::Json,
            };
        }
        "quit" => return true,
        _ => {}
    }
    false
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

    // navigation keys: move cursor only, no recompile/reeval
    if is_nav_key(&key) {
        app.expr_area.input(key);
        return Ok(());
    }

    // regular input — feed textarea (Enter inserts newline), refresh completions
    app.expr_area.input(key);
    app.refresh_completions();
    app.popup_open = !app.candidates.is_empty();
    app.evaluate();
    Ok(())
}

fn is_nav_key(key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt  = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
        | KeyCode::Home | KeyCode::End
        | KeyCode::PageUp | KeyCode::PageDown => true,
        KeyCode::Char(c) if ctrl => matches!(c, 'a' | 'b' | 'e' | 'f' | 'n' | 'p' | 'v'),
        KeyCode::Char(c) if alt  => matches!(c, 'b' | 'f' | 'v'),
        _ => false,
    }
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
    init_palette(cli.theme);

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
