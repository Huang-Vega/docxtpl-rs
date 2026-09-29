use std::io::Cursor;

use docxtpl_rs::{
    DocxTemplate, InlineImage, MediaCompression, PackageLimits, RenderContext, RenderOptions,
    WriteOptions,
};
use zip::{CompressionMethod, ZipArchive};

const TEMPLATE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/templates/p4_img_png.docx"
);
const IMAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/media/p4_dot2x1.png"
);
const BMP_IMAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/media/p4_brick4x2.bmp"
);

fn rendered_bytes(
    media_compression: MediaCompression,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    rendered_image_bytes(media_compression, IMAGE)
}

fn rendered_image_bytes(
    media_compression: MediaCompression,
    image: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut context = RenderContext::new();
    context.insert("img", InlineImage::from_path(image, None, None, None)?);
    let document = template.render_ctx(&context, &RenderOptions::compat())?;
    let options = WriteOptions::compatible().with_media_compression(media_compression);
    Ok(document.to_bytes_with_options(&options)?)
}

fn compression_of(bytes: &[u8], name: &str) -> CompressionMethod {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("output should be a ZIP");
    let compression = archive
        .by_name(name)
        .expect("expected ZIP entry")
        .compression();
    compression
}

#[test]
fn media_compression_policy_is_explicit_and_outputs_valid_packages(
) -> Result<(), Box<dyn std::error::Error>> {
    let compatible = rendered_bytes(MediaCompression::Compatible)?;
    let fast = rendered_bytes(MediaCompression::FastDeflate)?;
    let stored = rendered_bytes(MediaCompression::Stored)?;
    let auto_png = rendered_bytes(MediaCompression::Auto)?;
    let auto_bmp = rendered_image_bytes(MediaCompression::Auto, BMP_IMAGE)?;

    assert_eq!(
        compression_of(&compatible, "word/media/image1.png"),
        CompressionMethod::Deflated
    );
    assert_eq!(
        compression_of(&fast, "word/media/image1.png"),
        CompressionMethod::Deflated
    );
    assert_eq!(
        compression_of(&stored, "word/media/image1.png"),
        CompressionMethod::Stored
    );
    assert_eq!(
        compression_of(&stored, "word/document.xml"),
        CompressionMethod::Deflated
    );
    assert_eq!(
        compression_of(&auto_png, "word/media/image1.png"),
        CompressionMethod::Stored
    );
    assert_eq!(
        compression_of(&auto_bmp, "word/media/image1.bmp"),
        CompressionMethod::Deflated
    );

    for bytes in [&compatible, &fast, &stored, &auto_png, &auto_bmp] {
        let package =
            docxtpl_opc::Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
        package.validate()?;
    }
    Ok(())
}

#[test]
fn lazy_image_is_streamed_and_source_changes_are_rejected() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::Builder::new()
        .prefix("docxtpl-lazy-image-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
    let image_path = directory.path().join("source.png");
    std::fs::copy(IMAGE, &image_path)?;

    let template = DocxTemplate::open(TEMPLATE)?;
    let mut context = RenderContext::new();
    let image = InlineImage::from_path_lazy(
        image_path.to_str().expect("temporary path is UTF-8"),
        None,
        None,
        None,
    )?;
    assert!(image.blob.is_empty());
    context.insert("img", image);
    let document =
        template.render_ctx(&context, &RenderOptions::compat().with_image_parallelism(2))?;
    let bytes = document.to_bytes_with_options(
        &WriteOptions::compatible().with_media_compression(MediaCompression::Auto),
    )?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut media = Vec::new();
    std::io::Read::read_to_end(&mut archive.by_name("word/media/image1.png")?, &mut media)?;
    assert_eq!(media, std::fs::read(&image_path)?);

    let template = DocxTemplate::open(TEMPLATE)?;
    let mut context = RenderContext::new();
    context.insert(
        "img",
        InlineImage::from_path_lazy(
            image_path.to_str().expect("temporary path is UTF-8"),
            None,
            None,
            None,
        )?,
    );
    let document = template.render_ctx(&context, &RenderOptions::compat())?;
    let original_len = std::fs::metadata(&image_path)?.len() as usize;
    std::fs::write(&image_path, vec![0x55; original_len])?;
    let error = document
        .to_bytes_with_options(
            &WriteOptions::compatible().with_media_compression(MediaCompression::Auto),
        )
        .expect_err("a changed lazy source must fail serialization");
    assert!(error.to_string().contains("source changed"), "{error}");
    Ok(())
}
