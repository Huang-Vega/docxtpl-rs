//! P7 分阶段微基准：把打开、渲染、写出从 CLI 启动时间中分离。
use docxtpl_rs::{DocxTemplate, RenderOptions};
use std::time::Instant;

#[cfg(windows)]
fn peak_rss_bytes() -> usize {
    #[repr(C)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
    }
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut core::ffi::c_void,
            counters: *mut Counters,
            size: u32,
        ) -> i32;
    }
    let mut counters = Counters {
        cb: std::mem::size_of::<Counters>() as u32,
        page_fault_count: 0,
        peak_working_set_size: 0,
        working_set_size: 0,
        quota_peak_paged_pool_usage: 0,
        quota_paged_pool_usage: 0,
        quota_peak_non_paged_pool_usage: 0,
        quota_non_paged_pool_usage: 0,
        pagefile_usage: 0,
        peak_pagefile_usage: 0,
    };
    // SAFETY: the handle is for this process and the writable buffer has the declared size.
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            std::mem::size_of::<Counters>() as u32,
        )
    };
    if ok == 0 {
        0
    } else {
        counters.peak_working_set_size
    }
}

#[cfg(unix)]
fn peak_rss_bytes() -> usize {
    // SAFETY: getrusage initializes the caller-owned rusage buffer for this process.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: the pointer is valid and writable for the duration of the call.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return 0;
    }
    #[cfg(target_os = "macos")]
    {
        usage.ru_maxrss.max(0) as usize
    }
    #[cfg(not(target_os = "macos"))]
    {
        (usage.ru_maxrss.max(0) as usize).saturating_mul(1024)
    }
}

#[cfg(not(any(windows, unix)))]
fn peak_rss_bytes() -> usize {
    0
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: phase_bench TEMPLATE CONTEXT ITERATIONS".into());
    }
    let context: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&args[2])?)?;
    let iterations: u32 = args[3].parse()?;
    let mut open = 0u128;
    let mut render = 0u128;
    let mut write = 0u128;
    let mut output = 0usize;
    for _ in 0..iterations {
        let t = Instant::now();
        let tpl = DocxTemplate::open(&args[1])?;
        open += t.elapsed().as_nanos();
        let t = Instant::now();
        let doc = tpl.render(&context, &RenderOptions::compat())?;
        render += t.elapsed().as_nanos();
        let t = Instant::now();
        output = doc.to_bytes()?.len();
        write += t.elapsed().as_nanos();
    }
    let peak_rss = peak_rss_bytes();
    println!("{{\"iterations\":{iterations},\"open_ms\":{:.3},\"render_ms\":{:.3},\"write_ms\":{:.3},\"output_bytes\":{output},\"peak_rss_bytes\":{peak_rss}}}", open as f64 / iterations as f64 / 1e6, render as f64 / iterations as f64 / 1e6, write as f64 / iterations as f64 / 1e6);
    Ok(())
}
