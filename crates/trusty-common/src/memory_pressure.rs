//! The host's memory PRESSURE, read as the kernel reports it (#8261).
//!
//! Why: builder admission used a free-megabytes floor over `sysinfo`'s
//! `available_memory()`. That figure is an estimate the kernel does not act on:
//! on macOS it read ~71 GB "available" on a 128 GB host while the compressor and
//! swap were the real story, and a fixed floor in megabytes means something
//! different on a 16 GB laptop and a 128 GB workstation. Both kernels already
//! publish a pressure verdict — the one they use to decide when to start
//! killing processes — so admission reads that verdict instead of re-deriving
//! it from a byte count.
//!
//! What: [`read_memory_pressure`] returns a [`MemoryPressure`]: a
//! [`PressureLevel`] (normal / warn / critical), the available-memory
//! percentage where the platform has one, the source it came from, and every
//! raw signal as a `name=value` pair so a refusal can quote exactly what was
//! read. Sources, in order:
//!
//! - macOS: `kern.memorystatus_vm_pressure_level` (1 normal, 2 warn,
//!   4 critical) and `kern.memorystatus_level` (percent available), both via
//!   `sysctlbyname(3)`, which needs no privileges.
//! - Linux inside a container: the cgroup v2 `memory.pressure` file of this
//!   process's own cgroup.
//! - Linux: PSI `/proc/pressure/memory` (`some`/`full` `avg10`).
//! - Linux without PSI: `MemAvailable / MemTotal` from `/proc/meminfo`.
//!
//! Nothing here substitutes a guessed level for a reading it could not take:
//! every failure is a [`MemoryPressureError`], and deciding what an unreadable
//! signal means is the caller's policy, not this reader's.
//! Test: the `#[cfg(test)]` suite below; the live read is
//! `live_pressure_reads_on_this_host`.

use std::fmt;
use std::path::{Path, PathBuf};

/// The kernel's memory-pressure verdict, ordered from least to most severe.
///
/// Why: admission compares a reading against a configured maximum, so the
/// levels must be ordered, and the three names are the ones both kernels'
/// documentation use.
/// What: `Normal < Warn < Critical`; [`Self::name`] is the lowercase word a
/// config file and a refusal message both use.
/// Test: `levels_are_ordered_and_parse_by_name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum PressureLevel {
    /// The kernel sees no memory pressure.
    Normal,
    /// The kernel is reclaiming and compressing; new work should wait.
    Warn,
    /// The kernel is about to kill processes to free memory.
    Critical,
}

impl PressureLevel {
    /// The lowercase name: `normal`, `warn` or `critical`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }

    /// Parse a level from its [`Self::name`], case-insensitively.
    ///
    /// Test: `levels_are_ordered_and_parse_by_name`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "normal" => Some(Self::Normal),
            "warn" | "warning" => Some(Self::Warn),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }

    /// Map macOS `kern.memorystatus_vm_pressure_level` to a level.
    ///
    /// Why: the sysctl's values are the kernel's `kVMPressure*` constants —
    /// 1 normal, 2 warn, 4 critical — and any other value is not a reading
    /// this reader understands, so it is `None` rather than a guess.
    /// Test: `macos_levels_map_from_the_kernel_constants`.
    #[must_use]
    pub fn from_macos_sysctl(raw: i64) -> Option<Self> {
        match raw {
            1 => Some(Self::Normal),
            2 => Some(Self::Warn),
            4 => Some(Self::Critical),
            _ => None,
        }
    }
}

impl fmt::Display for PressureLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Where a [`MemoryPressure`] reading came from.
///
/// Test: `a_psi_file_classifies_by_avg10`, `live_pressure_reads_on_this_host`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PressureSource {
    /// macOS `kern.memorystatus_*` sysctls.
    MacosSysctl,
    /// Linux host-wide PSI, `/proc/pressure/memory`.
    LinuxPsi(PathBuf),
    /// Linux cgroup v2 PSI for this process's own cgroup (a container).
    LinuxCgroupPsi(PathBuf),
    /// Linux `/proc/meminfo`, used when no PSI file is readable.
    LinuxMeminfo(PathBuf),
}

impl fmt::Display for PressureSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MacosSysctl => f.write_str("macOS memorystatus sysctls"),
            Self::LinuxPsi(p) => write!(f, "PSI {}", p.display()),
            Self::LinuxCgroupPsi(p) => write!(f, "cgroup PSI {}", p.display()),
            Self::LinuxMeminfo(p) => write!(f, "{} (no PSI)", p.display()),
        }
    }
}

/// Thresholds for the Linux sources, which report numbers rather than a level.
///
/// Why: macOS hands back a level; PSI and `/proc/meminfo` hand back stall
/// percentages and byte counts, so a level has to be derived, and the cut
/// points are policy the caller may configure.
/// What: `psi_warn_some_avg10` — `some avg10` at or above this is warn;
/// `psi_critical_full_avg10` — `full avg10` at or above this is critical;
/// `min_available_pct` — for `/proc/meminfo`, available below this is warn and
/// below half of it is critical. The defaults are initial values to revisit
/// from live data, not measurements.
/// Test: `a_psi_file_classifies_by_avg10`, `meminfo_classifies_by_available_pct`.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct PressureThresholds {
    /// `some avg10` percentage at or above which the level is warn.
    pub psi_warn_some_avg10: f64,
    /// `full avg10` percentage at or above which the level is critical.
    pub psi_critical_full_avg10: f64,
    /// Available-memory percentage below which `/proc/meminfo` reads warn.
    pub min_available_pct: f64,
}

impl PressureThresholds {
    /// The defaults with `min_available_pct` replaced.
    ///
    /// Test: `meminfo_classifies_by_available_pct`.
    #[must_use]
    pub fn with_min_available_pct(mut self, pct: f64) -> Self {
        self.min_available_pct = pct;
        self
    }
}

impl Default for PressureThresholds {
    fn default() -> Self {
        Self {
            psi_warn_some_avg10: 10.0,
            psi_critical_full_avg10: 5.0,
            min_available_pct: 10.0,
        }
    }
}

/// One raw signal as read, for quoting in a refusal.
///
/// Test: `render_signals_quotes_every_signal`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PressureSignal {
    /// The signal's own name, e.g. `kern.memorystatus_vm_pressure_level`.
    pub name: String,
    /// Its value as rendered, e.g. `2 (warn)` or `18%`.
    pub value: String,
}

impl PressureSignal {
    /// A signal from its name and rendered value.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// One memory-pressure reading.
///
/// Why: the level decides admission, and the raw signals are what an operator
/// needs to see to believe it — so they travel together.
/// What: `level`, `available_pct` when the source has one, the `source`, and
/// the raw `signals` in the order read.
/// Test: `render_signals_quotes_every_signal`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MemoryPressure {
    /// The derived or reported level.
    pub level: PressureLevel,
    /// Percentage of memory available, when the platform reports one.
    pub available_pct: Option<f64>,
    /// Where the reading came from.
    pub source: PressureSource,
    /// Every raw signal read, as `name=value`.
    pub signals: Vec<PressureSignal>,
}

impl MemoryPressure {
    /// A reading from its parts; for callers outside this crate, which cannot
    /// use the struct literal.
    ///
    /// Test: `render_signals_quotes_every_signal`.
    #[must_use]
    pub fn new(
        level: PressureLevel,
        available_pct: Option<f64>,
        source: PressureSource,
        signals: Vec<PressureSignal>,
    ) -> Self {
        Self {
            level,
            available_pct,
            source,
            signals,
        }
    }

    /// `kern.memorystatus_vm_pressure_level=2 (warn), kern.memorystatus_level=18%`.
    ///
    /// Test: `render_signals_quotes_every_signal`.
    #[must_use]
    pub fn render_signals(&self) -> String {
        self.signals
            .iter()
            .map(|s| format!("{}={}", s.name, s.value))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Why a pressure reading could not be taken.
///
/// Test: `a_missing_psi_file_is_a_read_error`, `a_garbled_psi_file_is_a_parse_error`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MemoryPressureError {
    /// This platform has no pressure source this reader knows.
    #[error("no memory-pressure source on this platform ({0})")]
    Unsupported(&'static str),
    /// A sysctl or file read failed.
    #[error("could not read {what}: {source}")]
    Read {
        /// The sysctl name or file path.
        what: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// A file was read but its contents are not the expected shape.
    #[error("could not parse {what}: {detail}")]
    Parse {
        /// The file path.
        what: String,
        /// What was wrong with it.
        detail: String,
    },
}

impl MemoryPressureError {
    /// The OS errno, when the failure carried one.
    #[must_use]
    pub fn errno(&self) -> Option<i32> {
        match self {
            Self::Read { source, .. } => source.raw_os_error(),
            _ => None,
        }
    }
}

/// This host's memory pressure right now.
///
/// Why: the one entry point, so admission and `tm doctor` read the same thing.
/// What: macOS reads the two `kern.memorystatus_*` sysctls; Linux reads
/// [`read_linux_pressure`] against `/`; every other platform is
/// [`MemoryPressureError::Unsupported`].
///
/// # Errors
///
/// [`MemoryPressureError`] when no source could be read.
///
/// Test: `live_pressure_reads_on_this_host`.
pub fn read_memory_pressure(
    thresholds: &PressureThresholds,
) -> Result<MemoryPressure, MemoryPressureError> {
    #[cfg(target_os = "macos")]
    {
        let _ = thresholds;
        read_macos_pressure()
    }
    #[cfg(target_os = "linux")]
    {
        read_linux_pressure(Path::new("/"), in_container(Path::new("/")), thresholds)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = thresholds;
        Err(MemoryPressureError::Unsupported(std::env::consts::OS))
    }
}

/// Read one integer sysctl by name (macOS).
#[cfg(target_os = "macos")]
fn sysctl_i64(name: &str) -> Result<i64, MemoryPressureError> {
    let read_err = |source| MemoryPressureError::Read {
        what: name.to_string(),
        source,
    };
    let cname = std::ffi::CString::new(name)
        .map_err(|e| read_err(std::io::Error::new(std::io::ErrorKind::InvalidInput, e)))?;
    let mut buf = [0u8; 8];
    let mut len: libc::size_t = buf.len();
    // SAFETY: `cname` is a valid NUL-terminated string, `buf` is a writable
    // buffer of `len` bytes, and no new value is being set.
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            buf.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(read_err(std::io::Error::last_os_error()));
    }
    match len {
        4 => Ok(i64::from(i32::from_ne_bytes([
            buf[0], buf[1], buf[2], buf[3],
        ]))),
        8 => Ok(i64::from_ne_bytes(buf)),
        other => Err(read_err(std::io::Error::other(format!(
            "unexpected {other}-byte value"
        )))),
    }
}

/// The macOS reading: the kernel's level plus its available percentage.
#[cfg(target_os = "macos")]
fn read_macos_pressure() -> Result<MemoryPressure, MemoryPressureError> {
    const LEVEL: &str = "kern.memorystatus_vm_pressure_level";
    const AVAILABLE: &str = "kern.memorystatus_level";
    let raw = sysctl_i64(LEVEL)?;
    let level =
        PressureLevel::from_macos_sysctl(raw).ok_or_else(|| MemoryPressureError::Parse {
            what: LEVEL.to_string(),
            detail: format!("{raw} is not one of 1, 2, 4"),
        })?;
    let mut signals = vec![PressureSignal::new(LEVEL, format!("{raw} ({level})"))];
    // The percentage is a second opinion, not the verdict: a failure to read
    // it leaves the level standing rather than discarding it.
    let available_pct = match sysctl_i64(AVAILABLE) {
        Ok(pct) => {
            signals.push(PressureSignal::new(AVAILABLE, format!("{pct}%")));
            #[allow(clippy::cast_precision_loss)]
            Some(pct as f64)
        }
        Err(err) => {
            signals.push(PressureSignal::new(
                AVAILABLE,
                format!("unreadable ({err})"),
            ));
            None
        }
    };
    Ok(MemoryPressure {
        level,
        available_pct,
        source: PressureSource::MacosSysctl,
        signals,
    })
}

/// Whether this process looks like it runs inside a container.
///
/// Why: inside a container the host-wide `/proc/pressure/memory` describes a
/// machine whose limit is not this process's limit; the cgroup's own file is
/// the one that predicts an OOM kill here.
/// What: `/.dockerenv` or `/run/.containerenv` under `root`, or a non-empty
/// `container` environment variable (set by systemd-nspawn and podman).
/// Test: `a_container_prefers_the_cgroup_file`.
#[must_use]
pub fn in_container(root: &Path) -> bool {
    root.join(".dockerenv").exists()
        || root.join("run/.containerenv").exists()
        || std::env::var_os("container").is_some_and(|v| !v.is_empty())
}

/// The Linux reading, against a filesystem root (`/` in production).
///
/// Why: `root` is a parameter so every branch is testable against a temp
/// directory holding fake `/proc` and `/sys` files.
/// What: inside a container, the cgroup v2 `memory.pressure` of the cgroup
/// named in `proc/self/cgroup` is tried first; then `proc/pressure/memory`;
/// then `proc/meminfo`. The first readable source wins. The available
/// percentage is read from `proc/meminfo` alongside a PSI reading, best effort.
///
/// # Errors
///
/// The LAST source's error, when none of them could be read.
///
/// Test: `a_psi_file_classifies_by_avg10`, `a_container_prefers_the_cgroup_file`,
/// `without_psi_meminfo_is_the_fallback`, `a_missing_psi_file_is_a_read_error`.
pub fn read_linux_pressure(
    root: &Path,
    container: bool,
    thresholds: &PressureThresholds,
) -> Result<MemoryPressure, MemoryPressureError> {
    let meminfo = root.join("proc/meminfo");
    let available = read_meminfo_pct(&meminfo).ok();
    let mut psi_paths = Vec::new();
    if container && let Some(cgroup) = own_cgroup_pressure_path(root) {
        psi_paths.push((cgroup.clone(), PressureSource::LinuxCgroupPsi(cgroup)));
    }
    let host_psi = root.join("proc/pressure/memory");
    psi_paths.push((host_psi.clone(), PressureSource::LinuxPsi(host_psi)));
    for (path, source) in psi_paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (some, full) = parse_psi(&text).map_err(|detail| MemoryPressureError::Parse {
            what: path.display().to_string(),
            detail,
        })?;
        let level = classify_psi(some, full, thresholds);
        let mut signals = vec![
            PressureSignal::new(
                format!("{} some avg10", path.display()),
                format!("{some:.2}"),
            ),
            PressureSignal::new(
                format!("{} full avg10", path.display()),
                format!("{full:.2}"),
            ),
        ];
        if let Some(pct) = available {
            signals.push(PressureSignal::new(
                "MemAvailable/MemTotal",
                format!("{pct:.0}%"),
            ));
        }
        return Ok(MemoryPressure {
            level,
            available_pct: available,
            source,
            signals,
        });
    }
    let pct = read_meminfo_pct(&meminfo)?;
    Ok(MemoryPressure {
        level: classify_available_pct(pct, thresholds),
        available_pct: Some(pct),
        source: PressureSource::LinuxMeminfo(meminfo),
        signals: vec![PressureSignal::new(
            "MemAvailable/MemTotal",
            format!("{pct:.0}%"),
        )],
    })
}

/// The cgroup v2 `memory.pressure` file for this process's own cgroup.
///
/// What: reads the `0::<path>` line of `proc/self/cgroup` under `root` and
/// joins it onto `sys/fs/cgroup`. `None` for a cgroup v1 host or a read error.
fn own_cgroup_pressure_path(root: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(root.join("proc/self/cgroup")).ok()?;
    let rel = text.lines().find_map(|l| l.strip_prefix("0::"))?.trim();
    Some(
        root.join("sys/fs/cgroup")
            .join(rel.trim_start_matches('/'))
            .join("memory.pressure"),
    )
}

/// Parse a PSI file into `(some avg10, full avg10)`.
///
/// Why: the format is `some avg10=0.00 avg60=0.00 avg300=0.00 total=0` then a
/// `full` line; older kernels omit `full`, which reads as `0.0` there.
///
/// # Errors
///
/// A description of what was missing when there is no `some avg10`.
///
/// Test: `a_psi_file_classifies_by_avg10`, `a_garbled_psi_file_is_a_parse_error`.
pub fn parse_psi(text: &str) -> Result<(f64, f64), String> {
    let avg10 = |kind: &str| {
        text.lines()
            .find(|l| l.split_whitespace().next() == Some(kind))
            .and_then(|l| {
                l.split_whitespace()
                    .find_map(|kv| kv.strip_prefix("avg10="))
                    .and_then(|v| v.parse::<f64>().ok())
            })
    };
    let some = avg10("some").ok_or_else(|| "no `some avg10=` field".to_string())?;
    Ok((some, avg10("full").unwrap_or(0.0)))
}

/// The level a PSI reading maps to under `thresholds`.
///
/// Test: `a_psi_file_classifies_by_avg10`.
#[must_use]
pub fn classify_psi(
    some_avg10: f64,
    full_avg10: f64,
    thresholds: &PressureThresholds,
) -> PressureLevel {
    if full_avg10 >= thresholds.psi_critical_full_avg10 {
        PressureLevel::Critical
    } else if some_avg10 >= thresholds.psi_warn_some_avg10 {
        PressureLevel::Warn
    } else {
        PressureLevel::Normal
    }
}

/// The level an available-memory percentage maps to under `thresholds`.
///
/// What: below `min_available_pct` is warn, below half of it critical.
/// Test: `meminfo_classifies_by_available_pct`.
#[must_use]
pub fn classify_available_pct(pct: f64, thresholds: &PressureThresholds) -> PressureLevel {
    if pct < thresholds.min_available_pct / 2.0 {
        PressureLevel::Critical
    } else if pct < thresholds.min_available_pct {
        PressureLevel::Warn
    } else {
        PressureLevel::Normal
    }
}

/// `MemAvailable / MemTotal` as a percentage, from a `meminfo` file.
///
/// # Errors
///
/// A read error, or a parse error naming the missing field.
///
/// Test: `meminfo_classifies_by_available_pct`.
fn read_meminfo_pct(path: &Path) -> Result<f64, MemoryPressureError> {
    let text = std::fs::read_to_string(path).map_err(|source| MemoryPressureError::Read {
        what: path.display().to_string(),
        source,
    })?;
    parse_meminfo_pct(&text).map_err(|detail| MemoryPressureError::Parse {
        what: path.display().to_string(),
        detail,
    })
}

/// `MemAvailable / MemTotal * 100` from `/proc/meminfo` text.
///
/// # Errors
///
/// A description of the missing or zero field.
///
/// Test: `meminfo_classifies_by_available_pct`.
pub fn parse_meminfo_pct(text: &str) -> Result<f64, String> {
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
    };
    let total = field("MemTotal:").ok_or("no MemTotal field")?;
    let available = field("MemAvailable:").ok_or("no MemAvailable field")?;
    if total == 0 {
        return Err("MemTotal is 0".to_string());
    }
    #[allow(clippy::cast_precision_loss)]
    Ok(available as f64 * 100.0 / total as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_ordered_and_parse_by_name() {
        assert!(PressureLevel::Normal < PressureLevel::Warn);
        assert!(PressureLevel::Warn < PressureLevel::Critical);
        for level in [
            PressureLevel::Normal,
            PressureLevel::Warn,
            PressureLevel::Critical,
        ] {
            assert_eq!(PressureLevel::parse(level.name()), Some(level));
        }
        assert_eq!(PressureLevel::parse(" WARNING "), Some(PressureLevel::Warn));
        assert_eq!(PressureLevel::parse("high"), None);
    }

    #[test]
    fn macos_levels_map_from_the_kernel_constants() {
        assert_eq!(
            PressureLevel::from_macos_sysctl(1),
            Some(PressureLevel::Normal)
        );
        assert_eq!(
            PressureLevel::from_macos_sysctl(2),
            Some(PressureLevel::Warn)
        );
        assert_eq!(
            PressureLevel::from_macos_sysctl(4),
            Some(PressureLevel::Critical)
        );
        assert_eq!(
            PressureLevel::from_macos_sysctl(3),
            None,
            "no kernel constant is 3"
        );
    }

    #[test]
    fn render_signals_quotes_every_signal() {
        let reading = MemoryPressure::new(
            PressureLevel::Warn,
            Some(18.0),
            PressureSource::MacosSysctl,
            vec![
                PressureSignal::new("kern.memorystatus_vm_pressure_level", "2 (warn)"),
                PressureSignal::new("kern.memorystatus_level", "18%"),
            ],
        );
        assert_eq!(
            reading.render_signals(),
            "kern.memorystatus_vm_pressure_level=2 (warn), kern.memorystatus_level=18%"
        );
    }

    /// A fake filesystem root with the given files.
    fn fake_root(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for (rel, body) in files {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, body).expect("write");
        }
        dir
    }

    const MEMINFO: &str =
        "MemTotal:       16000000 kB\nMemFree: 1 kB\nMemAvailable:    8000000 kB\n";

    #[test]
    fn a_psi_file_classifies_by_avg10() {
        let t = PressureThresholds::default();
        for (psi, want) in [
            (
                "some avg10=0.50 avg60=0 avg300=0 total=1\nfull avg10=0.00 avg60=0 avg300=0 total=0\n",
                PressureLevel::Normal,
            ),
            (
                "some avg10=12.00 avg60=0 avg300=0 total=1\nfull avg10=1.00 avg60=0 avg300=0 total=0\n",
                PressureLevel::Warn,
            ),
            (
                "some avg10=40.00 avg60=0 avg300=0 total=1\nfull avg10=9.00 avg60=0 avg300=0 total=0\n",
                PressureLevel::Critical,
            ),
        ] {
            let root = fake_root(&[("proc/pressure/memory", psi), ("proc/meminfo", MEMINFO)]);
            let got = read_linux_pressure(root.path(), false, &t).expect("readable");
            assert_eq!(got.level, want, "{psi}");
            assert!(matches!(got.source, PressureSource::LinuxPsi(_)));
            assert_eq!(got.available_pct.map(f64::round), Some(50.0));
            assert!(
                got.render_signals().contains("some avg10="),
                "{}",
                got.render_signals()
            );
        }
    }

    #[test]
    fn a_container_prefers_the_cgroup_file() {
        let root = fake_root(&[
            ("proc/self/cgroup", "0::/docker/abc\n"),
            (
                "sys/fs/cgroup/docker/abc/memory.pressure",
                "some avg10=15.00 avg60=0 avg300=0 total=1\n",
            ),
            (
                "proc/pressure/memory",
                "some avg10=0.00 avg60=0 avg300=0 total=1\n",
            ),
            ("proc/meminfo", MEMINFO),
        ]);
        let t = PressureThresholds::default();
        let got = read_linux_pressure(root.path(), true, &t).expect("readable");
        assert!(
            matches!(got.source, PressureSource::LinuxCgroupPsi(_)),
            "{:?}",
            got.source
        );
        assert_eq!(got.level, PressureLevel::Warn);
        let host = read_linux_pressure(root.path(), false, &t).expect("readable");
        assert!(matches!(host.source, PressureSource::LinuxPsi(_)));
        assert_eq!(host.level, PressureLevel::Normal);
    }

    #[test]
    fn without_psi_meminfo_is_the_fallback() {
        let root = fake_root(&[("proc/meminfo", MEMINFO)]);
        let got = read_linux_pressure(root.path(), false, &PressureThresholds::default())
            .expect("meminfo readable");
        assert!(matches!(got.source, PressureSource::LinuxMeminfo(_)));
        assert_eq!(got.level, PressureLevel::Normal);
    }

    #[test]
    fn meminfo_classifies_by_available_pct() {
        let t = PressureThresholds::default();
        assert_eq!(classify_available_pct(50.0, &t), PressureLevel::Normal);
        let strict = PressureThresholds::default().with_min_available_pct(60.0);
        assert_eq!(classify_available_pct(50.0, &strict), PressureLevel::Warn);
        assert_eq!(classify_available_pct(8.0, &t), PressureLevel::Warn);
        assert_eq!(classify_available_pct(4.0, &t), PressureLevel::Critical);
        assert!(parse_meminfo_pct("MemTotal: 0 kB\nMemAvailable: 0 kB\n").is_err());
        let pct = parse_meminfo_pct(MEMINFO).expect("parses");
        assert!((pct - 50.0).abs() < 0.01);
    }

    #[test]
    fn a_missing_psi_file_is_a_read_error() {
        let root = fake_root(&[("unrelated", "")]);
        let err = read_linux_pressure(root.path(), false, &PressureThresholds::default())
            .expect_err("nothing to read");
        assert!(matches!(err, MemoryPressureError::Read { .. }), "{err}");
        assert!(err.errno().is_some(), "an ENOENT read carries its errno");
    }

    #[test]
    fn a_garbled_psi_file_is_a_parse_error() {
        let root = fake_root(&[("proc/pressure/memory", "nonsense\n")]);
        let err = read_linux_pressure(root.path(), false, &PressureThresholds::default())
            .expect_err("garbled");
        assert!(matches!(err, MemoryPressureError::Parse { .. }), "{err}");
    }

    #[test]
    fn live_pressure_reads_on_this_host() {
        let got = read_memory_pressure(&PressureThresholds::default());
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            let got = got.expect("a supported host has a pressure source");
            assert!(!got.signals.is_empty());
        } else {
            assert!(matches!(got, Err(MemoryPressureError::Unsupported(_))));
        }
    }
}
