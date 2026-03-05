use regex::Regex;
use std::path::Path;

/// Shared file-matching logic used by find, hash, diff, and watch.
/// Matches against filename or full path depending on mode.
pub struct Matcher {
    pattern: String,
    pattern_lower: String,
    regex: Option<Regex>,
    ignore_case: bool,
    is_regex: bool,
}

impl Matcher {
    /// Create a new matcher. Returns None if no filter is active (pattern is empty).
    pub fn new(pattern: &str, is_regex: bool, ignore_case: bool) -> Option<Self> {
        if pattern.is_empty() {
            return None;
        }

        let regex = if is_regex {
            let pat = if ignore_case {
                format!("(?i){}", pattern)
            } else {
                pattern.to_string()
            };
            match Regex::new(&pat) {
                Ok(r) => Some(r),
                Err(e) => {
                    eprintln!("  invalid regex: {}", e);
                    std::process::exit(1);
                }
            }
        } else {
            None
        };

        let pattern_lower = if ignore_case && !is_regex {
            pattern.to_lowercase()
        } else {
            String::new()
        };

        Some(Self {
            pattern: pattern.to_string(),
            pattern_lower,
            regex,
            ignore_case,
            is_regex,
        })
    }

    /// Test if a path matches the filter.
    /// Checks against filename first, then full path for regex mode.
    pub fn matches(&self, path: &Path) -> bool {
        let file_name = match path.file_name() {
            Some(n) => n.to_string_lossy(),
            None => return false,
        };

        if let Some(ref re) = self.regex {
            re.is_match(&file_name) || re.is_match(&path.to_string_lossy())
        } else if self.ignore_case {
            file_name.to_lowercase() == self.pattern_lower
        } else {
            file_name.as_ref() == self.pattern
        }
    }

    /// Test if a path string matches the filter (for snapshot entries that are already strings).
    pub fn matches_str(&self, path_str: &str) -> bool {
        let path = Path::new(path_str);
        self.matches(path)
    }

    /// Description for output messages.
    pub fn describe(&self) -> String {
        if self.is_regex {
            format!("regex:{}", self.pattern)
        } else if self.ignore_case {
            format!("{}  (case-insensitive)", self.pattern)
        } else {
            self.pattern.clone()
        }
    }
}
