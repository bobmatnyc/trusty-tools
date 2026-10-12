//! Host sensors: load, data-disk use, swap and memory, and the alerts on them.
//!
//! Why: a saturated host stalls every PM at once, and the builder band decides
//! how many Rust builders may run (#9171 A3a).
//! What: [`HostProbe`] is the seam over `os.getloadavg`, `os.cpu_count`,
//! `shutil.disk_usage`, `swap_used_mb` and `mem_free_pct`; [`SystemHostProbe`]
//! reads the real host through libc and bounded subprocesses. [`host_alerts`]
//! is the prototype's `host_alerts`: `load` when the 15-minute load is over
//! `LOAD_FACTOR` x cores, `disk` over [`DISK_HIGH`]% used, `swap` when any swap
//! is in use, and the band alert when a band is stored.
//! Test: `test_a_sustained_5min_average_above_1_5x_cores_alerts`,
//! `test_high_15m_load_stops_and_names_load15`, `host_alerts_match_python_goldens`.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::band::band_alerts;
use super::config::Config;
use super::pyfmt::{py_float, py_float_repr};
use crate::classify::{Alert, alert};
use crate::process::run_bounded;
use crate::pystr::is_digit;

/// Data-disk percent used that raises `disk`.
pub const DISK_HIGH: f64 = 90.0;
/// macOS: `/` is the sealed system volume; user data lives here.
const DATA_VOLUME: &str = "/System/Volumes/Data";

/// `shutil.disk_usage`: bytes total, used and free to an unprivileged user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskUsage {
    /// Volume size.
    pub total: u64,
    /// Bytes in use.
    pub used: u64,
    /// Bytes available.
    pub free: u64,
}

/// What [`host_alerts`] reads from the host.
pub trait HostProbe {
    /// 1-, 5- and 15-minute load averages.
    fn loadavg(&self) -> io::Result<[f64; 3]>;
    /// Logical CPUs (`os.cpu_count() or 1`).
    fn cores(&self) -> usize;
    /// The volume that holds user data.
    fn data_volume(&self) -> PathBuf;
    /// Usage of the volume at `path`.
    fn disk_usage(&self, path: &Path) -> io::Result<DiskUsage>;
    /// Swap in use in MB; `Ok(None)` when unknown.
    fn swap_used_mb(&self) -> io::Result<Option<f64>>;
    /// System-wide memory free percent, when the host reports one.
    fn mem_free_pct(&self) -> Option<f64>;
}

/// Alerts on host load, disk and swap; with `band` set, the band alert too.
///
/// A swap read that fails counts as unknown (no `swap` alert, no swap input to
/// the band), as in the prototype. With no usable `LOAD_FACTOR`
/// ([`Config::load_factor`] is `None`) it returns no alerts and reads nothing.
///
/// # Errors
/// The load-average or disk read failing (the prototype fails the cycle).
pub fn host_alerts(
    probe: &dyn HostProbe,
    cfg: &Config,
    now: f64,
    band: Option<&mut Option<String>>,
) -> io::Result<Vec<Alert>> {
    // #9171 A3b-2 ruling Q4: a bad LOAD_FACTOR turns off every host alert,
    // the band included; the prototype aborts the whole cycle instead.
    let Some(factor) = cfg.load_factor else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let [load1, load5, load15] = probe.loadavg()?;
    let cores = probe.cores().max(1);
    if load15 > factor * cores as f64 {
        let detail = format!(
            "load {load1:.1}/{load5:.1}/{load15:.1} (15m > {}x{cores} cores)",
            py_float_repr(factor)
        );
        out.push(alert("load", "load", &detail, "", "", now));
    }
    let du = probe.disk_usage(&probe.data_volume())?;
    if du.total > 0 {
        let pct = (100 * u128::from(du.used)) as f64 / du.total as f64;
        if pct > DISK_HIGH {
            let detail = format!("data disk {pct:.0}% used");
            out.push(alert("disk", "disk", &detail, "", "", now));
        }
    }
    let swap = probe.swap_used_mb().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "swap read failed; treated as unknown");
        None
    });
    if let Some(mb) = swap.filter(|mb| *mb != 0.0) {
        let detail = format!("swap in use: {mb:.0} MB");
        out.push(alert("swap", "swap", &detail, "", "", now));
    }
    if let Some(stored) = band {
        let free_gib = du.free as f64 / f64::from(1u32 << 30);
        let mem = probe.mem_free_pct();
        out.extend(band_alerts(
            &cfg.band, stored, now, load15, cores, free_gib, mem, swap,
        ));
    }
    Ok(out)
}

/// The real host: libc for load, CPUs and disk; `sysctl` / `/proc/meminfo`
/// for swap; macOS `memory_pressure` for free memory.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemHostProbe;

/// The value after the first `marker` whose digit run ends at `end`:
/// `re.search(marker + r"([\d.]+)" + end)` for the shapes below.
fn number_after(text: &str, marker: &str, allow_dot: bool) -> Option<(String, char)> {
    text.match_indices(marker).find_map(|(i, m)| {
        let rest = &text[i + m.len()..];
        let n: String = rest
            .chars()
            .take_while(|c| is_digit(*c) || (allow_dot && *c == '.'))
            .collect();
        let next = rest[n.len()..].chars().next()?;
        (!n.is_empty()).then_some((n, next))
    })
}

/// `swap_used_mb` on macOS: `sysctl -n vm.swapusage`'s `used = <n><K|M|G>`.
pub(crate) fn parse_swapusage(out: &str) -> Result<Option<f64>, io::Error> {
    let found = out.match_indices("used = ").find_map(|(i, m)| {
        let rest = &out[i + m.len()..];
        let n: String = rest
            .chars()
            .take_while(|c| is_digit(*c) || *c == '.')
            .collect();
        let unit = rest[n.len()..].chars().next()?;
        (!n.is_empty() && matches!(unit, 'K' | 'M' | 'G')).then_some((n, unit))
    });
    let Some((n, unit)) = found else {
        return Ok(None);
    };
    let v = py_float(&n).ok_or_else(|| io::Error::other(format!("bad swap figure {n:?}")))?;
    let scale = match unit {
        'K' => 1.0 / 1024.0,
        'M' => 1.0,
        _ => 1024.0,
    };
    Ok(Some(v * scale))
}

/// `mem_free_pct`: `System-wide memory free percentage: <n>%`.
pub(crate) fn parse_memory_pressure(out: &str) -> Option<f64> {
    let marker = "System-wide memory free percentage:";
    out.match_indices(marker).find_map(|(i, m)| {
        let rest = out[i + m.len()..].trim_start_matches(crate::pystr::is_space);
        let (n, next) = number_after(rest, "", false)?;
        (next == '%').then(|| py_float(&n)).flatten()
    })
}

fn run_text(program: &str, args: &[&str], timeout: Duration) -> io::Result<String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    let done =
        run_bounded(cmd, timeout).map_err(|f| io::Error::other(format!("{program}: {f:?}")))?;
    Ok(String::from_utf8_lossy(&done.stdout).into_owned())
}

impl HostProbe for SystemHostProbe {
    fn loadavg(&self) -> io::Result<[f64; 3]> {
        let mut l = [0.0f64; 3];
        // SAFETY: `l` holds three doubles, the count passed.
        let n = unsafe { libc::getloadavg(l.as_mut_ptr(), 3) };
        if n == 3 {
            Ok(l)
        } else {
            Err(io::Error::other("getloadavg failed"))
        }
    }

    fn cores(&self) -> usize {
        // SAFETY: sysconf reads a system constant and has no preconditions.
        let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
        usize::try_from(n).ok().filter(|n| *n > 0).unwrap_or(1)
    }

    fn data_volume(&self) -> PathBuf {
        let data = Path::new(DATA_VOLUME);
        PathBuf::from(if data.is_dir() { DATA_VOLUME } else { "/" })
    }

    fn disk_usage(&self, path: &Path) -> io::Result<DiskUsage> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: an all-zero statvfs is a valid out-parameter.
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is NUL-terminated and `st` is a valid out-parameter.
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            return Err(io::Error::last_os_error());
        }
        #[allow(clippy::unnecessary_cast)] // field widths differ by platform
        let (frsize, blocks, bfree, bavail) = (
            st.f_frsize as u64,
            st.f_blocks as u64,
            st.f_bfree as u64,
            st.f_bavail as u64,
        );
        Ok(DiskUsage {
            total: blocks * frsize,
            used: (blocks - bfree) * frsize,
            free: bavail * frsize,
        })
    }

    fn swap_used_mb(&self) -> io::Result<Option<f64>> {
        if cfg!(target_os = "macos") {
            let out = run_text("sysctl", &["-n", "vm.swapusage"], Duration::from_secs(5))?;
            return parse_swapusage(&out);
        }
        let text = std::fs::read_to_string("/proc/meminfo")?;
        let field = |name: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(name)?.strip_prefix(':'))
                .and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok())
                .unwrap_or(0.0)
        };
        Ok(Some((field("SwapTotal") - field("SwapFree")) / 1024.0))
    }

    fn mem_free_pct(&self) -> Option<f64> {
        run_text("memory_pressure", &[], Duration::from_secs(10))
            .ok()
            .and_then(|out| parse_memory_pressure(&out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_text_parsers_match_the_prototype_patterns() -> io::Result<()> {
        let sw = "total = 2048.00M  used = 1.50G  free = 546.00M  (encrypted)";
        assert_eq!(parse_swapusage(sw)?, Some(1536.0));
        assert_eq!(parse_swapusage("used = 512.00K")?, Some(0.5));
        assert_eq!(parse_swapusage("total = 0.00M  used = 0.00M")?, Some(0.0));
        assert_eq!(parse_swapusage("nothing here")?, None);
        assert!(parse_swapusage("used = 1.2.3M").is_err());
        let mp =
            "The system has 17179869184 (1048576 pages)\nSystem-wide memory free percentage: 63%\n";
        assert_eq!(parse_memory_pressure(mp), Some(63.0));
        assert_eq!(
            parse_memory_pressure("System-wide memory free percentage: n/a"),
            None
        );
        Ok(())
    }

    /// The live probe runs: load and CPUs are readable on any Unix host.
    #[test]
    fn system_probe_reads_load_and_cores() -> io::Result<()> {
        let p = SystemHostProbe;
        assert!(p.loadavg()?.iter().all(|l| *l >= 0.0));
        assert!(p.cores() >= 1);
        let du = p.disk_usage(&p.data_volume())?;
        assert!(du.total >= du.used && du.total > 0);
        Ok(())
    }
}
