use rbitcoin_store::sorted_run::*;

#[test]
fn workers_for_free_ram_1gib_head_is_not_sh_1_5gib() {
    const HEAD: u64 = rbitcoin_store::tx_table::TX_HEAD_REBUILD_WORKER_FREE_RAM_BYTES;
    assert_eq!(workers_for_free_ram(8, 0, HEAD), 1);
    assert_eq!(workers_for_free_ram(8, HEAD.saturating_sub(1), HEAD), 1);
    assert_eq!(workers_for_free_ram(8, HEAD, HEAD), 1);
    assert_eq!(workers_for_free_ram(8, 2 * HEAD, HEAD), 2);
    assert_eq!(workers_for_free_ram(8, 3 * HEAD, HEAD), 3);
    assert_eq!(workers_for_free_ram(4, 20 * HEAD, HEAD), 4);
    assert_eq!(workers_for_free_ram(8, 3 * (1 << 30) / 2, HEAD), 1);
    assert_eq!(workers_for_free_ram(8, 2 * (1 << 30), HEAD), 2);
}

#[test]
fn mem_available_parses_proc_meminfo() {
    let text = "MemTotal:       16384000 kB\nMemFree:         1000000 kB\nMemAvailable:    3145728 kB\nBuffers:          200000 kB\n";
    assert_eq!(mem_available_from_meminfo(text), Some(3145728 * 1024));
    assert_eq!(mem_available_from_meminfo("MemTotal: 1 kB\n"), None);
}

#[test]
fn darwin_vm_pages_count_as_free_ram() {
    assert_eq!(mem_available_from_darwin_vm(0, 10, 10), None);
    assert_eq!(mem_available_from_darwin_vm(4096, 0, 0), Some(0));
    assert_eq!(mem_available_from_darwin_vm(4096, 1, 0), Some(4096));
    assert_eq!(mem_available_from_darwin_vm(16384, 2, 2), Some(4 * 16384));
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn host_mem_available_bytes_is_some_on_linux_macos_windows() {
    let n = host_mem_available_bytes().expect("Linux MemAvailable / Darwin vm / Windows AvailPhys");
    assert!(n >= 1024 * 1024, "probe too small: {n}");
    let w = workers_for_free_ram(logical_cpus(), n, 2 << 30);
    assert!((1..=256).contains(&w));
    assert!(w <= logical_cpus());
    let label = free_gib_label();
    assert!(label == "?" || label.parse::<f64>().is_ok(), "{label}");
}
