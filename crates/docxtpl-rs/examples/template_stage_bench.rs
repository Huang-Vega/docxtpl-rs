//! Measures independent preprocessing stages for a DOCX main document part.

use docxtpl_opc::{Package, PackageLimits};
use docxtpl_template::minijinja::Environment;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("usage: template_stage_bench TEMPLATE ITERATIONS".into());
    }
    let iterations = args[2].parse::<u32>()?;
    let package = Package::open(&args[1], &PackageLimits::default())?;
    let main_name = package.main_document_uri()?.as_str().to_string();
    let source = std::str::from_utf8(
        package
            .part(&main_name)
            .ok_or("main document part is missing")?
            .bytes()?,
    )?
    .to_string();
    let patched = docxtpl_compat::patch_xml(&source);
    let mut patch_ns = 0u128;
    let mut compile_ns = 0u128;
    let mut normalize_ns = 0u128;
    for _ in 0..iterations {
        let started = Instant::now();
        std::hint::black_box(docxtpl_compat::patch_xml(&source));
        patch_ns += started.elapsed().as_nanos();

        let started = Instant::now();
        let environment = Environment::new();
        std::hint::black_box(environment.template_from_str(&patched))?;
        compile_ns += started.elapsed().as_nanos();

        let started = Instant::now();
        std::hint::black_box(docxtpl_template::normalize_part_xml(&source, &main_name))?;
        normalize_ns += started.elapsed().as_nanos();
    }
    let divisor = f64::from(iterations) * 1e6;
    println!(
        "{{\"iterations\":{iterations},\"source_bytes\":{},\"patched_bytes\":{},\"patch_ms\":{:.3},\"jinja_compile_ms\":{:.3},\"xml_parse_serialize_ms\":{:.3}}}",
        source.len(),
        patched.len(),
        patch_ns as f64 / divisor,
        compile_ns as f64 / divisor,
        normalize_ns as f64 / divisor,
    );
    Ok(())
}
