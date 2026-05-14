//! NDJSON batch mode. File-only: forward or reverse iteration over a path,
//! optional early-exit on a row count. Each non-empty line is evaluated
//! independently through a shared `JetroEngine` so the plan cache amortizes
//! across rows.

use anyhow::{anyhow, Result};
use jetro_core::io::{NdjsonOptions, NdjsonSource};
use jetro_core::JetroEngine;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use crate::Cli;

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
    if cli.distinct_by.is_some() && (!cli.reverse || cli.limit.is_none()) {
        return Err(anyhow!(
            "--distinct-by requires --ndjson --reverse --limit <N>"
        ));
    }

    let expr = cli
        .expr_pos
        .clone()
        .or_else(|| cli.expr.clone())
        .unwrap_or_default();
    if expr.trim().is_empty() {
        return Err(anyhow!("--ndjson requires an expression"));
    }

    let mut opts = NdjsonOptions::default();
    if let Some(n) = cli.max_line_bytes {
        opts = opts.with_max_line_len(n);
    }
    if let Some(n) = cli.reverse_chunk {
        opts = opts.with_reverse_chunk_size(n);
    }

    let engine = JetroEngine::new();
    let stdout = io::stdout();
    let out = BufWriter::new(stdout.lock());

    let result = match (cli.reverse, cli.limit, cli.distinct_by.as_deref()) {
        (false, None, None) => run_forward(&engine, path, &expr, opts, out),
        (false, Some(n), None) => run_forward_limited(&engine, path, &expr, opts, out, n),
        (true, None, None) => run_reverse(&engine, path, &expr, opts, out),
        (true, Some(n), None) => run_reverse_limited(&engine, path, &expr, opts, out, n),
        (true, Some(n), Some(key)) => run_reverse_distinct(&engine, path, key, &expr, opts, out, n),
        _ => unreachable!("validated NDJSON distinct_by option combination"),
    };

    match result {
        Ok(_)  => Ok(0),
        Err(e) => {
            eprintln!("jetrocli: {}: {}", path.display(), e);
            Ok(1)
        }
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
    engine
        .run_ndjson_rev_limit_with_options(path, expr, limit, out, opts)
        .map(|_| ())
        .map_err(|e| anyhow!("{}", e))
}

fn run_reverse_distinct<W: Write>(
    engine: &JetroEngine,
    path: &Path,
    key_expr: &str,
    expr: &str,
    opts: NdjsonOptions,
    out: W,
    limit: usize,
) -> Result<()> {
    engine
        .run_ndjson_rev_distinct_by_with_options(path, key_expr, expr, limit, out, opts)
        .map(|_| ())
        .map_err(|e| anyhow!("{}", e))
}
