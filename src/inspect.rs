//! Opt-in query inspection mode.

use anyhow::{anyhow, Result};
use jetro_core::introspect::{InspectContext, InspectLevel, InspectOptions};
use jetro_core::io::NdjsonSourceMode;
use jetro_core::JetroEngine;
use std::io::{self, IsTerminal, Write};

use crate::{ndjson, Cli, InspectLevelArg};

pub fn run(cli: &Cli, expr: &str) -> Result<i32> {
    if expr.trim().is_empty() {
        return Err(anyhow!("--inspect requires an expression"));
    }

    let engine = JetroEngine::new();
    let level = inspect_level(cli.inspect_level);
    let report = if cli.ndjson {
        if cli.input.is_none() {
            return Err(anyhow!("--inspect --ndjson requires -i <FILE>"));
        }
        engine.inspect_ndjson_query_with_options(
            expr,
            NdjsonSourceMode::File,
            ndjson::options(cli)?,
            level,
        )
    } else {
        engine.inspect_query(
            expr,
            InspectOptions {
                level,
                context: inspect_context(cli),
            },
        )
    };

    match report {
        Ok(report) => {
            let stdout = io::stdout();
            let mut out = stdout.lock();
            out.write_all(report.format_tree().as_bytes())?;
            out.write_all(b"\n")?;
            Ok(0)
        }
        Err(e) => {
            eprintln!("error: {e}");
            Ok(1)
        }
    }
}

fn inspect_level(level: InspectLevelArg) -> InspectLevel {
    match level {
        InspectLevelArg::Summary => InspectLevel::Summary,
        InspectLevelArg::Plan => InspectLevel::Plan,
        InspectLevelArg::Detailed => InspectLevel::Detailed,
    }
}

fn inspect_context(cli: &Cli) -> InspectContext {
    if cli.input.is_some() || !io::stdin().is_terminal() {
        InspectContext::Bytes
    } else {
        InspectContext::Value
    }
}
