use serde::Serialize;
use std::io::{self, Write};

/// Write a single NDJSON line to stdout and flush.
pub fn emit<T: Serialize>(value: &T) {
    let mut out = io::stdout().lock();
    let _ = serde_json::to_writer(&mut out, value);
    let _ = writeln!(out);
    let _ = out.flush();
}

// ── Find events ──────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct FindResult {
    pub event: &'static str,
    pub path: String,
    pub size: u64,
    pub modified: u64,
}

#[derive(Serialize)]
pub struct FindSummary {
    pub event: &'static str,
    pub checked: u64,
    pub found: u64,
    pub elapsed_ms: u64,
    pub errors: bool,
}

// ── Hash events ──────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct HashComplete {
    pub event: &'static str,
    pub hashed: u64,
    pub snapshot: String,
    pub elapsed_ms: u64,
    pub bytes_hashed: u64,
}

// ── Diff events ──────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct DiffChanged {
    pub event: &'static str,
    pub kind: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_hash: Option<String>,
}

#[derive(Serialize)]
pub struct DiffSummary {
    pub event: &'static str,
    pub modified: u64,
    pub created: u64,
    pub deleted: u64,
    pub checked: u64,
    pub rehashed: u64,
    pub elapsed_ms: u64,
}

// ── Watch events ─────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct WatchStarted {
    pub event: &'static str,
    pub path: String,
    pub timestamp: u64,
}

#[derive(Serialize)]
pub struct WatchChange {
    pub event: &'static str,
    pub kind: String,
    pub path: String,
    pub timestamp: u64,
}
