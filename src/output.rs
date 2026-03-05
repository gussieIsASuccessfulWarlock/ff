use crossterm::style::{Color, Stylize};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static STDERR_IS_TTY: AtomicBool = AtomicBool::new(false);

pub fn init() {
    STDERR_IS_TTY.store(
        crossterm::tty::IsTty::is_tty(&io::stderr()),
        Ordering::Relaxed,
    );
}

pub fn is_tty() -> bool {
    STDERR_IS_TTY.load(Ordering::Relaxed)
}

/// Write a status line to stderr that overwrites itself (carriage return).
/// Only does anything if stderr is a TTY.
pub fn status_line(msg: &str) {
    if !is_tty() {
        return;
    }
    let mut err = io::stderr().lock();
    let _ = write!(err, "\r\x1b[2K  {}", msg);
    let _ = err.flush();
}

/// Clear the status line.
pub fn clear_status() {
    if !is_tty() {
        return;
    }
    let mut err = io::stderr().lock();
    let _ = write!(err, "\r\x1b[2K");
    let _ = err.flush();
}

/// Print a result path to stdout (one per line, indented).
pub fn print_result(path: &str) {
    println!("  {}", path);
}

/// Print a summary line to stderr.
pub fn print_summary(msg: &str) {
    eprintln!("\n  {}", msg);
}

/// Print a warning line to stderr.
pub fn print_warning(msg: &str) {
    if is_tty() {
        eprintln!("  {} {}", "!".with(Color::Yellow), msg);
    } else {
        eprintln!("  ! {}", msg);
    }
}

/// Format a file count with K/M suffixes.
pub fn fmt_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Format elapsed time from an Instant.
pub fn fmt_elapsed(start: Instant) -> String {
    let secs = start.elapsed().as_secs_f64();
    if secs < 0.1 {
        format!("{:.0}ms", secs * 1000.0)
    } else {
        format!("{:.1}s", secs)
    }
}

/// Format a byte count (for throughput display).
pub fn fmt_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{} B", bytes)
    }
}

/// Color helpers for change kinds.
pub fn color_created(s: &str) -> String {
    if is_tty() {
        s.with(Color::Green).to_string()
    } else {
        s.to_string()
    }
}

pub fn color_modified(s: &str) -> String {
    if is_tty() {
        s.with(Color::Yellow).to_string()
    } else {
        s.to_string()
    }
}

pub fn color_deleted(s: &str) -> String {
    if is_tty() {
        s.with(Color::Red).to_string()
    } else {
        s.to_string()
    }
}

/// Color a path dimly.
pub fn dim(s: &str) -> String {
    if is_tty() {
        s.with(Color::DarkGrey).to_string()
    } else {
        s.to_string()
    }
}
