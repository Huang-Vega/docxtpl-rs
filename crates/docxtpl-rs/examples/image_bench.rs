//! Phased rich-context benchmark for image-heavy templates.
//!
//! The template must iterate `rows` and render `row.img`, as the
//! `p4_img_in_table.docx` fixture does. IMAGE_SOURCE may be one image file or
//! a directory of image files; directory entries are used in sorted order and
//! cycled until COUNT rows have been created.

use docxtpl_rs::{
    DocxTemplate, InlineImage, MediaCompression, RenderContext, RenderOptions, RenderValue,
    WriteOptions,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
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

fn image_paths(source: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    if source.is_file() {
        if source.extension().and_then(|extension| extension.to_str()) == Some("txt") {
            let paths = std::fs::read_to_string(source)?
                .lines()
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .collect::<Vec<_>>();
            if paths.is_empty() {
                return Err("IMAGE_SOURCE manifest contains no paths".into());
            }
            return Ok(paths);
        }
        return Ok(vec![source.to_path_buf()]);
    }
    let mut paths = Vec::new();
    let mut directories = vec![source.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                directories.push(path);
            } else if file_type.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        matches!(
                            extension.to_ascii_lowercase().as_str(),
                            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "tif" | "tiff"
                        )
                    })
            {
                paths.push(path);
            }
        }
    }
    paths.sort();
    if paths.is_empty() {
        return Err("IMAGE_SOURCE directory contains no files".into());
    }
    Ok(paths)
}

fn build_context(
    paths: &[PathBuf],
    count: usize,
    lazy: bool,
) -> Result<RenderContext, Box<dyn std::error::Error>> {
    let images = paths
        .iter()
        .map(|path| {
            let path = path.to_string_lossy();
            if lazy {
                InlineImage::from_path_lazy(&path, None, None, None).map(Arc::new)
            } else {
                InlineImage::from_path(&path, None, None, None).map(Arc::new)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut rows = Vec::with_capacity(count);
    for index in 0..count {
        let image = Arc::clone(&images[index % images.len()]);
        rows.push(RenderValue::object(vec![
            ("n".to_string(), format!("row-{index}").into()),
            ("img".to_string(), image.into()),
        ]));
    }
    let mut context = RenderContext::new();
    context.insert("rows", RenderValue::array(rows));
    Ok(context)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    if !(5..=10).contains(&args.len()) {
        return Err(
            "usage: image_bench TEMPLATE IMAGE_SOURCE COUNT ITERATIONS [compatible|fast|stored|auto] [IMAGE_WORKERS] [eager|lazy] [bytes|file] [MAX_PARALLEL_IMAGE_BYTES]".into(),
        );
    }
    let count = args[3].parse::<usize>()?;
    let iterations = args[4].parse::<u32>()?;
    if count == 0 || iterations == 0 {
        return Err("COUNT and ITERATIONS must be greater than zero".into());
    }
    let mut paths = image_paths(Path::new(&args[2]))?;
    paths.truncate(paths.len().min(count));
    let compression = match args.get(5).map(String::as_str).unwrap_or("compatible") {
        "compatible" => MediaCompression::Compatible,
        "fast" => MediaCompression::FastDeflate,
        "stored" => MediaCompression::Stored,
        "auto" => MediaCompression::Auto,
        _ => return Err("compression must be compatible, fast, stored, or auto".into()),
    };
    let write_options = WriteOptions::compatible().with_media_compression(compression);
    let image_workers = args
        .get(6)
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(1);
    let max_parallel_image_bytes = args
        .get(9)
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(64 * 1024 * 1024);
    let render_options = RenderOptions::compat()
        .with_image_parallelism(image_workers)
        .with_max_parallel_image_bytes(max_parallel_image_bytes);
    let lazy_images = match args.get(7).map(String::as_str).unwrap_or("eager") {
        "eager" => false,
        "lazy" => true,
        _ => return Err("image source mode must be eager or lazy".into()),
    };
    let write_to_file = match args.get(8).map(String::as_str).unwrap_or("bytes") {
        "bytes" => false,
        "file" => true,
        _ => return Err("output mode must be bytes or file".into()),
    };
    let output_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.tmp/image-bench-output.docx");
    let mut context_build = 0u128;
    let mut open = 0u128;
    let mut render = 0u128;
    let mut write = 0u128;
    let mut output_bytes = 0usize;

    for _ in 0..iterations {
        let started = Instant::now();
        let context = build_context(&paths, count, lazy_images)?;
        context_build += started.elapsed().as_nanos();

        let started = Instant::now();
        let template = DocxTemplate::open(&args[1])?;
        open += started.elapsed().as_nanos();

        let started = Instant::now();
        let document = template.render_ctx(&context, &render_options)?;
        render += started.elapsed().as_nanos();

        let started = Instant::now();
        output_bytes = if write_to_file {
            document.save_with_options(&output_path, &write_options)?;
            usize::try_from(std::fs::metadata(&output_path)?.len())?
        } else {
            document.to_bytes_with_options(&write_options)?.len()
        };
        write += started.elapsed().as_nanos();
    }

    let divisor = f64::from(iterations) * 1e6;
    println!(
        "{{\"iterations\":{iterations},\"image_count\":{count},\"source_image_count\":{},\"image_source\":\"{}\",\"output_mode\":\"{}\",\"media_compression\":\"{compression:?}\",\"image_workers\":{},\"max_parallel_image_bytes\":{},\"context_build_ms\":{:.3},\"open_ms\":{:.3},\"render_ms\":{:.3},\"write_ms\":{:.3},\"output_bytes\":{output_bytes},\"peak_rss_bytes\":{}}}",
        paths.len(),
        if lazy_images { "lazy" } else { "eager" },
        if write_to_file { "file" } else { "bytes" },
        render_options.image_parallelism(),
        render_options.max_parallel_image_bytes(),
        context_build as f64 / divisor,
        open as f64 / divisor,
        render as f64 / divisor,
        write as f64 / divisor,
        peak_rss_bytes(),
    );
    Ok(())
}
