//! Golden: pair-wise validation of every <id>.pre_recover.xml /
//! <id>.recovered.xml under tests/fixtures/stages/.
//!
//! The tree obtained by leniently parsing pre_recover must be structurally
//! identical to the tree obtained by strictly parsing recovered (tags,
//! attribute sequences, xmlns declarations, children and text).

mod common;

use std::path::PathBuf;

use common::trees_equal;
use docxtpl_xml::{XmlDocument, XmlLimits};

/// These two fixtures fail as expected already at the patch/Jinja stage, so
/// they only have `full_patched` evidence and never enter recover to produce
/// the latter two goldens.
const PATCH_ONLY_FIXTURES: &[&str] = &["p4_img_bad", "r2_syntax_error"];

fn stages_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("stages")
}

fn ids_with_suffix(dir: &std::path::Path, suffix: &str) -> Vec<String> {
    let mut ids: Vec<String> = std::fs::read_dir(dir)
        .expect("reading stages directory")
        .map(|entry| entry.expect("reading directory entry"))
        .filter_map(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .strip_suffix(suffix)
                .map(str::to_owned)
        })
        .collect();
    ids.sort();
    ids
}

#[test]
fn golden_recovery_all_pairs() {
    let dir = stages_dir();
    assert!(
        dir.is_dir(),
        "cannot find stages directory: {}",
        dir.display()
    );
    let full_patched_ids = ids_with_suffix(&dir, ".full_patched.xml");
    assert!(
        !full_patched_ids.is_empty(),
        "did not enumerate any full_patched fixture"
    );
    for excluded in PATCH_ONLY_FIXTURES {
        assert!(
            full_patched_ids.iter().any(|id| id == excluded),
            "explicit patch-only fixture {excluded} is no longer in the full_patched set; sync the gate policy"
        );
    }
    let expected_ids: Vec<String> = full_patched_ids
        .into_iter()
        .filter(|id| !PATCH_ONLY_FIXTURES.contains(&id.as_str()))
        .collect();
    let ids = ids_with_suffix(&dir, ".pre_recover.xml");
    let recovered_ids = ids_with_suffix(&dir, ".recovered.xml");
    assert_eq!(
        ids, expected_ids,
        "pre_recover goldens must cover every full_patched fixture (minus the explicit patch-only cases)"
    );
    assert_eq!(
        recovered_ids, expected_ids,
        "recovered goldens must cover every full_patched fixture (minus the explicit patch-only cases)"
    );

    let limits = XmlLimits::default();
    let mut failures: Vec<String> = Vec::new();
    for id in &ids {
        let pre_path = dir.join(format!("{id}.pre_recover.xml"));
        let recovered_path = dir.join(format!("{id}.recovered.xml"));
        if !recovered_path.is_file() {
            failures.push(format!("{id}: missing paired recovered.xml"));
            continue;
        }
        let pre = std::fs::read_to_string(&pre_path).expect("reading pre_recover");
        let recovered = std::fs::read_to_string(&recovered_path).expect("reading recovered");

        let outcome = XmlDocument::parse_lenient(&pre, &limits)
            .unwrap_or_else(|e| panic!("{id}: lenient parsing failed: {e}"));
        let expected = XmlDocument::parse_strict(&recovered, &limits)
            .unwrap_or_else(|e| panic!("{id}: strict parsing of recovered failed: {e}"));

        if let Err(diff) = trees_equal(&outcome.doc, &expected) {
            failures.push(format!("{id}: {diff}"));
        }
    }
    assert!(
        failures.is_empty(),
        "the following fixtures have inconsistent tree structures:\n{}",
        failures.join("\n")
    );
}
