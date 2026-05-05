//! Background eval worker for jetro expressions.
//!
//! Owns a cached `Jetro` document rebuilt only when the JSON changes. Receives
//! doc updates and expression submissions on a channel; debounces under typing
//! load by collecting messages for a short window before evaluating, then
//! evaluates only the latest expression. Results carry a generation counter so
//! the UI thread can discard stale ones. The worker thread reuses jetro's
//! thread-local `VM` (compile + path caches accumulate across queries).

use serde_json::Value;
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(20);

/// Unified result/error state replacing scattered (parsed_doc, parse_err,
/// result_text, last_eval_ns, last_result_bytes) fields.
#[derive(Clone, Debug)]
pub enum EvalState {
    /// No expression yet, or no valid document to evaluate against.
    Empty,
    /// JSON document failed to parse; evaluation cannot proceed.
    ParseErr(String),
    /// Document parsed but expression evaluation failed.
    EvalErr(String),
    /// Successful evaluation with pretty-printed output and timing.
    Ok { pretty: String, eval_ns: u128 },
}

impl EvalState {
    pub fn display_text(&self) -> &str {
        match self {
            Self::Empty            => "",
            Self::ParseErr(s)      => s,
            Self::EvalErr(s)       => s,
            Self::Ok { pretty, .. } => pretty,
        }
    }

    pub fn is_err(&self) -> bool {
        matches!(self, Self::ParseErr(_) | Self::EvalErr(_))
    }

    pub fn is_parse_err(&self) -> bool {
        matches!(self, Self::ParseErr(_))
    }

    pub fn eval_ns(&self) -> u128 {
        if let Self::Ok { eval_ns, .. } = self { *eval_ns } else { 0 }
    }

    pub fn bytes(&self) -> usize {
        match self {
            Self::Ok { pretty, .. } => pretty.len(),
            Self::EvalErr(s) | Self::ParseErr(s) => s.len(),
            _ => 0,
        }
    }
}

/// Doc state set by the UI: parsed Value, parse error, or no document.
#[derive(Clone, Debug)]
pub enum DocState {
    None,
    ParseErr(String),
    Ok(Value),
}

enum Msg {
    SetDoc(DocState),
    Eval { expr: String, gen: u64 },
    Stop,
}

pub struct EvalResult {
    pub gen: u64,
    pub state: EvalState,
}

pub struct EvalWorker {
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<EvalResult>,
    join: Option<thread::JoinHandle<()>>,
    next_gen: u64,
    latest_seen_gen: u64,
}

impl EvalWorker {
    pub fn spawn() -> Self {
        let (tx_msg, rx_msg) = mpsc::channel::<Msg>();
        let (tx_res, rx_res) = mpsc::channel::<EvalResult>();
        let join = thread::spawn(move || run_worker(rx_msg, tx_res));
        Self {
            tx: tx_msg,
            rx: rx_res,
            join: Some(join),
            next_gen: 0,
            latest_seen_gen: 0,
        }
    }

    pub fn set_doc(&self, doc: DocState) {
        let _ = self.tx.send(Msg::SetDoc(doc));
    }

    pub fn submit_expr(&mut self, expr: String) -> u64 {
        self.next_gen += 1;
        let gen = self.next_gen;
        let _ = self.tx.send(Msg::Eval { expr, gen });
        gen
    }

    /// Drain pending results, returning the most recent one (highest gen) if any.
    pub fn poll_latest(&mut self) -> Option<EvalResult> {
        let mut latest: Option<EvalResult> = None;
        loop {
            match self.rx.try_recv() {
                Ok(r) => {
                    if r.gen >= self.latest_seen_gen {
                        self.latest_seen_gen = r.gen;
                        latest = Some(r);
                    }
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        latest
    }
}

impl Drop for EvalWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(j) = self.join.take() { let _ = j.join(); }
    }
}

fn run_worker(rx: mpsc::Receiver<Msg>, tx: mpsc::Sender<EvalResult>) {
    let mut doc_state: DocState = DocState::None;
    let mut doc: Option<jetro_core::Jetro> = None;
    let mut last_expr: Option<String> = None;
    let mut last_gen: u64 = 0;
    // Pending eval requested for next batch.
    let mut pending: Option<(String, u64)> = None;
    // Set when doc changed and we need to re-run last expr after debounce.
    let mut doc_dirty = false;

    loop {
        let first = match rx.recv() {
            Ok(m) => m,
            Err(_) => return,
        };
        let mut buf = vec![first];
        let deadline = Instant::now() + DEBOUNCE;
        loop {
            let now = Instant::now();
            if now >= deadline { break; }
            match rx.recv_timeout(deadline - now) {
                Ok(m) => buf.push(m),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }

        for m in buf.drain(..) {
            match m {
                Msg::SetDoc(s) => {
                    doc_state = s;
                    doc = match &doc_state {
                        DocState::Ok(v)   => Some(jetro_core::Jetro::from(v.clone())),
                        _                 => None,
                    };
                    doc_dirty = true;
                }
                Msg::Eval { expr, gen } => {
                    last_expr = Some(expr.clone());
                    last_gen = gen;
                    if pending.as_ref().map_or(true, |(_, g)| gen >= *g) {
                        pending = Some((expr, gen));
                    }
                }
                Msg::Stop => return,
            }
        }

        // Determine what to evaluate. New eval submissions take precedence;
        // otherwise re-run the last expression against the new document.
        let to_eval: Option<(String, u64)> = pending.take().or_else(|| {
            if doc_dirty {
                last_expr.clone().map(|e| (e, last_gen))
            } else {
                None
            }
        });
        doc_dirty = false;

        if let Some((expr, gen)) = to_eval {
            let state = compute(&doc_state, doc.as_ref(), &expr);
            let _ = tx.send(EvalResult { gen, state });
        }
    }
}

fn compute(
    doc_state: &DocState,
    doc: Option<&jetro_core::Jetro>,
    expr: &str,
) -> EvalState {
    if expr.trim().is_empty() {
        return match doc_state {
            DocState::ParseErr(e) => EvalState::ParseErr(format!("(JSON parse error)\n{}", e)),
            _ => EvalState::Empty,
        };
    }
    match doc_state {
        DocState::ParseErr(e) => return EvalState::ParseErr(format!("(JSON parse error)\n{}", e)),
        DocState::None        => return EvalState::Empty,
        DocState::Ok(_)       => {}
    }
    let Some(d) = doc else { return EvalState::Empty; };
    let t0 = Instant::now();
    match d.collect(expr) {
        Ok(v) => {
            let pretty = serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string());
            EvalState::Ok { pretty, eval_ns: t0.elapsed().as_nanos() }
        }
        Err(e) => EvalState::EvalErr(format!("error: {}", e)),
    }
}
