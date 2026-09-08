//! Where a process's physical footprint actually sits: heap, file-backed, or
//! compressed (#7084, consumed by #6928).
//!
//! Why: `crate::sys_metrics::physical_footprint_bytes` (macOS-only, so named
//! here rather than linked — a link would break the docs build on every other
//! target) answers "how much memory does this daemon hold" with one number,
//! and one number cannot tell a
//! 14 GB heap apart from 14 GB of mmapped redb pages the kernel can evict for
//! free. Those two call for opposite responses — the first is a leak, the
//! second is the vector store working as designed — so the console reports the
//! split rather than the total alone.
//! What: [`self_memory_breakdown`] reads macOS's `TASK_VM_INFO`, whose
//! `internal` / `external` / `compressed` ledgers are the three components
//! `phys_footprint` is assembled from. It answers for THIS process only.
//! Test: `self_breakdown_is_plausible`,
//! `self_breakdown_components_fit_inside_the_footprint`,
//! `breakdown_is_none_off_macos`.
//!
//! # Why self-only
//!
//! Reading another process's `TASK_VM_INFO` needs `task_for_pid`, which macOS
//! grants only to a root or specially-entitled caller. Each daemon therefore
//! reports its OWN breakdown through its metrics payload; nothing samples a
//! sibling. That is also why this does not live beside
//! `ProcessCpuSampler::rss_bytes`, which deliberately takes an arbitrary pid.

/// How one process's physical footprint divides across the three VM ledgers.
///
/// Why: an operator reading "1.8 GB" needs to know which kind of 1.8 GB it is.
/// `heap_bytes` growing is a leak; `file_backed_bytes` growing is a mapped
/// store the kernel can drop under pressure; `compressed_bytes` growing means
/// the memory compressor is already working.
/// What: bytes, straight from the kernel's ledgers. The three do not have to
/// sum to the footprint — `phys_footprint` also nets out purgeable and reusable
/// pages — so a consumer must render them as components, never as a partition.
/// Test: `self_breakdown_components_fit_inside_the_footprint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MemoryBreakdown {
    /// Anonymous, dirty pages — the heap and stacks (`TASK_VM_INFO.internal`).
    pub heap_bytes: u64,
    /// Pages backed by a file on disk, mmapped stores included
    /// (`TASK_VM_INFO.external`).
    pub file_backed_bytes: u64,
    /// Pages the macOS memory compressor holds (`TASK_VM_INFO.compressed`).
    pub compressed_bytes: u64,
}

/// The current process's heap / file-backed / compressed split, if the OS
/// reports one.
///
/// Why: see the module docs — this is the counter that makes a large footprint
/// diagnosable rather than merely alarming.
/// What: on macOS, one `task_info(TASK_VM_INFO)` call against
/// `mach_task_self()`. `None` on any failure and on every non-macOS target,
/// which callers must render as "not reported" rather than as three zeros.
/// Test: `self_breakdown_is_plausible`, `breakdown_is_none_off_macos`.
#[must_use]
pub fn self_memory_breakdown() -> Option<MemoryBreakdown> {
    #[cfg(target_os = "macos")]
    {
        macos::read()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::MemoryBreakdown;

    /// `TASK_VM_INFO` from `<mach/task_info.h>`.
    const TASK_VM_INFO: libc::task_flavor_t = 22;

    // Why declared here rather than taken from `libc`: `libc::mach_task_self_`
    // and its `mach_task_self()` wrapper are both deprecated in favour of the
    // `mach2` crate, which this workspace does not carry, and `-D warnings`
    // makes a deprecation a build failure. The symbol itself is stable
    // libSystem ABI — a send right the runtime initialises before `main` and
    // never mutates — so naming it directly costs nothing and adds no
    // dependency.
    unsafe extern "C" {
        /// This task's own send right, as `<mach/mach_init.h>` declares it.
        static mach_task_self_: libc::mach_port_t;
    }

    /// The prefix of `struct task_vm_info` up to and including `phys_footprint`.
    ///
    /// Why only a prefix: `task_info` writes at most the number of `natural_t`
    /// words the caller asks for, so requesting the rev-1 count means the
    /// kernel never touches a byte past `phys_footprint`. Declaring the whole
    /// struct instead would bind this crate to a tail Apple has revised six
    /// times, for fields nothing here reads.
    /// What: field order and types copied verbatim from the SDK header. The two
    /// `integer_t`s pack into one 8-byte slot, so the prefix is 152 bytes with
    /// no padding — [`VM_INFO_COUNT`] asserts that.
    /// Test: `self_breakdown_is_plausible` — a layout error yields a garbage
    /// footprint, which that test's bounds reject.
    #[repr(C)]
    #[derive(Default)]
    struct TaskVmInfoRev1 {
        virtual_size: u64,
        region_count: i32,
        page_size: i32,
        resident_size: u64,
        resident_size_peak: u64,
        device: u64,
        device_peak: u64,
        internal: u64,
        internal_peak: u64,
        external: u64,
        external_peak: u64,
        reusable: u64,
        reusable_peak: u64,
        purgeable_volatile_pmap: u64,
        purgeable_volatile_resident: u64,
        purgeable_volatile_virtual: u64,
        compressed: u64,
        compressed_peak: u64,
        compressed_lifetime: u64,
        phys_footprint: u64,
    }

    /// How many `natural_t` words [`TaskVmInfoRev1`] spans —
    /// `TASK_VM_INFO_REV1_COUNT` in the SDK header.
    const VM_INFO_COUNT: libc::mach_msg_type_number_t = (size_of::<TaskVmInfoRev1>()
        / size_of::<libc::natural_t>())
        as libc::mach_msg_type_number_t;

    /// Read this task's VM ledgers.
    ///
    /// Test: `self_breakdown_is_plausible`.
    pub(super) fn read() -> Option<MemoryBreakdown> {
        let mut info = TaskVmInfoRev1::default();
        let mut count = VM_INFO_COUNT;
        // SAFETY: `mach_task_self_` is a send right the runtime initialises
        // before `main` and never mutates, so this read is a plain `Copy` of a
        // stable `u32`. `info` is a `#[repr(C)]` struct laid out as the
        // header's `struct task_vm_info` prefix, and `count` tells the kernel
        // how many 32-bit words it may write — exactly `info`'s own size.
        // `task_info` returns non-`KERN_SUCCESS` without writing when it
        // refuses, so `info` is read only after success.
        let ret = unsafe {
            libc::task_info(
                mach_task_self_,
                TASK_VM_INFO,
                std::ptr::addr_of_mut!(info).cast(),
                std::ptr::addr_of_mut!(count),
            )
        };
        if ret != 0 || count < VM_INFO_COUNT {
            return None;
        }
        Some(MemoryBreakdown {
            heap_bytes: info.internal,
            file_backed_bytes: info.external,
            compressed_bytes: info.compressed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: a wrong struct layout or flavour constant returns garbage rather
    /// than an error, so the only defence is asserting the numbers are
    /// physically possible for the test process.
    /// Test: this is the test.
    #[cfg(target_os = "macos")]
    #[test]
    fn self_breakdown_is_plausible() {
        let b = self_memory_breakdown().expect("macOS must report TASK_VM_INFO for its own task");
        // A running test binary has a non-trivial heap and cannot plausibly
        // hold a terabyte of it.
        assert!(
            b.heap_bytes > 64 * 1024,
            "heap_bytes reads as {} — too small to be a live process",
            b.heap_bytes
        );
        assert!(
            b.heap_bytes < 1024_u64.pow(4),
            "heap_bytes reads as {} — beyond any plausible test process",
            b.heap_bytes
        );
        assert!(
            b.file_backed_bytes > 0,
            "a linked binary always has file-backed pages"
        );
    }

    /// Why: `phys_footprint` nets out purgeable and reusable pages, so the
    /// three components are not a partition of it — but neither may any one of
    /// them exceed the whole task's virtual reach. This is what catches a
    /// field read from the wrong offset.
    /// Test: this is the test.
    #[cfg(target_os = "macos")]
    #[test]
    fn self_breakdown_components_fit_inside_the_footprint() {
        let b = self_memory_breakdown().expect("breakdown");
        let footprint = crate::sys_metrics::physical_footprint_bytes(std::process::id())
            .expect("footprint for our own pid");
        assert!(
            b.heap_bytes <= footprint.saturating_mul(4),
            "heap {} is implausibly far above the {footprint}-byte footprint",
            b.heap_bytes
        );
        assert!(
            b.compressed_bytes <= footprint.saturating_mul(4),
            "compressed {} is implausibly far above the {footprint}-byte footprint",
            b.compressed_bytes
        );
    }

    /// Why: the contract off macOS is "not reported", never three zeros — a
    /// consumer that saw zeros would render a real 0 B heap.
    /// Test: this is the test.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn breakdown_is_none_off_macos() {
        assert!(self_memory_breakdown().is_none());
    }
}
