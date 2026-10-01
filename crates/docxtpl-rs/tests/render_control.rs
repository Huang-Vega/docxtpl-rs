mod test_support;

use std::fs;
use std::time::{Duration, Instant};

use docxtpl_rs::{
    DocxTemplate, FailurePolicy, RenderControl, RenderControlError, RenderLimits, RenderOptions,
    RenderStopReason,
};
use serde_json::json;

const TEMPLATE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/templates/r2_var_basic.docx"
);

#[test]
fn render_control_distinguishes_deadline_and_cancellation() -> Result<(), Box<dyn std::error::Error>>
{
    let template = DocxTemplate::open(TEMPLATE)?;
    let expired = RenderControl::new().with_timeout(Duration::ZERO);
    assert_eq!(
        expired.stop_reason(),
        Some(RenderStopReason::DeadlineExceeded)
    );
    assert!(matches!(
        template
            .render_with_control(&json!({"name": "late"}), &RenderOptions::compat(), &expired)
            .unwrap_err(),
        RenderControlError::DeadlineExceeded
    ));

    let cancelled = RenderControl::new().with_deadline(Instant::now());
    cancelled.cancel();
    assert_eq!(cancelled.stop_reason(), Some(RenderStopReason::Cancelled));
    assert!(matches!(
        template
            .render_with_control(
                &json!({"name": "cancelled"}),
                &RenderOptions::compat(),
                &cancelled,
            )
            .unwrap_err(),
        RenderControlError::Cancelled
    ));
    Ok(())
}

#[test]
fn future_deadline_and_render_limits_preserve_normal_rendering(
) -> Result<(), Box<dyn std::error::Error>> {
    let limits = RenderLimits::default().with_max_rendered_xml_bytes(8 * 1024 * 1024);
    let template = DocxTemplate::open_with_limits(TEMPLATE, limits)?;
    let control = RenderControl::new().with_timeout(Duration::from_secs(10));
    let bytes = template
        .render_with_control(
            &json!({"name": "controlled"}),
            &RenderOptions::compat(),
            &control,
        )?
        .to_bytes()?;
    assert!(!bytes.is_empty());
    assert_eq!(control.stop_reason(), None);
    Ok(())
}

#[test]
fn deadline_rolls_back_an_active_postprocess_pass() -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "original"}), &RenderOptions::compat())?;
    let control = RenderControl::new().with_timeout(Duration::from_millis(50));
    let error = document
        .postprocess_with_control(&control, |pipeline| {
            pipeline.pass("deadline", FailurePolicy::WarnAndRollback, |transaction| {
                transaction.set_part_bytes("word/document.xml", b"broken".to_vec())?;
                std::thread::sleep(Duration::from_millis(75));
                Ok(())
            })?;
            Ok(())
        })
        .expect_err("deadline must abort even with warning policy");
    assert!(matches!(error, RenderControlError::DeadlineExceeded));

    let bytes = document.to_bytes()?;
    let package = docxtpl_rs::Package::from_reader(
        std::io::Cursor::new(bytes),
        &docxtpl_rs::PackageLimits::default(),
    )?;
    let xml = std::str::from_utf8(package.part("word/document.xml").unwrap().bytes()?)?;
    assert!(xml.contains("original"));
    Ok(())
}

#[test]
fn expired_control_preserves_existing_atomic_save_target() -> Result<(), Box<dyn std::error::Error>>
{
    let temporary = tempfile::Builder::new()
        .prefix("render-control-")
        .tempdir_in(test_support::target_dir())?;
    let output = temporary.path().join("output.docx");
    fs::write(&output, b"previous")?;
    let document =
        DocxTemplate::open(TEMPLATE)?.render(&json!({"name": "new"}), &RenderOptions::compat())?;
    let control = RenderControl::new().with_timeout(Duration::ZERO);
    assert!(matches!(
        document.save_with_control(&output, &control).unwrap_err(),
        RenderControlError::DeadlineExceeded
    ));
    assert_eq!(fs::read(output)?, b"previous");
    Ok(())
}
