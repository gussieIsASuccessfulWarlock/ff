use crate::{json, matcher::Matcher, output, skip};
use jwalk::WalkDir;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub struct FindOpts {
    pub pattern: String,
    pub path: String,
    pub ignore_case: bool,
    pub is_regex: bool,
    pub json: bool,
    pub no_skip: bool,
}

pub fn run(opts: FindOpts) {
    let start = Instant::now();
    let checked = Arc::new(AtomicU64::new(0));
    let found = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicBool::new(false));

    let m = Matcher::new(&opts.pattern, opts.is_regex, opts.ignore_case).unwrap_or_else(|| {
        eprintln!("  pattern cannot be empty");
        std::process::exit(1);
    });

    let root = Path::new(&opts.path);

    let no_skip = opts.no_skip;
    let walker = WalkDir::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .process_read_dir(move |_depth, _path, _read_dir_state, children| {
            if !no_skip {
                children.retain(|entry| match entry {
                    Ok(e) => !skip::should_skip(&e.path()),
                    Err(_) => false,
                });
            }
        });

    for entry in walker {
        match entry {
            Ok(e) => {
                let count = checked.fetch_add(1, Ordering::Relaxed) + 1;

                if !e.file_type().is_file() {
                    if !opts.json && count % 50_000 == 0 {
                        output::status_line(&format!(
                            "{}  checked  {}  found  {}",
                            output::fmt_count(count),
                            output::fmt_count(found.load(Ordering::Relaxed)),
                            output::fmt_elapsed(start),
                        ));
                    }
                    continue;
                }

                let path = e.path();

                if m.matches(&path) {
                    found.fetch_add(1, Ordering::Relaxed);

                    if opts.json {
                        let meta = std::fs::metadata(&path);
                        let (size, modified) = meta
                            .map(|m| {
                                let size = m.len();
                                let mtime = m
                                    .modified()
                                    .ok()
                                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0);
                                (size, mtime)
                            })
                            .unwrap_or((0, 0));

                        json::emit(&json::FindResult {
                            event: "result",
                            path: path.to_string_lossy().to_string(),
                            size,
                            modified,
                        });
                    } else {
                        output::clear_status();
                        output::print_result(&path.to_string_lossy());
                    }
                }

                if !opts.json && count % 50_000 == 0 {
                    output::status_line(&format!(
                        "{}  checked  {}  found  {}",
                        output::fmt_count(count),
                        output::fmt_count(found.load(Ordering::Relaxed)),
                        output::fmt_elapsed(start),
                    ));
                }
            }
            Err(_) => {
                errors.store(true, Ordering::Relaxed);
            }
        }
    }

    let total_checked = checked.load(Ordering::Relaxed);
    let total_found = found.load(Ordering::Relaxed);
    let had_errors = errors.load(Ordering::Relaxed);
    let elapsed_ms = start.elapsed().as_millis() as u64;

    if opts.json {
        json::emit(&json::FindSummary {
            event: "summary",
            checked: total_checked,
            found: total_found,
            elapsed_ms,
            errors: had_errors,
        });
    } else {
        output::clear_status();
        output::print_summary(&format!(
            "{}  checked  {}  found  {}",
            output::fmt_count(total_checked),
            output::fmt_count(total_found),
            output::fmt_elapsed(start),
        ));

        if had_errors {
            output::print_warning("some paths were inaccessible (run with sudo for full coverage)");
        }
    }
}
