//! Worker cap from free RAM. Used by `tx.head` rebuild and scripthash extract.
//!
//! The sorted-run codec is gone. Tip materialize does not write `scripthash.runs`.
//! Store open unlinks leftover names in that directory except `SEAL`.

/// Cap workers at 1 per `per_worker` free bytes (floor 1, clamp 1..=256 vs CPUs).
pub fn workers_for_free_ram(cpus: usize, free_bytes: u64, per_worker: u64) -> usize {
    let cpus = cpus.clamp(1, 256);
    if per_worker == 0 {
        return 1;
    }
    let ram_cap = (free_bytes / per_worker) as usize;
    cpus.min(ram_cap.clamp(1, 256))
}

/// `MemAvailable` from `/proc/meminfo` text (kB → bytes).
#[cfg(any(debug_assertions, target_os = "linux"))]
#[doc(hidden)]
pub fn mem_available_from_meminfo(text: &str) -> Option<u64> {
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("MemAvailable:") else {
            continue;
        };
        let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
        return Some(kb.saturating_mul(1024));
    }
    None
}

/// Darwin reclaimable pages (`free_count + inactive_count`) × page size.
/// Speculative pages are already inside `free_count`.
#[cfg(any(debug_assertions, target_os = "macos"))]
#[doc(hidden)]
pub fn mem_available_from_darwin_vm(
    page_size: u64,
    free_count: u64,
    inactive_count: u64,
) -> Option<u64> {
    if page_size == 0 {
        return None;
    }
    Some(
        free_count
            .saturating_add(inactive_count)
            .saturating_mul(page_size),
    )
}

#[cfg(target_os = "macos")]
#[doc(hidden)]
pub fn mem_available_from_darwin_host() -> Option<u64> {
    let page_size = {
        let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if n <= 0 {
            return None;
        }
        n as u64
    };
    let mut vm = unsafe { std::mem::zeroed::<libc::vm_statistics64>() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    let kr = unsafe {
        // SAFETY: process host port; HOST_VM_INFO64 writes into this stack vm_statistics64.
        #[allow(deprecated)]
        let host = libc::mach_host_self();
        libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            &mut vm as *mut _ as libc::host_info64_t,
            &mut count,
        )
    };
    if kr != libc::KERN_SUCCESS {
        return None;
    }
    mem_available_from_darwin_vm(
        page_size,
        u64::from(vm.free_count),
        u64::from(vm.inactive_count),
    )
}

#[cfg(windows)]
#[repr(C)]
#[allow(dead_code)]
#[doc(hidden)]
pub struct MemoryStatusEx {
    pub dw_length: u32,
    pub dw_memory_load: u32,
    pub ull_total_phys: u64,
    pub ull_avail_phys: u64,
    pub ull_total_page_file: u64,
    pub ull_avail_page_file: u64,
    pub ull_total_virtual: u64,
    pub ull_avail_virtual: u64,
    pub ull_avail_extended_virtual: u64,
}

#[cfg(windows)]
extern "system" {
    fn GlobalMemoryStatusEx(status: *mut MemoryStatusEx) -> i32;
}

#[cfg(windows)]
#[doc(hidden)]
pub fn mem_available_from_windows_host() -> Option<u64> {
    let mut st = MemoryStatusEx {
        dw_length: std::mem::size_of::<MemoryStatusEx>() as u32,
        dw_memory_load: 0,
        ull_total_phys: 0,
        ull_avail_phys: 0,
        ull_total_page_file: 0,
        ull_avail_page_file: 0,
        ull_total_virtual: 0,
        ull_avail_virtual: 0,
        ull_avail_extended_virtual: 0,
    };
    // SAFETY: dw_length is size_of Self; the API fills the remaining fields.
    if unsafe { GlobalMemoryStatusEx(&mut st) } == 0 {
        return None;
    }
    Some(st.ull_avail_phys)
}

/// Host memory available for new allocations.
///
/// Linux: `MemAvailable`. Darwin: free+inactive pages. Windows: `ullAvailPhys`.
/// Other OS: `None` (worker cap falls back to 1).
pub fn host_mem_available_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        mem_available_from_meminfo(&text)
    }
    #[cfg(target_os = "macos")]
    {
        mem_available_from_darwin_host()
    }
    #[cfg(windows)]
    {
        return mem_available_from_windows_host();
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        None
    }
}

/// `MemAvailable` as a log label (`"12.3"` GiB, or `"?"` when unknown).
pub fn free_gib_label() -> String {
    host_mem_available_bytes()
        .map(|b| format!("{:.1}", b as f64 / (1u64 << 30) as f64))
        .unwrap_or_else(|| "?".into())
}

pub fn logical_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, 256)
}
