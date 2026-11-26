//! OS の数（機能フラグ `metrics-system`）：メモリー・CPU 時間・ディスクの空き。
//!
//! 依存の木にすでにある `libc`（Unix）と `windows-sys`（Windows）を直接使います。
//! 新しいクレートは足しません。**unsafe はこのファイルの中だけです。**
//!
//! | 値 | Linux | macOS | Windows |
//! |---|---|---|---|
//! | メモリー | `/proc/self/status` の `VmRSS` | `proc_pidinfo` の `pti_resident_size` | `K32GetProcessMemoryInfo` の `WorkingSetSize` |
//! | CPU 時間 | `getrusage` | `getrusage` | `GetProcessTimes` |
//! | ディスク | `statvfs` | `statvfs` | `GetDiskFreeSpaceExW` |
//!
//! ほかの OS と、フラグが無いときは全部「無し」です（ログでは鍵を書かない。SQLite では NULL）。

use std::path::Path;
use std::time::Duration;

/// 1 回分の OS の数。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Sample {
    pub(crate) rss_bytes: Option<u64>,
    /// このプロセスが使った CPU 時間（ユーザー＋カーネル）。
    pub(crate) cpu_time: Option<Duration>,
    pub(crate) disk_free_bytes: Option<u64>,
    pub(crate) disk_total_bytes: Option<u64>,
}

/// いまの数を取る。待つ処理なので `spawn_blocking` の中で呼びます。
pub(crate) fn sample(storage: &Path) -> Sample {
    let (disk_free_bytes, disk_total_bytes) = match imp::disk(storage) {
        Some((free, total)) => (Some(free), Some(total)),
        None => (None, None),
    };
    Sample {
        rss_bytes: imp::rss_bytes(),
        cpu_time: imp::cpu_time(),
        disk_free_bytes,
        disk_total_bytes,
    }
}

#[cfg(all(feature = "metrics-system", target_os = "linux"))]
mod imp {
    use std::path::Path;
    use std::time::Duration;

    pub(super) fn rss_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        parse_vm_rss(&status)
    }

    /// `VmRSS:     1234 kB` の行から、バイト数を出す。
    pub(super) fn parse_vm_rss(status: &str) -> Option<u64> {
        let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
        let mut parts = line["VmRSS:".len()..].split_whitespace();
        let value: u64 = parts.next()?.parse().ok()?;
        match parts.next() {
            Some("kB") | None => value.checked_mul(1024),
            Some(_) => None,
        }
    }

    pub(super) fn cpu_time() -> Option<Duration> {
        super::unix::cpu_time()
    }

    pub(super) fn disk(path: &Path) -> Option<(u64, u64)> {
        super::unix::disk(path)
    }
}

#[cfg(all(feature = "metrics-system", target_os = "macos"))]
mod imp {
    use std::path::Path;
    use std::time::Duration;

    pub(super) fn rss_bytes() -> Option<u64> {
        let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
        let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_taskinfo>()).ok()?;
        // SAFETY: `info` は `proc_taskinfo` の大きさの書き込める領域で、その大きさを `size` で渡している。
        // `getpid` は常に成功する。`proc_pidinfo` は書いたバイト数を返すので、下で全部書かれたかを確かめる。
        let written = unsafe {
            libc::proc_pidinfo(
                libc::getpid(),
                libc::PROC_PIDTASKINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // SAFETY: 上で `size` バイトすべてが書かれたことを確かめた。0 で埋めてもあるので、どの欄も初期化済み。
        let info = unsafe { info.assume_init() };
        Some(info.pti_resident_size)
    }

    pub(super) fn cpu_time() -> Option<Duration> {
        super::unix::cpu_time()
    }

    pub(super) fn disk(path: &Path) -> Option<(u64, u64)> {
        super::unix::disk(path)
    }
}

#[cfg(all(
    feature = "metrics-system",
    any(target_os = "linux", target_os = "macos")
))]
mod unix {
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;
    use std::time::Duration;

    pub(super) fn cpu_time() -> Option<Duration> {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `usage` は `rusage` の大きさの書き込める領域。`RUSAGE_SELF` は常に正しい引数。
        let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        if result != 0 {
            return None;
        }
        // SAFETY: `getrusage` が 0（成功）を返したので、`usage` は書かれている。0 で埋めてもある。
        let usage = unsafe { usage.assume_init() };
        Some(timeval(usage.ru_utime)? + timeval(usage.ru_stime)?)
    }

    fn timeval(value: libc::timeval) -> Option<Duration> {
        let seconds = u64::try_from(value.tv_sec).ok()?;
        let micros = u64::try_from(value.tv_usec).ok()?;
        Some(Duration::from_secs(seconds) + Duration::from_micros(micros))
    }

    /// `(呼び出し側が使える空き, 全体)` のバイト数。
    pub(super) fn disk(path: &Path) -> Option<(u64, u64)> {
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
        // SAFETY: `path` は NUL で終わる文字列で、呼び出しの間は生きている。
        // `stats` は `statvfs` の大きさの書き込める領域。
        let result = unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) };
        if result != 0 {
            return None;
        }
        // SAFETY: `statvfs` が 0（成功）を返したので、`stats` は書かれている。0 で埋めてもある。
        let stats = unsafe { stats.assume_init() };
        #[allow(clippy::unnecessary_cast)] // 型の幅は OS で違う
        let fragment = stats.f_frsize as u64;
        #[allow(clippy::unnecessary_cast)]
        let free = (stats.f_bavail as u64).checked_mul(fragment)?;
        #[allow(clippy::unnecessary_cast)]
        let total = (stats.f_blocks as u64).checked_mul(fragment)?;
        Some((free, total))
    }
}

#[cfg(all(feature = "metrics-system", windows))]
mod imp {
    use std::os::windows::ffi::OsStrExt as _;
    use std::path::Path;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    pub(super) fn rss_bytes() -> Option<u64> {
        let mut counters = std::mem::MaybeUninit::<PROCESS_MEMORY_COUNTERS>::zeroed();
        let size = u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()).ok()?;
        // SAFETY: `GetCurrentProcess` は閉じなくてよい疑似ハンドルを返す。`counters` は
        // `PROCESS_MEMORY_COUNTERS` の大きさの書き込める領域で、その大きさを `size` で渡している。
        let ok =
            unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), counters.as_mut_ptr(), size) };
        if ok == 0 {
            return None;
        }
        // SAFETY: 成功（0 以外）を返したので、`counters` は書かれている。0 で埋めてもある。
        let counters = unsafe { counters.assume_init() };
        u64::try_from(counters.WorkingSetSize).ok()
    }

    pub(super) fn cpu_time() -> Option<Duration> {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut creation, mut exit, mut kernel, mut user) = (zero, zero, zero, zero);
        // SAFETY: 疑似ハンドルと、書き込める 4 つの `FILETIME` を渡している。
        let ok = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        };
        if ok == 0 {
            return None;
        }
        // 100 ns 単位。
        let ticks = filetime(kernel).checked_add(filetime(user))?;
        Some(Duration::from_nanos(ticks.checked_mul(100)?))
    }

    fn filetime(value: FILETIME) -> u64 {
        (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
    }

    /// `(呼び出し側が使える空き, 全体)` のバイト数。
    pub(super) fn disk(path: &Path) -> Option<(u64, u64)> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
        // SAFETY: `wide` は NUL で終わる UTF-16 で、呼び出しの間は生きている。3 つの出力は書き込める `u64`。
        let ok =
            unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free) };
        if ok == 0 {
            return None;
        }
        Some((available, total))
    }
}

#[cfg(not(all(
    feature = "metrics-system",
    any(target_os = "linux", target_os = "macos", windows)
)))]
mod imp {
    use std::path::Path;
    use std::time::Duration;

    pub(super) fn rss_bytes() -> Option<u64> {
        None
    }

    pub(super) fn cpu_time() -> Option<Duration> {
        None
    }

    pub(super) fn disk(_path: &Path) -> Option<(u64, u64)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(
        feature = "metrics-system",
        any(target_os = "linux", target_os = "macos", windows)
    ))]
    #[test]
    fn 対応しているosでは数が取れる() {
        let dir = std::env::temp_dir();
        let first = sample(&dir);
        assert!(first.rss_bytes.is_some_and(|rss| rss > 0), "{first:?}");
        let total = first.disk_total_bytes.expect("全体の大きさ");
        let free = first.disk_free_bytes.expect("空き");
        assert!(total > 0, "{first:?}");
        assert!(free <= total, "{first:?}");

        // 重い計算の後で、CPU 時間が増える。
        let before = first.cpu_time.expect("CPU 時間");
        let started = std::time::Instant::now();
        let mut x: u64 = 1;
        while started.elapsed() < Duration::from_millis(300) {
            for i in 0..100_000u64 {
                x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(i));
            }
        }
        std::hint::black_box(x);
        let after = sample(&dir).cpu_time.expect("CPU 時間");
        assert!(after > before, "{before:?} → {after:?}");
    }

    #[cfg(not(all(
        feature = "metrics-system",
        any(target_os = "linux", target_os = "macos", windows)
    )))]
    #[test]
    fn 対応していなければ全部なし() {
        assert_eq!(sample(&std::env::temp_dir()), Sample::default());
    }

    #[cfg(all(feature = "metrics-system", target_os = "linux"))]
    #[test]
    fn vmrssの行を読む() {
        let status = "Name:\tx\nVmPeak:\t  2000 kB\nVmRSS:\t   1234 kB\nThreads:\t3\n";
        assert_eq!(imp::parse_vm_rss(status), Some(1234 * 1024));
        assert_eq!(imp::parse_vm_rss("Name:\tx\n"), None);
        assert_eq!(imp::parse_vm_rss("VmRSS:\tabc kB\n"), None);
    }
}
