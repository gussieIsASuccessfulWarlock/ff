use crate::{json, matcher::Matcher, output};
use std::collections::HashMap;
use std::fs;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct WatchOpts {
    pub path: String,
    pub debounce_ms: u64,
    pub json: bool,
    pub filter: Option<String>,
    pub filter_regex: bool,
    pub filter_ignore_case: bool,
}

// fanotify constants (from linux/fanotify.h)
const FAN_CLASS_NOTIF: u32 = 0x00000000;
const FAN_UNLIMITED_QUEUE: u32 = 0x00000010;
const FAN_UNLIMITED_MARKS: u32 = 0x00000020;
const FAN_REPORT_DIR_FID: u32 = 0x00000400;
const FAN_REPORT_NAME: u32 = 0x00000800;
const FAN_REPORT_DFID_NAME: u32 = FAN_REPORT_DIR_FID | FAN_REPORT_NAME;
const FAN_REPORT_FID: u32 = 0x00000200;

const FAN_MARK_ADD: u32 = 0x00000001;
const FAN_MARK_FILESYSTEM: u32 = 0x00000100;

const FAN_CREATE: u64 = 0x00000100;
const FAN_DELETE: u64 = 0x00000200;
const FAN_MODIFY: u64 = 0x00000002;
const FAN_MOVED_FROM: u64 = 0x00000040;
const FAN_MOVED_TO: u64 = 0x00000080;
const FAN_CLOSE_WRITE: u64 = 0x00000008;

const FAN_NOFD: i32 = -1;

// Event info record types
const FAN_EVENT_INFO_TYPE_FID: u8 = 1;
const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;
const FAN_EVENT_INFO_TYPE_DFID: u8 = 3;

#[repr(C)]
struct FanotifyEventMetadata {
    event_len: u32,
    vers: u8,
    reserved: u8,
    metadata_len: u16,
    mask: u64,
    fd: i32,
    pid: i32,
}

#[repr(C)]
struct FanotifyEventInfoHeader {
    info_type: u8,
    pad: u8,
    len: u16,
}

#[repr(C)]
struct FanotifyEventInfoFid {
    hdr: FanotifyEventInfoHeader,
    fsid: [u32; 2],
    // followed by file_handle: struct file_handle { handle_bytes, handle_type, f_handle[] }
}

const FANOTIFY_METADATA_VERSION: u8 = 3;
const FAN_EVENT_METADATA_LEN: usize = std::mem::size_of::<FanotifyEventMetadata>();

extern "C" {
    fn fanotify_init(flags: u32, event_f_flags: u32) -> i32;
    fn fanotify_mark(
        fanotify_fd: i32,
        flags: u32,
        mask: u64,
        dirfd: i32,
        pathname: *const libc::c_char,
    ) -> i32;
}

pub fn run(opts: WatchOpts) {
    // Build file filter if specified
    let file_filter = opts
        .filter
        .as_deref()
        .and_then(|f| Matcher::new(f, opts.filter_regex, opts.filter_ignore_case));

    let (fan_fd, use_fid) = init_fanotify();

    if fan_fd < 0 {
        eprintln!("  failed to initialize fanotify (errno: {})", errno());
        eprintln!("  fanotify requires root/CAP_SYS_ADMIN — run with sudo");
        std::process::exit(1);
    }

    let mask = if use_fid {
        FAN_CREATE | FAN_DELETE | FAN_MODIFY | FAN_MOVED_FROM | FAN_MOVED_TO
    } else {
        FAN_CLOSE_WRITE | FAN_MODIFY
    };

    let mark_flags = FAN_MARK_ADD | FAN_MARK_FILESYSTEM;
    let path_cstr = std::ffi::CString::new(opts.path.as_str()).expect("invalid path");

    let ret =
        unsafe { fanotify_mark(fan_fd, mark_flags, mask, libc::AT_FDCWD, path_cstr.as_ptr()) };

    if ret < 0 {
        let ret2 = unsafe {
            fanotify_mark(
                fan_fd,
                FAN_MARK_ADD,
                mask,
                libc::AT_FDCWD,
                path_cstr.as_ptr(),
            )
        };
        if ret2 < 0 {
            eprintln!("  failed to mark path for monitoring (errno: {})", errno());
            eprintln!("  ensure the path exists and you have sufficient permissions");
            std::process::exit(1);
        }
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if opts.json {
        json::emit(&json::WatchStarted {
            event: "started",
            path: opts.path.clone(),
            timestamp: now,
        });
    } else {
        let filter_msg = match &file_filter {
            Some(m) => format!("  filter: {}", m.describe()),
            None => String::new(),
        };
        if filter_msg.is_empty() {
            eprintln!("  watching {}  Ctrl+C to quit\n", opts.path);
        } else {
            eprintln!(
                "  watching {}  {}\n  Ctrl+C to quit\n",
                opts.path, filter_msg
            );
        }
    }

    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let r = running.clone();
    let _ = ctrlc_setup(r);

    let mut debounce_map: HashMap<String, (String, Instant)> = HashMap::new();
    let debounce_duration = Duration::from_millis(opts.debounce_ms);

    let mut buf = vec![0u8; 8192];

    while running.load(std::sync::atomic::Ordering::Relaxed) {
        // Flush debounce buffer
        let now_instant = Instant::now();
        let mut to_flush = Vec::new();
        debounce_map.retain(|path, (kind, last_seen)| {
            if now_instant.duration_since(*last_seen) >= debounce_duration {
                to_flush.push((path.clone(), kind.clone()));
                false
            } else {
                true
            }
        });
        for (path, kind) in to_flush {
            emit_change(&path, &kind, opts.json);
        }

        let n = unsafe {
            let mut pfd = libc::pollfd {
                fd: fan_fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let poll_ret = libc::poll(&mut pfd, 1, 50);
            if poll_ret <= 0 {
                continue;
            }
            libc::read(fan_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
        };

        if n <= 0 {
            continue;
        }

        let bytes = &buf[..n as usize];
        let mut offset = 0usize;

        while offset + FAN_EVENT_METADATA_LEN <= bytes.len() {
            let meta = unsafe { &*(bytes.as_ptr().add(offset) as *const FanotifyEventMetadata) };

            if meta.vers != FANOTIFY_METADATA_VERSION {
                break;
            }

            let event_len = meta.event_len as usize;
            if event_len < FAN_EVENT_METADATA_LEN || offset + event_len > bytes.len() {
                break;
            }

            let kind = mask_to_kind(meta.mask);

            if use_fid && meta.fd == FAN_NOFD {
                // FID-based event: parse info records to get directory + filename
                if let Some(path_str) = parse_fid_event(bytes, offset, event_len) {
                    let dominated = match &file_filter {
                        Some(m) => m.matches_str(&path_str),
                        None => true,
                    };
                    if dominated {
                        debounce_map.insert(path_str, (kind.to_string(), Instant::now()));
                    }
                }
            } else if meta.fd >= 0 {
                // Classic fd-based event
                let fd_path = format!("/proc/self/fd/{}", meta.fd);
                if let Ok(resolved) = fs::read_link(&fd_path) {
                    let path_str = resolved.to_string_lossy().to_string();
                    let pass = match &file_filter {
                        Some(m) => m.matches_str(&path_str),
                        None => true,
                    };
                    if pass {
                        debounce_map.insert(path_str, (kind.to_string(), Instant::now()));
                    }
                }
                unsafe { libc::close(meta.fd) };
            }

            offset += event_len;
        }
    }

    // Flush remaining
    for (path, (kind, _)) in &debounce_map {
        emit_change(path, kind, opts.json);
    }

    unsafe { libc::close(fan_fd) };

    if !opts.json {
        eprintln!("\n  stopped watching");
    }
}

/// Parse FID-based fanotify event info records to extract the full path.
///
/// FID events contain info records with:
/// - DFID_NAME: directory file handle + child name
/// - DFID: directory file handle only (for directory events)
/// - FID: file handle only
///
/// We use open_by_handle_at() to resolve file handles to paths.
fn parse_fid_event(buf: &[u8], event_offset: usize, event_len: usize) -> Option<String> {
    let info_start = event_offset + FAN_EVENT_METADATA_LEN;
    let event_end = event_offset + event_len;
    let mut pos = info_start;

    let mut dir_path: Option<String> = None;
    let mut file_name: Option<String> = None;

    while pos + std::mem::size_of::<FanotifyEventInfoHeader>() <= event_end {
        let hdr = unsafe { &*(buf.as_ptr().add(pos) as *const FanotifyEventInfoHeader) };
        let record_len = hdr.len as usize;

        if record_len < std::mem::size_of::<FanotifyEventInfoHeader>()
            || pos + record_len > event_end
        {
            break;
        }

        match hdr.info_type {
            FAN_EVENT_INFO_TYPE_DFID_NAME | FAN_EVENT_INFO_TYPE_DFID => {
                // After the header: fsid (8 bytes) + file_handle struct
                let fid_hdr_size = std::mem::size_of::<FanotifyEventInfoFid>();
                if pos + fid_hdr_size > event_end {
                    break;
                }

                // file_handle starts after fsid
                let fh_offset = pos + std::mem::size_of::<FanotifyEventInfoHeader>() + 8; // 8 = sizeof(fsid)
                if fh_offset + 8 > event_end {
                    break;
                }

                // Read file_handle: handle_bytes (u32) + handle_type (i32)
                let handle_bytes =
                    u32::from_ne_bytes(buf[fh_offset..fh_offset + 4].try_into().ok()?) as usize;
                let handle_type =
                    i32::from_ne_bytes(buf[fh_offset + 4..fh_offset + 8].try_into().ok()?);

                let f_handle_offset = fh_offset + 8;
                if f_handle_offset + handle_bytes > event_end {
                    break;
                }

                // Try to resolve the directory handle using open_by_handle_at
                let resolved = resolve_file_handle(
                    handle_type,
                    &buf[f_handle_offset..f_handle_offset + handle_bytes],
                );
                if let Some(path) = resolved {
                    dir_path = Some(path);
                }

                // For DFID_NAME, the filename follows after the file_handle
                if hdr.info_type == FAN_EVENT_INFO_TYPE_DFID_NAME {
                    let name_offset = f_handle_offset + handle_bytes;
                    if name_offset < pos + record_len {
                        let name_bytes = &buf[name_offset..pos + record_len];
                        // Name is null-terminated
                        let name_end = name_bytes
                            .iter()
                            .position(|&b| b == 0)
                            .unwrap_or(name_bytes.len());
                        if name_end > 0 {
                            file_name = String::from_utf8(name_bytes[..name_end].to_vec()).ok();
                        }
                    }
                }
            }
            FAN_EVENT_INFO_TYPE_FID => {
                // Same structure as DFID but for the file itself
                let fh_offset = pos + std::mem::size_of::<FanotifyEventInfoHeader>() + 8;
                if fh_offset + 8 > event_end {
                    break;
                }
                let handle_bytes =
                    u32::from_ne_bytes(buf[fh_offset..fh_offset + 4].try_into().ok()?) as usize;
                let handle_type =
                    i32::from_ne_bytes(buf[fh_offset + 4..fh_offset + 8].try_into().ok()?);
                let f_handle_offset = fh_offset + 8;
                if f_handle_offset + handle_bytes > event_end {
                    break;
                }
                let resolved = resolve_file_handle(
                    handle_type,
                    &buf[f_handle_offset..f_handle_offset + handle_bytes],
                );
                if dir_path.is_none() {
                    dir_path = resolved;
                }
            }
            _ => {} // Unknown info type, skip
        }

        pos += record_len;
    }

    // Combine dir_path + file_name
    match (dir_path, file_name) {
        (Some(dir), Some(name)) => {
            if dir.ends_with('/') {
                Some(format!("{}{}", dir, name))
            } else {
                Some(format!("{}/{}", dir, name))
            }
        }
        (Some(dir), None) => Some(dir),
        (None, Some(name)) => Some(name),
        (None, None) => None,
    }
}

/// Resolve a file handle to a path using open_by_handle_at + /proc/self/fd.
fn resolve_file_handle(handle_type: i32, f_handle: &[u8]) -> Option<String> {
    let handle_bytes = f_handle.len() as u32;

    // Build the file_handle struct in memory
    // struct file_handle { unsigned int handle_bytes; int handle_type; unsigned char f_handle[]; }
    let mut fh_buf = Vec::with_capacity(8 + f_handle.len());
    fh_buf.extend_from_slice(&handle_bytes.to_ne_bytes());
    fh_buf.extend_from_slice(&handle_type.to_ne_bytes());
    fh_buf.extend_from_slice(f_handle);

    unsafe {
        // open_by_handle_at(mount_fd, file_handle, flags)
        // Use AT_FDCWD as mount_fd — requires CAP_DAC_READ_SEARCH
        let fd = libc::syscall(
            libc::SYS_open_by_handle_at,
            libc::AT_FDCWD,
            fh_buf.as_ptr(),
            libc::O_RDONLY | libc::O_PATH,
        ) as i32;

        if fd < 0 {
            return None;
        }

        let fd_path = format!("/proc/self/fd/{}", fd);
        let result = fs::read_link(&fd_path)
            .ok()
            .map(|p| p.to_string_lossy().to_string());

        libc::close(fd);
        result
    }
}

fn mask_to_kind(mask: u64) -> &'static str {
    if mask & FAN_CREATE != 0 {
        "created"
    } else if mask & FAN_DELETE != 0 {
        "deleted"
    } else if mask & FAN_MOVED_FROM != 0 {
        "moved_from"
    } else if mask & FAN_MOVED_TO != 0 {
        "moved_to"
    } else if mask & (FAN_MODIFY | FAN_CLOSE_WRITE) != 0 {
        "modified"
    } else {
        "unknown"
    }
}

fn init_fanotify() -> (i32, bool) {
    // Try FID-based first (kernel 5.1+)
    let flags_fid = FAN_CLASS_NOTIF
        | FAN_UNLIMITED_QUEUE
        | FAN_UNLIMITED_MARKS
        | FAN_REPORT_DFID_NAME
        | FAN_REPORT_FID;
    let fd = unsafe { fanotify_init(flags_fid, 0) };
    if fd >= 0 {
        return (fd, true);
    }

    // Fall back to classic fd-based
    let flags_classic = FAN_CLASS_NOTIF | FAN_UNLIMITED_QUEUE | FAN_UNLIMITED_MARKS;
    let o_rdonly = libc::O_RDONLY as u32;
    let o_largefile = libc::O_LARGEFILE as u32;
    let fd = unsafe { fanotify_init(flags_classic, o_rdonly | o_largefile) };
    (fd, false)
}

fn emit_change(path: &str, kind: &str, json_mode: bool) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if json_mode {
        json::emit(&json::WatchChange {
            event: "change",
            kind: kind.to_string(),
            path: path.to_string(),
            timestamp: now,
        });
    } else {
        let time_str = format_time_hms();
        let kind_colored = match kind {
            "created" | "moved_to" => output::color_created(&format!("{:<10}", kind)),
            "modified" => output::color_modified(&format!("{:<10}", kind)),
            "deleted" | "moved_from" => output::color_deleted(&format!("{:<10}", kind)),
            _ => format!("{:<10}", kind),
        };
        eprintln!("  {}  {}  {}", output::dim(&time_str), kind_colored, path);
    }
}

fn format_time_hms() -> String {
    unsafe {
        let mut now: libc::time_t = 0;
        libc::time(&mut now);
        let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
        libc::localtime_r(&now, tm.as_mut_ptr());
        let tm = tm.assume_init();
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

fn errno() -> i32 {
    unsafe { *libc::__errno_location() }
}

fn ctrlc_setup(running: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<(), ()> {
    unsafe {
        let r = running;
        libc::signal(
            libc::SIGINT,
            signal_handler as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            signal_handler as *const () as libc::sighandler_t,
        );
        RUNNING_FLAG.store(
            std::sync::Arc::into_raw(r) as *mut bool as usize,
            std::sync::atomic::Ordering::SeqCst,
        );
    }
    Ok(())
}

static RUNNING_FLAG: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

extern "C" fn signal_handler(_sig: i32) {
    let ptr = RUNNING_FLAG.load(std::sync::atomic::Ordering::SeqCst);
    if ptr != 0 {
        unsafe {
            let flag = ptr as *const std::sync::atomic::AtomicBool;
            (*flag).store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
}
