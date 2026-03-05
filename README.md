# Fast Finder (ff)

A single-binary filesystem tool for Linux. Find files, hash everything, detect changes, and monitor the filesystem in real time.

Built in Rust. 2.5 MB binary. Zero dependencies at runtime.

## Install

Download a prebuilt binary from [Releases](../../releases), or build from source:

```bash
cargo build --release
sudo cp target/release/ff /usr/local/bin/
```

## Commands

### Find files

```bash
ff <pattern> [path]           # exact filename match
ff -e <regex> [path]          # regex match against filename or full path
ff -i <pattern> [path]        # case-insensitive
```

```
$ ff Cargo.toml /home
  /home/shearer/personal/rust_finder/Cargo.toml

  916.8K  checked  1  found  0.3s
```

### Hash the filesystem

Hash every file under a path using xxHash (xxh3-128). Snapshots are stored in `~/.ff/` with timestamps.

```bash
ff hash [path]                # default: hash /
ff hash /etc                  # hash only /etc
ff hash /etc -e -f '\.conf$'  # hash only .conf files in /etc
```

```
$ ff hash /etc
  1.9K  files hashed  10.7 MB  processed  1.8 GB/s  6ms
  snapshot: /home/shearer/.ff/etc_20260305_035039.tsv
```

Snapshots are TSV files:

```
# ff snapshot 1772681413 1937 /etc
# hash	size	mtime	path
04bf023323ff84a22ba99adbbc1065bc	10013	1772681381	/etc/fstab
...
```

### Diff against a snapshot

Compare the current filesystem state to the last snapshot. Automatically finds the most recent snapshot for the given path.

```bash
ff diff [path]                # diff / against latest snapshot
ff diff /etc                  # diff /etc against its latest snapshot
ff diff --since 2h /etc       # only check files modified in the last 2 hours
ff diff -e -f '\.conf$' /etc  # only diff .conf files
ff diff --snapshot ~/.ff/etc_20260305_035039.tsv /etc  # use a specific snapshot
```

```
$ ff diff /etc
  comparing against snapshot from 2026-03-05 03:51:13  (1.9K  files)  scope: /etc
  snapshot: /home/shearer/.ff/etc_20260305_035113.tsv

  modified  /etc/resolv.conf
  created   /etc/new-config.conf

  2  changed  1.9K  checked  2  rehashed  6ms
```

The diff is fast because it uses a **size+mtime fast path** — files where both size and mtime are unchanged skip re-hashing entirely. Only files with changed metadata get re-hashed.

### Watch the filesystem in real time

Monitor filesystem changes using Linux fanotify. Requires root.

```bash
sudo ff watch [path]                # watch entire filesystem
sudo ff watch /etc                  # watch /etc
sudo ff watch / -e -f '\.rs$'       # only show .rs file changes
sudo ff watch / --json              # NDJSON stream for piping
```

```
$ sudo ff watch /
  watching /  Ctrl+C to quit

  14:32:01  modified    /home/shearer/project/src/main.rs
  14:32:03  created     /tmp/rust_analyzer_xyz
  14:32:05  deleted     /tmp/rust_analyzer_xyz
```

Events are color-coded: green = created, yellow = modified, red = deleted.

## Filtering

All commands that process files support `--filter` / `-f` to narrow scope:

| Flag | Effect |
|---|---|
| `-f <name>` | Exact filename match |
| `-f <pattern> -e` | Regex match against filename or full path |
| `-f <pattern> -i` | Case-insensitive exact match |
| `-f <pattern> -e -i` | Case-insensitive regex |

```bash
ff hash /home -e -f '\.json$'         # hash only .json files
ff diff /etc -f resolv.conf           # diff only resolv.conf
sudo ff watch / -e -f '\.py$'         # watch only .py changes
```

## JSON API

Every command supports `--json` for machine-consumable NDJSON (newline-delimited JSON) output. Each line is a valid JSON object.

```bash
ff --json Cargo.toml /
```
```json
{"event":"result","path":"/home/shearer/project/Cargo.toml","size":514,"modified":1709654400}
{"event":"summary","checked":916793,"found":1,"elapsed_ms":316,"errors":false}
```

```bash
ff diff --json /etc
```
```json
{"event":"changed","kind":"modified","path":"/etc/resolv.conf","old_hash":"abc..","new_hash":"def.."}
{"event":"summary","modified":1,"created":0,"deleted":0,"checked":1937,"rehashed":1,"elapsed_ms":6}
```

```bash
sudo ff watch --json /
```
```json
{"event":"started","path":"/","timestamp":1709654400}
{"event":"change","kind":"modified","path":"/home/shearer/project/src/main.rs","timestamp":1709654521}
{"event":"change","kind":"created","path":"/tmp/new_file","timestamp":1709654523}
```

The watch JSON stream runs indefinitely until SIGINT/SIGTERM, making it suitable for piping into other tools.

## Global options

| Flag | Effect |
|---|---|
| `-a` / `--all` | Include system paths (`/proc`, `/sys`, `/dev`, `/run`, `/snap`) |
| `--json` | NDJSON output |
| `-h` / `--help` | Help |
| `-V` / `--version` | Version |

By default, virtual filesystems are skipped for speed. Use `-a` to include them.

## How it works

| Component | Implementation |
|---|---|
| File traversal | [jwalk](https://crates.io/crates/jwalk) — parallel directory walk using all CPU cores |
| Hashing | [xxHash](https://crates.io/crates/xxhash-rust) xxh3-128 — ~30 GB/s per core |
| Large file I/O | [memmap2](https://crates.io/crates/memmap2) — zero-copy memory-mapped reads for files >1 MB |
| Real-time monitoring | Linux fanotify with FID-based event parsing — single fd for the entire filesystem |
| Diff fast path | Size + mtime comparison skips re-hashing unchanged files |
| Regex | [regex](https://crates.io/crates/regex) crate with full syntax |
| CLI | [clap](https://crates.io/crates/clap) with derive |

### Performance

Measured on a 24-core machine with ~900K files:

| Operation | Time |
|---|---|
| Find (full `/` scan, 916K files) | 0.3s |
| Hash (55K files, 12 GB) | 1.9s |
| Diff (no changes, mtime fast-path) | 0.1s |
| Watch (event latency) | <100ms |

## Snapshot management

Snapshots are stored in `~/.ff/` (or `/root/.ff/` when running as root), named by target path and timestamp:

```
~/.ff/root_20260305_035039.tsv           # ff hash /
~/.ff/etc_20260305_035039.tsv            # ff hash /etc
~/.ff/etc_20260305_041100.tsv            # ff hash /etc (second run)
~/.ff/home_shearer_20260305_035039.tsv   # ff hash /home/shearer
```

- `ff diff` automatically uses the most recent snapshot for the given path
- Old snapshots are kept for manual comparison via `--snapshot`
- Snapshots are plain TSV — grep, awk, and sort all work on them

## Building

Requires Rust 1.70+. Linux only (fanotify is a Linux-specific API).

```bash
git clone <repo>
cd ff
cargo build --release
```

The release binary is at `target/release/ff` (~2.5 MB, stripped, LTO-optimized).

## License

MIT
