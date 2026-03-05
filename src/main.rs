mod diff;
mod find;
mod hash;
mod json;
mod matcher;
mod output;
mod skip;
mod watch;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "ff",
    version,
    about = "Fast file finder, hasher, and change monitor",
    long_about = None,
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// File pattern to search for (default mode)
    #[arg()]
    pattern: Option<String>,

    /// Directory to search in
    #[arg(default_value = "/")]
    path: Option<String>,

    /// Treat pattern as a regular expression
    #[arg(short = 'e', long = "regex")]
    is_regex: bool,

    /// Case-insensitive matching
    #[arg(short, long = "ignore-case")]
    ignore_case: bool,

    /// Output NDJSON for machine consumption
    #[arg(long)]
    json: bool,

    /// Don't skip system paths (/proc, /sys, /dev, /run, /snap)
    #[arg(short, long)]
    all: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Hash all files and save a snapshot
    Hash {
        /// Directory to hash
        #[arg(default_value = "/")]
        path: String,

        /// Output snapshot file path
        #[arg(short, long)]
        output: Option<String>,

        /// Output NDJSON for machine consumption
        #[arg(long)]
        json: bool,

        /// Don't skip system paths (/proc, /sys, /dev, /run, /snap)
        #[arg(short, long)]
        all: bool,

        /// Only hash files matching this pattern
        #[arg(short, long)]
        filter: Option<String>,

        /// Treat filter as a regular expression
        #[arg(short = 'e', long = "regex")]
        filter_regex: bool,

        /// Case-insensitive filter matching
        #[arg(short, long = "ignore-case")]
        ignore_case: bool,
    },

    /// Compare current filesystem state against a snapshot
    Diff {
        /// Directory to diff
        #[arg(default_value = "/")]
        path: String,

        /// Snapshot file to compare against
        #[arg(long)]
        snapshot: Option<String>,

        /// Only check files modified within this duration (e.g. 2h, 30m, 1d)
        #[arg(long)]
        since: Option<String>,

        /// Output NDJSON for machine consumption
        #[arg(long)]
        json: bool,

        /// Don't skip system paths (/proc, /sys, /dev, /run, /snap)
        #[arg(short, long)]
        all: bool,

        /// Only diff files matching this pattern
        #[arg(short, long)]
        filter: Option<String>,

        /// Treat filter as a regular expression
        #[arg(short = 'e', long = "regex")]
        filter_regex: bool,

        /// Case-insensitive filter matching
        #[arg(short, long = "ignore-case")]
        ignore_case: bool,
    },

    /// Real-time filesystem change monitor (requires root)
    Watch {
        /// Directory/mount to watch
        #[arg(default_value = "/")]
        path: String,

        /// Debounce window in milliseconds
        #[arg(long, default_value = "100")]
        debounce: u64,

        /// Output NDJSON for machine consumption
        #[arg(long)]
        json: bool,

        /// Only show changes to files matching this pattern
        #[arg(short, long)]
        filter: Option<String>,

        /// Treat filter as a regular expression
        #[arg(short = 'e', long = "regex")]
        filter_regex: bool,

        /// Case-insensitive filter matching
        #[arg(short, long = "ignore-case")]
        ignore_case: bool,
    },
}

fn main() {
    output::init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Hash {
            path,
            output: out,
            json,
            all,
            filter,
            filter_regex,
            ignore_case,
        }) => {
            hash::run(hash::HashOpts {
                path,
                output_path: out,
                json,
                no_skip: all,
                filter,
                filter_regex,
                filter_ignore_case: ignore_case,
            });
        }
        Some(Commands::Diff {
            path,
            snapshot,
            since,
            json,
            all,
            filter,
            filter_regex,
            ignore_case,
        }) => {
            let since_secs = since.map(|s| parse_duration(&s));
            diff::run(diff::DiffOpts {
                path,
                snapshot_path: snapshot,
                since: since_secs,
                json,
                no_skip: all,
                filter,
                filter_regex,
                filter_ignore_case: ignore_case,
            });
        }
        Some(Commands::Watch {
            path,
            debounce,
            json,
            filter,
            filter_regex,
            ignore_case,
        }) => {
            watch::run(watch::WatchOpts {
                path,
                debounce_ms: debounce,
                json,
                filter,
                filter_regex,
                filter_ignore_case: ignore_case,
            });
        }
        None => {
            // Default mode: find
            let pattern = match cli.pattern {
                Some(p) => p,
                None => {
                    eprintln!("  usage: ff <pattern> [path]");
                    eprintln!("  run 'ff --help' for all commands");
                    std::process::exit(1);
                }
            };
            let path = cli.path.unwrap_or_else(|| "/".to_string());

            find::run(find::FindOpts {
                pattern,
                path,
                ignore_case: cli.ignore_case,
                is_regex: cli.is_regex,
                json: cli.json,
                no_skip: cli.all,
            });
        }
    }
}

/// Parse a human-friendly duration string into seconds.
/// Supports: 30s, 5m, 2h, 1d, or plain number (treated as seconds).
fn parse_duration(s: &str) -> u64 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }

    let (num_str, multiplier) = if s.ends_with('d') {
        (&s[..s.len() - 1], 86400u64)
    } else if s.ends_with('h') {
        (&s[..s.len() - 1], 3600u64)
    } else if s.ends_with('m') {
        (&s[..s.len() - 1], 60u64)
    } else if s.ends_with('s') {
        (&s[..s.len() - 1], 1u64)
    } else {
        (s, 1u64)
    };

    num_str.parse::<u64>().unwrap_or_else(|_| {
        eprintln!("  invalid duration: {}", s);
        std::process::exit(1);
    }) * multiplier
}
