//! InlineImage: inline image values and `wp:inline` XML generation.
//!
//! Aligned with docxtpl 0.20.2 `inline_image.py` (split-run wrapping + `new_pic_inline`) and
//! the python-docx 1.2.0 pretty-serialization probe (2-space-per-level indentation), with
//! forward support for the `title` / `descr` accessibility metadata already merged in upstream
//! master.

use std::borrow::Cow;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::image::{probe, py_round, ImageDigest, ImageError, ImageInfo};
use sha1::{Digest, Sha1};

const MAX_INLINE_IMAGE_XML_BYTES: usize = 64 * 1024 * 1024;

/// Inline image value (the data portion of upstream `InlineImage`).
///
/// `width` / `height` are in EMU (upstream accepts `Length` objects, converted by the
/// constructor); `anchor` is an external link URL (its relationship rId is allocated by the
/// relationship registry at render time; see ADR-005).
#[derive(Debug, Clone)]
pub struct InlineImage {
    /// File path given by the caller (used only to derive the basename).
    pub path: String,
    /// Image bytes (read at construction or supplied explicitly). This is
    /// empty for images created with [`InlineImage::from_path_lazy`].
    pub blob: Vec<u8>,
    /// Width in EMU; `None` means use the native size or derive it from the height.
    pub width: Option<i64>,
    /// Height in EMU.
    pub height: Option<i64>,
    /// External link URL; `None` means no anchor.
    pub anchor: Option<String>,
    /// Image title; `Some("")` also writes an empty attribute, per upstream master.
    pub title: Option<String>,
    /// Image alternative description; `Some("")` also writes an empty attribute, per upstream
    /// master.
    pub descr: Option<String>,
    lazy_file: Option<LazyImageFile>,
}

/// Snapshot of a path-backed image used by [`InlineImage::from_path_lazy`].
#[derive(Debug, Clone)]
pub struct LazyImageFile {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
}

impl LazyImageFile {
    /// Source path captured when the lazy image was constructed.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// File length captured when the lazy image was constructed.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the captured file was empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Modification timestamp captured when the lazy image was constructed.
    pub fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    fn validate_metadata(&self, metadata: &std::fs::Metadata) -> std::io::Result<()> {
        let modified = metadata.modified().ok();
        if metadata.len() != self.len || modified != self.modified {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("lazy image source changed: {}", self.path.display()),
            ));
        }
        Ok(())
    }
}

/// Failure while loading or probing an inline image.
#[derive(Debug, thiserror::Error)]
pub enum InlineImageLoadError {
    /// The path-backed image could not be opened or changed after construction.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The image format or metadata is invalid.
    #[error(transparent)]
    Image(#[from] ImageError),
}

impl InlineImage {
    /// Constructs from a file path: reads the image bytes (upstream reads them at render time
    /// via `open(path,'rb')`).
    ///
    /// Returns [`std::io::Error`] when the file does not exist or cannot be read.
    pub fn from_path(
        path: &str,
        width: Option<i64>,
        height: Option<i64>,
        anchor: Option<String>,
    ) -> std::io::Result<Self> {
        let blob = std::fs::read(path)?;
        Ok(Self::from_bytes(path, blob, width, height, anchor))
    }

    /// Constructs a path-backed image without reading its contents.
    ///
    /// The file length and modification timestamp are captured immediately and
    /// validated whenever the image is read. The caller must keep the file
    /// unchanged until the rendered document has been written.
    pub fn from_path_lazy(
        path: &str,
        width: Option<i64>,
        height: Option<i64>,
        anchor: Option<String>,
    ) -> std::io::Result<Self> {
        Self::from_path_lazy_path(path, width, height, anchor)
    }

    /// Constructs a path-backed image from an arbitrary platform path without
    /// reading its contents.
    pub fn from_path_lazy_path(
        path: impl AsRef<Path>,
        width: Option<i64>,
        height: Option<i64>,
        anchor: Option<String>,
    ) -> std::io::Result<Self> {
        let path = path.as_ref();
        let metadata = std::fs::metadata(path)?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "lazy image source is not a regular file: {}",
                    path.display()
                ),
            ));
        }
        Ok(Self {
            path: path.to_string_lossy().into_owned(),
            blob: Vec::new(),
            width,
            height,
            anchor,
            title: None,
            descr: None,
            lazy_file: Some(LazyImageFile {
                path: path.to_path_buf(),
                len: metadata.len(),
                modified: metadata.modified().ok(),
            }),
        })
    }

    /// Constructs from in-memory bytes; `path_name` still participates in basename computation.
    pub fn from_bytes(
        path_name: &str,
        blob: Vec<u8>,
        width: Option<i64>,
        height: Option<i64>,
        anchor: Option<String>,
    ) -> Self {
        Self {
            path: path_name.to_owned(),
            blob,
            width,
            height,
            anchor,
            title: None,
            descr: None,
            lazy_file: None,
        }
    }

    /// Returns the path snapshot for a lazy image.
    pub fn lazy_file(&self) -> Option<&LazyImageFile> {
        self.lazy_file.as_ref()
    }

    /// Source byte length used for bounded parallel-probe scheduling.
    pub fn source_len(&self) -> u64 {
        self.lazy_file
            .as_ref()
            .map_or(self.blob.len() as u64, LazyImageFile::len)
    }

    /// Reads lazy image bytes or borrows the bytes of an eager image.
    pub fn bytes(&self) -> std::io::Result<Cow<'_, [u8]>> {
        let Some(source) = &self.lazy_file else {
            return Ok(Cow::Borrowed(&self.blob));
        };
        let before = std::fs::metadata(&source.path)?;
        source.validate_metadata(&before)?;
        let bytes = std::fs::read(&source.path)?;
        let after = std::fs::metadata(&source.path)?;
        source.validate_metadata(&after)?;
        if bytes.len() as u64 != source.len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("lazy image source changed: {}", source.path.display()),
            ));
        }
        Ok(Cow::Owned(bytes))
    }

    /// Loads, probes, and hashes this image without retaining lazy file bytes.
    pub fn probe_with_digest(&self) -> Result<(ImageInfo, ImageDigest), InlineImageLoadError> {
        match self.probe_with_digest_interruptible(&|| false)? {
            Some(result) => Ok(result),
            None => unreachable!("non-interruptible image probe cannot be cancelled"),
        }
    }

    /// Loads, probes, and hashes this image with a cancellation checkpoint
    /// between file or in-memory chunks. `Ok(None)` means cancellation.
    pub fn probe_with_digest_interruptible(
        &self,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<Option<(ImageInfo, ImageDigest)>, InlineImageLoadError> {
        let Some(source) = &self.lazy_file else {
            return Ok(crate::image::probe_with_digest_interruptible(
                &self.blob,
                should_cancel,
            )?);
        };
        let before = std::fs::metadata(&source.path)?;
        source.validate_metadata(&before)?;
        let mut file = File::open(&source.path)?;
        let mut hasher = Sha1::new();
        let mut header = Vec::new();
        let mut info = None;
        let mut total = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            if should_cancel() {
                return Ok(None);
            }
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            total = total.saturating_add(read as u64);
            if total > source.len {
                return Err(changed_source_error(&source.path).into());
            }
            hasher.update(&buffer[..read]);
            if info.is_none() {
                header.extend_from_slice(&buffer[..read]);
                info = probe(&header).ok();
            }
        }
        let after = std::fs::metadata(&source.path)?;
        source.validate_metadata(&after)?;
        if total != source.len {
            return Err(changed_source_error(&source.path).into());
        }
        let digest: ImageDigest = hasher.finalize().into();
        let mut info = info.ok_or(ImageError::Unrecognized)?;
        info.sha1 = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        if should_cancel() {
            return Ok(None);
        }
        Ok(Some((info, digest)))
    }

    /// Sets image accessibility metadata.
    ///
    /// This capability comes from Python docxtpl master (after 0.20.2); the attributes are
    /// written to both `wp:docPr` and `pic:cNvPr`. `Some("")` is preserved because upstream
    /// decides whether to write based on `is not None`, not on string truthiness.
    #[must_use]
    pub fn with_accessibility(
        mut self,
        title: Option<String>,
        description: Option<String>,
    ) -> Self {
        self.title = title;
        self.descr = description;
        self
    }

    /// File basename (including extension), matching the value semantics of
    /// `os.path.basename`.
    pub fn filename(&self) -> String {
        std::path::Path::new(&self.path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

fn changed_source_error(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("lazy image source changed: {}", path.display()),
    )
}

/// Escapes text per lxml's serialization rules for double-quoted XML attributes.
fn escape_xml_attr(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '"' => escaped.push_str("&quot;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '\t' => escaped.push_str("&#9;"),
            '\n' => escaped.push_str("&#10;"),
            '\r' => escaped.push_str("&#13;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn escaped_xml_attr_len(value: &str) -> Result<usize, ImageError> {
    value.chars().try_fold(0usize, |total, ch| {
        let valid_xml_char = matches!(
            ch,
            '\t' | '\n' | '\r'
                | '\u{20}'..='\u{d7ff}'
                | '\u{e000}'..='\u{fffd}'
                | '\u{10000}'..='\u{10ffff}'
        );
        if !valid_xml_char {
            return Err(ImageError::InvalidXmlMetadata);
        }
        let encoded = match ch {
            '&' => 5,
            '"' => 6,
            '<' | '>' => 4,
            '\t' | '\r' => 5,
            '\n' => 6,
            other => other.len_utf8(),
        };
        total
            .checked_add(encoded)
            .ok_or(ImageError::MetadataTooLarge {
                max: MAX_INLINE_IMAGE_XML_BYTES,
            })
    })
}

/// Native size in EMU: `int((px / dpi) * 914400)` (f64 divide → multiply → truncate toward
/// zero).
///
/// Upstream raises ZeroDivisionError on `dpi==0`; here it collapses into
/// [`ImageError::Unrecognized`].
fn native_emu(px: u32, dpi: u32) -> Result<i64, ImageError> {
    if dpi == 0 {
        return Err(ImageError::Unrecognized);
    }
    Ok(((f64::from(px) / f64::from(dpi)) * 914400.0) as i64)
}

/// Scaled dimensions (python-docx `Image.scaled_dimensions`, in EMU).
///
/// - `(None, None)` → native dimensions;
/// - when only one side is given, the other is derived from the aspect ratio, using Python
///   round's banker's rounding (see [`py_round`]);
/// - when both sides are given, they are used as-is with no aspect-ratio constraint.
///
/// Returns [`ImageError::Unrecognized`] when a divisor required for the conversion is zero
/// (the upstream ZeroDivisionError case).
pub fn scaled_dimensions(
    info: &ImageInfo,
    width: Option<i64>,
    height: Option<i64>,
) -> Result<(i64, i64), ImageError> {
    match (width, height) {
        (None, None) => {
            let native_w = native_emu(info.px_w, info.dpi_x)?;
            let native_h = native_emu(info.px_h, info.dpi_y)?;
            Ok((native_w, native_h))
        }
        (None, Some(h)) => {
            let native_w = native_emu(info.px_w, info.dpi_x)?;
            let native_h = native_emu(info.px_h, info.dpi_y)?;
            if native_h == 0 {
                return Err(ImageError::Unrecognized);
            }
            let factor = h as f64 / native_h as f64;
            let w = py_round(native_w as f64 * factor) as i64;
            Ok((w, h))
        }
        (Some(w), None) => {
            let native_w = native_emu(info.px_w, info.dpi_x)?;
            let native_h = native_emu(info.px_h, info.dpi_y)?;
            if native_w == 0 {
                return Err(ImageError::Unrecognized);
            }
            let factor = w as f64 / native_w as f64;
            let h = py_round(native_h as f64 * factor) as i64;
            Ok((w, h))
        }
        // Both sides given: use as-is
        (Some(w), Some(h)) => Ok((w, h)),
    }
}

/// All-in-one probe + size calculation + XML generation (the output of upstream
/// `InlineImage.__str__`).
///
/// - `shape_id`: the id of `wp:docPr` (computed once at render time against the original
///   pre-render document tree and passed in; see ADR-005);
/// - `blip_rid`: the image relationship ID for `a:blip r:embed`;
/// - `hyperlink_rid`: the relationship ID for the `anchor` external link; `None` means no
///   anchor, otherwise an `a:hlinkClick` is inserted under both `wp:docPr` and `pic:cNvPr`.
///
/// Returns [`ImageError::Unrecognized`] when the image bytes cannot be identified (or the
/// dimensions cannot be converted); invalid or oversized XML metadata returns the
/// corresponding metadata error.
pub fn render_inline_image(
    img: &InlineImage,
    shape_id: i64,
    blip_rid: &str,
    hyperlink_rid: Option<&str>,
) -> Result<String, ImageError> {
    let bytes = img.bytes().map_err(|_| ImageError::Unrecognized)?;
    let info = probe(&bytes)?;
    render_inline_image_with_info(img, &info, shape_id, blip_rid, hyperlink_rid)
}

/// Generates inline-image XML using image metadata that was already probed.
///
/// This avoids hashing and parsing the same image again when a package-level
/// registry also needs its format and SHA-1 digest for media deduplication.
pub fn render_inline_image_with_info(
    img: &InlineImage,
    info: &ImageInfo,
    shape_id: i64,
    blip_rid: &str,
    hyperlink_rid: Option<&str>,
) -> Result<String, ImageError> {
    let (cx, cy) = scaled_dimensions(info, img.width, img.height)?;
    let raw_filename = img.filename();
    let mut metadata_xml_bytes = escaped_xml_attr_len(&raw_filename)?;
    for value in [img.title.as_deref(), img.descr.as_deref()]
        .into_iter()
        .flatten()
    {
        let escaped_len = escaped_xml_attr_len(value)?;
        metadata_xml_bytes = metadata_xml_bytes
            .checked_add(escaped_len.saturating_mul(2))
            .ok_or(ImageError::MetadataTooLarge {
                max: MAX_INLINE_IMAGE_XML_BYTES,
            })?;
    }
    if metadata_xml_bytes > MAX_INLINE_IMAGE_XML_BYTES {
        return Err(ImageError::MetadataTooLarge {
            max: MAX_INLINE_IMAGE_XML_BYTES,
        });
    }
    let filename = escape_xml_attr(&raw_filename);
    let mut doc_pr_attrs = format!("id=\"{shape_id}\" name=\"Picture {shape_id}\"");
    let mut c_nv_pr_attrs = format!("id=\"0\" name=\"{filename}\"");
    for (name, value) in [
        ("title", img.title.as_deref()),
        ("descr", img.descr.as_deref()),
    ] {
        if let Some(value) = value {
            let value = escape_xml_attr(value);
            doc_pr_attrs.push_str(&format!(" {name}=\"{value}\""));
            c_nv_pr_attrs.push_str(&format!(" {name}=\"{value}\""));
        }
    }

    // Byte-for-byte identical to the upstream pretty-serialization probe (2 spaces per
    // level); with an anchor, docPr / cNvPr change from self-closing to carrying an
    // a:hlinkClick child.
    let doc_pr = match hyperlink_rid {
        Some(rid) => format!(
            "  <wp:docPr {doc_pr_attrs}>\n    <a:hlinkClick r:id=\"{rid}\"/>\n  </wp:docPr>"
        ),
        None => format!("  <wp:docPr {doc_pr_attrs}/>"),
    };
    let c_nv_pr = match hyperlink_rid {
        Some(rid) => format!(
            "          <pic:cNvPr {c_nv_pr_attrs}>\n            <a:hlinkClick r:id=\"{rid}\"/>\n          </pic:cNvPr>"
        ),
        None => format!("          <pic:cNvPr {c_nv_pr_attrs}/>"),
    };
    let xml = format!(
        "</w:t></w:r><w:r><w:drawing><wp:inline xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\n  <wp:extent cx=\"{cx}\" cy=\"{cy}\"/>\n{doc_pr}\n  <wp:cNvGraphicFramePr>\n    <a:graphicFrameLocks noChangeAspect=\"1\"/>\n  </wp:cNvGraphicFramePr>\n  <a:graphic>\n    <a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\n      <pic:pic>\n        <pic:nvPicPr>\n{c_nv_pr}\n          <pic:cNvPicPr/>\n        </pic:nvPicPr>\n        <pic:blipFill>\n          <a:blip r:embed=\"{blip_rid}\"/>\n          <a:stretch>\n            <a:fillRect/>\n          </a:stretch>\n        </pic:blipFill>\n        <pic:spPr>\n          <a:xfrm>\n            <a:off x=\"0\" y=\"0\"/>\n            <a:ext cx=\"{cx}\" cy=\"{cy}\"/>\n          </a:xfrm>\n          <a:prstGeom prst=\"rect\"/>\n        </pic:spPr>\n      </pic:pic>\n    </a:graphicData>\n  </a:graphic>\n</wp:inline>\n</w:drawing></w:r><w:r><w:t xml:space=\"preserve\">"
    );
    Ok(xml)
}

/// Generate a standalone `w:drawing` element for direct Story DOM insertion.
///
/// Unlike [`render_inline_image_with_info`], this does not contain split-run
/// sentinels. Both DrawingML non-visual property ids are caller supplied so a
/// Story-scoped allocator can keep inserted and cloned drawings unique.
pub fn render_inline_drawing_with_info(
    img: &InlineImage,
    info: &ImageInfo,
    doc_pr_id: u64,
    picture_id: u64,
    blip_rid: &str,
    hyperlink_rid: Option<&str>,
) -> Result<String, ImageError> {
    let (cx, cy) = scaled_dimensions(info, img.width, img.height)?;
    let raw_filename = img.filename();
    let mut metadata_xml_bytes = escaped_xml_attr_len(&raw_filename)?;
    for value in [img.title.as_deref(), img.descr.as_deref()]
        .into_iter()
        .flatten()
    {
        let escaped_len = escaped_xml_attr_len(value)?;
        metadata_xml_bytes = metadata_xml_bytes
            .checked_add(escaped_len.saturating_mul(2))
            .ok_or(ImageError::MetadataTooLarge {
                max: MAX_INLINE_IMAGE_XML_BYTES,
            })?;
    }
    if metadata_xml_bytes > MAX_INLINE_IMAGE_XML_BYTES {
        return Err(ImageError::MetadataTooLarge {
            max: MAX_INLINE_IMAGE_XML_BYTES,
        });
    }

    let filename = escape_xml_attr(&raw_filename);
    let mut doc_pr_attrs = format!("id=\"{doc_pr_id}\" name=\"Picture {doc_pr_id}\"");
    let mut c_nv_pr_attrs = format!("id=\"{picture_id}\" name=\"{filename}\"");
    for (name, value) in [
        ("title", img.title.as_deref()),
        ("descr", img.descr.as_deref()),
    ] {
        if let Some(value) = value {
            let value = escape_xml_attr(value);
            doc_pr_attrs.push_str(&format!(" {name}=\"{value}\""));
            c_nv_pr_attrs.push_str(&format!(" {name}=\"{value}\""));
        }
    }

    let doc_pr = match hyperlink_rid {
        Some(rid) => format!(
            "  <wp:docPr {doc_pr_attrs}>\n    <a:hlinkClick r:id=\"{rid}\"/>\n  </wp:docPr>"
        ),
        None => format!("  <wp:docPr {doc_pr_attrs}/>"),
    };
    let c_nv_pr = match hyperlink_rid {
        Some(rid) => format!(
            "          <pic:cNvPr {c_nv_pr_attrs}>\n            <a:hlinkClick r:id=\"{rid}\"/>\n          </pic:cNvPr>"
        ),
        None => format!("          <pic:cNvPr {c_nv_pr_attrs}/>"),
    };

    Ok(format!(
        "<w:drawing xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><wp:inline xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\n  <wp:extent cx=\"{cx}\" cy=\"{cy}\"/>\n{doc_pr}\n  <wp:cNvGraphicFramePr>\n    <a:graphicFrameLocks noChangeAspect=\"1\"/>\n  </wp:cNvGraphicFramePr>\n  <a:graphic>\n    <a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\n      <pic:pic>\n        <pic:nvPicPr>\n{c_nv_pr}\n          <pic:cNvPicPr/>\n        </pic:nvPicPr>\n        <pic:blipFill>\n          <a:blip r:embed=\"{blip_rid}\"/>\n          <a:stretch>\n            <a:fillRect/>\n          </a:stretch>\n        </pic:blipFill>\n        <pic:spPr>\n          <a:xfrm>\n            <a:off x=\"0\" y=\"0\"/>\n            <a:ext cx=\"{cx}\" cy=\"{cy}\"/>\n          </a:xfrm>\n          <a:prstGeom prst=\"rect\"/>\n        </pic:spPr>\n      </pic:pic>\n    </a:graphicData>\n  </a:graphic>\n</wp:inline></w:drawing>"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const PNG_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/media/p4_dot2x1.png"
    );
    const WIDE_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/media/p4_wide4x1.png"
    );
    const JPG_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/media/p4_rect4x2.jpg"
    );

    /// Expected output matching the upstream probe character for character (no anchor:
    /// shape_id=1, blip rId9).
    const EXPECTED_WITHOUT_ANCHOR: &str = "</w:t></w:r><w:r><w:drawing><wp:inline xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\n  <wp:extent cx=\"25400\" cy=\"12700\"/>\n  <wp:docPr id=\"1\" name=\"Picture 1\"/>\n  <wp:cNvGraphicFramePr>\n    <a:graphicFrameLocks noChangeAspect=\"1\"/>\n  </wp:cNvGraphicFramePr>\n  <a:graphic>\n    <a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\n      <pic:pic>\n        <pic:nvPicPr>\n          <pic:cNvPr id=\"0\" name=\"p4_dot2x1.png\"/>\n          <pic:cNvPicPr/>\n        </pic:nvPicPr>\n        <pic:blipFill>\n          <a:blip r:embed=\"rId9\"/>\n          <a:stretch>\n            <a:fillRect/>\n          </a:stretch>\n        </pic:blipFill>\n        <pic:spPr>\n          <a:xfrm>\n            <a:off x=\"0\" y=\"0\"/>\n            <a:ext cx=\"25400\" cy=\"12700\"/>\n          </a:xfrm>\n          <a:prstGeom prst=\"rect\"/>\n        </pic:spPr>\n      </pic:pic>\n    </a:graphicData>\n  </a:graphic>\n</wp:inline>\n</w:drawing></w:r><w:r><w:t xml:space=\"preserve\">";

    /// Expected output matching the upstream probe character for character (anchor:
    /// hyperlink rId10).
    const EXPECTED_WITH_ANCHOR: &str = "</w:t></w:r><w:r><w:drawing><wp:inline xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\n  <wp:extent cx=\"25400\" cy=\"12700\"/>\n  <wp:docPr id=\"1\" name=\"Picture 1\">\n    <a:hlinkClick r:id=\"rId10\"/>\n  </wp:docPr>\n  <wp:cNvGraphicFramePr>\n    <a:graphicFrameLocks noChangeAspect=\"1\"/>\n  </wp:cNvGraphicFramePr>\n  <a:graphic>\n    <a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\n      <pic:pic>\n        <pic:nvPicPr>\n          <pic:cNvPr id=\"0\" name=\"p4_dot2x1.png\">\n            <a:hlinkClick r:id=\"rId10\"/>\n          </pic:cNvPr>\n          <pic:cNvPicPr/>\n        </pic:nvPicPr>\n        <pic:blipFill>\n          <a:blip r:embed=\"rId9\"/>\n          <a:stretch>\n            <a:fillRect/>\n          </a:stretch>\n        </pic:blipFill>\n        <pic:spPr>\n          <a:xfrm>\n            <a:off x=\"0\" y=\"0\"/>\n            <a:ext cx=\"25400\" cy=\"12700\"/>\n          </a:xfrm>\n          <a:prstGeom prst=\"rect\"/>\n        </pic:spPr>\n      </pic:pic>\n    </a:graphicData>\n  </a:graphic>\n</wp:inline>\n</w:drawing></w:r><w:r><w:t xml:space=\"preserve\">";

    #[test]
    fn native_size_no_scale_args() -> TestResult {
        let info = probe(&std::fs::read(PNG_PATH)?)?;
        assert_eq!(scaled_dimensions(&info, None, None)?, (25400, 12700));
        Ok(())
    }

    #[test]
    fn wide_image_150dpi_native_and_width_scaling() -> TestResult {
        let info = probe(&std::fs::read(WIDE_PATH)?)?;
        // Native: int((4/150)*914400) x int((1/150)*914400)
        assert_eq!(scaled_dimensions(&info, None, None)?, (24384, 6096));
        // Width only: the other side uses aspect ratio with banker's rounding
        assert_eq!(
            scaled_dimensions(&info, Some(720000), None)?,
            (720000, 180000)
        );
        Ok(())
    }

    #[test]
    fn jpg_native_and_both_sides_scaling() -> TestResult {
        let info = probe(&std::fs::read(JPG_PATH)?)?;
        // APP0 units=1, density=300: native int((4/300)*914400) x int((2/300)*914400)
        assert_eq!(scaled_dimensions(&info, None, None)?, (12192, 6096));
        assert_eq!(
            scaled_dimensions(&info, Some(360000), None)?,
            (360000, 180000)
        );
        // Both sides given: use as-is, with no aspect-ratio constraint
        assert_eq!(
            scaled_dimensions(&info, Some(360000), Some(108000))?,
            (360000, 108000)
        );
        // Height only: symmetric derivation
        assert_eq!(
            scaled_dimensions(&info, None, Some(108000))?,
            (216000, 108000)
        );
        Ok(())
    }

    #[test]
    fn tiff_native_size() -> TestResult {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/media/p4_tile4x2.tiff"
        );
        let info = probe(&std::fs::read(path)?)?;
        assert_eq!(scaled_dimensions(&info, None, None)?, (24384, 12192));
        Ok(())
    }

    #[test]
    fn zero_dpi_returns_unrecognized() {
        let info = ImageInfo {
            sha1: String::new(),
            px_w: 4,
            px_h: 2,
            dpi_x: 0,
            dpi_y: 72,
            ext: "png",
            content_type: "image/png",
        };
        assert!(matches!(
            scaled_dimensions(&info, None, None),
            Err(ImageError::Unrecognized)
        ));
        // With both sides given the native size is not computed, so zero dpi does not matter
        assert_eq!(
            scaled_dimensions(&info, Some(10), Some(5)).ok(),
            Some((10, 5))
        );
    }

    #[test]
    fn from_path_reads_bytes_and_uses_basename() -> TestResult {
        let img = InlineImage::from_path(PNG_PATH, None, None, None)?;
        assert_eq!(img.filename(), "p4_dot2x1.png");
        assert_eq!(img.blob, std::fs::read(PNG_PATH)?);
        // When the path contains directory separators, only the final segment is taken
        let img = InlineImage::from_bytes("some/dir/p4_dot2x1.png", vec![], None, None, None);
        assert_eq!(img.filename(), "p4_dot2x1.png");
        Ok(())
    }

    #[test]
    fn lazy_path_probe_matches_eager_without_retaining_bytes() -> TestResult {
        let eager = InlineImage::from_path(PNG_PATH, None, None, None)?;
        let lazy = InlineImage::from_path_lazy(PNG_PATH, None, None, None)?;
        assert!(lazy.blob.is_empty());
        assert_eq!(lazy.filename(), eager.filename());
        assert_eq!(lazy.probe_with_digest()?, eager.probe_with_digest()?);
        Ok(())
    }

    #[test]
    fn render_no_anchor_byte_matches_upstream() -> TestResult {
        let img = InlineImage::from_path(PNG_PATH, None, None, None)?;
        let xml = render_inline_image(&img, 1, "rId9", None)?;
        assert_eq!(xml, EXPECTED_WITHOUT_ANCHOR);
        Ok(())
    }

    #[test]
    fn preprobed_render_matches_public_render_entry() -> TestResult {
        let img = InlineImage::from_path(PNG_PATH, None, None, None)?;
        let info = probe(&img.blob)?;
        assert_eq!(
            render_inline_image(&img, 1, "rId9", None)?,
            render_inline_image_with_info(&img, &info, 1, "rId9", None)?
        );
        Ok(())
    }

    #[test]
    fn special_filename_escaped_per_xml_attribute_rules() -> TestResult {
        let img = InlineImage::from_bytes(
            r#"some/dir/a&"<>'b.png"#,
            std::fs::read(PNG_PATH)?,
            None,
            None,
            None,
        );
        let xml = render_inline_image(&img, 1, "rId9", None)?;
        assert!(xml.contains(r#"name="a&amp;&quot;&lt;&gt;'b.png""#));
        assert_eq!(escape_xml_attr("a\tb\nc\rd"), "a&#9;b&#10;c&#13;d");
        Ok(())
    }

    #[test]
    fn render_with_anchor() -> TestResult {
        let img =
            InlineImage::from_path(PNG_PATH, None, None, Some("https://example.com/".into()))?;
        let xml = render_inline_image(&img, 1, "rId9", Some("rId10"))?;
        assert_eq!(xml, EXPECTED_WITH_ANCHOR);
        Ok(())
    }

    #[test]
    fn render_accessibility_attributes_written_in_both_places_and_escaped() -> TestResult {
        let img = InlineImage::from_path(PNG_PATH, None, None, None)?
            .with_accessibility(Some("pic & \"title\"".into()), Some(String::new()));
        let xml = render_inline_image(&img, 1, "rId9", None)?;
        let attrs = r#"title="pic &amp; &quot;title&quot;" descr="""#;
        assert_eq!(xml.matches(attrs).count(), 2);
        assert!(xml.contains(&format!(r#"<wp:docPr id="1" name="Picture 1" {attrs}/>"#)));
        assert!(xml.contains(&format!(
            r#"<pic:cNvPr id="0" name="p4_dot2x1.png" {attrs}/>"#
        )));
        Ok(())
    }

    #[test]
    fn render_accessibility_attributes_rejects_xml_illegal_chars() -> TestResult {
        let img = InlineImage::from_path(PNG_PATH, None, None, None)?
            .with_accessibility(Some("bad\u{000b}title".into()), None);
        assert!(matches!(
            render_inline_image(&img, 1, "rId9", None),
            Err(ImageError::InvalidXmlMetadata)
        ));
        Ok(())
    }

    // Test names keep the original OOXML term (wp:extent), which is not snake_case;
    // this allow can be removed if the project adopts English snake_case test naming.
    #[allow(non_snake_case)]
    #[test]
    fn render_scaled_by_width_extent_uses_scaled_values() -> TestResult {
        // Corresponds to upstream InlineImage(tpl, p4_wide4x1.png, width=Mm(20)):
        // cx=720000, cy=180000
        let img = InlineImage::from_path(WIDE_PATH, Some(720000), None, None)?;
        let xml = render_inline_image(&img, 1, "rId9", None)?;
        assert_eq!(xml.matches("cx=\"720000\"").count(), 2);
        assert_eq!(xml.matches("cy=\"180000\"").count(), 2);
        assert!(xml.contains("name=\"p4_wide4x1.png\""));
        Ok(())
    }

    #[test]
    fn render_bad_image_returns_unrecognized() -> TestResult {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/media/p4_bad.png"
        );
        let img = InlineImage::from_path(path, None, None, None)?;
        assert!(matches!(
            render_inline_image(&img, 1, "rId9", None),
            Err(ImageError::Unrecognized)
        ));
        Ok(())
    }
}
