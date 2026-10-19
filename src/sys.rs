//! Small OS helpers: data directory, time, peak RSS.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// `$MEMORYCASTLE_HOME`, else `~/.local/share/memorycastle`.
pub fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("MEMORYCASTLE_HOME") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".local/share/memorycastle")
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Peak resident set size of this process in bytes.
pub fn peak_rss_bytes() -> u64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    if rc != 0 {
        return 0;
    }
    let raw = ru.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        raw
    } else {
        raw * 1024
    }
}

/// Current resident set size in bytes, from the OS where cheap to read.
pub fn current_rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/statm") {
            if let Some(pages) = s.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()) {
                return pages * 4096;
            }
        }
        0
    }
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        let rc = unsafe {
            libc::proc_pidinfo(
                std::process::id() as i32,
                libc::PROC_PIDTASKINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if rc == size {
            info.pti_resident_size
        } else {
            peak_rss_bytes()
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        peak_rss_bytes()
    }
}
