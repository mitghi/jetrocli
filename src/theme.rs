//! Theme palette + color accessors. Process-global state via OnceLock.

use ratatui::style::Color;
use std::sync::{Mutex, OnceLock};

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
pub enum ThemeArg { Dark, Light }

#[allow(dead_code)]
pub struct Palette {
    pub theme:        ThemeArg,
    pub accent:       Color,
    pub accent2:      Color,
    pub ok:           Color,
    pub err:          Color,
    pub warn:         Color,
    pub muted:        Color,
    pub str_:         Color,
    pub key:          Color,
    pub num:          Color,
    pub bool_:        Color,
    pub brace:        Color,
    pub punct:        Color,
    pub body:         Color,
    pub hint:         Color,
    pub cursor_line:  Color,
    pub modeline_bg:  Color,
    pub on_chip:      Color,
    pub pane_bg:      Color,
}

static PALETTE: OnceLock<Mutex<Palette>> = OnceLock::new();

fn pal_color<F: Fn(&Palette) -> Color>(f: F) -> Color {
    let g = PALETTE.get().expect("palette uninitialized").lock().unwrap();
    f(&*g)
}

pub fn current_theme() -> ThemeArg {
    PALETTE.get().expect("palette uninitialized").lock().unwrap().theme
}

pub fn set_palette(theme: ThemeArg) {
    let p = build_palette(theme);
    if let Some(slot) = PALETTE.get() {
        *slot.lock().unwrap() = p;
    } else {
        let _ = PALETTE.set(Mutex::new(p));
    }
}

pub fn toggle_theme() {
    let new = match current_theme() {
        ThemeArg::Dark => ThemeArg::Light,
        ThemeArg::Light => ThemeArg::Dark,
    };
    set_palette(new);
}

pub fn init_palette(theme: ThemeArg) { set_palette(theme); }

fn build_palette(theme: ThemeArg) -> Palette {
    match theme {
        ThemeArg::Dark => Palette {
            theme,
            accent:      Color::Rgb(0x7a, 0xa2, 0xf7),
            accent2:     Color::Rgb(0xbb, 0x9a, 0xf7),
            ok:          Color::Rgb(0x9e, 0xce, 0x6a),
            err:         Color::Rgb(0xf7, 0x76, 0x8e),
            warn:        Color::Rgb(0xe0, 0xaf, 0x68),
            muted:       Color::Rgb(0x56, 0x5f, 0x89),
            str_:        Color::Rgb(0x9e, 0xce, 0x6a),
            key:         Color::Rgb(0x7d, 0xcf, 0xff),
            num:         Color::Rgb(0xff, 0x9e, 0x64),
            bool_:       Color::Rgb(0xbb, 0x9a, 0xf7),
            brace:       Color::Rgb(0xc0, 0xca, 0xf5),
            punct:       Color::Rgb(0x56, 0x5f, 0x89),
            body:        Color::Rgb(0xc0, 0xca, 0xf5),
            hint:        Color::Rgb(0xa9, 0xb1, 0xd6),
            cursor_line: Color::Rgb(0x29, 0x2e, 0x42),
            modeline_bg: Color::Rgb(0x1a, 0x1b, 0x26),
            on_chip:     Color::Black,
            pane_bg:     Color::Reset,
        },
        ThemeArg::Light => Palette {
            theme,
            accent:      Color::Rgb(0x1e, 0x66, 0xf5),
            accent2:     Color::Rgb(0xfa, 0xa3, 0x4c),
            ok:          Color::Rgb(0x40, 0xa0, 0x2b),
            err:         Color::Rgb(0xd2, 0x0f, 0x39),
            warn:        Color::Rgb(0xdf, 0x8e, 0x1d),
            muted:       Color::Rgb(0x9c, 0x90, 0x7a),
            str_:        Color::Rgb(0x40, 0xa0, 0x2b),
            key:         Color::Rgb(0x04, 0xa5, 0xe5),
            num:         Color::Rgb(0xfe, 0x64, 0x0b),
            bool_:       Color::Rgb(0xfa, 0xa3, 0x4c),
            brace:       Color::Rgb(0x4c, 0x4a, 0x40),
            punct:       Color::Rgb(0x9c, 0x90, 0x7a),
            body:        Color::Rgb(0x4c, 0x4a, 0x40),
            hint:        Color::Rgb(0x6c, 0x66, 0x55),
            cursor_line: Color::Rgb(0xf0, 0xe6, 0xc8),
            modeline_bg: Color::Rgb(0xf2, 0xe7, 0xc8),
            on_chip:     Color::Rgb(0xfb, 0xf3, 0xde),
            pane_bg:     Color::Rgb(0xfb, 0xf3, 0xde),
        },
    }
}

pub fn c_accent()      -> Color { pal_color(|p| p.accent) }
pub fn c_accent2()     -> Color { pal_color(|p| p.accent2) }
#[allow(dead_code)]
pub fn c_ok()          -> Color { pal_color(|p| p.ok) }
pub fn c_err()         -> Color { pal_color(|p| p.err) }
pub fn c_warn()        -> Color { pal_color(|p| p.warn) }
pub fn c_muted()       -> Color { pal_color(|p| p.muted) }
pub fn c_str()         -> Color { pal_color(|p| p.str_) }
pub fn c_key()         -> Color { pal_color(|p| p.key) }
pub fn c_num()         -> Color { pal_color(|p| p.num) }
pub fn c_bool()        -> Color { pal_color(|p| p.bool_) }
pub fn c_brace()       -> Color { pal_color(|p| p.brace) }
pub fn c_punct()       -> Color { pal_color(|p| p.punct) }
pub fn c_body()        -> Color { pal_color(|p| p.body) }
pub fn c_hint()        -> Color { pal_color(|p| p.hint) }
pub fn c_cursor_line() -> Color { pal_color(|p| p.cursor_line) }
pub fn c_modeline_bg() -> Color { pal_color(|p| p.modeline_bg) }
pub fn c_pane_bg()     -> Color { pal_color(|p| p.pane_bg) }
#[allow(dead_code)]
pub fn c_on_chip()     -> Color { pal_color(|p| p.on_chip) }
