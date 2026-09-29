//! Image header parsing: field-by-field alignment with the probe-verified semantics of
//! python-docx 1.2.0 `docx/image/*`.
//!
//! - Signatures are matched in order (upstream `_ImageHeaderFactory`): PNG@0, JFIF@6, Exif@6,
//!   GIF87a/GIF89a@0, TIFF (MM/II)@0, BMP@0; if none match, returns [`ImageError::Unrecognized`].
//! - Upstream truncation/missing-segment exceptions (UnexpectedEndOfFileError, KeyError, etc.)
//!   are all collapsed into `Unrecognized` (a failed probe means the image cannot be identified).
//! - Wherever `int(round(x))` is used, Python's banker's rounding is applied (see [`py_round`]).

use sha1::{Digest, Sha1};

/// Binary SHA-1 digest used by package-level image deduplication.
pub type ImageDigest = [u8; 20];

/// Parsed image header (fields visible to the probe, aligned with python-docx `Image`).
///
/// # Examples
///
/// ```
/// // Unrecognized bytes return Unrecognized
/// assert!(matches!(
///     docxtpl_rich::probe(b"not an image"),
///     Err(docxtpl_rich::ImageError::Unrecognized)
/// ));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    /// SHA-1 of the full blob, lowercase hexadecimal (matches `hashlib.sha1(...).hexdigest()`).
    pub sha1: String,
    /// Width in pixels.
    pub px_w: u32,
    /// Height in pixels.
    pub px_h: u32,
    /// Horizontal dpi. Per-format default when the header omits it (72 for PNG/JPEG/GIF/TIFF, 96 for BMP).
    pub dpi_x: u32,
    /// Vertical dpi.
    pub dpi_y: u32,
    /// Default file extension (upstream `default_ext`; note JPEG is `jpg`, not `jpeg`).
    pub ext: &'static str,
    /// MIME content type.
    pub content_type: &'static str,
}

/// Image parsing error.
///
/// Upstream (python-docx) raises various exceptions such as UnrecognizedImageError /
/// UnexpectedEndOfFileError / KeyError when a signature does not match, the header is truncated,
/// or a required segment is missing; per the P4 convention this crate collapses all such
/// **image parsing** failures into `Unrecognized`; the other variants validate InlineImage XML
/// metadata and enforce resource budgets.
#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    /// Unrecognized image: the signature matches no supported format, or the header is unavailable.
    #[error("unrecognized image: no supported image signature matched (PNG/JPEG/GIF/TIFF/BMP) or header unavailable")]
    Unrecognized,
    /// The InlineImage XML attributes contain Unicode scalars forbidden by XML 1.0.
    #[error("image metadata contains characters not allowed in XML 1.0 attributes")]
    InvalidXmlMetadata,
    /// The escaped InlineImage XML attributes exceed the rendering budget.
    #[error("escaped image metadata exceeds the {max}-byte limit")]
    MetadataTooLarge {
        /// Maximum allowed size of the attribute XML in bytes.
        max: usize,
    },
}

/// Python `round(x)` banker's rounding (round-half-to-even).
///
/// [`f64::round`] cannot be used directly (it rounds half away from zero):
/// - Exactly x.5 rounds to the nearest even number (0.5 → 0, 1.5 → 2, 2.5 → 2, -1.5 → -2);
/// - Otherwise rounds to the nearest value.
///
/// # Examples
///
/// ```
/// assert_eq!(docxtpl_rich::py_round(0.5), 0.0);
/// assert_eq!(docxtpl_rich::py_round(1.5), 2.0);
/// assert_eq!(docxtpl_rich::py_round(2.5), 2.0);
/// assert_eq!(docxtpl_rich::py_round(-1.5), -2.0);
/// ```
#[must_use]
pub fn py_round(x: f64) -> f64 {
    let frac = x.fract().abs();
    if frac > 0.5 {
        // The farther side is nearer: round away from zero
        if x < 0.0 {
            x.floor()
        } else {
            x.ceil()
        }
    } else if frac < 0.5 {
        // The nearer side is closer: round toward zero
        if x < 0.0 {
            x.ceil()
        } else {
            x.floor()
        }
    } else {
        // Exactly x.5: round to even
        let lower = x.floor();
        if (lower as i64) % 2 == 0 {
            lower
        } else {
            lower + 1.0
        }
    }
}

/// Parses image bytes and returns sha1, pixel dimensions, dpi, extension, and content type.
///
/// Supported formats and field semantics align with python-docx 1.2.0: PNG (IHDR/pHYs chunks),
/// JPEG (first APP0/APP1 and first SOFn, per JFIF/Exif), GIF, BMP (BITMAPINFOHEADER),
/// TIFF (SHORT/LONG/RATIONAL entries in IFD0).
pub fn probe(blob: &[u8]) -> Result<ImageInfo, ImageError> {
    probe_with_digest(blob).map(|(info, _)| info)
}

/// Parses image bytes and returns both compatible metadata and the binary digest.
///
/// Callers that use SHA-1 as an internal map key can avoid decoding the
/// hexadecimal string stored in [`ImageInfo`].
pub fn probe_with_digest(blob: &[u8]) -> Result<(ImageInfo, ImageDigest), ImageError> {
    let digest = sha1_digest(blob);
    let info = probe_with_sha1(blob, digest_hex(&digest))?;
    Ok((info, digest))
}

fn probe_with_sha1(blob: &[u8], sha1: String) -> Result<ImageInfo, ImageError> {
    if has_signature(blob, 0, b"\x89PNG\r\n\x1a\n") {
        let (px_w, px_h, dpi_x, dpi_y) = parse_png(blob)?;
        return Ok(ImageInfo {
            sha1,
            px_w,
            px_h,
            dpi_x,
            dpi_y,
            ext: "png",
            content_type: "image/png",
        });
    }
    if has_signature(blob, 6, b"JFIF") {
        let (px_w, px_h, dpi_x, dpi_y) = parse_jpeg(blob, JpegKind::Jfif)?;
        return Ok(ImageInfo {
            sha1,
            px_w,
            px_h,
            dpi_x,
            dpi_y,
            ext: "jpg",
            content_type: "image/jpeg",
        });
    }
    if has_signature(blob, 6, b"Exif") {
        let (px_w, px_h, dpi_x, dpi_y) = parse_jpeg(blob, JpegKind::Exif)?;
        return Ok(ImageInfo {
            sha1,
            px_w,
            px_h,
            dpi_x,
            dpi_y,
            ext: "jpg",
            content_type: "image/jpeg",
        });
    }
    if has_signature(blob, 0, b"GIF87a") || has_signature(blob, 0, b"GIF89a") {
        let (px_w, px_h, dpi_x, dpi_y) = parse_gif(blob)?;
        return Ok(ImageInfo {
            sha1,
            px_w,
            px_h,
            dpi_x,
            dpi_y,
            ext: "gif",
            content_type: "image/gif",
        });
    }
    if has_signature(blob, 0, b"MM\x00*") || has_signature(blob, 0, b"II*\x00") {
        let (px_w, px_h, dpi_x, dpi_y) = parse_tiff(blob)?;
        return Ok(ImageInfo {
            sha1,
            px_w,
            px_h,
            dpi_x,
            dpi_y,
            ext: "tiff",
            content_type: "image/tiff",
        });
    }
    if has_signature(blob, 0, b"BM") {
        let (px_w, px_h, dpi_x, dpi_y) = parse_bmp(blob)?;
        return Ok(ImageInfo {
            sha1,
            px_w,
            px_h,
            dpi_x,
            dpi_y,
            ext: "bmp",
            content_type: "image/bmp",
        });
    }
    Err(ImageError::Unrecognized)
}

/// Upstream signature table matched in order: compares the signature substring at `offset`.
fn has_signature(blob: &[u8], offset: usize, sig: &[u8]) -> bool {
    blob.len() >= offset + sig.len() && &blob[offset..offset + sig.len()] == sig
}

/// Computes the binary SHA-1 digest of the complete image blob.
#[must_use]
pub fn sha1_digest(blob: &[u8]) -> ImageDigest {
    Sha1::digest(blob).into()
}

fn digest_hex(digest: &ImageDigest) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(40);
    for byte in digest {
        hex.push(HEX[(byte >> 4) as usize] as char);
        hex.push(HEX[(byte & 0x0f) as usize] as char);
    }
    hex
}

// ---------- Integer read helpers that return None out of bounds (upstream StreamReader raises) ----------

fn byte_at(blob: &[u8], off: usize) -> Option<u8> {
    blob.get(off).copied()
}

fn be_u16(blob: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*blob.get(off)?, *blob.get(off + 1)?]))
}

fn be_u32(blob: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *blob.get(off)?,
        *blob.get(off + 1)?,
        *blob.get(off + 2)?,
        *blob.get(off + 3)?,
    ]))
}

fn le_u16(blob: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*blob.get(off)?, *blob.get(off + 1)?]))
}

fn le_u32(blob: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *blob.get(off)?,
        *blob.get(off + 1)?,
        *blob.get(off + 2)?,
        *blob.get(off + 3)?,
    ]))
}

// ---------- PNG (docx/image/png.py) ----------

/// IHDR provides pixel dimensions; pHYs provides resolution; iteration stops at IEND.
fn parse_png(blob: &[u8]) -> Result<(u32, u32, u32, u32), ImageError> {
    let mut px_w = None;
    let mut px_h = None;
    let mut phys = None;
    // chunk layout: len (BE u32) + 4-byte type + data; iterate from offset 8
    let mut off = 8usize;
    loop {
        let len = be_u32(blob, off).ok_or(ImageError::Unrecognized)? as usize;
        let chunk_type = blob.get(off + 4..off + 8).ok_or(ImageError::Unrecognized)?;
        let data = off + 8;
        match chunk_type {
            b"IHDR" => {
                px_w = Some(be_u32(blob, data).ok_or(ImageError::Unrecognized)?);
                px_h = Some(be_u32(blob, data + 4).ok_or(ImageError::Unrecognized)?);
            }
            b"pHYs" => {
                let horz_px_per_unit = be_u32(blob, data).ok_or(ImageError::Unrecognized)?;
                let vert_px_per_unit = be_u32(blob, data + 4).ok_or(ImageError::Unrecognized)?;
                let unit = byte_at(blob, data + 8).ok_or(ImageError::Unrecognized)?;
                phys = Some((horz_px_per_unit, vert_px_per_unit, unit));
            }
            // IEND: end of iteration
            b"IEND" => break,
            _ => {}
        }
        // Advance by 4 (len) + 4 (type) + data + 4 (CRC); addition overflow is treated as unparseable
        off = off
            .checked_add(len)
            .and_then(|o| o.checked_add(12))
            .ok_or(ImageError::Unrecognized)?;
    }
    // No IHDR: upstream raises; collapse into Unrecognized
    let px_w = px_w.ok_or(ImageError::Unrecognized)?;
    let px_h = px_h.ok_or(ImageError::Unrecognized)?;
    let (dpi_x, dpi_y) = match phys {
        Some((horz, vert, unit)) => (png_dpi(horz, unit), png_dpi(vert, unit)),
        // No pHYs: default to 72
        None => (72, 72),
    };
    Ok((px_w, px_h, dpi_x, dpi_y))
}

/// Upstream: `unit==1 and nonzero px_per_unit → int(round(px_per_unit*0.0254))`, otherwise 72.
fn png_dpi(px_per_unit: u32, unit: u8) -> u32 {
    if unit == 1 && px_per_unit != 0 {
        py_round(f64::from(px_per_unit) * 0.0254) as u32
    } else {
        72
    }
}

// ---------- JPEG (JFIF/Exif paths in docx/image/jpeg.py) ----------

/// python-docx `_ImageHeaderFactory` selects the JPEG subclass by the signature at offset 6:
/// `Jfif` takes dpi from the first APP0, `Exif` from the TIFF embedded in the first APP1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JpegKind {
    Jfif,
    Exif,
}

/// Marker scan: per [`JpegKind`], take the first APP0 or APP1 and the first SOFn;
/// a missing required marker or SOF is treated as unrecognized.
fn parse_jpeg(blob: &[u8], kind: JpegKind) -> Result<(u32, u32, u32, u32), ImageError> {
    let mut app0 = None;
    let mut app1 = None;
    let mut sof = None;
    let mut start = 0usize;
    while let Some((code, seg_off)) = next_marker(blob, start) {
        let next_start = if is_standalone(code) {
            // Standalone markers have segment length 0
            seg_off
        } else {
            // Segment length = BE u16 at segment start (includes the 2 length bytes, excludes the 2 marker bytes)
            let seg_len = be_u16(blob, seg_off).ok_or(ImageError::Unrecognized)? as usize;
            // First APP0 (segment-relative offsets: units@9, x_density@10, y_density@12)
            if code == 0xE0 && app0.is_none() {
                let units = byte_at(blob, seg_off + 9).ok_or(ImageError::Unrecognized)?;
                let x_density = be_u16(blob, seg_off + 10).ok_or(ImageError::Unrecognized)?;
                let y_density = be_u16(blob, seg_off + 12).ok_or(ImageError::Unrecognized)?;
                app0 = Some((units, x_density, y_density));
            }
            // First APP1. python-docx returns the default 72 dpi for a non-Exif APP1,
            // and parses the rest of the payload as TIFF for `Exif\0\0`.
            if code == 0xE1 && app1.is_none() {
                let signature = blob.get(seg_off + 2..seg_off + 8);
                let dpi = if signature == Some(&b"Exif\0\0"[..]) {
                    let tiff_start = seg_off.checked_add(8).ok_or(ImageError::Unrecognized)?;
                    let segment_end = seg_off
                        .checked_add(seg_len)
                        .ok_or(ImageError::Unrecognized)?;
                    let tiff = blob
                        .get(tiff_start..segment_end)
                        .ok_or(ImageError::Unrecognized)?;
                    parse_tiff_dpi(tiff)?
                } else {
                    (72, 72)
                };
                app1 = Some(dpi);
            }
            // First SOFn (segment-relative offsets: px_h@3, px_w@5)
            if is_sof(code) && sof.is_none() {
                let px_h = be_u16(blob, seg_off + 3).ok_or(ImageError::Unrecognized)?;
                let px_w = be_u16(blob, seg_off + 5).ok_or(ImageError::Unrecognized)?;
                sof = Some((u32::from(px_w), u32::from(px_h)));
            }
            seg_off + seg_len
        };
        // EOI: end of scan; SOS: upstream marker collection stops here
        if code == 0xD9 || code == 0xDA {
            break;
        }
        start = next_start;
    }
    // No SOF: upstream raises KeyError; collapse into Unrecognized
    let (px_w, px_h) = sof.ok_or(ImageError::Unrecognized)?;
    let (dpi_x, dpi_y) = match kind {
        JpegKind::Jfif => {
            let (units, x_density, y_density) = app0.ok_or(ImageError::Unrecognized)?;
            (jpeg_dpi(units, x_density), jpeg_dpi(units, y_density))
        }
        JpegKind::Exif => app1.ok_or(ImageError::Unrecognized)?,
    };
    Ok((px_w, px_h, dpi_x, dpi_y))
}

/// Upstream `JPEG_MARKER_CODE.STANDALONE_MARKERS`: TEM, SOI, EOI, RST0-7.
fn is_standalone(code: u8) -> bool {
    matches!(code, 0x01 | 0xD0..=0xD9)
}

/// Upstream `JPEG_MARKER_CODE.SOF_MARKER_CODES`.
fn is_sof(code: u8) -> bool {
    matches!(
        code,
        0xC0 | 0xC1 | 0xC2 | 0xC3 | 0xC5 | 0xC6 | 0xC7 | 0xC9 | 0xCA | 0xCB | 0xCD | 0xCE | 0xCF
    )
}

/// Upstream `_MarkerFinder.next`: locates the next marker starting at `start`.
///
/// Returns `(marker code, segment offset)`; the segment offset immediately follows the
/// 2-byte marker code. An `FF 00` sequence is not a marker, so scanning restarts at the `00`
/// byte. Returns `None` when the file is exhausted.
fn next_marker(blob: &[u8], start: usize) -> Option<(u8, usize)> {
    let mut pos = start;
    loop {
        // Skip non-FF bytes
        pos = next_ff(blob, pos)?;
        // Skip FF fill bytes and take the first non-FF byte
        let (non_ff, byte) = next_non_ff(blob, pos + 1)?;
        if byte == 0x00 {
            // FF 00 is not a marker: restart scanning at the 00 byte
            pos = non_ff;
            continue;
        }
        return Some((byte, non_ff + 1));
    }
}

/// Offset of the first `0xFF` at or after `start`; `None` if absent.
fn next_ff(blob: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i < blob.len() {
        if blob[i] == 0xFF {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Offset and value of the first non-`0xFF` byte at or after `start`; `None` at end of file.
fn next_non_ff(blob: &[u8], start: usize) -> Option<(usize, u8)> {
    let mut i = start;
    loop {
        let byte = *blob.get(i)?;
        if byte != 0xFF {
            return Some((i, byte));
        }
        i += 1;
    }
}

/// Upstream `_App0Marker._dpi`: units==1 → dots/inch as-is; units==2 → convert from
/// dots/cm; otherwise 72.
fn jpeg_dpi(units: u8, density: u16) -> u32 {
    match units {
        1 => u32::from(density),
        2 => py_round(f64::from(density) * 2.54) as u32,
        _ => 72,
    }
}

// ---------- GIF (docx/image/gif.py) ----------

/// GIF carries no resolution information; dpi is always 72,72.
fn parse_gif(blob: &[u8]) -> Result<(u32, u32, u32, u32), ImageError> {
    let px_w = le_u16(blob, 6).ok_or(ImageError::Unrecognized)?;
    let px_h = le_u16(blob, 8).ok_or(ImageError::Unrecognized)?;
    Ok((u32::from(px_w), u32::from(px_h), 72, 72))
}

// ---------- BMP (docx/image/bmp.py) ----------

/// Pixel dimensions and ppm resolution from BITMAPINFOHEADER.
fn parse_bmp(blob: &[u8]) -> Result<(u32, u32, u32, u32), ImageError> {
    let px_w = le_u32(blob, 0x12).ok_or(ImageError::Unrecognized)?;
    let px_h = le_u32(blob, 0x16).ok_or(ImageError::Unrecognized)?;
    let x_ppm = le_u32(blob, 0x26).ok_or(ImageError::Unrecognized)?;
    let y_ppm = le_u32(blob, 0x2A).ok_or(ImageError::Unrecognized)?;
    Ok((px_w, px_h, bmp_dpi(x_ppm), bmp_dpi(y_ppm)))
}

/// Upstream: `ppm==0 → 96`, otherwise `int(round(ppm*0.0254))`.
fn bmp_dpi(px_per_meter: u32) -> u32 {
    if px_per_meter == 0 {
        96
    } else {
        py_round(f64::from(px_per_meter) * 0.0254) as u32
    }
}

// ---------- TIFF (docx/image/tiff.py) ----------

/// Fields InlineImage needs from TIFF IFD0. The IFD0 of Exif-in-JPEG usually lacks
/// ImageWidth/ImageLength, so dimensions stay optional and are provided by the JPEG SOF.
struct TiffFields {
    width: Option<u32>,
    height: Option<u32>,
    x_res: Option<f64>,
    y_res: Option<f64>,
    unit: u32,
}

/// IFD0 entry parsing: tag/type/count/value, 12 bytes per entry.
fn parse_tiff_fields(blob: &[u8]) -> Result<TiffFields, ImageError> {
    // 'MM' is big-endian; everything else (including 'II') is little-endian
    let big_endian = blob.get(0..2) == Some(&b"MM"[..]);
    let read_u16 = |off: usize| {
        if big_endian {
            be_u16(blob, off)
        } else {
            le_u16(blob, off)
        }
    };
    let read_u32 = |off: usize| {
        if big_endian {
            be_u32(blob, off)
        } else {
            le_u32(blob, off)
        }
    };
    let ifd0 = read_u32(4).ok_or(ImageError::Unrecognized)? as usize;
    let entry_count = read_u16(ifd0).ok_or(ImageError::Unrecognized)? as usize;
    let mut width = None;
    let mut height = None;
    let mut x_res = None;
    let mut y_res = None;
    let mut unit = None;
    for i in 0..entry_count {
        // Addition overflow is treated as unparseable
        let entry = ifd0
            .checked_add(2)
            .and_then(|o| o.checked_add(i * 12))
            .ok_or(ImageError::Unrecognized)?;
        let tag = read_u16(entry).ok_or(ImageError::Unrecognized)?;
        let field_type = read_u16(entry + 2).ok_or(ImageError::Unrecognized)?;
        let count = read_u32(entry + 4).ok_or(ImageError::Unrecognized)?;
        if count != 1 {
            // Upstream only supports single-value entries (multi-value returns a placeholder
            // string, unusable in practice)
            continue;
        }
        match (u32::from(tag), u32::from(field_type)) {
            // ImageWidth / ImageLength: SHORT takes the first 2 bytes of the value area,
            // LONG takes 4
            (0x0100, 3) => width = read_u16(entry + 8).map(u32::from),
            (0x0100, 4) => width = read_u32(entry + 8),
            (0x0101, 3) => height = read_u16(entry + 8).map(u32::from),
            (0x0101, 4) => height = read_u32(entry + 8),
            // XResolution / YResolution: RATIONAL reads num/den at value_offset
            (0x011A, 5) => {
                x_res = read_rational(&read_u32, entry)?;
            }
            (0x011B, 5) => {
                y_res = read_rational(&read_u32, entry)?;
            }
            // ResolutionUnit: defaults to 2 (inches)
            (0x0128, 3) => unit = read_u16(entry + 8).map(u32::from),
            (0x0128, 4) => unit = read_u32(entry + 8),
            _ => {}
        }
    }
    Ok(TiffFields {
        width,
        height,
        x_res,
        y_res,
        unit: unit.unwrap_or(2),
    })
}

fn parse_tiff(blob: &[u8]) -> Result<(u32, u32, u32, u32), ImageError> {
    let fields = parse_tiff_fields(blob)?;
    // A standalone TIFF without pixel dimensions cannot form a valid image upstream or
    // downstream; collapse into Unrecognized.
    let px_w = fields.width.ok_or(ImageError::Unrecognized)?;
    let px_h = fields.height.ok_or(ImageError::Unrecognized)?;
    Ok((
        px_w,
        px_h,
        tiff_dpi(fields.x_res, fields.unit),
        tiff_dpi(fields.y_res, fields.unit),
    ))
}

/// Exif JPEG takes dpi only from the embedded TIFF; pixel dimensions come from the JPEG SOF,
/// so an IFD0 without dimensions is valid.
fn parse_tiff_dpi(blob: &[u8]) -> Result<(u32, u32), ImageError> {
    let fields = parse_tiff_fields(blob)?;
    Ok((
        tiff_dpi(fields.x_res, fields.unit),
        tiff_dpi(fields.y_res, fields.unit),
    ))
}

/// Reads a RATIONAL entry (type 5): two u32s, num/den, at value_offset, returned as an
/// f64 ratio.
///
/// Upstream `num/den` raises ZeroDivisionError on `den==0`; here it collapses into Unrecognized.
fn read_rational(
    read_u32: &dyn Fn(usize) -> Option<u32>,
    entry: usize,
) -> Result<Option<f64>, ImageError> {
    let value_offset = read_u32(entry + 8).ok_or(ImageError::Unrecognized)? as usize;
    let num = read_u32(value_offset).ok_or(ImageError::Unrecognized)?;
    let den = read_u32(value_offset + 4).ok_or(ImageError::Unrecognized)?;
    if den == 0 {
        return Err(ImageError::Unrecognized);
    }
    Ok(Some(f64::from(num) / f64::from(den)))
}

/// Upstream `_TiffParser._dpi`: no resolution → 72; unit==1 (aspect ratio only) → 72;
/// unit==2 (inches) → int(round(dots)); otherwise (centimeters, etc.) →
/// int(round(dots*2.54)).
fn tiff_dpi(dots: Option<f64>, unit: u32) -> u32 {
    match dots {
        None => 72,
        Some(dots) => {
            if unit == 1 {
                72
            } else {
                let units_per_inch = if unit == 2 { 1.0 } else { 2.54 };
                py_round(dots * units_per_inch) as u32
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn media(name: &str) -> String {
        format!(
            "{}/../../tests/fixtures/media/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        )
    }

    fn probe_media(name: &str) -> Result<ImageInfo, Box<dyn std::error::Error>> {
        Ok(probe(&std::fs::read(media(name))?)?)
    }

    fn push_jpeg_segment(jpeg: &mut Vec<u8>, marker: u8, payload: &[u8]) {
        jpeg.extend_from_slice(&[0xFF, marker]);
        let segment_len = u16::try_from(payload.len() + 2).expect("test segment fits u16");
        jpeg.extend_from_slice(&segment_len.to_be_bytes());
        jpeg.extend_from_slice(payload);
    }

    fn jfif_payload(dpi_x: u16, dpi_y: u16) -> Vec<u8> {
        let mut payload = b"JFIF\0\x01\x01\x01".to_vec();
        payload.extend_from_slice(&dpi_x.to_be_bytes());
        payload.extend_from_slice(&dpi_y.to_be_bytes());
        payload.extend_from_slice(&[0, 0]);
        payload
    }

    /// Generates an Exif TIFF without width/height entries; JPEG pixel dimensions should
    /// still come from the SOF.
    fn exif_payload(dpi_x: u32, dpi_y: u32) -> Vec<u8> {
        let mut payload = b"Exif\0\0II*\0\x08\0\0\0".to_vec();
        payload.extend_from_slice(&3u16.to_le_bytes());

        for (tag, value_offset) in [(0x011Au16, 50u32), (0x011Bu16, 58u32)] {
            payload.extend_from_slice(&tag.to_le_bytes());
            payload.extend_from_slice(&5u16.to_le_bytes());
            payload.extend_from_slice(&1u32.to_le_bytes());
            payload.extend_from_slice(&value_offset.to_le_bytes());
        }

        payload.extend_from_slice(&0x0128u16.to_le_bytes());
        payload.extend_from_slice(&3u16.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&2u16.to_le_bytes());
        payload.extend_from_slice(&0u16.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&dpi_x.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&dpi_y.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload
    }

    fn sof_payload(px_w: u16, px_h: u16) -> Vec<u8> {
        let mut payload = vec![8];
        payload.extend_from_slice(&px_h.to_be_bytes());
        payload.extend_from_slice(&px_w.to_be_bytes());
        payload.extend_from_slice(&[1, 1, 0x11, 0]);
        payload
    }

    fn jpeg_with_segments(segments: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut jpeg = vec![0xFF, 0xD8];
        for (marker, payload) in segments {
            push_jpeg_segment(&mut jpeg, *marker, payload);
        }
        jpeg.extend_from_slice(&[0xFF, 0xD9]);
        jpeg
    }

    #[test]
    fn png_dotmatrix_2x1_header_fields_and_sha1() -> TestResult {
        let info = probe_media("p4_dot2x1.png")?;
        assert_eq!(info.sha1, "df3bd707bf83d81a1f6bb1272baad6bcbc4f96bd");
        assert_eq!((info.px_w, info.px_h), (2, 1));
        assert_eq!((info.dpi_x, info.dpi_y), (72, 72));
        assert_eq!(info.ext, "png");
        assert_eq!(info.content_type, "image/png");
        Ok(())
    }

    #[test]
    fn png_150dpi_wide_image_parse() -> TestResult {
        let info = probe_media("p4_wide4x1.png")?;
        assert_eq!(info.sha1, "0a6de7e8063359f3a5895dfc5fcfbd0061de0b73");
        assert_eq!((info.px_w, info.px_h), (4, 1));
        assert_eq!((info.dpi_x, info.dpi_y), (150, 150));
        assert_eq!(info.ext, "png");
        Ok(())
    }

    #[test]
    fn jpg_jfif_header_parse() -> TestResult {
        let info = probe_media("p4_rect4x2.jpg")?;
        // generate.py make_jpeg builds APP0 units=1 (inches) density=300
        assert_eq!(info.sha1, "ac963b361f4842ac5ab75153fdecc42e93a50a94");
        assert_eq!((info.px_w, info.px_h), (4, 2));
        assert_eq!((info.dpi_x, info.dpi_y), (300, 300));
        // Extension is jpg, not jpeg
        assert_eq!(info.ext, "jpg");
        assert_eq!(info.content_type, "image/jpeg");
        Ok(())
    }

    #[test]
    fn jpeg_app0_app1_coexist_picks_dpi_by_first_segment_signature() -> TestResult {
        let jfif_first = jpeg_with_segments(&[
            (0xE0, jfif_payload(96, 110)),
            (0xE1, exif_payload(300, 150)),
            (0xC0, sof_payload(4, 2)),
        ]);
        let info = probe(&jfif_first)?;
        assert_eq!((info.px_w, info.px_h), (4, 2));
        assert_eq!((info.dpi_x, info.dpi_y), (96, 110));

        let exif_first = jpeg_with_segments(&[
            (0xE1, exif_payload(300, 150)),
            (0xE0, jfif_payload(96, 110)),
            (0xC0, sof_payload(4, 2)),
        ]);
        let info = probe(&exif_first)?;
        assert_eq!((info.px_w, info.px_h), (4, 2));
        assert_eq!((info.dpi_x, info.dpi_y), (300, 150));
        Ok(())
    }

    #[test]
    fn bmp_parse() -> TestResult {
        let info = probe_media("p4_brick4x2.bmp")?;
        assert_eq!(info.sha1, "65f05d6d497c917f3426abcac669a76295faa27e");
        assert_eq!((info.px_w, info.px_h), (4, 2));
        assert_eq!((info.dpi_x, info.dpi_y), (72, 72));
        assert_eq!(info.ext, "bmp");
        assert_eq!(info.content_type, "image/bmp");
        Ok(())
    }

    #[test]
    fn gif_parse() -> TestResult {
        let info = probe_media("p4_arrow4x2.gif")?;
        assert_eq!(info.sha1, "138fe209a2958e4bcd0a8941d2e7aa7162f93963");
        assert_eq!((info.px_w, info.px_h), (4, 2));
        assert_eq!((info.dpi_x, info.dpi_y), (72, 72));
        assert_eq!(info.ext, "gif");
        assert_eq!(info.content_type, "image/gif");
        Ok(())
    }

    #[test]
    fn tiff_rational_resolution_parse() -> TestResult {
        let info = probe_media("p4_tile4x2.tiff")?;
        assert_eq!(info.sha1, "68484f8ca87829e2987090872ed529be1c369b7a");
        assert_eq!((info.px_w, info.px_h), (4, 2));
        assert_eq!((info.dpi_x, info.dpi_y), (150, 150));
        assert_eq!(info.ext, "tiff");
        assert_eq!(info.content_type, "image/tiff");
        Ok(())
    }

    #[test]
    fn bad_png_returns_unrecognized() -> TestResult {
        let blob = std::fs::read(media("p4_bad.png"))?;
        assert!(matches!(probe(&blob), Err(ImageError::Unrecognized)));
        Ok(())
    }

    #[test]
    fn empty_input_returns_unrecognized() {
        assert!(matches!(probe(&[]), Err(ImageError::Unrecognized)));
        assert!(matches!(probe(b"\x89PNG"), Err(ImageError::Unrecognized)));
    }

    #[test]
    fn truncated_png_returns_unrecognized() -> TestResult {
        let blob = std::fs::read(media("p4_dot2x1.png"))?;
        // Truncate into the middle of the IHDR data: the signature can match but the chunk
        // data is unreadable
        assert!(matches!(probe(&blob[..16]), Err(ImageError::Unrecognized)));
        Ok(())
    }

    #[test]
    fn py_round_bankers_rounding_semantics() {
        // Exactly x.5: take the even one
        assert_eq!(py_round(0.5), 0.0);
        assert_eq!(py_round(1.5), 2.0);
        assert_eq!(py_round(2.5), 2.0);
        assert_eq!(py_round(3.5), 4.0);
        assert_eq!(py_round(-0.5), 0.0);
        assert_eq!(py_round(-1.5), -2.0);
        assert_eq!(py_round(-2.5), -2.0);
        assert_eq!(py_round(-4.5), -4.0);
        // Not x.5: take the nearest value
        assert_eq!(py_round(2.675), 3.0);
        assert_eq!(py_round(71.889), 72.0);
        assert_eq!(py_round(28.349_999), 28.0);
        assert_eq!(py_round(4.4), 4.0);
        assert_eq!(py_round(4.6), 5.0);
        assert_eq!(py_round(-2.6), -3.0);
        assert_eq!(py_round(-2.4), -2.0);
    }
}
