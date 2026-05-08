//! Non-interactive batch mode: evaluate a Jetro expression against stdin
//! and print the result. Used when stdin is piped/redirected, so no TUI.
//!
//! Fast path: when stdin is backed by a regular file (e.g. `jetrocli EXPR
//! < big.json`) we mmap it; otherwise drain the pipe into a `Vec`. The
//! parsed document goes through `Jetro::from_bytes` so feature-gated
//! lazy/SIMD parsing in jetro-core can kick in.
//!
//! Output is colorized like `jq` when stdout is a TTY (and `NO_COLOR` is
//! unset). Piping into another program drops the ANSI escapes.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;

pub fn run(expr: &str) -> Result<i32> {
    let bytes = read_stdin_bytes()?;
    if bytes.is_empty() {
        return Err(anyhow!("empty input on stdin"));
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let color = use_color();

    // No expression → pretty-print stdin as JSON.
    if expr.trim().is_empty() {
        let val: Value = serde_json::from_slice(&bytes).context("parse JSON from stdin")?;
        write_value(&mut out, &val, color)?;
        out.write_all(b"\n")?;
        return Ok(0);
    }

    let doc = jetro_core::Jetro::from_bytes(bytes).context("parse JSON from stdin")?;

    match doc.collect(expr) {
        Ok(v) => {
            let val = serde_json::to_value(&v).unwrap_or(Value::Null);
            write_value(&mut out, &val, color)?;
            out.write_all(b"\n")?;
            Ok(0)
        }
        Err(e) => {
            eprintln!("error: {e}");
            Ok(1)
        }
    }
}

fn write_value<W: Write>(w: &mut W, v: &Value, color: bool) -> io::Result<()> {
    if color {
        write_colored(w, v, 0)
    } else {
        let s = serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string());
        w.write_all(s.as_bytes())
    }
}

fn read_stdin_bytes() -> Result<Vec<u8>> {
    let stdin = io::stdin();
    let fd = stdin.as_raw_fd();

    // mmap when fd 0 is a regular file (`< file.json`). For real pipes /
    // FIFOs this fails fast and we fall through to the streaming reader.
    if let Ok(mmap) = unsafe { memmap2::Mmap::map(fd) } {
        if !mmap.is_empty() {
            #[cfg(unix)]
            let _ = mmap.advise(memmap2::Advice::Sequential);
            return Ok(mmap.to_vec());
        }
    }

    let mut buf = Vec::with_capacity(64 * 1024);
    stdin.lock().read_to_end(&mut buf).context("read stdin")?;
    Ok(buf)
}

fn use_color() -> bool {
    if matches!(std::env::var("JETROCLI_COLOR").ok().as_deref(), Some("never") | Some("0")) {
        return false;
    }
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    true
}

// jq default palette: null;false;true;number;string;array;object;objectkey
const C_NULL:   &str = "\x1b[1;30m";
const C_FALSE:  &str = "\x1b[0;39m";
const C_TRUE:   &str = "\x1b[0;39m";
const C_NUM:    &str = "\x1b[0;39m";
const C_STR:    &str = "\x1b[0;32m";
const C_PUNCT:  &str = "\x1b[1;39m";
const C_KEY:    &str = "\x1b[34;1m";
const C_RESET:  &str = "\x1b[0m";
const INDENT:   &str = "  ";

fn write_indent<W: Write>(w: &mut W, depth: usize) -> io::Result<()> {
    for _ in 0..depth {
        w.write_all(INDENT.as_bytes())?;
    }
    Ok(())
}

fn write_colored<W: Write>(w: &mut W, v: &Value, depth: usize) -> io::Result<()> {
    match v {
        Value::Null => write!(w, "{}null{}", C_NULL, C_RESET),
        Value::Bool(true)  => write!(w, "{}true{}", C_TRUE, C_RESET),
        Value::Bool(false) => write!(w, "{}false{}", C_FALSE, C_RESET),
        Value::Number(n)   => write!(w, "{}{}{}", C_NUM, n, C_RESET),
        Value::String(s) => {
            let esc = serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
            write!(w, "{}{}{}", C_STR, esc, C_RESET)
        }
        Value::Array(arr) => {
            if arr.is_empty() {
                return write!(w, "{}[]{}", C_PUNCT, C_RESET);
            }
            write!(w, "{}[{}\n", C_PUNCT, C_RESET)?;
            for (i, item) in arr.iter().enumerate() {
                write_indent(w, depth + 1)?;
                write_colored(w, item, depth + 1)?;
                if i + 1 < arr.len() {
                    write!(w, "{},{}", C_PUNCT, C_RESET)?;
                }
                w.write_all(b"\n")?;
            }
            write_indent(w, depth)?;
            write!(w, "{}]{}", C_PUNCT, C_RESET)
        }
        Value::Object(map) => {
            if map.is_empty() {
                return write!(w, "{}{{}}{}", C_PUNCT, C_RESET);
            }
            write!(w, "{}{{{}\n", C_PUNCT, C_RESET)?;
            let len = map.len();
            for (i, (k, val)) in map.iter().enumerate() {
                write_indent(w, depth + 1)?;
                let key_esc = serde_json::to_string(k).unwrap_or_else(|_| "\"\"".into());
                write!(w, "{}{}{}{}: {}", C_KEY, key_esc, C_RESET, C_PUNCT, C_RESET)?;
                write_colored(w, val, depth + 1)?;
                if i + 1 < len {
                    write!(w, "{},{}", C_PUNCT, C_RESET)?;
                }
                w.write_all(b"\n")?;
            }
            write_indent(w, depth)?;
            write!(w, "{}}}{}", C_PUNCT, C_RESET)
        }
    }
}
