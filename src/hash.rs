use crate::{json, matcher::Matcher, output, skip};
use jwalk::WalkDir;
use memmap2::Mmap;
use rayon::prelude::*;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use xxhash_rust::xxh3::xxh3_128;

/// Threshold for memory-mapping vs buffered read.
const MMAP_THRESHOLD: u64 = 1024 * 1024; // 1 MB

pub struct HashOpts {
    pub path: String,
    pub output_path: Option<String>,
    pub json: bool,
    pub no_skip: bool,
    pub filter: Option<String>,
    pub filter_regex: bool,
    pub filter_ignore_case: bool,
}

/// Hash a single file. Returns the xxh3-128 hash and file size.
fn hash_file(path: &Path) -> Option<(u128, u64)> {
    let meta = fs::metadata(path).ok()?;
    let size = meta.len();

    if size == 0 {
        return Some((xxh3_128(b""), size));
    }

    if size >= MMAP_THRESHOLD {
        let file = File::open(path).ok()?;
        let mmap = unsafe { Mmap::map(&file).ok()? };
        Some((xxh3_128(&mmap), size))
    } else {
        let mut file = File::open(path).ok()?;
        let mut buf = vec![0u8; size as usize];
        file.read_exact(&mut buf).ok()?;
        Some((xxh3_128(&buf), size))
    }
}

// ── Snapshot path helpers ────────────────────────────────────────────

/// Returns ~/.ff/, creating it if needed.
fn snapshot_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let dir = format!("{}/.ff", home);
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Turn a target path into a safe prefix for snapshot filenames.
///   /          → "root"
///   /etc       → "etc"
///   /home/user → "home_user"
fn sanitize_path(path: &str) -> String {
    let canonical = fs::canonicalize(path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string());

    let trimmed = canonical.trim_matches('/');
    if trimmed.is_empty() {
        "root".to_string()
    } else {
        trimmed.replace('/', "_")
    }
}

/// Format a unix timestamp as YYYYMMDD_HHMMSS.
fn format_timestamp(epoch: u64) -> String {
    unsafe {
        let time = epoch as libc::time_t;
        let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
        libc::localtime_r(&time, tm.as_mut_ptr());
        let tm = tm.assume_init();
        format!(
            "{:04}{:02}{:02}_{:02}{:02}{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
        )
    }
}

/// Generate a snapshot filename for a given target path and timestamp.
///   ~/.ff/etc_20260305_033700.tsv
pub fn snapshot_filename(target_path: &str, epoch: u64) -> String {
    let dir = snapshot_dir();
    let prefix = sanitize_path(target_path);
    let ts = format_timestamp(epoch);
    format!("{}/{}_{}.tsv", dir, prefix, ts)
}

/// Find the most recent snapshot for a given target path.
/// Looks for files matching ~/.ff/<prefix>_*.tsv and returns the one
/// with the latest timestamp (lexicographic sort works since YYYYMMDD_HHMMSS).
pub fn find_latest_snapshot(target_path: &str) -> Option<String> {
    let dir = snapshot_dir();
    let prefix = sanitize_path(target_path);
    let pattern = format!("{}_", prefix);

    let mut matches: Vec<String> = Vec::new();

    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&pattern) && name.ends_with(".tsv") {
                matches.push(entry.path().to_string_lossy().to_string());
            }
        }
    }

    if matches.is_empty() {
        return None;
    }

    // Sort lexicographically — latest timestamp sorts last
    matches.sort();
    matches.pop()
}

/// List all snapshots, grouped by target path.
/// Returns Vec<(display_path, file_path, timestamp_str, file_count)>.
#[allow(dead_code)]
pub fn list_snapshots() -> Vec<(String, String, String, u64)> {
    let dir = snapshot_dir();
    let mut results = Vec::new();

    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let file_path = entry.path().to_string_lossy().to_string();
            let name = entry.file_name().to_string_lossy().to_string();

            if !name.ends_with(".tsv") {
                continue;
            }

            // Parse header to get metadata
            if let Ok((ts, _, scope)) = load_snapshot_header(&file_path) {
                let display = if scope.is_empty() {
                    // Derive from filename
                    name.trim_end_matches(".tsv")
                        .rsplitn(2, '_')
                        .last()
                        .unwrap_or(&name)
                        .replace('_', "/")
                } else {
                    scope
                };

                let ts_str = format_timestamp(ts);
                // Count lines (subtract header lines)
                let count = fs::read_to_string(&file_path)
                    .map(|c| {
                        c.lines()
                            .filter(|l| !l.starts_with('#') && !l.is_empty())
                            .count() as u64
                    })
                    .unwrap_or(0);

                results.push((display, file_path, ts_str, count));
            }
        }
    }

    results.sort();
    results
}

// ── Snapshot file format ─────────────────────────────────────────────
//
// Header:
//   # ff snapshot <timestamp> <file_count> <scoped_path>
//   # hash\tsize\tmtime\tpath
//
// Body:
//   <xxh3-128 hex>\t<size>\t<mtime_epoch>\t<path>

pub fn run(opts: HashOpts) {
    let start = Instant::now();
    let root = Path::new(&opts.path);

    // Build file filter if specified
    let file_filter = opts
        .filter
        .as_deref()
        .and_then(|f| Matcher::new(f, opts.filter_regex, opts.filter_ignore_case));

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Determine output path
    let snapshot_path = opts
        .output_path
        .clone()
        .unwrap_or_else(|| snapshot_filename(&opts.path, timestamp));

    if !opts.json {
        let filter_msg = match &file_filter {
            Some(m) => format!("collecting files matching {}...", m.describe()),
            None => "collecting file list...".to_string(),
        };
        output::status_line(&filter_msg);
    }

    // Phase 1: Collect all file paths (parallel traversal)
    let mut file_paths: Vec<PathBuf> = Vec::new();
    let errors = Arc::new(AtomicU64::new(0));
    let errors_clone = errors.clone();
    let no_skip = opts.no_skip;

    for entry in WalkDir::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .process_read_dir(move |_depth, _path, _state, children| {
            if !no_skip {
                children.retain(|entry| match entry {
                    Ok(e) => !skip::should_skip(&e.path()),
                    Err(_) => false,
                });
            }
        })
    {
        match entry {
            Ok(e) if e.file_type().is_file() => {
                let path = e.path();
                if let Some(ref m) = file_filter {
                    if !m.matches(&path) {
                        continue;
                    }
                }
                file_paths.push(path);
            }
            Err(_) => {
                errors_clone.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    let total = file_paths.len() as u64;

    if !opts.json {
        output::clear_status();
        output::status_line(&format!(
            "{}  files found, hashing...",
            output::fmt_count(total)
        ));
    }

    // Phase 2: Hash files in parallel using rayon
    let hashed_count = Arc::new(AtomicU64::new(0));
    let bytes_hashed = Arc::new(AtomicU64::new(0));

    let hashed_count_clone = hashed_count.clone();
    let bytes_hashed_clone = bytes_hashed.clone();
    let json_mode = opts.json;
    let start_clone = start;

    let results: Vec<Option<(PathBuf, u128, u64, u64)>> = file_paths
        .par_iter()
        .map(|path| {
            let result = hash_file(path);
            let count = hashed_count_clone.fetch_add(1, Ordering::Relaxed) + 1;

            if let Some((hash, size)) = result {
                bytes_hashed_clone.fetch_add(size, Ordering::Relaxed);

                let mtime = fs::metadata(path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                if !json_mode && count % 50_000 == 0 {
                    let bh = bytes_hashed_clone.load(Ordering::Relaxed);
                    let elapsed = start_clone.elapsed().as_secs_f64();
                    let throughput = if elapsed > 0.0 {
                        bh as f64 / elapsed
                    } else {
                        0.0
                    };
                    output::status_line(&format!(
                        "{}/{}  hashed  {}/s  {}",
                        output::fmt_count(count),
                        output::fmt_count(total),
                        output::fmt_bytes(throughput as u64),
                        output::fmt_elapsed(start_clone),
                    ));
                }

                Some((path.clone(), hash, size, mtime))
            } else {
                None
            }
        })
        .collect();

    if !json_mode {
        output::clear_status();
    }

    // Phase 3: Write TSV snapshot
    let file = File::create(&snapshot_path).unwrap_or_else(|e| {
        eprintln!("  failed to create snapshot: {}", e);
        std::process::exit(1);
    });
    let mut writer = BufWriter::with_capacity(256 * 1024, file);

    // Header includes scoped path so diff can verify it's looking at the right snapshot
    let canonical = fs::canonicalize(&opts.path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| opts.path.clone());
    let _ = writeln!(
        writer,
        "# ff snapshot {} {} {}",
        timestamp, total, canonical
    );
    let _ = writeln!(writer, "# hash\tsize\tmtime\tpath");

    let mut written = 0u64;
    for result in &results {
        if let Some((path, hash, size, mtime)) = result {
            let _ = writeln!(
                writer,
                "{:032x}\t{}\t{}\t{}",
                hash,
                size,
                mtime,
                path.to_string_lossy()
            );
            written += 1;
        }
    }
    let _ = writer.flush();

    let total_bytes = bytes_hashed.load(Ordering::Relaxed);
    let elapsed_ms = start.elapsed().as_millis() as u64;
    let had_errors = errors.load(Ordering::Relaxed) > 0;

    if opts.json {
        json::emit(&json::HashComplete {
            event: "complete",
            hashed: written,
            snapshot: snapshot_path.clone(),
            elapsed_ms,
            bytes_hashed: total_bytes,
        });
    } else {
        let elapsed_secs = start.elapsed().as_secs_f64();
        let throughput = if elapsed_secs > 0.0 {
            total_bytes as f64 / elapsed_secs
        } else {
            0.0
        };

        output::print_summary(&format!(
            "{}  files hashed  {}  processed  {}/s  {}",
            output::fmt_count(written),
            output::fmt_bytes(total_bytes),
            output::fmt_bytes(throughput as u64),
            output::fmt_elapsed(start),
        ));
        eprintln!("  snapshot: {}", snapshot_path);

        if had_errors {
            output::print_warning("some paths were inaccessible (run with sudo for full coverage)");
        }
    }
}

// ── Snapshot loading ─────────────────────────────────────────────────

/// Load snapshot header only. Returns (timestamp, file_count, scoped_path).
#[allow(dead_code)]
fn load_snapshot_header(path: &str) -> std::io::Result<(u64, u64, String)> {
    let file = File::open(path)?;
    let reader = std::io::BufReader::new(file);
    use std::io::BufRead;
    for line in reader.lines() {
        let line = line?;
        if line.starts_with("# ff snapshot ") {
            let parts: Vec<&str> = line.splitn(6, ' ').collect();
            // # ff snapshot <ts> <count> <path>
            let ts = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let count = parts.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            let scope = parts.get(5).map(|s| s.to_string()).unwrap_or_default();
            return Ok((ts, count, scope));
        }
    }
    Ok((0, 0, String::new()))
}

/// Load a snapshot file into a HashMap for diff operations.
/// Returns (timestamp, scoped_path, map of path -> (hash_hex, size, mtime)).
pub fn load_snapshot(
    path: &str,
) -> std::io::Result<(
    u64,
    String,
    std::collections::HashMap<String, (String, u64, u64)>,
)> {
    let content = fs::read_to_string(path)?;
    let mut map = std::collections::HashMap::new();
    let mut timestamp = 0u64;
    let mut scope = String::new();

    for line in content.lines() {
        if line.starts_with("# ff snapshot ") {
            let parts: Vec<&str> = line.splitn(6, ' ').collect();
            timestamp = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            scope = parts.get(5).map(|s| s.to_string()).unwrap_or_default();
            continue;
        }
        if line.starts_with('#') || line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.splitn(4, '\t').collect();
        if parts.len() == 4 {
            let hash = parts[0].to_string();
            let size: u64 = parts[1].parse().unwrap_or(0);
            let mtime: u64 = parts[2].parse().unwrap_or(0);
            let file_path = parts[3].to_string();
            map.insert(file_path, (hash, size, mtime));
        }
    }

    Ok((timestamp, scope, map))
}

/// Hash a single file and return the hex string. Used by diff.
pub fn hash_file_hex(path: &Path) -> Option<String> {
    hash_file(path).map(|(h, _)| format!("{:032x}", h))
}

/// Get file metadata (size, mtime). Used by diff.
pub fn file_meta(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    let size = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some((size, mtime))
}
