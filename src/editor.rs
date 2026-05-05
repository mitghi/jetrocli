//! Line-buffer editor with structural fold tracking. Used for both the
//! editable JSON pane and the read-only result pane.

use std::collections::{HashMap, HashSet};

pub struct JsonEditor {
    pub lines:      Vec<String>,
    pub row:        usize,
    pub col:        usize,
    pub scroll_row: usize,
    pub folded:     HashSet<usize>,
    pub view_h:     usize,
}

impl JsonEditor {
    pub fn from_text(s: &str) -> Self {
        let lines: Vec<String> = if s.is_empty() {
            vec![String::new()]
        } else {
            s.split('\n').map(|l| l.to_string()).collect()
        };
        Self { lines, row: 0, col: 0, scroll_row: 0, folded: HashSet::new(), view_h: 20 }
    }

    pub fn text(&self) -> String { self.lines.join("\n") }

    pub fn clamp_col(&mut self) {
        let max = self.lines[self.row].chars().count();
        if self.col > max { self.col = max; }
    }

    pub fn clamp_all(&mut self) {
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

    pub fn insert_char(&mut self, c: char) {
        self.folded.clear();
        let byte = self.col_byte();
        self.lines[self.row].insert(byte, c);
        self.col += 1;
        self.clamp_all();
    }

    pub fn insert_str(&mut self, s: &str) {
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

    pub fn newline(&mut self) { self.folded.clear(); self.newline_raw(); self.clamp_all(); }

    pub fn backspace(&mut self) {
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

    pub fn delete(&mut self) {
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

    pub fn home(&mut self) { self.col = 0; }
    pub fn end(&mut self)  { self.col = self.lines[self.row].chars().count(); }

    pub fn kill_line(&mut self) {
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

    pub fn move_left(&mut self) {
        if self.col > 0 { self.col -= 1; }
        else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].chars().count();
        }
    }

    pub fn move_right(&mut self) {
        let len = self.lines[self.row].chars().count();
        if self.col < len { self.col += 1; }
        else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    pub fn move_up(&mut self, folds: &HashMap<usize, usize>) {
        if self.row == 0 { self.col = 0; return; }
        self.row -= 1;
        for (&h, &e) in folds {
            if self.folded.contains(&h) && self.row > h && self.row <= e {
                self.row = h;
                break;
            }
        }
        self.clamp_col();
    }

    pub fn move_down(&mut self, folds: &HashMap<usize, usize>) {
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

    pub fn page_down(&mut self, folds: &HashMap<usize, usize>) {
        let n = self.view_h.saturating_sub(1).max(1);
        for _ in 0..n { self.move_down(folds); }
    }

    pub fn page_up(&mut self, folds: &HashMap<usize, usize>) {
        let n = self.view_h.saturating_sub(1).max(1);
        for _ in 0..n { self.move_up(folds); }
    }

    pub fn toggle_fold(&mut self, folds: &HashMap<usize, usize>) {
        if folds.contains_key(&self.row) {
            if self.folded.contains(&self.row) { self.folded.remove(&self.row); }
            else { self.folded.insert(self.row); }
            return;
        }
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

    pub fn fold_all(&mut self, folds: &HashMap<usize, usize>) {
        for &h in folds.keys() { self.folded.insert(h); }
        let cur = self.row;
        let mut best: Option<usize> = None;
        for (&h, &e) in folds {
            if cur >= h && cur <= e {
                if best.map_or(true, |bh| h > bh) { best = Some(h); }
            }
        }
        if let Some(h) = best { self.row = h; self.clamp_col(); }
    }

    pub fn unfold_all(&mut self) { self.folded.clear(); }
}

pub fn detect_folds(lines: &[String]) -> HashMap<usize, usize> {
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
