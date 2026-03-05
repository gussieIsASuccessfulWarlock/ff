/// Paths to skip during traversal — virtual/pseudo filesystems that are
/// not real files on disk and would waste time or cause hangs.
const SKIP_PATHS: &[&str] = &[
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/snap",
    "/var/run",
    "/var/lock",
];

/// Returns true if the given path should be skipped during traversal.
pub fn should_skip(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy();
    for skip in SKIP_PATHS {
        if s.as_ref() == *skip || s.starts_with(&format!("{}/", skip)) {
            return true;
        }
    }
    false
}
