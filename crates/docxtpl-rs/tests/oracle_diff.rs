//! End-to-end differential against the Python oracle (planning doc section 9, code spec section 5.2).
//!
//! Compiled and run only under `--features oracle`: for every fixture with
//! mode=render in the manifest, render through the Rust facade and invoke
//! `tests/oracle/compare.py` to compare semantics (c14n +
//! part/rels/content-types) against the pinned Python docxtpl 0.20.2 output.
//!
//! - `context_kind="json"` (or the default): load the context from the JSON
//!   file referenced by the manifest;
//! - `context_kind="python"` (p4_*): the context is reproduced 1:1 in this
//!   test with Rust rich-content types from `build_context(tpl)` in
//!   `tests/fixtures/contexts/<id>.py` (including external links
//!   pre-registered via tpl.build_url_id), without running Python.
//!
//! For error-expected fixtures: compare error categories (oracle error_type
//! vs Rust TemplateErrorKind::oracle_exception).

#![cfg(feature = "oracle")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use docxtpl_rs::{
    DocxTemplate, InlineImage, Listing, RenderContext, RenderOptions, RenderSession, RenderValue,
    RichText, RichTextParagraph, RichTextProps, TemplateErrorKind,
};
use serde_json::Value;

fn fixtures_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/crates/docxtpl-rs
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
}

fn oracle_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/crates/docxtpl-rs
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("oracle")
}

fn project_target_dir() -> PathBuf {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                workspace.join(path)
            }
        }
        None => workspace.join("target"),
    }
}

fn python() -> String {
    std::env::var("DOCXTPL_PYTHON").unwrap_or_else(|_| "python".to_string())
}

fn load_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()))
}

/// EMU per 1 mm (python-docx `Mm`: 914400/25.4 = 36000).
const EMU_PER_MM: i64 = 36_000;

/// Test helper for building RichTextProps (mirrors the Python keyword arguments).
fn props(build: impl FnOnce(&mut RichTextProps)) -> RichTextProps {
    let mut p = RichTextProps::new();
    build(&mut p);
    p
}

/// Build an InlineImage from an image under tests/fixtures/media
/// (mirrors `InlineImage(tpl, _img(name), ...)` in contexts/*.py).
fn media_image(
    root: &Path,
    name: &str,
    width: Option<i64>,
    height: Option<i64>,
    anchor: Option<&str>,
) -> InlineImage {
    let path = root.join("media").join(name);
    InlineImage::from_path(
        path.to_str().expect("the media path is valid UTF-8"),
        width,
        height,
        anchor.map(str::to_string),
    )
    .unwrap_or_else(|e| panic!("failed to read image {}: {e}", path.display()))
}

/// Read the raw bytes of tests/fixtures/media/<name> (P7 replacement material: source/target bytes).
fn media_bytes(root: &Path, name: &str) -> Vec<u8> {
    let path = root.join("media").join(name);
    std::fs::read(&path)
        .unwrap_or_else(|e| panic!("failed to read material {}: {e}", path.display()))
}

/// Reproduce build_context(tpl) from contexts/<id>.py 1:1.
///
/// Every context_kind="python" fixture from p4 onwards is reproduced here;
/// when a new fixture is added, a matching branch must be added here as
/// well (an unknown id panics directly to prevent a missing construction).
/// `tpl` is used for DocxTemplate-level calls such as P7 template
/// introspection (undeclared_variables).
fn build_python_context(
    id: &str,
    root: &Path,
    tpl: &DocxTemplate,
    session: &mut RenderSession,
) -> RenderContext {
    let mut ctx = RenderContext::new();
    match id {
        "p4_rt_basic" => {
            // RichText("Chinese<b>Bold", bold=True, color="#FF0000", size="20")
            // then add italic/underline (strikethrough? no: underline=True)/strikethrough/highlight
            let mut rt = RichText::text_with(
                "Chinese<b>Bold",
                &props(|p| {
                    p.bold = true;
                    p.color = Some("#FF0000".to_owned());
                    p.size = Some("20".to_owned());
                }),
            );
            rt.add_with("Italic", &props(|p| p.italic = true));
            // Upstream normalizes underline=True (not a valid string) to "single"
            rt.add_with(
                "Underline",
                &props(|p| p.underline = Some("single".to_owned())),
            );
            rt.add_with("Strikethrough", &props(|p| p.strike = true));
            rt.add_with(
                "Highlight",
                &props(|p| p.highlight = Some("#FFFF00".to_owned())),
            );
            ctx.insert("rt", rt);
            ctx.insert("empty", RichText::new());
        }
        "p4_rt_style_font" => {
            let mut rt1 =
                RichText::text_with("Style", &props(|p| p.style = Some("Emphasis".to_owned())));
            rt1.add_with(
                "regional font",
                &props(|p| p.font = Some("eastAsia:SimSun".to_owned())),
            );
            let rt2 = RichText::text_with(
                "Superscript",
                &props(|p| {
                    p.superscript = true;
                    p.lang = Some("zh-CN".to_owned());
                }),
            );
            let mut rt3 = RichText::text_with(
                "RTL",
                &props(|p| {
                    p.bold = true;
                    p.rtl = true;
                }),
            );
            rt3.add_with("Subscript", &props(|p| p.subscript = true));
            ctx.insert("rt1", rt1);
            ctx.insert("rt2", rt2);
            ctx.insert("rt3", rt3);
        }
        "p4_rt_url" => {
            // url_id = tpl.build_url_id("https://docxtpl.readthedocs.io/")
            let url_id = session.build_url_id("https://docxtpl.readthedocs.io/");
            let rt = RichText::text_with(
                "Link text",
                &props(|p| {
                    p.url_id = Some(url_id);
                    p.color = Some("0563C1".to_owned());
                    p.underline = Some("single".to_owned());
                }),
            );
            ctx.insert("rt", rt);
        }
        "p4_rt_in_table" => {
            let rt = RichText::text_with(
                "Cell",
                &props(|p| {
                    p.bold = true;
                    p.size = Some("18".to_owned());
                }),
            );
            ctx.insert("rt", rt);
        }
        "p4_rtp_basic" => {
            // RichTextParagraph("First paragraph", parastyle="ListBullet")
            // rp.add(RichText("Second paragraph bold", bold=True), parastyle=None)
            let mut rp = RichTextParagraph::with_text("First paragraph", "ListBullet");
            let bold = RichText::text_with("Second paragraph bold", &props(|p| p.bold = true));
            rp.add_rich(&bold, "");
            ctx.insert("rp", rp);
        }
        "p4_listing_basic" => {
            ctx.insert("lst", Listing::new("L1\nL2\tT3\u{7}P4\u{c}PAGE"));
        }
        "p4_listing_after_rt" => {
            ctx.insert("rt", RichText::text("Before"));
            ctx.insert("lst", Listing::new("X\nY"));
        }
        "p4_autoescape_rich" => {
            ctx.insert("rt", RichText::text_with("R<&", &props(|p| p.bold = true)));
            ctx.insert("lst", Listing::new("L<&\nN"));
            ctx.insert("img", media_image(root, "p4_dot2x1.png", None, None, None));
        }
        "p4_img_png" => {
            ctx.insert("img", media_image(root, "p4_dot2x1.png", None, None, None));
        }
        "p4_img_scale_w" => {
            // width=Mm(20)
            ctx.insert(
                "img",
                media_image(root, "p4_wide4x1.png", Some(20 * EMU_PER_MM), None, None),
            );
        }
        "p4_img_wh" => {
            // width=Mm(10), height=Mm(3)
            ctx.insert(
                "img",
                media_image(
                    root,
                    "p4_rect4x2.jpg",
                    Some(10 * EMU_PER_MM),
                    Some(3 * EMU_PER_MM),
                    None,
                ),
            );
        }
        "p4_img_dup" => {
            // Two independent InlineImage objects with identical bytes: sha1 deduplication reuses the same part/rId
            ctx.insert("i1", media_image(root, "p4_dot2x1.png", None, None, None));
            ctx.insert("i2", media_image(root, "p4_dot2x1.png", None, None, None));
        }
        "p4_img_two" => {
            ctx.insert("i1", media_image(root, "p4_dot2x1.png", None, None, None));
            ctx.insert("i2", media_image(root, "p4_rect4x2.jpg", None, None, None));
        }
        "p4_img_anchor" => {
            ctx.insert(
                "img",
                media_image(
                    root,
                    "p4_dot2x1.png",
                    None,
                    None,
                    Some("https://example.com/"),
                ),
            );
        }
        "p4_img_formats" => {
            ctx.insert(
                "bmp",
                media_image(root, "p4_brick4x2.bmp", None, None, None),
            );
            ctx.insert(
                "gif",
                media_image(root, "p4_arrow4x2.gif", None, None, None),
            );
            ctx.insert(
                "tif",
                media_image(root, "p4_tile4x2.tiff", None, None, None),
            );
        }
        "p4_img_in_table" => {
            // The same InlineImage value is reused in two rows (resolved twice during rendering, idempotent)
            let img = media_image(root, "p4_dot2x1.png", None, None, None);
            let rows = RenderValue::array(vec![
                RenderValue::object(vec![
                    ("n".to_owned(), "r1".into()),
                    ("img".to_owned(), img.clone().into()),
                ]),
                RenderValue::object(vec![
                    ("n".to_owned(), "r2".into()),
                    ("img".to_owned(), img.into()),
                ]),
            ]);
            ctx.insert("rows", rows);
        }
        "p4_img_bad" => {
            // The file is readable but its bytes are not a supported image: the render-time probe raises UnrecognizedImageError
            ctx.insert("img", media_image(root, "p4_bad.png", None, None, None));
        }
        "p4_combo_rich" => {
            let title = RichText::text_with(
                "INV-2026-001",
                &props(|p| {
                    p.bold = true;
                    p.color = Some("1F4E79".to_owned());
                    p.size = Some("24".to_owned());
                }),
            );
            let img1 = media_image(root, "p4_dot2x1.png", None, None, None);
            let img2 = media_image(root, "p4_dot2x1.png", None, None, None);
            let rows = RenderValue::array(vec![
                RenderValue::object(vec![
                    (
                        "name".to_owned(),
                        RichText::text_with("Alpha", &props(|p| p.bold = true)).into(),
                    ),
                    ("qty".to_owned(), 1_i64.into()),
                    ("img".to_owned(), img1.into()),
                ]),
                RenderValue::object(vec![
                    (
                        "name".to_owned(),
                        RichText::text_with("Beta", &props(|p| p.italic = true)).into(),
                    ),
                    ("qty".to_owned(), 2_i64.into()),
                    ("img".to_owned(), img2.into()),
                ]),
            ]);
            ctx.insert("title", title);
            ctx.insert("rows", rows);
            ctx.insert("notes", Listing::new("first line\nsecond line\tlast"));
        }
        "p5_footnotes_basic" => {
            // The footnotes part goes through the generic binary part path (raw strings written back)
            ctx.insert("bn", "Body reference");
            ctx.insert("fn", "Footnote value");
            ctx.insert(
                "rt",
                RichText::text_with("Rich text", &props(|p| p.bold = true)),
            );
            ctx.insert("lst", Listing::new("L1\nL2\tT2"));
        }
        "p5_hf_image" => {
            // Identical-byte images in body/header/footer: package-level media
            // deduplication, three independent rels sets. The insertion order
            // matches the template reference order in each part (lazy
            // resolution; a relationship lands only in the part that
            // references it).
            ctx.insert("ht", "Header");
            ctx.insert("himg", media_image(root, "p4_dot2x1.png", None, None, None));
            ctx.insert(
                "himg2",
                media_image(
                    root,
                    "p4_dot2x1.png",
                    None,
                    None,
                    Some("https://example.com/anchor"),
                ),
            );
            ctx.insert("ft", "Footer");
            ctx.insert("fimg", media_image(root, "p4_dot2x1.png", None, None, None));
            ctx.insert("bt", "Body");
            ctx.insert("bimg", media_image(root, "p4_dot2x1.png", None, None, None));
        }
        "p5_hf_richtext" => {
            // url_id is always registered in the main document rels (build_url_id) and referenced by the body RichText
            let url_id = session.build_url_id("https://docxtpl.readthedocs.io/p5");
            ctx.insert(
                "hrt",
                RichText::text_with(
                    "Header rich",
                    &props(|p| {
                        p.bold = true;
                        p.color = Some("1F4E79".to_owned());
                    }),
                ),
            );
            ctx.insert("hlst", Listing::new("HL1\nHL2"));
            ctx.insert(
                "frt",
                RichText::text_with("Footer rich", &props(|p| p.italic = true)),
            );
            ctx.insert(
                "brt",
                RichText::text_with(
                    "Body link",
                    &props(|p| {
                        p.url_id = Some(url_id);
                        p.underline = Some("single".to_owned());
                        p.color = Some("0563C1".to_owned());
                    }),
                ),
            );
        }
        "p6_subdoc_basic" | "p6_subdoc_style" | "p6_subdoc_image" | "p6_subdoc_verbatim"
        | "p6_subdoc_untagged" => {
            // Reproduce contexts/p6_*.py 1:1: sd = tpl.new_subdoc(<id>_sub.docx)
            // (upstream performs part merging while build_context runs; here it likewise happens before finish).
            let sub_path = root.join("templates").join(format!("{id}_sub.docx"));
            let sd = session
                .new_subdoc(&sub_path)
                .unwrap_or_else(|e| panic!("{id}: new_subdoc merge failed: {e}"));
            ctx.insert("sd", sd);
        }

        // ---------- P7 media/embedded replacement family (ADR-008) ----------
        "p7_media_body" => {
            // Reproduce tpl.replace_media(...) from contexts/p7_media_body.py 1:1.
            session.replace_media(
                media_bytes(root, "p7_dummy.png"),
                media_bytes(root, "p7_media3x2.png"),
            );
            ctx.insert("x", "body media replacement");
        }
        "p7_media_header" => {
            session.replace_media(
                media_bytes(root, "p7_dummy.png"),
                media_bytes(root, "p7_media3x2.png"),
            );
            ctx.insert("y", "header media replacement");
        }
        "p7_pic_match" => {
            // Registration order mirrors the upstream dict insertion order:
            // image.png matches the first picture and my-title the second
            // (the second picture's cNvPr name was renamed to avoid shadowing).
            session.replace_pic("image.png", media_bytes(root, "p7_media3x2.png"));
            session.replace_pic("my-title", media_bytes(root, "p7_new4x4.png"));
            ctx.insert("z", "picture marker replacement");
        }
        "p7_pic_missing" => {
            // The template has no nope.png key -> finish's pre_processing raises ValueError.
            session.replace_pic("nope.png", media_bytes(root, "p7_media3x2.png"));
            ctx.insert("w", "missing marker");
        }
        "p7_embedded_zipname" => {
            // ole1 uses CRC (word/embeddings/ prefix), ole2 matches exactly by zipname.
            session.replace_embedded(
                media_bytes(root, "p7_embed_orig1.bin"),
                media_bytes(root, "p7_embed_new1.bin"),
            );
            session.replace_zipname(
                "word/embeddings/p7_ole2.bin",
                media_bytes(root, "p7_embed_new2.bin"),
            );
            ctx.insert("e", "embedded replacement");
        }
        "p7_replace_only" => {
            // Save without rendering (skip_render): only register CRC media replacements.
            session.replace_media(
                media_bytes(root, "p7_dummy.png"),
                media_bytes(root, "p7_media3x2.png"),
            );
        }
        "p7_undeclared_vars" => {
            // Mirror ctx: vars = ",".join(sorted(get_undeclared_template_variables()))
            let names = tpl
                .undeclared_variables()
                .unwrap_or_else(|e| panic!("{id}: undeclared_variables failed: {e}"));
            ctx.insert("vars", names.into_iter().collect::<Vec<_>>().join(","));
        }

        // ---------- P7b hand-made real-Word templates (reproduce contexts/p7b_*.py 1:1) ----------
        "p7b_word2016" => {
            // Pure spaces/tabs: one pair each for str and RichText (xml:space preserve verified).
            ctx.insert("test_space", "          ");
            ctx.insert("test_tabs", "\t".repeat(5));
            ctx.insert("test_space_r", RichText::text("          "));
            ctx.insert("test_tabs_r", RichText::text(&"\t".repeat(5)));
        }
        "p7b_cellbg" => {
            // Four alert rows: row 1 RichText is red and bold; bg is the cell-background variable.
            let alerts = RenderValue::array(vec![
                RenderValue::object(vec![
                    ("date".to_owned(), "2015-03-10".into()),
                    (
                        "desc".to_owned(),
                        RichText::text_with(
                            "Very critical alert",
                            &props(|p| {
                                p.color = Some("FF0000".to_owned());
                                p.bold = true;
                            }),
                        )
                        .into(),
                    ),
                    ("type".to_owned(), "CRITICAL".into()),
                    ("bg".to_owned(), "FF0000".into()),
                ]),
                RenderValue::object(vec![
                    ("date".to_owned(), "2015-03-11".into()),
                    ("desc".to_owned(), RichText::text("Just a warning").into()),
                    ("type".to_owned(), "WARNING".into()),
                    ("bg".to_owned(), "FFDD00".into()),
                ]),
                RenderValue::object(vec![
                    ("date".to_owned(), "2015-03-12".into()),
                    ("desc".to_owned(), RichText::text("Information").into()),
                    ("type".to_owned(), "INFO".into()),
                    ("bg".to_owned(), "8888FF".into()),
                ]),
                RenderValue::object(vec![
                    ("date".to_owned(), "2015-03-13".into()),
                    ("desc".to_owned(), RichText::text("Debug trace").into()),
                    ("type".to_owned(), "DEBUG".into()),
                    ("bg".to_owned(), "FF00FF".into()),
                ]),
            ]);
            ctx.insert("alerts", alerts);
        }
        "p7b_richtext_if" => {
            ctx.insert(
                "foobar",
                RichText::text_with("Foobar!", &props(|p| p.color = Some("ff0000".to_owned()))),
            );
        }
        "p7b_eastasia" => {
            // The eastAsia: prefix maps to rFonts w:eastAsia (three font-name spellings).
            ctx.insert(
                "example",
                RichText::text_with(
                    "TestTEST",
                    &props(|p| p.font = Some("eastAsia:Microsoft YaHei".to_owned())),
                ),
            );
            ctx.insert(
                "Chinese",
                RichText::text_with(
                    "TestTEST",
                    &props(|p| p.font = Some("eastAsia:Microsoft YaHei".to_owned())),
                ),
            );
            ctx.insert(
                "simsun",
                RichText::text_with(
                    "TestTEST",
                    &props(|p| p.font = Some("eastAsia:SimSun".to_owned())),
                ),
            );
        }
        other => panic!("unimplemented python context fixture: {other}"),
    }
    ctx
}

/// Invoke compare.py and return (successful exit?, stdout).
fn run_compare(expected: &Path, actual: &Path) -> (bool, String) {
    let script = oracle_dir().join("compare.py");
    let output = Command::new(python())
        .arg(&script)
        .arg(expected)
        .arg(actual)
        .output()
        .expect("failed to start python; install Python or set DOCXTPL_PYTHON to an interpreter");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let ok = output.status.success() || stdout.trim() == "MATCH";
    (ok, stdout + &String::from_utf8_lossy(&output.stderr))
}

#[test]
fn oracle_differential_render_fixtures() {
    let root = fixtures_root();
    let manifest = load_json(&root.join("manifest.json"));
    let report = load_json(&oracle_dir().join("expected").join("report.json"));

    let fixtures = manifest["fixtures"]
        .as_array()
        .expect("manifest.fixtures is not an array");
    let report_fixtures = report["fixtures"]
        .as_array()
        .expect("report.fixtures is not an array");

    assert_eq!(
        manifest["upstream"]["docxtpl"], report["versions"]["docxtpl"],
        "the manifest and the oracle report must pin the same docxtpl version"
    );

    // Counts are derived dynamically from the manifest; the gate verifies
    // the schema, phase coverage and the one-to-one correspondence with the
    // report, so adding a fixture requires no hard-coded constant changes.
    let mut manifest_ids = BTreeSet::new();
    let mut phase_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for fixture in fixtures {
        let id = fixture["id"].as_str().expect("fixture.id must be a string");
        assert!(
            manifest_ids.insert(id),
            "duplicate manifest fixture id: {id}"
        );
        let phase = fixture["phase"]
            .as_str()
            .unwrap_or_else(|| panic!("{id}: phase must be a string"));
        let mode = fixture["mode"]
            .as_str()
            .unwrap_or_else(|| panic!("{id}: mode must be a string"));
        match mode {
            "roundtrip" => assert_eq!(phase, "P1", "{id}: roundtrip must belong to P1"),
            "render" => assert!(
                matches!(phase, "P2" | "P3" | "P4" | "P5" | "P6" | "P7"),
                "{id}: invalid render phase: {phase}"
            ),
            other => panic!("{id}: unknown mode {other}"),
        }
        let expected = fixture["expected"]
            .as_str()
            .unwrap_or_else(|| panic!("{id}: expected must be a string"));
        assert!(
            matches!(expected, "ok" | "error"),
            "{id}: unknown expected {expected}"
        );
        *phase_counts.entry(phase).or_default() += 1;

        let deviations = fixture["known_deviations"]
            .as_array()
            .unwrap_or_else(|| panic!("{id}: known_deviations must be an array"));
        for deviation in deviations {
            let value = deviation
                .as_str()
                .unwrap_or_else(|| panic!("{id}: known_deviations items must be strings"));
            let digits = value.strip_prefix("DEV-").unwrap_or("");
            assert!(
                digits.len() == 4 && digits.bytes().all(|byte| byte.is_ascii_digit()),
                "{id}: invalid deviation id {value}"
            );
        }
    }
    for phase in ["P1", "P2", "P3", "P4", "P5", "P6", "P7"] {
        assert!(
            phase_counts.get(phase).is_some_and(|count| *count > 0),
            "manifest is missing phase {phase}"
        );
    }

    let mut report_ids = BTreeSet::new();
    for fixture in report_fixtures {
        let id = fixture["id"]
            .as_str()
            .expect("report fixture.id must be a string");
        assert!(
            report_ids.insert(id),
            "duplicate oracle report fixture id: {id}"
        );
        let manifest_fixture = fixtures
            .iter()
            .find(|candidate| candidate["id"] == id)
            .unwrap_or_else(|| {
                panic!("the oracle report contains a fixture absent from the manifest: {id}")
            });
        assert_eq!(
            fixture["mode"], manifest_fixture["mode"],
            "{id}: report/manifest mode mismatch"
        );
        let expected = manifest_fixture["expected"]
            .as_str()
            .unwrap_or_else(|| panic!("{id}: manifest expected must be a string"));
        let status = fixture["status"]
            .as_str()
            .unwrap_or_else(|| panic!("{id}: report status must be a string"));
        assert_eq!(
            status, expected,
            "{id}: report.status must match manifest.expected exactly"
        );
    }
    assert_eq!(
        report_ids, manifest_ids,
        "the oracle report must correspond one-to-one with the manifest fixture set"
    );

    let render_ids: Vec<&str> = fixtures
        .iter()
        .filter(|fx| fx["mode"] == "render")
        .map(|fx| fx["id"].as_str().unwrap())
        .collect();
    assert!(
        !render_ids.is_empty(),
        "the manifest defines no render fixture"
    );

    let actual_parent = project_target_dir().join("oracle-actual");
    std::fs::create_dir_all(&actual_parent).unwrap_or_else(|error| {
        panic!(
            "failed to create the in-project oracle output directory {}: {error}",
            actual_parent.display()
        )
    });
    let actual_dir = tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(&actual_parent)
        .unwrap_or_else(|error| panic!("failed to create the oracle run directory: {error}"));

    let mut matched = 0usize;
    let mut error_matched = 0usize;
    let mut failures = Vec::new();

    for id in render_ids {
        let fx = fixtures
            .iter()
            .find(|f| f["id"] == id)
            .expect("the manifest entry exists");
        let template_path = root.join(
            fx["template"]
                .as_str()
                .unwrap()
                .replace('/', std::path::MAIN_SEPARATOR_STR),
        );

        let expected_error_type = report["fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["id"] == id)
            .and_then(|f| f["error_type"].as_str())
            .map(str::to_string);

        let is_python_context = fx
            .get("context_kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "python");

        let tpl = DocxTemplate::open(&template_path)
            .unwrap_or_else(|e| panic!("{id}: failed to open the template: {e}"));

        // P7: skip_render=true mirrors upstream saving directly without render
        // (build_context still runs first to register replace_*, then
        // finish_without_render is used).
        let skip_render = fx
            .get("skip_render")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let autoescape = fx
            .get("autoescape")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let options = RenderOptions::compat().with_autoescape(autoescape);

        // Python contexts go through the rich-content render session
        // (build_url_id is pre-registered inside the session); all others
        // use the plain JSON path.
        let rendered = if is_python_context {
            let mut session = tpl
                .render_session(&options)
                .unwrap_or_else(|e| panic!("{id}: failed to start the render session: {e}"));
            let ctx = build_python_context(id, &root, &tpl, &mut session);
            if skip_render {
                session.finish_without_render()
            } else {
                session.finish(&ctx)
            }
        } else {
            let ctx = match fx.get("context").and_then(Value::as_str) {
                Some(rel) => load_json(&root.join(rel)),
                None => Value::Object(serde_json::Map::new()),
            };
            tpl.render(&ctx, &options)
        };

        if let Some(expected_type) = expected_error_type {
            // Error expectation: compare only the stable error category.
            match rendered {
                Err(err) => {
                    let actual = err
                        .kind()
                        .map(TemplateErrorKind::oracle_exception)
                        .unwrap_or("(xml/io error)");
                    if actual == expected_type {
                        error_matched += 1;
                    } else {
                        failures.push(format!(
                            "{id}: error category mismatch oracle={expected_type} rust={actual} msg={err}"
                        ));
                    }
                }
                Ok(_) => failures.push(format!(
                    "{id}: oracle raised {expected_type} but Rust rendered successfully"
                )),
            }
            continue;
        }

        let doc = match rendered {
            Ok(doc) => doc,
            Err(e) => {
                failures.push(format!("{id}: Rust rendering failed unexpectedly: {e:?}"));
                continue;
            }
        };

        let tmp = actual_dir.path().join(format!("{id}.docx"));
        doc.save(&tmp)
            .unwrap_or_else(|e| panic!("{id}: failed to write the temporary file: {e}"));
        let expected = oracle_dir().join("expected").join(format!("{id}.docx"));
        let (ok, detail) = run_compare(&expected, &tmp);
        if ok {
            matched += 1;
        } else {
            failures.push(format!("{id}: differential mismatch\n{detail}"));
        }
    }

    eprintln!(
        "oracle differential: successful semantic matches {matched}, error-category matches {error_matched}, failures {}",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "{} fixtures differ from the oracle:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
