mod test_support;

use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use docxtpl_rs::{
    DocxTemplate, PreparedCachePolicy, PreparedTemplateCache, RenderOptions, ResourceLimits,
};
use serde_json::json;

const TEMPLATE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/templates/r2_var_basic.docx"
);

fn project_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("prepared-cache-")
        .tempdir_in(test_support::target_dir())
        .expect("create project-local cache test directory")
}

#[test]
fn persistent_cache_hits_across_template_instances() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = project_tempdir();
    let cache = PreparedTemplateCache::new(temporary.path(), PreparedCachePolicy::default())?;

    let first = DocxTemplate::open(TEMPLATE)?
        .with_prepared_cache(cache.clone())
        .render(&json!({"name": "cache"}), &RenderOptions::compat())?
        .to_bytes()?;
    let after_first = cache.stats();
    assert!(after_first.misses > 0);
    assert!(after_first.writes > 0);

    let second = DocxTemplate::open(TEMPLATE)?
        .with_prepared_cache(cache.clone())
        .render(&json!({"name": "cache"}), &RenderOptions::compat())?
        .to_bytes()?;
    assert_eq!(first, second);
    assert!(cache.stats().hits > after_first.hits);
    Ok(())
}

#[test]
fn corrupt_entry_is_removed_and_rebuilt() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = project_tempdir();
    let cache = PreparedTemplateCache::new(temporary.path(), PreparedCachePolicy::default())?;
    DocxTemplate::open(TEMPLATE)?
        .with_prepared_cache(cache.clone())
        .render(&json!({"name": "first"}), &RenderOptions::compat())?;

    let entry = fs::read_dir(cache.directory())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "dptc")
        })
        .expect("cache entry");
    fs::write(entry, b"corrupt")?;
    let before = cache.stats();

    let bytes = DocxTemplate::open(TEMPLATE)?
        .with_prepared_cache(cache.clone())
        .render(&json!({"name": "rebuilt"}), &RenderOptions::compat())?
        .to_bytes()?;
    assert!(!bytes.is_empty());
    let after = cache.stats();
    assert_eq!(after.corruptions, before.corruptions + 1);
    assert!(after.writes > before.writes);
    Ok(())
}

#[test]
fn zero_ttl_expires_entries_without_affecting_rendering() -> Result<(), Box<dyn std::error::Error>>
{
    let temporary = project_tempdir();
    let cache = PreparedTemplateCache::new(
        temporary.path(),
        PreparedCachePolicy {
            ttl: Duration::ZERO,
            ..PreparedCachePolicy::default()
        },
    )?;
    DocxTemplate::open(TEMPLATE)?
        .with_prepared_cache(cache.clone())
        .render(&json!({"name": "first"}), &RenderOptions::compat())?;
    let before = cache.stats();
    DocxTemplate::open(TEMPLATE)?
        .with_prepared_cache(cache.clone())
        .render(&json!({"name": "second"}), &RenderOptions::compat())?;
    let after = cache.stats();
    assert_eq!(after.hits, before.hits);
    assert!(after.expirations > before.expirations);
    Ok(())
}

#[test]
fn capacity_policy_evicts_owned_entries() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = project_tempdir();
    let cache = PreparedTemplateCache::new(
        temporary.path(),
        PreparedCachePolicy {
            max_entries: 0,
            ..PreparedCachePolicy::default()
        },
    )?;
    DocxTemplate::open(TEMPLATE)?
        .with_prepared_cache(cache.clone())
        .render(&json!({"name": "evict"}), &RenderOptions::compat())?;
    assert!(cache.stats().evictions > 0);
    assert_eq!(
        fs::read_dir(cache.directory())?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "dptc"))
            .count(),
        0
    );
    Ok(())
}

#[test]
fn preprocessing_limit_is_part_of_the_cache_identity() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = project_tempdir();
    let cache = PreparedTemplateCache::new(temporary.path(), PreparedCachePolicy::default())?;
    DocxTemplate::open(TEMPLATE)?
        .with_prepared_cache(cache.clone())
        .render(&json!({"name": "default"}), &RenderOptions::compat())?;
    let before = cache.stats();

    DocxTemplate::open_with_limits(
        TEMPLATE,
        ResourceLimits::default().with_max_rendered_xml_bytes(8 * 1024 * 1024),
    )?
    .with_prepared_cache(cache.clone())
    .render(&json!({"name": "bounded"}), &RenderOptions::compat())?;
    let after = cache.stats();
    assert_eq!(after.hits, before.hits);
    assert!(after.misses > before.misses);
    Ok(())
}

#[test]
fn independent_cache_handles_survive_concurrent_writer_races(
) -> Result<(), Box<dyn std::error::Error>> {
    const WORKERS: usize = 12;
    const RENDERS_PER_WORKER: usize = 8;

    let temporary = project_tempdir();
    let root = temporary.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(WORKERS));
    let mut workers = Vec::new();
    for worker in 0..WORKERS {
        let root = root.clone();
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || -> Result<(), String> {
            // Each handle intentionally has its own in-process lock. Atomic
            // cache installation must therefore resolve the writer race.
            let cache = PreparedTemplateCache::new(&root, PreparedCachePolicy::default())
                .map_err(|error| error.to_string())?;
            let template = DocxTemplate::open(TEMPLATE)
                .map_err(|error| error.to_string())?
                .with_prepared_cache(cache);
            barrier.wait();
            for iteration in 0..RENDERS_PER_WORKER {
                let bytes = template
                    .render(
                        &json!({"name": format!("worker-{worker}-{iteration}")}),
                        &RenderOptions::compat(),
                    )
                    .and_then(|document| document.to_bytes())
                    .map_err(|error| error.to_string())?;
                if bytes.is_empty() {
                    return Err("concurrent render produced no bytes".to_string());
                }
            }
            Ok(())
        }));
    }
    for worker in workers {
        worker.join().map_err(|_| "cache worker panicked")??;
    }

    let cache = PreparedTemplateCache::new(&root, PreparedCachePolicy::default())?;
    DocxTemplate::open(TEMPLATE)?
        .with_prepared_cache(cache.clone())
        .render(&json!({"name": "post-race"}), &RenderOptions::compat())?;
    let stats = cache.stats();
    assert!(stats.hits > 0, "a complete entry should survive the race");
    assert_eq!(stats.corruptions, 0);
    assert_eq!(stats.io_errors, 0);
    assert_eq!(
        fs::read_dir(cache.directory())?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".tmp-"))
            .count(),
        0,
        "writer races must not leave temporary entries"
    );
    Ok(())
}
