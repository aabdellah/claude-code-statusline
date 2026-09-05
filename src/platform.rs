//! OS-specific primitives behind one small cross-platform surface.
//!
//! Every other module stays platform-agnostic by routing through here:
//! home directory, scratch/cache locations, detached process spawning,
//! local-midnight arithmetic, console width, and private-file creation.
//! Unix keeps its historical behavior unchanged (`/tmp`, `$HOME`,
//! `setsid`, `libc::localtime_r`); Windows gets the closest native
//! equivalent. No new crates: the Windows side declares the handful of
//! kernel32 entry points it needs directly.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

// --- Home -------------------------------------------------------------------

/// The user's home directory. `$HOME` on Unix. On Windows `USERPROFILE`
/// first — Git Bash exports `HOME` too, but CC may run us from PowerShell
/// where it's unset — then `HOME` as a fallback.
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(p) = std::env::var_os("USERPROFILE") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("HOME").map(PathBuf::from)
}

// --- Scratch / state directories --------------------------------------------

/// Machine-shared scratch directory for PUBLIC caches — the pricing table,
/// the status-page indicator, the today rollup, debug dumps. `/tmp` on
/// Unix (deliberately machine-global: the data is identical for every
/// user). Windows has no `/tmp`; `%TEMP%` is per-user there, which only
/// tightens the posture.
pub fn shared_tmp_dir() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from("/tmp")
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir()
    }
}

/// Per-user PRIVATE state directory for account-specific data (the oauth
/// usage cache) — see `usage.rs` for why this must never be world-shared.
///
/// Unix: `$TMPDIR` when it exists (macOS: `/var/folders/.../T`, mode 0700,
/// per-user), else `~/.cache/cc-statusline` created 0700.
/// Windows: `%LOCALAPPDATA%\cc-statusline` (the profile tree is ACL-limited
/// to the owning user), else `%TEMP%\cc-statusline`.
///
/// Returns `None` only when nothing usable exists — callers then no-op
/// rather than fall back to a shared world-writable path.
pub fn private_state_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            let p = PathBuf::from(tmp);
            if p.is_dir() {
                return Some(p);
            }
        }
        let dir = home_dir()?.join(".cache/cc-statusline");
        fs::create_dir_all(&dir).ok()?;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        Some(dir)
    }
    #[cfg(not(unix))]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join("cc-statusline");
        fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }
}

/// Create `path` fresh for writing — `create_new` (O_EXCL) so a planted
/// symlink is never followed — owner-only (0600) where the OS has a mode
/// bit. On Windows the file inherits the private directory's ACL.
pub fn create_private_new(path: &Path) -> std::io::Result<fs::File> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

// --- Processes --------------------------------------------------------------

/// Spawn `cmd` detached from this render so it outlives our exit.
///
/// Unix: `setsid()` between fork and exec — new session, no controlling
/// terminal, so the child survives the terminal closing.
/// Windows: `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` — no inherited
/// console, and a Ctrl+C aimed at CC's process group never reaches it.
pub fn spawn_detached(cmd: &mut Command) -> std::io::Result<Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: pre_exec runs between fork and exec. setsid() is
        // async-signal-safe and touches no parent-process state.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn()
}

/// The system curl at a fixed absolute path — invoked without a PATH
/// lookup for the one call that carries the OAuth token (`usage.rs`).
///
/// Unix: `/usr/bin/curl`. Windows has shipped curl in the system
/// directory since 10 1803: `%SystemRoot%\System32\curl.exe`.
pub fn system_curl() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from("/usr/bin/curl")
    }
    #[cfg(not(unix))]
    {
        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32")
            .join("curl.exe")
    }
}

// --- Time -------------------------------------------------------------------

/// UTC milliseconds of today's local-wall-clock midnight, or `None` if the
/// system clock or TZ database is unusable. DST-correct on both OSes: the
/// conversion applies the zone rules in force at that instant.
pub fn local_midnight_utc_ms() -> Option<i64> {
    imp::local_midnight_utc_ms()
}

// --- Console (Windows) ------------------------------------------------------

/// Columns of the console behind `handle`, or `None` when the handle isn't
/// a console (a pipe, a file, NUL). Windows only — Unix uses
/// `ioctl(TIOCGWINSZ)` directly in `width.rs`.
#[cfg(windows)]
pub fn console_columns(handle: std::os::windows::io::RawHandle) -> Option<u16> {
    imp::console_columns(handle)
}

// --- Unix implementation ----------------------------------------------------

#[cfg(unix)]
mod imp {
    /// `localtime_r(now)` gives today's broken-down local time. Zero the
    /// hour/minute/second fields and call `mktime` to convert back to epoch
    /// seconds — `mktime` honors the current TZ rules so the result is
    /// correct across DST boundaries.
    pub fn local_midnight_utc_ms() -> Option<i64> {
        unsafe {
            let now = libc::time(std::ptr::null_mut());
            if now < 0 {
                return None;
            }
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&now, &mut tm).is_null() {
                return None;
            }
            tm.tm_hour = 0;
            tm.tm_min = 0;
            tm.tm_sec = 0;
            tm.tm_isdst = -1;
            let midnight_secs = libc::mktime(&mut tm);
            if midnight_secs < 0 {
                return None;
            }
            Some((midnight_secs as i64) * 1000)
        }
    }
}

// --- Windows implementation -------------------------------------------------

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::os::windows::io::RawHandle;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Coord {
        x: i16,
        y: i16,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ConsoleScreenBufferInfo {
        size: Coord,
        cursor_position: Coord,
        attributes: u16,
        window: SmallRect,
        maximum_window_size: Coord,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLocalTime(out: *mut SystemTime);
        fn TzSpecificLocalTimeToSystemTime(
            time_zone: *const c_void,
            local: *const SystemTime,
            universal: *mut SystemTime,
        ) -> i32;
        fn SystemTimeToFileTime(system: *const SystemTime, file: *mut FileTime) -> i32;
        fn GetConsoleScreenBufferInfo(
            console: *mut c_void,
            info: *mut ConsoleScreenBufferInfo,
        ) -> i32;
    }

    /// FILETIME counts 100 ns ticks since 1601-01-01; this many ticks
    /// separate that epoch from 1970-01-01.
    const FILETIME_UNIX_EPOCH_DIFF: u64 = 116_444_736_000_000_000;

    /// `GetLocalTime` gives today's local wall-clock date; zero the time
    /// fields and convert that local midnight to UTC with the zone rules
    /// in force at that instant (a NULL zone selects the process's
    /// current zone), then to FILETIME for epoch arithmetic.
    pub fn local_midnight_utc_ms() -> Option<i64> {
        let mut local = SystemTime::default();
        let mut utc = SystemTime::default();
        let mut ft = FileTime::default();
        // SAFETY: each call writes only into the out-param we own; the
        // NULL time-zone pointer is documented as "current zone".
        unsafe {
            GetLocalTime(&mut local);
            local.hour = 0;
            local.minute = 0;
            local.second = 0;
            local.milliseconds = 0;
            if TzSpecificLocalTimeToSystemTime(std::ptr::null(), &local, &mut utc) == 0 {
                return None;
            }
            if SystemTimeToFileTime(&utc, &mut ft) == 0 {
                return None;
            }
        }
        let ticks = (u64::from(ft.high) << 32) | u64::from(ft.low);
        let unix_ticks = ticks.checked_sub(FILETIME_UNIX_EPOCH_DIFF)?;
        i64::try_from(unix_ticks / 10_000).ok()
    }

    pub fn console_columns(handle: RawHandle) -> Option<u16> {
        let mut info = ConsoleScreenBufferInfo::default();
        // SAFETY: read-only query into a struct we own; returns 0 (and
        // writes nothing) when the handle isn't a console.
        if unsafe { GetConsoleScreenBufferInfo(handle, &mut info) } == 0 {
            return None;
        }
        // Visible window width, not the (often much wider) scrollback
        // buffer width in `size.x`.
        let cols = i32::from(info.window.right) - i32::from(info.window.left) + 1;
        u16::try_from(cols).ok().filter(|c| *c > 0)
    }
}
