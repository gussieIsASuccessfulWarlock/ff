use crate::{hash, json, matcher::Matcher, output, skip};
use jwalk::WalkDir;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct DiffOpts {
    pub path: String,
    pub snapshot_path: Option<String>,
    pub since: Option<u64>, // seconds ago
    pub json: bool,
    pub no_skip: bool,
    pub filter: Option<String>,
    pub filter_regex: bool,
    pub filter_ignore_case: bool,
}

#[derive(Clone)]
enum ChangeKind {
    Created,
    Modified,
    Deleted,
}

impl ChangeKind {
    fn as_str(&self) -> &'static str {
        match self {
            ChangeKind::Created => "created",
            ChangeKind::Modified => "modified",
            ChangeKind::Deleted => "deleted",
        }
    }
}

struct Change {
    kind: ChangeKind,
    path: String,
    old_hash: Option<String>,
    new_hash: Option<String>,
}

pub fn run(opts: DiffOpts) {
    let start = Instant::now();

    // Build file filter if specified
    let file_filter = opts
        .filter
        .as_deref()
        .and_then(|f| Matcher::new(f, opts.filter_regex, opts.filter_ignore_case));

    // Determine snapshot path: explicit > auto-find latest for this target path
    let snapshot_path = if let Some(ref explicit) = opts.snapshot_path {
        explicit.clone()
    } else {
        match hash::find_latest_snapshot(&opts.path) {
            Some(p) => p,
            None => {
                eprintln!("  no snapshot found for '{}'", opts.path);
                eprintln!("  run 'ff hash {}' first to create one", opts.path);
                std::process::exit(1);
            }
        }
    };

    // Load snapshot
    let (snap_timestamp, snap_scope, snap_map) = match hash::load_snapshot(&snapshot_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("  failed to load snapshot: {}", e);
            eprintln!("  run 'ff hash {}' first to create one", opts.path);
            std::process::exit(1);
        }
    };

    let snap_count = snap_map.len() as u64;

    if !opts.json {
        let ts = chrono_format(snap_timestamp);
        let scope_display = if snap_scope.is_empty() {
            String::new()
        } else {
            format!("  scope: {}", snap_scope)
        };
        let filter_display = match &file_filter {
            Some(m) => format!("  filter: {}", m.describe()),
            None => String::new(),
        };
        eprintln!(
            "  comparing against snapshot from {}  ({}  files){}{}",
            ts,
            output::fmt_count(snap_count),
            scope_display,
            filter_display,
        );
        eprintln!("  snapshot: {}", snapshot_path);
    }

    // Compute the mtime cutoff if --since is specified
    let since_cutoff = opts.since.map(|secs_ago| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs().saturating_sub(secs_ago))
            .unwrap_or(0)
    });

    let root = Path::new(&opts.path);
    let checked = Arc::new(AtomicU64::new(0));
    let rehashed = Arc::new(AtomicU64::new(0));
    let changes: Arc<Mutex<Vec<Change>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_paths: Arc<Mutex<HashMap<String, ()>>> = Arc::new(Mutex::new(HashMap::new()));

    // Phase 1: Collect current file paths
    let mut file_paths: Vec<std::path::PathBuf> = Vec::new();
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
        if let Ok(e) = entry {
            if e.file_type().is_file() {
                let path = e.path();
                if let Some(ref m) = file_filter {
                    if !m.matches(&path) {
                        continue;
                    }
                }
                file_paths.push(path);
            }
        }
    }

    if !opts.json {
        output::clear_status();
        output::status_line(&format!(
            "{}  files found, comparing...",
            output::fmt_count(file_paths.len() as u64),
        ));
    }

    // Phase 2: Compare in parallel
    let snap_map = Arc::new(snap_map);
    let snap_map_par = snap_map.clone();
    let changes_par = changes.clone();
    let seen_par = seen_paths.clone();
    let checked_par = checked.clone();
    let rehashed_par = rehashed.clone();

    file_paths.par_iter().for_each(|path| {
        let path_str = path.to_string_lossy().to_string();
        checked_par.fetch_add(1, Ordering::Relaxed);

        seen_par.lock().unwrap().insert(path_str.clone(), ());

        if let Some((snap_hash, snap_size, snap_mtime)) = snap_map_par.get(&path_str) {
            // File existed in snapshot — check if changed
            if let Some((cur_size, cur_mtime)) = hash::file_meta(path) {
                // Fast path: if size and mtime match, skip re-hash
                if cur_size == *snap_size && cur_mtime == *snap_mtime {
                    return;
                }

                // If --since specified, skip files older than cutoff
                if let Some(cutoff) = since_cutoff {
                    if cur_mtime < cutoff {
                        return;
                    }
                }

                // Re-hash needed
                rehashed_par.fetch_add(1, Ordering::Relaxed);
                if let Some(cur_hash) = hash::hash_file_hex(path) {
                    if cur_hash != *snap_hash {
                        changes_par.lock().unwrap().push(Change {
                            kind: ChangeKind::Modified,
                            path: path_str,
                            old_hash: Some(snap_hash.clone()),
                            new_hash: Some(cur_hash),
                        });
                    }
                }
            }
        } else {
            // New file (not in snapshot)
            if let Some(cutoff) = since_cutoff {
                if let Some((_, cur_mtime)) = hash::file_meta(path) {
                    if cur_mtime < cutoff {
                        return;
                    }
                }
            }

            let new_hash = hash::hash_file_hex(path);
            rehashed_par.fetch_add(1, Ordering::Relaxed);
            changes_par.lock().unwrap().push(Change {
                kind: ChangeKind::Created,
                path: path_str,
                old_hash: None,
                new_hash,
            });
        }
    });

    // Phase 3: Find deleted files (only those matching the filter)
    let seen = seen_paths.lock().unwrap();
    for (snap_path, (snap_hash, _, _)) in snap_map.iter() {
        if !seen.contains_key(snap_path) {
            if let Some(ref m) = file_filter {
                if !m.matches_str(snap_path) {
                    continue;
                }
            }
            changes.lock().unwrap().push(Change {
                kind: ChangeKind::Deleted,
                path: snap_path.clone(),
                old_hash: Some(snap_hash.clone()),
                new_hash: None,
            });
        }
    }

    let all_changes = changes.lock().unwrap();
    let total_checked = checked.load(Ordering::Relaxed);
    let total_rehashed = rehashed.load(Ordering::Relaxed);
    let elapsed_ms = start.elapsed().as_millis() as u64;

    let mut n_created = 0u64;
    let mut n_modified = 0u64;
    let mut n_deleted = 0u64;

    if opts.json {
        for c in all_changes.iter() {
            match c.kind {
                ChangeKind::Created => n_created += 1,
                ChangeKind::Modified => n_modified += 1,
                ChangeKind::Deleted => n_deleted += 1,
            }
            json::emit(&json::DiffChanged {
                event: "changed",
                kind: c.kind.as_str().to_string(),
                path: c.path.clone(),
                old_hash: c.old_hash.clone(),
                new_hash: c.new_hash.clone(),
            });
        }
        json::emit(&json::DiffSummary {
            event: "summary",
            modified: n_modified,
            created: n_created,
            deleted: n_deleted,
            checked: total_checked,
            rehashed: total_rehashed,
            elapsed_ms,
        });
    } else {
        output::clear_status();

        if all_changes.is_empty() {
            output::print_summary("no changes detected");
        } else {
            // Sort: deleted, modified, created
            let mut sorted: Vec<&Change> = all_changes.iter().collect();
            sorted.sort_by(|a, b| {
                let order = |k: &ChangeKind| match k {
                    ChangeKind::Deleted => 0,
                    ChangeKind::Modified => 1,
                    ChangeKind::Created => 2,
                };
                order(&a.kind)
                    .cmp(&order(&b.kind))
                    .then(a.path.cmp(&b.path))
            });

            eprintln!();
            for c in &sorted {
                let kind_str = match c.kind {
                    ChangeKind::Created => output::color_created("created "),
                    ChangeKind::Modified => output::color_modified("modified"),
                    ChangeKind::Deleted => output::color_deleted("deleted "),
                };
                eprintln!("  {}  {}", kind_str, c.path);

                match c.kind {
                    ChangeKind::Created => n_created += 1,
                    ChangeKind::Modified => n_modified += 1,
                    ChangeKind::Deleted => n_deleted += 1,
                }
            }
        }

        let total_changes = n_created + n_modified + n_deleted;
        output::print_summary(&format!(
            "{}  changed  {}  checked  {}  rehashed  {}",
            total_changes,
            output::fmt_count(total_checked),
            output::fmt_count(total_rehashed),
            output::fmt_elapsed(start),
        ));
    }
}

/// Simple timestamp formatting without pulling in chrono.
fn chrono_format(epoch_secs: u64) -> String {
    use std::ffi::CStr;
    use std::mem::MaybeUninit;

    unsafe {
        let time = epoch_secs as libc::time_t;
        let mut tm = MaybeUninit::<libc::tm>::zeroed();
        libc::localtime_r(&time, tm.as_mut_ptr());
        let tm = tm.assume_init();

        let mut buf = [0u8; 64];
        let fmt = b"%Y-%m-%d %H:%M:%S\0";
        let len = libc::strftime(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            fmt.as_ptr() as *const libc::c_char,
            &tm,
        );
        if len > 0 {
            let cstr = CStr::from_ptr(buf.as_ptr() as *const libc::c_char);
            cstr.to_string_lossy().to_string()
        } else {
            format!("{}", epoch_secs)
        }
    }
}
