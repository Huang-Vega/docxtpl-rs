//! InlineImage：内联图片值与 `wp:inline` XML 生成。
//!
//! 对齐 docxtpl 0.20.2 `inline_image.py`（拆 run 包装 + `new_pic_inline`）与
//! python-docx 1.2.0 的 pretty 序列化探针（2 空格/层缩进）。

use crate::image::{probe, py_round, ImageError, ImageInfo};

/// 内联图片值（上游 `InlineImage` 的数据部分）。
///
/// `width` / `height` 为 EMU（上游接受 `Length` 对象，由构造方换算）；
/// `anchor` 为外链 URL（其关系 rId 由渲染期的关系注册表分配，见 ADR-005）。
#[derive(Debug, Clone)]
pub struct InlineImage {
    /// 调用方给定的文件路径（仅用于取 basename）。
    pub path: String,
    /// 图片字节（构造时读入或显式给定）。
    pub blob: Vec<u8>,
    /// 宽（EMU）；`None` 表示按原生尺寸/按高换算。
    pub width: Option<i64>,
    /// 高（EMU）。
    pub height: Option<i64>,
    /// 外链 URL；`None` 表示无锚点。
    pub anchor: Option<String>,
}

impl InlineImage {
    /// 从文件路径构造：读入图片字节（上游在渲染期经 `open(path,'rb')` 读入）。
    ///
    /// 文件不存在或不可读时返回 [`std::io::Error`]。
    pub fn from_path(
        path: &str,
        width: Option<i64>,
        height: Option<i64>,
        anchor: Option<String>,
    ) -> std::io::Result<Self> {
        let blob = std::fs::read(path)?;
        Ok(Self::from_bytes(path, blob, width, height, anchor))
    }

    /// 从内存字节构造；`path_name` 仍参与 basename 计算。
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
        }
    }

    /// 文件 basename（含扩展名），对齐 `os.path.basename` 的取值语义。
    pub fn filename(&self) -> String {
        std::path::Path::new(&self.path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// 原生尺寸（EMU）：`int((px / dpi) * 914400)`（f64 除→乘→向零截断）。
///
/// `dpi==0` 时上游发生 ZeroDivisionError，此处归并为 [`ImageError::Unrecognized`]。
fn native_emu(px: u32, dpi: u32) -> Result<i64, ImageError> {
    if dpi == 0 {
        return Err(ImageError::Unrecognized);
    }
    Ok(((f64::from(px) / f64::from(dpi)) * 914400.0) as i64)
}

/// 缩放尺寸（python-docx `Image.scaled_dimensions`，单位 EMU）。
///
/// - `(None, None)` → 原生尺寸；
/// - 仅给一边时按纵横比换算另一边，换算用 Python round 的银行家舍入
///   （见 [`py_round`]）；
/// - 两边都给时原样使用，不做纵横比约束。
///
/// 换算所需的除数为零（上游 ZeroDivisionError 场景）时返回
/// [`ImageError::Unrecognized`]。
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
        // 两边都给：原样使用
        (Some(w), Some(h)) => Ok((w, h)),
    }
}

/// probe + 尺寸计算 + XML 生成一体化（上游 `InlineImage.__str__` 的产物）。
///
/// - `shape_id`：`wp:docPr` 的 id（渲染期对渲染前的原始 document 树一次性
///   算好传入，见 ADR-005）；
/// - `blip_rid`：`a:blip r:embed` 的图片关系 ID；
/// - `hyperlink_rid`：`anchor` 外链的关系 ID；`None` 表示无锚点，有值时
///   `wp:docPr` 与 `pic:cNvPr` 下各插入一个 `a:hlinkClick`。
///
/// 图片字节无法识别（或尺寸不可换算）时返回 [`ImageError::Unrecognized`]。
pub fn render_inline_image(
    img: &InlineImage,
    shape_id: i64,
    blip_rid: &str,
    hyperlink_rid: Option<&str>,
) -> Result<String, ImageError> {
    let info = probe(&img.blob)?;
    let (cx, cy) = scaled_dimensions(&info, img.width, img.height)?;
    let filename = img.filename();

    // 与上游 pretty 序列化探针逐字节一致（2 空格/层缩进）；
    // 有锚点时 docPr / cNvPr 由自闭合改为带 a:hlinkClick 子元素。
    let doc_pr = match hyperlink_rid {
        Some(rid) => format!(
            "  <wp:docPr id=\"{shape_id}\" name=\"Picture {shape_id}\">\n    <a:hlinkClick r:id=\"{rid}\"/>\n  </wp:docPr>"
        ),
        None => format!("  <wp:docPr id=\"{shape_id}\" name=\"Picture {shape_id}\"/>"),
    };
    let c_nv_pr = match hyperlink_rid {
        Some(rid) => format!(
            "          <pic:cNvPr id=\"0\" name=\"{filename}\">\n            <a:hlinkClick r:id=\"{rid}\"/>\n          </pic:cNvPr>"
        ),
        None => format!("          <pic:cNvPr id=\"0\" name=\"{filename}\"/>"),
    };
    let xml = format!(
        "</w:t></w:r><w:r><w:drawing><wp:inline xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\n  <wp:extent cx=\"{cx}\" cy=\"{cy}\"/>\n{doc_pr}\n  <wp:cNvGraphicFramePr>\n    <a:graphicFrameLocks noChangeAspect=\"1\"/>\n  </wp:cNvGraphicFramePr>\n  <a:graphic>\n    <a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\n      <pic:pic>\n        <pic:nvPicPr>\n{c_nv_pr}\n          <pic:cNvPicPr/>\n        </pic:nvPicPr>\n        <pic:blipFill>\n          <a:blip r:embed=\"{blip_rid}\"/>\n          <a:stretch>\n            <a:fillRect/>\n          </a:stretch>\n        </pic:blipFill>\n        <pic:spPr>\n          <a:xfrm>\n            <a:off x=\"0\" y=\"0\"/>\n            <a:ext cx=\"{cx}\" cy=\"{cy}\"/>\n          </a:xfrm>\n          <a:prstGeom prst=\"rect\"/>\n        </pic:spPr>\n      </pic:pic>\n    </a:graphicData>\n  </a:graphic>\n</wp:inline>\n</w:drawing></w:r><w:r><w:t xml:space=\"preserve\">"
    );
    Ok(xml)
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

    /// 与上游探针逐字符一致的期望输出（无锚点：shape_id=1、blip rId9）。
    const 期望_无锚点: &str = "</w:t></w:r><w:r><w:drawing><wp:inline xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\n  <wp:extent cx=\"25400\" cy=\"12700\"/>\n  <wp:docPr id=\"1\" name=\"Picture 1\"/>\n  <wp:cNvGraphicFramePr>\n    <a:graphicFrameLocks noChangeAspect=\"1\"/>\n  </wp:cNvGraphicFramePr>\n  <a:graphic>\n    <a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\n      <pic:pic>\n        <pic:nvPicPr>\n          <pic:cNvPr id=\"0\" name=\"p4_dot2x1.png\"/>\n          <pic:cNvPicPr/>\n        </pic:nvPicPr>\n        <pic:blipFill>\n          <a:blip r:embed=\"rId9\"/>\n          <a:stretch>\n            <a:fillRect/>\n          </a:stretch>\n        </pic:blipFill>\n        <pic:spPr>\n          <a:xfrm>\n            <a:off x=\"0\" y=\"0\"/>\n            <a:ext cx=\"25400\" cy=\"12700\"/>\n          </a:xfrm>\n          <a:prstGeom prst=\"rect\"/>\n        </pic:spPr>\n      </pic:pic>\n    </a:graphicData>\n  </a:graphic>\n</wp:inline>\n</w:drawing></w:r><w:r><w:t xml:space=\"preserve\">";

    /// 与上游探针逐字符一致的期望输出（锚点：hyperlink rId10）。
    const 期望_锚点: &str = "</w:t></w:r><w:r><w:drawing><wp:inline xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\n  <wp:extent cx=\"25400\" cy=\"12700\"/>\n  <wp:docPr id=\"1\" name=\"Picture 1\">\n    <a:hlinkClick r:id=\"rId10\"/>\n  </wp:docPr>\n  <wp:cNvGraphicFramePr>\n    <a:graphicFrameLocks noChangeAspect=\"1\"/>\n  </wp:cNvGraphicFramePr>\n  <a:graphic>\n    <a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\n      <pic:pic>\n        <pic:nvPicPr>\n          <pic:cNvPr id=\"0\" name=\"p4_dot2x1.png\">\n            <a:hlinkClick r:id=\"rId10\"/>\n          </pic:cNvPr>\n          <pic:cNvPicPr/>\n        </pic:nvPicPr>\n        <pic:blipFill>\n          <a:blip r:embed=\"rId9\"/>\n          <a:stretch>\n            <a:fillRect/>\n          </a:stretch>\n        </pic:blipFill>\n        <pic:spPr>\n          <a:xfrm>\n            <a:off x=\"0\" y=\"0\"/>\n            <a:ext cx=\"25400\" cy=\"12700\"/>\n          </a:xfrm>\n          <a:prstGeom prst=\"rect\"/>\n        </pic:spPr>\n      </pic:pic>\n    </a:graphicData>\n  </a:graphic>\n</wp:inline>\n</w:drawing></w:r><w:r><w:t xml:space=\"preserve\">";

    #[test]
    fn 原生尺寸_无缩放参数() -> TestResult {
        let info = probe(&std::fs::read(PNG_PATH)?)?;
        assert_eq!(scaled_dimensions(&info, None, None)?, (25400, 12700));
        Ok(())
    }

    #[test]
    fn 宽图150dpi_原生与按宽换算() -> TestResult {
        let info = probe(&std::fs::read(WIDE_PATH)?)?;
        // 原生：int((4/150)*914400) x int((1/150)*914400)
        assert_eq!(scaled_dimensions(&info, None, None)?, (24384, 6096));
        // 仅给宽：另一边按纵横比银行家舍入
        assert_eq!(
            scaled_dimensions(&info, Some(720000), None)?,
            (720000, 180000)
        );
        Ok(())
    }

    #[test]
    fn jpg_原生与两边换算() -> TestResult {
        let info = probe(&std::fs::read(JPG_PATH)?)?;
        // APP0 units=1、density=300：原生 int((4/300)*914400) x int((2/300)*914400)
        assert_eq!(scaled_dimensions(&info, None, None)?, (12192, 6096));
        assert_eq!(
            scaled_dimensions(&info, Some(360000), None)?,
            (360000, 180000)
        );
        // 两边都给：原样使用，不做纵横比约束
        assert_eq!(
            scaled_dimensions(&info, Some(360000), Some(108000))?,
            (360000, 108000)
        );
        // 仅给高：对称换算
        assert_eq!(
            scaled_dimensions(&info, None, Some(108000))?,
            (216000, 108000)
        );
        Ok(())
    }

    #[test]
    fn tiff_原生尺寸() -> TestResult {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/media/p4_tile4x2.tiff"
        );
        let info = probe(&std::fs::read(path)?)?;
        assert_eq!(scaled_dimensions(&info, None, None)?, (24384, 12192));
        Ok(())
    }

    #[test]
    fn dpi为零_返回无法识别() {
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
        // 两边都给时不计算原生尺寸，dpi 为零也不影响
        assert_eq!(
            scaled_dimensions(&info, Some(10), Some(5)).ok(),
            Some((10, 5))
        );
    }

    #[test]
    fn 从路径构造_读取字节并取basename() -> TestResult {
        let img = InlineImage::from_path(PNG_PATH, None, None, None)?;
        assert_eq!(img.filename(), "p4_dot2x1.png");
        assert_eq!(img.blob, std::fs::read(PNG_PATH)?);
        // 路径含目录分隔符时只取末段
        let img = InlineImage::from_bytes("some/dir/p4_dot2x1.png", vec![], None, None, None);
        assert_eq!(img.filename(), "p4_dot2x1.png");
        Ok(())
    }

    #[test]
    fn 渲染_无锚点_逐字符对齐上游() -> TestResult {
        let img = InlineImage::from_path(PNG_PATH, None, None, None)?;
        let xml = render_inline_image(&img, 1, "rId9", None)?;
        assert_eq!(xml, 期望_无锚点);
        Ok(())
    }

    #[test]
    fn 渲染_锚点_逐字符对齐上游() -> TestResult {
        let img =
            InlineImage::from_path(PNG_PATH, None, None, Some("https://example.com/".into()))?;
        let xml = render_inline_image(&img, 1, "rId9", Some("rId10"))?;
        assert_eq!(xml, 期望_锚点);
        Ok(())
    }

    // 测试名保留 OOXML 术语原名（wp:extent），不属于 snake_case；
    // 若项目改用英文 snake_case 测试命名规范，可移除此 allow。
    #[allow(non_snake_case)]
    #[test]
    fn 渲染_按宽缩放_Extent取换算值() -> TestResult {
        // 对应上游 InlineImage(tpl, p4_wide4x1.png, width=Mm(20))：
        // cx=720000，cy=180000
        let img = InlineImage::from_path(WIDE_PATH, Some(720000), None, None)?;
        let xml = render_inline_image(&img, 1, "rId9", None)?;
        assert_eq!(xml.matches("cx=\"720000\"").count(), 2);
        assert_eq!(xml.matches("cy=\"180000\"").count(), 2);
        assert!(xml.contains("name=\"p4_wide4x1.png\""));
        Ok(())
    }

    #[test]
    fn 渲染_坏图片_返回无法识别() -> TestResult {
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
