//! 图片头解析：逐字段对齐 python-docx 1.2.0 `docx/image/*` 的探针实证语义。
//!
//! - 签名表按序匹配（上游 `_ImageHeaderFactory`）：PNG@0、JFIF@6、Exif@6、
//!   GIF87a/GIF89a@0、TIFF(MM/II)@0、BMP@0；全部不中即 [`ImageError::Unrecognized`]。
//! - 上游各种截断/缺段异常（UnexpectedEndOfFileError、KeyError 等）统一归并为
//!   `Unrecognized`（探针失败即无法识别）。
//! - 涉及 `int(round(x))` 之处采用 Python 的银行家舍入（见 [`py_round`]）。

use sha1::{Digest, Sha1};

/// 图片头解析结果（对齐 python-docx `Image` 的探针可见字段）。
///
/// # 示例
///
/// ```
/// // 无法识别的字节返回 Unrecognized
/// assert!(matches!(
///     docxtpl_rich::probe(b"not an image"),
///     Err(docxtpl_rich::ImageError::Unrecognized)
/// ));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    /// 全量 blob 的 SHA-1，小写十六进制（对齐 `hashlib.sha1(...).hexdigest()`）。
    pub sha1: String,
    /// 像素宽。
    pub px_w: u32,
    /// 像素高。
    pub px_h: u32,
    /// 水平 dpi。头信息缺失时按各格式缺省（PNG/JPEG/GIF/TIFF 为 72，BMP 为 96）。
    pub dpi_x: u32,
    /// 垂直 dpi。
    pub dpi_y: u32,
    /// 默认扩展名（上游 `default_ext`；注意 JPEG 为 `jpg` 而非 `jpeg`）。
    pub ext: &'static str,
    /// MIME 内容类型。
    pub content_type: &'static str,
}

/// 图片解析错误。
///
/// 上游（python-docx）在签名不匹配、头信息截断、缺关键段等场景抛出
/// UnrecognizedImageError / UnexpectedEndOfFileError / KeyError 等多种异常；
/// 本库按 P4 口径统一归并为单一变体。
#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    /// 无法识别的图片：签名不匹配任何受支持格式，或头信息不可用。
    #[error("无法识别的图片：不匹配受支持的图片签名（PNG/JPEG/GIF/TIFF/BMP）或头信息不可用")]
    Unrecognized,
}

/// Python `round(x)` 的银行家舍入（round-half-to-even）。
///
/// 不能直接用 [`f64::round`]（那是四舍五入到远离零的一侧）：
/// - 恰为 x.5 时舍入到最近的偶数（0.5 → 0、1.5 → 2、2.5 → 2、-1.5 → -2）；
/// - 其余情况舍入到最近值。
///
/// # 示例
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
        // 距离更远的一侧更近：远离零取整
        if x < 0.0 {
            x.floor()
        } else {
            x.ceil()
        }
    } else if frac < 0.5 {
        // 距离更近的一侧更近：向零取整
        if x < 0.0 {
            x.ceil()
        } else {
            x.floor()
        }
    } else {
        // 恰为 x.5：舍入到偶数
        let lower = x.floor();
        if (lower as i64) % 2 == 0 {
            lower
        } else {
            lower + 1.0
        }
    }
}

/// 解析图片字节，返回 sha1、像素尺寸、dpi、扩展名与内容类型。
///
/// 支持格式与字段语义对齐 python-docx 1.2.0：PNG（IHDR/pHYs chunk）、
/// JPEG（marker 扫描取首个 APP0 与首个 SOFn）、GIF、BMP（BITMAPINFOHEADER）、
/// TIFF（IFD0 的 SHORT/LONG/RATIONAL 条目）。
pub fn probe(blob: &[u8]) -> Result<ImageInfo, ImageError> {
    let sha1 = sha1_hex(blob);
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
    if has_signature(blob, 6, b"JFIF") || has_signature(blob, 6, b"Exif") {
        let (px_w, px_h, dpi_x, dpi_y) = parse_jpeg(blob)?;
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

/// 上游签名表按序匹配：在 `offset` 处比较签名子串。
fn has_signature(blob: &[u8], offset: usize, sig: &[u8]) -> bool {
    blob.len() >= offset + sig.len() && &blob[offset..offset + sig.len()] == sig
}

/// 全量 blob 的 SHA-1，小写十六进制输出。
fn sha1_hex(blob: &[u8]) -> String {
    let digest = Sha1::digest(blob);
    let mut hex = String::with_capacity(40);
    for byte in digest.iter() {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

// ---------- 越界即 None 的整数读取辅助（上游 StreamReader 越界抛异常） ----------

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

// ---------- PNG（docx/image/png.py） ----------

/// IHDR 提供像素尺寸；pHYs 提供分辨率；遇 IEND 停止遍历。
fn parse_png(blob: &[u8]) -> Result<(u32, u32, u32, u32), ImageError> {
    let mut px_w = None;
    let mut px_h = None;
    let mut phys = None;
    // chunk 布局：len(BE u32) + 类型 4 字节 + data；从 offset 8 起遍历
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
            // IEND：遍历终点
            b"IEND" => break,
            _ => {}
        }
        // 推进 4(len) + 4(类型) + data + 4(CRC)；加法溢出视为无法解析
        off = off
            .checked_add(len)
            .and_then(|o| o.checked_add(12))
            .ok_or(ImageError::Unrecognized)?;
    }
    // 无 IHDR：上游抛异常，归并为无法识别
    let px_w = px_w.ok_or(ImageError::Unrecognized)?;
    let px_h = px_h.ok_or(ImageError::Unrecognized)?;
    let (dpi_x, dpi_y) = match phys {
        Some((horz, vert, unit)) => (png_dpi(horz, unit), png_dpi(vert, unit)),
        // 无 pHYs：缺省 72
        None => (72, 72),
    };
    Ok((px_w, px_h, dpi_x, dpi_y))
}

/// 上游：`unit==1 且 px_per_unit 非零 → int(round(px_per_unit*0.0254))`，否则 72。
fn png_dpi(px_per_unit: u32, unit: u8) -> u32 {
    if unit == 1 && px_per_unit != 0 {
        py_round(f64::from(px_per_unit) * 0.0254) as u32
    } else {
        72
    }
}

// ---------- JPEG（docx/image/jpeg.py 的 JFIF/APP0 路径） ----------

/// marker 扫描：取第一个 APP0 与第一个 SOFn；无 SOF 即无法识别。
///
/// 注意：Exif 签名（`Exif`@6）同样走该统一路径（上游 `_App1Marker` 的
/// Exif-in-JPEG dpi 提取未纳入本阶段；无 APP0 时按缺省 72 处理）。
fn parse_jpeg(blob: &[u8]) -> Result<(u32, u32, u32, u32), ImageError> {
    let mut app0 = None;
    let mut sof = None;
    let mut start = 0usize;
    while let Some((code, seg_off)) = next_marker(blob, start) {
        let next_start = if is_standalone(code) {
            // standalone 标记段长为 0
            seg_off
        } else {
            // 段长 = BE u16@段首（含 2 字节长度本身，不含 2 字节标记码）
            let seg_len = be_u16(blob, seg_off).ok_or(ImageError::Unrecognized)? as usize;
            // 第一个 APP0（段内相对偏移：units@9、x_density@10、y_density@12）
            if code == 0xE0 && app0.is_none() {
                let units = byte_at(blob, seg_off + 9).ok_or(ImageError::Unrecognized)?;
                let x_density = be_u16(blob, seg_off + 10).ok_or(ImageError::Unrecognized)?;
                let y_density = be_u16(blob, seg_off + 12).ok_or(ImageError::Unrecognized)?;
                app0 = Some((units, x_density, y_density));
            }
            // 第一个 SOFn（段内相对偏移：px_h@3、px_w@5）
            if is_sof(code) && sof.is_none() {
                let px_h = be_u16(blob, seg_off + 3).ok_or(ImageError::Unrecognized)?;
                let px_w = be_u16(blob, seg_off + 5).ok_or(ImageError::Unrecognized)?;
                sof = Some((u32::from(px_w), u32::from(px_h)));
            }
            seg_off + seg_len
        };
        // EOI：扫描终点；SOS：上游 marker 收集到此为止
        if code == 0xD9 || code == 0xDA {
            break;
        }
        start = next_start;
    }
    // 无 SOF：上游抛 KeyError，归并为无法识别
    let (px_w, px_h) = sof.ok_or(ImageError::Unrecognized)?;
    let (dpi_x, dpi_y) = match app0 {
        Some((units, x_density, y_density)) => {
            (jpeg_dpi(units, x_density), jpeg_dpi(units, y_density))
        }
        // 无 APP0：缺省 72,72
        None => (72, 72),
    };
    Ok((px_w, px_h, dpi_x, dpi_y))
}

/// 上游 `JPEG_MARKER_CODE.STANDALONE_MARKERS`：TEM、SOI、EOI、RST0-7。
fn is_standalone(code: u8) -> bool {
    matches!(code, 0x01 | 0xD0..=0xD9)
}

/// 上游 `JPEG_MARKER_CODE.SOF_MARKER_CODES`。
fn is_sof(code: u8) -> bool {
    matches!(
        code,
        0xC0 | 0xC1 | 0xC2 | 0xC3 | 0xC5 | 0xC6 | 0xC7 | 0xC9 | 0xCA | 0xCB | 0xCD | 0xCE | 0xCF
    )
}

/// 上游 `_MarkerFinder.next`：从 `start` 起定位下一个标记。
///
/// 返回 `(标记码, 段偏移)`；段偏移紧跟 2 字节标记码之后。`FF 00` 序列
/// 不是标记，从 `00` 处重新扫描。文件耗尽返回 `None`。
fn next_marker(blob: &[u8], start: usize) -> Option<(u8, usize)> {
    let mut pos = start;
    loop {
        // 跳过非 FF 字节
        pos = next_ff(blob, pos)?;
        // 跳过 FF 填充，取首个非 FF 字节
        let (non_ff, byte) = next_non_ff(blob, pos + 1)?;
        if byte == 0x00 {
            // FF 00 不是标记：从 00 处重新扫描
            pos = non_ff;
            continue;
        }
        return Some((byte, non_ff + 1));
    }
}

/// 自 `start` 起首个 `0xFF` 的偏移；无则 `None`。
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

/// 自 `start` 起首个非 `0xFF` 字节的偏移与值；文件耗尽返回 `None`。
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

/// 上游 `_App0Marker._dpi`：units==1 → 点/英寸原值；units==2 → 点/厘米换算；否则 72。
fn jpeg_dpi(units: u8, density: u16) -> u32 {
    match units {
        1 => u32::from(density),
        2 => py_round(f64::from(density) * 2.54) as u32,
        _ => 72,
    }
}

// ---------- GIF（docx/image/gif.py） ----------

/// GIF 不含分辨率信息，dpi 恒 72,72。
fn parse_gif(blob: &[u8]) -> Result<(u32, u32, u32, u32), ImageError> {
    let px_w = le_u16(blob, 6).ok_or(ImageError::Unrecognized)?;
    let px_h = le_u16(blob, 8).ok_or(ImageError::Unrecognized)?;
    Ok((u32::from(px_w), u32::from(px_h), 72, 72))
}

// ---------- BMP（docx/image/bmp.py） ----------

/// BITMAPINFOHEADER 的像素尺寸与 ppm 分辨率。
fn parse_bmp(blob: &[u8]) -> Result<(u32, u32, u32, u32), ImageError> {
    let px_w = le_u32(blob, 0x12).ok_or(ImageError::Unrecognized)?;
    let px_h = le_u32(blob, 0x16).ok_or(ImageError::Unrecognized)?;
    let x_ppm = le_u32(blob, 0x26).ok_or(ImageError::Unrecognized)?;
    let y_ppm = le_u32(blob, 0x2A).ok_or(ImageError::Unrecognized)?;
    Ok((px_w, px_h, bmp_dpi(x_ppm), bmp_dpi(y_ppm)))
}

/// 上游：`ppm==0 → 96`，否则 `int(round(ppm*0.0254))`。
fn bmp_dpi(px_per_meter: u32) -> u32 {
    if px_per_meter == 0 {
        96
    } else {
        py_round(f64::from(px_per_meter) * 0.0254) as u32
    }
}

// ---------- TIFF（docx/image/tiff.py） ----------

/// IFD0 条目解析：tag/type/count/value 各 12 字节一条。
fn parse_tiff(blob: &[u8]) -> Result<(u32, u32, u32, u32), ImageError> {
    // 'MM' 大端，其余（含 'II'）按小端
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
        // 加法溢出视为无法解析
        let entry = ifd0
            .checked_add(2)
            .and_then(|o| o.checked_add(i * 12))
            .ok_or(ImageError::Unrecognized)?;
        let tag = read_u16(entry).ok_or(ImageError::Unrecognized)?;
        let field_type = read_u16(entry + 2).ok_or(ImageError::Unrecognized)?;
        let count = read_u32(entry + 4).ok_or(ImageError::Unrecognized)?;
        if count != 1 {
            // 上游仅支持单值条目（多值返回占位串，实际不可用）
            continue;
        }
        match (u32::from(tag), u32::from(field_type)) {
            // ImageWidth / ImageLength：SHORT 取 value 区前 2 字节，LONG 取 4 字节
            (0x0100, 3) => width = read_u16(entry + 8).map(u32::from),
            (0x0100, 4) => width = read_u32(entry + 8),
            (0x0101, 3) => height = read_u16(entry + 8).map(u32::from),
            (0x0101, 4) => height = read_u32(entry + 8),
            // XResolution / YResolution：RATIONAL 在 value_offset 处读 num/den
            (0x011A, 5) => {
                x_res = read_rational(&read_u32, entry)?;
            }
            (0x011B, 5) => {
                y_res = read_rational(&read_u32, entry)?;
            }
            // ResolutionUnit：缺省 2（英寸）
            (0x0128, 3) => unit = read_u16(entry + 8).map(u32::from),
            (0x0128, 4) => unit = read_u32(entry + 8),
            _ => {}
        }
    }
    // 缺像素尺寸：上游返回 None 并在下游崩溃，归并为无法识别
    let px_w = width.ok_or(ImageError::Unrecognized)?;
    let px_h = height.ok_or(ImageError::Unrecognized)?;
    let unit = unit.unwrap_or(2);
    Ok((px_w, px_h, tiff_dpi(x_res, unit), tiff_dpi(y_res, unit)))
}

/// 读取 RATIONAL 条目（type 5）：value_offset 处 num/den 两个 u32，值为 f64 商。
///
/// 上游 `num/den` 遇 `den==0` 抛 ZeroDivisionError，此处归并为无法识别。
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

/// 上游 `_TiffParser._dpi`：无分辨率 → 72；unit==1（仅纵横比）→ 72；
/// unit==2（英寸）→ int(round(dots))；其余（厘米等）→ int(round(dots*2.54))。
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

    #[test]
    fn png_点阵2x1_头字段与sha1() -> TestResult {
        let info = probe_media("p4_dot2x1.png")?;
        assert_eq!(info.sha1, "df3bd707bf83d81a1f6bb1272baad6bcbc4f96bd");
        assert_eq!((info.px_w, info.px_h), (2, 1));
        assert_eq!((info.dpi_x, info.dpi_y), (72, 72));
        assert_eq!(info.ext, "png");
        assert_eq!(info.content_type, "image/png");
        Ok(())
    }

    #[test]
    fn png_150dpi_宽图解析() -> TestResult {
        let info = probe_media("p4_wide4x1.png")?;
        assert_eq!(info.sha1, "0a6de7e8063359f3a5895dfc5fcfbd0061de0b73");
        assert_eq!((info.px_w, info.px_h), (4, 1));
        assert_eq!((info.dpi_x, info.dpi_y), (150, 150));
        assert_eq!(info.ext, "png");
        Ok(())
    }

    #[test]
    fn jpg_jfif头解析() -> TestResult {
        let info = probe_media("p4_rect4x2.jpg")?;
        // generate.py make_jpeg 构造 APP0 units=1（英寸）density=300
        assert_eq!(info.sha1, "ac963b361f4842ac5ab75153fdecc42e93a50a94");
        assert_eq!((info.px_w, info.px_h), (4, 2));
        assert_eq!((info.dpi_x, info.dpi_y), (300, 300));
        // 扩展名是 jpg 而非 jpeg
        assert_eq!(info.ext, "jpg");
        assert_eq!(info.content_type, "image/jpeg");
        Ok(())
    }

    #[test]
    fn bmp_解析() -> TestResult {
        let info = probe_media("p4_brick4x2.bmp")?;
        assert_eq!(info.sha1, "65f05d6d497c917f3426abcac669a76295faa27e");
        assert_eq!((info.px_w, info.px_h), (4, 2));
        assert_eq!((info.dpi_x, info.dpi_y), (72, 72));
        assert_eq!(info.ext, "bmp");
        assert_eq!(info.content_type, "image/bmp");
        Ok(())
    }

    #[test]
    fn gif_解析() -> TestResult {
        let info = probe_media("p4_arrow4x2.gif")?;
        assert_eq!(info.sha1, "138fe209a2958e4bcd0a8941d2e7aa7162f93963");
        assert_eq!((info.px_w, info.px_h), (4, 2));
        assert_eq!((info.dpi_x, info.dpi_y), (72, 72));
        assert_eq!(info.ext, "gif");
        assert_eq!(info.content_type, "image/gif");
        Ok(())
    }

    #[test]
    fn tiff_有理数分辨率解析() -> TestResult {
        let info = probe_media("p4_tile4x2.tiff")?;
        assert_eq!(info.sha1, "68484f8ca87829e2987090872ed529be1c369b7a");
        assert_eq!((info.px_w, info.px_h), (4, 2));
        assert_eq!((info.dpi_x, info.dpi_y), (150, 150));
        assert_eq!(info.ext, "tiff");
        assert_eq!(info.content_type, "image/tiff");
        Ok(())
    }

    #[test]
    fn 坏png_返回无法识别() -> TestResult {
        let blob = std::fs::read(media("p4_bad.png"))?;
        assert!(matches!(probe(&blob), Err(ImageError::Unrecognized)));
        Ok(())
    }

    #[test]
    fn 空输入_返回无法识别() {
        assert!(matches!(probe(&[]), Err(ImageError::Unrecognized)));
        assert!(matches!(probe(b"\x89PNG"), Err(ImageError::Unrecognized)));
    }

    #[test]
    fn 截断png_返回无法识别() -> TestResult {
        let blob = std::fs::read(media("p4_dot2x1.png"))?;
        // 截断到 IHDR data 中间：签名可中、chunk 数据不可读
        assert!(matches!(probe(&blob[..16]), Err(ImageError::Unrecognized)));
        Ok(())
    }

    #[test]
    fn py_round_银行家舍入语义() {
        // 恰为 x.5：取偶数
        assert_eq!(py_round(0.5), 0.0);
        assert_eq!(py_round(1.5), 2.0);
        assert_eq!(py_round(2.5), 2.0);
        assert_eq!(py_round(3.5), 4.0);
        assert_eq!(py_round(-0.5), 0.0);
        assert_eq!(py_round(-1.5), -2.0);
        assert_eq!(py_round(-2.5), -2.0);
        assert_eq!(py_round(-4.5), -4.0);
        // 非 x.5：取最近值
        assert_eq!(py_round(2.675), 3.0);
        assert_eq!(py_round(71.889), 72.0);
        assert_eq!(py_round(28.349_999), 28.0);
        assert_eq!(py_round(4.4), 4.0);
        assert_eq!(py_round(4.6), 5.0);
        assert_eq!(py_round(-2.6), -3.0);
        assert_eq!(py_round(-2.4), -2.0);
    }
}
