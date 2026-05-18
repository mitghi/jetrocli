//! NDJSON batch mode. File-only: forward or reverse iteration over a path,
//! optional early-exit on a row count. Each non-empty line is evaluated
//! independently through a shared `JetroEngine` so the plan cache amortizes
//! across rows.

use anyhow::{anyhow, Result};
use jetro_core::io::{NdjsonOptions, NdjsonRowFrame, NdjsonSource, NullPayload};
use jetro_core::JetroEngine;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use crate::Cli;
use crate::NullPayloadArg;

pub fn run(cli: &Cli) -> Result<i32> {
    let Some(path) = cli.input.as_ref() else {
        return Err(anyhow!("--ndjson requires -i <FILE>"));
    };
    if cli.reverse && !cli.ndjson {
        return Err(anyhow!("--reverse requires --ndjson"));
    }
    if matches!(cli.limit, Some(0)) {
        return Err(anyhow!("--limit must be >= 1"));
    }
    if cli.payload_after.is_none() && cli.null_payload != NullPayloadArg::Skip {
        return Err(anyhow!("--null-payload requires --payload-after"));
    }
    let expr = cli
        .expr_pos
        .clone()
        .or_else(|| cli.expr.clone())
        .unwrap_or_default();
    if expr.trim().is_empty() {
        return Err(anyhow!("--ndjson requires an expression"));
    }

    let opts = options(cli)?;

    let engine = JetroEngine::new();
    let stdout = io::stdout();
    let out = BufWriter::new(stdout.lock());

    let result = match (cli.reverse, cli.limit) {
        (false, None)    => run_forward(&engine, path, &expr, opts, out),
        (false, Some(n)) => run_forward_limited(&engine, path, &expr, opts, out, n),
        (true,  None)    => run_reverse(&engine, path, &expr, opts, out),
        (true,  Some(n)) => run_reverse_limited(&engine, path, &expr, opts, out, n),
    };

    match result {
        Ok(_)  => Ok(0),
        Err(e) => {
            eprintln!("jetrocli: {}: {}", path.display(), e);
            Ok(1)
        }
    }
}

fn parse_separator(separator: &str) -> Result<u8> {
    match separator {
        r"\t" => return Ok(b'\t'),
        r"\n" => return Ok(b'\n'),
        r"\r" => return Ok(b'\r'),
        _ => {}
    }
    if let Some(hex) = separator.strip_prefix(r"\x") {
        if hex.len() == 2 {
            return u8::from_str_radix(hex, 16)
                .map_err(|_| anyhow!("--payload-after expects one byte separator"));
        }
    }
    let bytes = separator.as_bytes();
    if bytes.len() == 1 {
        Ok(bytes[0])
    } else {
        Err(anyhow!("--payload-after expects one byte separator"))
    }
}

pub(crate) fn options(cli: &Cli) -> Result<NdjsonOptions> {
    let mut opts = NdjsonOptions::default();
    if let Some(n) = cli.max_line_bytes {
        opts = opts.with_max_line_len(n);
    }
    if let Some(n) = cli.reverse_chunk {
        opts = opts.with_reverse_chunk_size(n);
    }
    if let Some(separator) = cli.payload_after.as_deref() {
        opts = opts.with_row_frame(NdjsonRowFrame::DelimitedPayload {
            separator: parse_separator(separator)?,
            null_payload: match cli.null_payload {
                NullPayloadArg::Skip => NullPayload::Skip,
                NullPayloadArg::Keep => NullPayload::Keep,
                NullPayloadArg::Error => NullPayload::Error,
            },
        });
    }
    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::{is_rows_stream_expr, parse_separator};

    #[test]
    fn parses_payload_separator() {
        assert_eq!(parse_separator("|").unwrap(), b'|');
        assert_eq!(parse_separator(r"\t").unwrap(), b'\t');
        assert_eq!(parse_separator(r"\x1f").unwrap(), 0x1f);
        assert!(parse_separator("::").is_err());
    }

    #[test]
    fn detects_rows_stream_expression() {
        assert!(is_rows_stream_expr("$.rows().take(1)"));
        assert!(is_rows_stream_expr("  $.rows().reverse()"));
        assert!(!is_rows_stream_expr("$.items.rows().take(1)"));
        assert!(!is_rows_stream_expr("$.name"));
    }
}

fn run_forward<W: Write>(
    engine: &JetroEngine,
    path: &Path,
    expr: &str,
    opts: NdjsonOptions,
    out: W,
) -> Result<()> {
    engine
        .run_ndjson_source_with_options(NdjsonSource::file(path), expr, out, opts)
        .map(|_| ())
        .map_err(|e| anyhow!("{}", e))
}

fn run_forward_limited<W: Write>(
    engine: &JetroEngine,
    path: &Path,
    expr: &str,
    opts: NdjsonOptions,
    out: W,
    limit: usize,
) -> Result<()> {
    engine
        .run_ndjson_source_limit_with_options(NdjsonSource::file(path), expr, limit, out, opts)
        .map(|_| ())
        .map_err(|e| anyhow!("{}", e))
}

fn run_reverse<W: Write>(
    engine: &JetroEngine,
    path: &Path,
    expr: &str,
    opts: NdjsonOptions,
    out: W,
) -> Result<()> {
    if is_rows_stream_expr(expr) {
        return run_forward(engine, path, expr, opts, out);
    }
    engine
        .run_ndjson_rev_with_options(path, expr, out, opts)
        .map(|_| ())
        .map_err(|e| anyhow!("{}", e))
}

fn run_reverse_limited<W: Write>(
    engine: &JetroEngine,
    path: &Path,
    expr: &str,
    opts: NdjsonOptions,
    out: W,
    limit: usize,
) -> Result<()> {
    if is_rows_stream_expr(expr) {
        return run_forward_limited(engine, path, expr, opts, out, limit);
    }
    engine
        .run_ndjson_rev_limit_with_options(path, expr, limit, out, opts)
        .map(|_| ())
        .map_err(|e| anyhow!("{}", e))
}

fn is_rows_stream_expr(expr: &str) -> bool {
    expr.contains("$.rows(") || expr.contains("$.rows.")
}
